//! Video from the server: a file read by HTTP ranges, and HLS. The bytes come through [`Fetch`], so this crate knows nothing of TLS and tests
//! run without a network.
//!
//! The decoding thread must not wait on the network when it can help it: on one CPU a stall there is a stall on the screen. So what comes
//! next is fetched ahead of time, on a thread that spends its life waiting for the network, and costs nothing while it does.

use std::collections::{BTreeMap, HashSet};
use std::io::{self, Cursor, Read, Seek, SeekFrom};
use std::sync::{Arc, Condvar, Mutex};

use crate::demux::{Demuxer, Error, Packet, Track};
use crate::mp4::Mp4;

/// HTTP for this module.
pub trait Fetch: Send + Sync + 'static {
    /// `len` bytes at `start`, or fewer at the end of the file, with the file's whole size.
    fn range(&self, url: &str, start: u64, len: u64) -> io::Result<(Vec<u8>, u64)>;
    /// The whole of a small file: a playlist or a segment.
    fn get(&self, url: &str) -> io::Result<Vec<u8>>;
}

const CHUNK: u64 = 512 * 1024;
/// Chunks kept: what was read, and what is read next. 4 MiB: seconds of video at any rate this laptop plays.
const KEPT: usize = 8;
const AHEAD: u64 = 3;

#[derive(Default)]
struct Shared {
    chunks: BTreeMap<u64, Arc<Vec<u8>>>,
    fetching: HashSet<u64>,
    wanted: Vec<u64>,
    error: Option<String>,
    closing: bool,
}

/// A remote file as `Read + Seek`.
pub struct RangeReader {
    shared: Arc<(Mutex<Shared>, Condvar)>,
    pos: u64,
    total: u64,
}

impl RangeReader {
    /// Reads the first chunk, which also says how large the file is.
    pub fn open<F: Fetch>(fetch: Arc<F>, url: &str) -> io::Result<RangeReader> {
        let (first, total) = fetch.range(url, 0, CHUNK)?;
        let mut s = Shared::default();
        s.chunks.insert(0, Arc::new(first));
        let shared = Arc::new((Mutex::new(s), Condvar::new()));
        let (for_thread, url) = (shared.clone(), url.to_owned());
        std::thread::Builder::new().name("video-fetch".into()).spawn(move || fetch_loop(fetch, for_thread, url))?;
        Ok(RangeReader { shared, pos: 0, total })
    }

    fn chunk(&self, index: u64) -> io::Result<Arc<Vec<u8>>> {
        let (m, cv) = &*self.shared;
        let mut s = m.lock().map_err(|_| io::Error::other("the fetching thread died"))?;
        loop {
            // Whatever comes after this, in good time.
            let last = self.total.saturating_sub(1) / CHUNK;
            let mut woke = false;
            for i in index..=(index + AHEAD).min(last) {
                if !s.chunks.contains_key(&i) && !s.fetching.contains(&i) && !s.wanted.contains(&i) {
                    s.wanted.push(i);
                    woke = true;
                }
            }
            if woke {
                cv.notify_all();
            }
            if let Some(c) = s.chunks.get(&index) {
                let c = c.clone();
                // Keep what is near; a seek elsewhere lets the rest go.
                while s.chunks.len() > KEPT {
                    let far = s.chunks.keys().copied().max_by_key(|k| k.abs_diff(index) + if *k < index { CHUNK } else { 0 });
                    let Some(far) = far else { break };
                    s.chunks.remove(&far);
                }
                return Ok(c);
            }
            if let Some(e) = s.error.take() {
                return Err(io::Error::other(e));
            }
            s = cv.wait(s).map_err(|_| io::Error::other("the fetching thread died"))?;
        }
    }
}

fn fetch_loop<F: Fetch>(fetch: Arc<F>, shared: Arc<(Mutex<Shared>, Condvar)>, url: String) {
    let (m, cv) = &*shared;
    loop {
        let index = {
            let Ok(mut s) = m.lock() else { return };
            loop {
                if s.closing {
                    return;
                }
                if !s.wanted.is_empty() {
                    let i = s.wanted.remove(0);
                    s.fetching.insert(i);
                    break i;
                }
                let Ok(g) = cv.wait(s) else { return };
                s = g;
            }
        };
        let got = fetch.range(&url, index * CHUNK, CHUNK);
        let Ok(mut s) = m.lock() else { return };
        s.fetching.remove(&index);
        match got {
            Ok((bytes, _)) => {
                s.chunks.insert(index, Arc::new(bytes));
            }
            Err(e) => s.error = Some(e.to_string()),
        }
        cv.notify_all();
    }
}

impl Drop for RangeReader {
    fn drop(&mut self) {
        if let Ok(mut s) = self.shared.0.lock() {
            s.closing = true;
        }
        self.shared.1.notify_all();
    }
}

impl Read for RangeReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.total || buf.is_empty() {
            return Ok(0);
        }
        let (index, off) = (self.pos / CHUNK, (self.pos % CHUNK) as usize);
        let c = self.chunk(index)?;
        let Some(have) = c.get(off..) else { return Ok(0) };
        let n = have.len().min(buf.len());
        buf[..n].copy_from_slice(&have[..n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for RangeReader {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let p = match to {
            SeekFrom::Start(v) => v as i128,
            SeekFrom::End(v) => self.total as i128 + v as i128,
            SeekFrom::Current(v) => self.pos as i128 + v as i128,
        };
        self.pos = u64::try_from(p).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        Ok(self.pos)
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Segment {
    url: String,
    start_us: i64,
}

#[derive(Debug, PartialEq, Default)]
struct Playlist {
    init: Option<String>,
    segments: Vec<Segment>,
    ended: bool,
    duration_us: i64,
}

/// A media playlist, with its URLs made absolute against `base` (the playlist's own URL, up to and with the last `/`).
fn parse_playlist(text: &str, base: &str) -> Playlist {
    let mut p = Playlist::default();
    let mut next_dur = 0i64;
    let resolve = |u: &str| if u.contains("://") { u.to_owned() } else { format!("{base}{u}") };
    for line in text.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix("#EXT-X-MAP:") {
            if let Some(uri) = rest.split("URI=\"").nth(1).and_then(|r| r.split('"').next()) {
                p.init = Some(resolve(uri));
            }
        } else if let Some(rest) = line.strip_prefix("#EXTINF:") {
            let secs: f64 = rest.split(',').next().and_then(|s| s.trim().parse().ok()).unwrap_or(0.0);
            next_dur = (secs * 1_000_000.0) as i64;
        } else if line == "#EXT-X-ENDLIST" {
            p.ended = true;
        } else if !line.is_empty() && !line.starts_with('#') {
            p.segments.push(Segment { url: resolve(line), start_us: p.duration_us });
            p.duration_us += next_dur;
            next_dur = 0;
        }
    }
    p
}

/// Polls a playlist that is still being written.
const PLAYLIST_RETRY: std::time::Duration = std::time::Duration::from_millis(1000);
/// How long a transcode may leave nothing new before this gives up.
const PLAYLIST_PATIENCE: u32 = 60;

/// The server's HLS: fragmented MP4 segments, read in turn. Segments are fetched ahead, on a thread.
pub struct Hls<F: Fetch> {
    fetch: Arc<F>,
    url: String,
    base: String,
    playlist: Playlist,
    init: Vec<u8>,
    track: Track,
    at: usize,
    current: Option<Mp4<Cursor<Vec<u8>>>>,
    /// The next segment on its way.
    ahead: Option<(usize, std::thread::JoinHandle<io::Result<Vec<u8>>>)>,
    known_duration_us: Option<i64>,
}

impl<F: Fetch> Hls<F> {
    /// `duration_us` is what the server's `/play` said, since a playlist still being written does not know.
    pub fn open(fetch: Arc<F>, url: &str, duration_us: Option<i64>) -> Result<Hls<F>, Error> {
        let base = url.split('?').next().unwrap_or(url);
        let base = base.rfind('/').map_or(String::new(), |i| base[..=i].to_owned());
        let text = fetch.get(url)?;
        let playlist = parse_playlist(&String::from_utf8_lossy(&text), &base);
        let init_url = playlist.init.clone().ok_or(Error::Bad("an HLS playlist with no MP4 header"))?;
        let init = fetch.get(&init_url)?;
        let probe = Mp4::open(Cursor::new(init.clone()))?;
        let mut track = probe.track().clone();
        track.duration_us = duration_us.or(playlist.ended.then_some(playlist.duration_us));
        Ok(Hls { fetch, url: url.to_owned(), base, playlist, init, track, at: 0, current: None, ahead: None, known_duration_us: duration_us })
    }

    fn fetch_ahead(&mut self) {
        let next = self.at;
        if self.ahead.as_ref().is_some_and(|(i, _)| *i == next) {
            return;
        }
        let Some(s) = self.playlist.segments.get(next) else { return };
        let (fetch, url) = (self.fetch.clone(), s.url.clone());
        self.ahead = Some((next, std::thread::spawn(move || fetch.get(&url))));
    }

    fn segment(&mut self, index: usize) -> Result<Vec<u8>, Error> {
        if let Some((i, h)) = self.ahead.take() {
            if i == index {
                return h.join().map_err(|_| Error::Bad("the fetching thread died"))?.map_err(Error::Io);
            }
            // The thread goes on by itself and its answer is dropped.
        }
        Ok(self.fetch.get(&self.playlist.segments[index].url)?)
    }

    /// Moves to segment `at`, waiting while a transcode writes it. False at the end.
    fn enter(&mut self) -> Result<bool, Error> {
        let mut waited = 0;
        while self.at >= self.playlist.segments.len() {
            if self.playlist.ended {
                return Ok(false);
            }
            if waited >= PLAYLIST_PATIENCE {
                return Err(Error::Bad("the server stopped writing this video"));
            }
            std::thread::sleep(PLAYLIST_RETRY);
            waited += 1;
            let text = self.fetch.get(&self.url)?;
            self.playlist = parse_playlist(&String::from_utf8_lossy(&text), &self.base);
        }
        let bytes = self.segment(self.at)?;
        let mut all = Vec::with_capacity(self.init.len() + bytes.len());
        all.extend_from_slice(&self.init);
        all.extend_from_slice(&bytes);
        self.current = Some(Mp4::open(Cursor::new(all))?);
        self.at += 1;
        self.fetch_ahead();
        Ok(true)
    }
}

impl<F: Fetch> Demuxer for Hls<F> {
    fn track(&self) -> &Track {
        &self.track
    }

    fn next_packet(&mut self) -> Result<Option<Packet>, Error> {
        loop {
            if let Some(c) = &mut self.current {
                if let Some(p) = c.next_packet()? {
                    return Ok(Some(p));
                }
                self.current = None;
            }
            if !self.enter()? {
                return Ok(None);
            }
        }
    }

    fn seek_to(&mut self, pts_us: i64) -> Result<i64, Error> {
        // With a transcode that has not got that far, this lands on the last segment written.
        let s = &self.playlist.segments;
        let i = s.iter().rposition(|x| x.start_us <= pts_us).unwrap_or(0);
        self.at = i;
        self.current = None;
        self.ahead = None;
        if !self.enter()? {
            return Ok(self.known_duration_us.unwrap_or(0));
        }
        // A segment starts with a key frame: that is where this lands, as the segment itself says.
        let Some(c) = &mut self.current else { return Ok(0) };
        c.seek_to(i64::MIN)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct Fake {
        files: Mutex<BTreeMap<String, Vec<u8>>>,
        ranges: AtomicUsize,
        fail_after: Mutex<Option<usize>>,
    }

    impl Fetch for Fake {
        fn range(&self, url: &str, start: u64, len: u64) -> io::Result<(Vec<u8>, u64)> {
            let n = self.ranges.fetch_add(1, Ordering::SeqCst);
            if self.fail_after.lock().unwrap().is_some_and(|f| n >= f) {
                return Err(io::Error::other("the network went away"));
            }
            let files = self.files.lock().unwrap();
            let f = files.get(url).ok_or(io::ErrorKind::NotFound)?;
            let s = (start as usize).min(f.len());
            let e = (s + len as usize).min(f.len());
            Ok((f[s..e].to_vec(), f.len() as u64))
        }
        fn get(&self, url: &str) -> io::Result<Vec<u8>> {
            self.files.lock().unwrap().get(url).cloned().ok_or_else(|| io::ErrorKind::NotFound.into())
        }
    }

    fn pattern(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i % 251) as u8).collect()
    }

    #[test]
    fn a_remote_file_reads_like_a_local_one() {
        let f = Arc::new(Fake::default());
        let data = pattern(3 * CHUNK as usize + 1234);
        f.files.lock().unwrap().insert("u".into(), data.clone());
        let mut r = RangeReader::open(f.clone(), "u").unwrap();
        let mut got = Vec::new();
        r.read_to_end(&mut got).unwrap();
        assert_eq!(got, data);
        // Across a chunk's edge, after a seek.
        r.seek(SeekFrom::Start(CHUNK - 3)).unwrap();
        let mut b = [0u8; 6];
        r.read_exact(&mut b).unwrap();
        assert_eq!(b, data[CHUNK as usize - 3..CHUNK as usize + 3]);
        assert_eq!(r.seek(SeekFrom::End(-1)).unwrap(), data.len() as u64 - 1);
        assert!(r.seek(SeekFrom::Current(-(data.len() as i64) - 5)).is_err());
    }

    #[test]
    fn memory_stays_bounded_over_a_long_file() {
        let f = Arc::new(Fake::default());
        let data = pattern(40 * CHUNK as usize);
        f.files.lock().unwrap().insert("u".into(), data.clone());
        let mut r = RangeReader::open(f.clone(), "u").unwrap();
        let mut buf = vec![0u8; 64 * 1024];
        let mut read = 0;
        loop {
            let n = r.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            assert_eq!(buf[..n], data[read..read + n]);
            read += n;
            assert!(r.shared.0.lock().unwrap().chunks.len() <= KEPT + AHEAD as usize + 1);
        }
        assert_eq!(read, data.len());
    }

    #[test]
    fn a_network_failure_is_an_error_not_a_hang() {
        let f = Arc::new(Fake::default());
        f.files.lock().unwrap().insert("u".into(), pattern(10 * CHUNK as usize));
        let mut r = RangeReader::open(f.clone(), "u").unwrap();
        *f.fail_after.lock().unwrap() = Some(0);
        r.seek(SeekFrom::Start(5 * CHUNK)).unwrap();
        let mut b = [0u8; 16];
        assert!(r.read(&mut b).is_err());
    }

    #[test]
    fn reads_a_playlist() {
        let text = "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:6\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:6.0,\nseg_0.m4s\n#EXTINF:4.5,\nseg_1.m4s\n#EXT-X-ENDLIST\n";
        let p = parse_playlist(text, "https://x/hls/v/");
        assert_eq!(p.init.as_deref(), Some("https://x/hls/v/init.mp4"));
        assert_eq!(p.segments.len(), 2);
        assert_eq!(p.segments[1], Segment { url: "https://x/hls/v/seg_1.m4s".into(), start_us: 6_000_000 });
        assert!(p.ended);
        assert_eq!(p.duration_us, 10_500_000);
        let open = parse_playlist("#EXTM3U\n#EXTINF:2,\nseg_0.m4s\n", "b/");
        assert!(!open.ended);
        assert_eq!(parse_playlist("garbage # here\n", "b/").segments.len(), 1);
        assert_eq!(parse_playlist("", "b/"), Playlist::default());
    }

    /// Two "segments", each a whole fragmented file's `moof` and `mdat`, as ffmpeg's `-f hls -hls_segment_type fmp4` writes them.
    fn hls_server() -> Arc<Fake> {
        let bytes = std::fs::read(format!("{}/tests/fixtures/frag.mp4", env!("CARGO_MANIFEST_DIR"))).unwrap();
        // The fixture is ftyp, moov, then pairs of moof and mdat.
        let mut pos = 0;
        let mut boxes = Vec::new();
        while pos + 8 <= bytes.len() {
            let size = u32::from_be_bytes(bytes[pos..pos + 4].try_into().unwrap()) as usize;
            boxes.push((bytes[pos + 4..pos + 8].to_vec(), pos, size));
            pos += size;
        }
        let moof = boxes.iter().position(|b| b.0 == b"moof").unwrap();
        let init = bytes[..boxes[moof].1].to_vec();
        let f = Arc::new(Fake::default());
        let mut files = f.files.lock().unwrap();
        files.insert("h/init.mp4".into(), init);
        let mut list = "#EXTM3U\n#EXT-X-MAP:URI=\"init.mp4\"\n".to_owned();
        let mut n = 0;
        // The file may end with an index of its own, which no segment holds.
        for pair in boxes[moof..].chunks_exact(2).filter(|p| p[0].0 == b"moof" && p[1].0 == b"mdat") {
            let (s, e) = (pair[0].1, pair[1].1 + pair[1].2);
            files.insert(format!("h/seg_{n}.m4s"), bytes[s..e].to_vec());
            list.push_str(&format!("#EXTINF:0.5,\nseg_{n}.m4s\n"));
            n += 1;
        }
        list.push_str("#EXT-X-ENDLIST\n");
        files.insert("h/index.m3u8".into(), list.into_bytes());
        drop(files);
        f
    }

    #[test]
    fn hls_reads_every_frame_in_turn_and_seeks_by_segment() {
        let f = hls_server();
        let mut d = Hls::open(f, "h/index.m3u8", None).unwrap();
        assert_eq!(d.track().codec, crate::Codec::H264);
        assert_eq!(d.track().width, 64);
        assert!(d.track().duration_us.is_some());
        let mut times = Vec::new();
        while let Some(p) = d.next_packet().unwrap() {
            times.push(p.pts_us / 100_000);
        }
        assert_eq!(times, (0..12).collect::<Vec<_>>());
        let at = d.seek_to(700_000).unwrap();
        assert_eq!(at / 100_000, 5);
        let p = d.next_packet().unwrap().unwrap();
        assert!(p.key);
        assert_eq!(p.pts_us / 100_000, 5);
    }

    /// `tests/fixtures/hls/` is what the server's own ffmpeg arguments write for Juno (`secretary-media-stream` `ffmpeg_args`).
    #[test]
    fn reads_what_the_servers_ffmpeg_writes() {
        let dir = format!("{}/tests/fixtures/hls", env!("CARGO_MANIFEST_DIR"));
        let f = Arc::new(Fake::default());
        for e in std::fs::read_dir(&dir).unwrap().flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            f.files.lock().unwrap().insert(format!("s/{name}"), std::fs::read(e.path()).unwrap());
        }
        let mut d = Hls::open(f, "s/index.m3u8", Some(4_000_000)).unwrap();
        assert_eq!(d.track().codec, crate::Codec::H264);
        assert_eq!((d.track().width, d.track().height), (64, 48));
        let mut times = Vec::new();
        let mut keys = 0;
        while let Some(p) = d.next_packet().unwrap() {
            // Annex B by now, with the stream's parameter sets in front of a key frame.
            assert!(p.data.starts_with(&[0, 0, 0, 1]));
            keys += p.key as u32;
            times.push(p.pts_us / 100_000);
        }
        assert_eq!(times, (0..40).collect::<Vec<_>>());
        assert!(keys >= 3, "a key frame per segment: {keys}");
        let at = d.seek_to(2_500_000).unwrap();
        assert_eq!(at / 100_000, 20);
        assert_eq!(d.next_packet().unwrap().unwrap().pts_us / 100_000, 20);
    }

    #[test]
    fn a_playlist_still_being_written_is_waited_for() {
        let f = hls_server();
        let list = String::from_utf8(f.get("h/index.m3u8").unwrap()).unwrap();
        let first_only: String = list.lines().take(4).map(|l| format!("{l}\n")).collect();
        f.files.lock().unwrap().insert("h/index.m3u8".into(), first_only.into_bytes());
        let mut d = Hls::open(f.clone(), "h/index.m3u8", Some(1_200_000)).unwrap();
        assert_eq!(d.track().duration_us, Some(1_200_000));
        for _ in 0..5 {
            assert!(d.next_packet().unwrap().is_some());
        }
        // The rest of the list appears while the reader waits.
        let (f2, full) = (f.clone(), list);
        let t = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(200));
            f2.files.lock().unwrap().insert("h/index.m3u8".into(), full.into_bytes());
        });
        assert!(d.next_packet().unwrap().is_some());
        t.join().unwrap();
    }
}
