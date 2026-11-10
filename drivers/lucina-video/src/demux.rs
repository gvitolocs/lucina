//! Containers into packets. MP4 (plain and fragmented), Matroska and WebM, IVF, and raw H.264.

use std::io::{self, Read, Seek, SeekFrom};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    H264,
    Hevc,
    Vp8,
    Vp9,
    Av1,
    Mpeg2,
    Other,
}

impl Codec {
    pub fn name(self) -> &'static str {
        match self {
            Codec::H264 => "H.264",
            Codec::Hevc => "H.265",
            Codec::Vp8 => "VP8",
            Codec::Vp9 => "VP9",
            Codec::Av1 => "AV1",
            Codec::Mpeg2 => "MPEG-2",
            Codec::Other => "unknown",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Track {
    pub codec: Codec,
    pub width: u32,
    pub height: u32,
    pub duration_us: Option<i64>,
    /// `avcC` for H.264 in MP4 and Matroska: packets are then length-prefixed.
    pub config: Vec<u8>,
    /// The container's audio codec name, if there is sound in the file.
    pub audio: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Packet {
    pub data: Vec<u8>,
    pub pts_us: i64,
    pub key: bool,
}

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    /// Not a container this knows.
    Unknown,
    /// A container this knows with something wrong inside.
    Bad(&'static str),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(e) => write!(f, "cannot read: {e}"),
            Error::Unknown => write!(f, "not a video file this player knows (MP4, MKV, WebM, IVF, H.264)"),
            Error::Bad(what) => write!(f, "damaged file: {what}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Error {
        Error::Io(e)
    }
}

/// A file's video track as packets in decode order.
pub trait Demuxer: Send {
    fn track(&self) -> &Track;
    /// The next packet; `None` at the end.
    fn next_packet(&mut self) -> Result<Option<Packet>, Error>;
    /// Move to the last key frame at or before `pts_us`, and say what time that is.
    fn seek_to(&mut self, pts_us: i64) -> Result<i64, Error>;
}

/// Reads what kind of file this is and opens it.
pub fn open<R: Read + Seek + Send + 'static>(mut r: R) -> Result<Box<dyn Demuxer>, Error> {
    let mut head = [0u8; 12];
    let n = read_up_to(&mut r, &mut head)?;
    r.seek(SeekFrom::Start(0))?;
    let head = &head[..n];
    if head.starts_with(b"DKIF") {
        return Ok(Box::new(crate::ivf::Ivf::open(r)?));
    }
    if head.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]) {
        return Ok(Box::new(crate::mkv::Mkv::open(r)?));
    }
    if head.len() >= 8 && matches!(&head[4..8], b"ftyp" | b"moov" | b"moof" | b"styp" | b"free" | b"mdat" | b"skip") {
        return Ok(Box::new(crate::mp4::Mp4::open(r)?));
    }
    if head.starts_with(&[0, 0, 0, 1]) || head.starts_with(&[0, 0, 1]) {
        return Ok(Box::new(crate::ivf::AnnexB::open(r)?));
    }
    Err(Error::Unknown)
}

pub(crate) fn read_up_to(r: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

/// A length-prefixed H.264 packet in the Annex B form decoders read, with the stream's parameter sets in front of a key frame.
pub fn annexb_from_avcc(data: &[u8], avcc: &[u8], key: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 64);
    if key {
        out.extend_from_slice(&parameter_sets(avcc));
    }
    let len_size = avcc.get(4).map_or(4, |b| (b & 3) as usize + 1);
    let mut at = 0;
    while at + len_size <= data.len() {
        let n = data[at..at + len_size].iter().fold(0usize, |v, b| v << 8 | *b as usize);
        at += len_size;
        if n == 0 || at + n > data.len() {
            break;
        }
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&data[at..at + n]);
        at += n;
    }
    out
}

/// A length-prefixed H.265 packet in the Annex B form decoders read, with the stream's parameter sets in front of a key frame.
pub fn annexb_from_hvcc(data: &[u8], hvcc: &[u8], key: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 64);
    if key {
        out.extend_from_slice(&parameter_sets_hvcc(hvcc));
    }
    let len_size = hvcc.get(21).map_or(4, |b| (b & 3) as usize + 1);
    let mut at = 0;
    while at + len_size <= data.len() {
        let n = data[at..at + len_size].iter().fold(0usize, |v, b| v << 8 | *b as usize);
        at += len_size;
        if n == 0 || at + n > data.len() {
            break;
        }
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&data[at..at + n]);
        at += n;
    }
    out
}

fn parameter_sets(avcc: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let Some(&sps_count) = avcc.get(5) else { return out };
    let mut at = 6;
    let mut copy = |count: usize, at: &mut usize| {
        for _ in 0..count {
            let Some(len) = avcc.get(*at..*at + 2) else { return };
            let n = u16::from_be_bytes([len[0], len[1]]) as usize;
            *at += 2;
            let Some(s) = avcc.get(*at..*at + n) else { return };
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(s);
            *at += n;
        }
    };
    copy((sps_count & 0x1f) as usize, &mut at);
    let pps_count = avcc.get(at).copied().unwrap_or(0) as usize;
    at += 1;
    copy(pps_count, &mut at);
    out
}

fn parameter_sets_hvcc(hvcc: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    if hvcc.len() < 23 {
        return out;
    }
    let num_of_arrays = hvcc[22] as usize;
    let mut at = 23;
    for _ in 0..num_of_arrays {
        if at + 3 > hvcc.len() {
            break;
        }
        // hvcc[at] is the array's NAL unit type; every array is copied.
        let num_nalus = u16::from_be_bytes([hvcc[at + 1], hvcc[at + 2]]) as usize;
        at += 3;
        for _ in 0..num_nalus {
            if at + 2 > hvcc.len() {
                break;
            }
            let nal_unit_length = u16::from_be_bytes([hvcc[at], hvcc[at + 1]]) as usize;
            at += 2;
            if at + nal_unit_length > hvcc.len() {
                break;
            }
            let nal_unit = &hvcc[at..at + nal_unit_length];
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(nal_unit);
            at += nal_unit_length;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn fixture(name: &str) -> Box<dyn Demuxer> {
        let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
        open(Cursor::new(std::fs::read(path).unwrap())).unwrap()
    }

    fn all(d: &mut Box<dyn Demuxer>) -> Vec<Packet> {
        std::iter::from_fn(|| d.next_packet().unwrap()).collect()
    }

    fn times(p: &[Packet]) -> Vec<i64> {
        let mut t: Vec<i64> = p.iter().map(|p| p.pts_us / 100_000).collect();
        t.sort_unstable();
        t
    }

    #[test]
    fn plain_mp4_with_b_frames_and_sound() {
        let mut d = fixture("plain.mp4");
        let t = d.track().clone();
        assert_eq!((t.codec, t.width, t.height), (Codec::H264, 64, 48));
        assert_eq!(t.audio.as_deref(), Some("aac"));
        assert!(t.duration_us.unwrap() > 1_000_000);
        let p = all(&mut d);
        assert_eq!(p.len(), 12);
        assert!(p[0].key);
        // Decode order differs from display order, and every time is there once.
        assert!(p.windows(2).any(|w| w[1].pts_us < w[0].pts_us));
        assert_eq!(times(&p), (0..12).collect::<Vec<_>>());
    }

    #[test]
    fn fragmented_mp4() {
        let mut d = fixture("frag.mp4");
        assert_eq!(d.track().codec, Codec::H264);
        let p = all(&mut d);
        assert_eq!(p.len(), 12);
        assert_eq!(times(&p), (0..12).collect::<Vec<_>>());
        assert!(p[0].key && p[5].key && !p[1].key);
        assert_eq!(d.seek_to(700_000).unwrap() / 100_000, 5);
        assert_eq!(d.next_packet().unwrap().unwrap().pts_us / 100_000, 5);
    }

    #[test]
    fn matroska_h264() {
        let mut d = fixture("h264.mkv");
        let t = d.track().clone();
        assert_eq!((t.codec, t.width, t.height), (Codec::H264, 64, 48));
        assert!(!t.config.is_empty());
        let p = all(&mut d);
        assert_eq!(p.len(), 12);
        assert_eq!(times(&p), (0..12).collect::<Vec<_>>());
        // Back to a key frame, not past it.
        let at = d.seek_to(650_000).unwrap();
        assert!(at <= 650_000);
        let first = d.next_packet().unwrap().unwrap();
        assert!(first.key);
        assert_eq!(first.pts_us, at);
    }

    #[test]
    fn webm_vp9() {
        let mut d = fixture("vp9.webm");
        assert_eq!(d.track().codec, Codec::Vp9);
        let p = all(&mut d);
        assert!(p.len() >= 12);
        assert!(p[0].key);
        assert_eq!(d.seek_to(0).unwrap(), 0);
    }

    #[test]
    fn av1_in_mp4_and_ivf() {
        let mut d = fixture("av1.mp4");
        assert_eq!(d.track().codec, Codec::Av1);
        assert!(!all(&mut d).is_empty());
        let mut d = fixture("av1.ivf");
        assert_eq!((d.track().codec, d.track().width), (Codec::Av1, 64));
        assert!(!all(&mut d).is_empty());
        assert_eq!(d.seek_to(1_000_000_000).unwrap() >= 0, true);
    }

    #[test]
    fn raw_h264_is_cut_into_pictures() {
        let mut d = fixture("annexb.h264");
        assert_eq!(d.track().codec, Codec::H264);
        let p = all(&mut d);
        assert_eq!(p.len(), 12);
        assert!(p[0].key);
    }

    #[test]
    fn seeking_a_plain_mp4_lands_on_a_key_frame_before_the_time() {
        let mut d = fixture("plain.mp4");
        let at = d.seek_to(700_000).unwrap();
        assert!(at <= 700_000);
        let p = d.next_packet().unwrap().unwrap();
        assert!(p.key);
        assert_eq!(p.pts_us, at);
        // Past the end: the last key frame, not nothing.
        let at = d.seek_to(100_000_000).unwrap();
        assert!(at > 0);
        assert!(d.next_packet().unwrap().is_some());
    }

    #[test]
    fn length_prefixes_become_start_codes() {
        // Version 1, profile, compat, level, 4-byte lengths, one SPS "AB", one PPS "C".
        let avcc = [1, 100, 0, 40, 0xff, 0xe1, 0, 2, b'A', b'B', 1, 0, 1, b'C'];
        let data = [0, 0, 0, 2, 9, 9, 0, 0, 0, 1, 7];
        assert_eq!(annexb_from_avcc(&data, &avcc, false), [0, 0, 0, 1, 9, 9, 0, 0, 0, 1, 7]);
        let key = annexb_from_avcc(&data, &avcc, true);
        assert!(key.starts_with(&[0, 0, 0, 1, b'A', b'B', 0, 0, 0, 1, b'C', 0, 0, 0, 1, 9, 9]));
        // Nonsense does not panic.
        assert_eq!(annexb_from_avcc(&[0, 0, 0, 99, 1], &avcc, false), Vec::<u8>::new());
        assert!(annexb_from_avcc(&data, &[1, 2], true).len() > 0);
    }

    #[test]
    fn hevc_mp4() {
        let mut d = fixture("hevc.mp4");
        let t = d.track().clone();
        assert_eq!((t.codec, t.width, t.height), (Codec::Hevc, 64, 48));
        assert!(!t.config.is_empty());
        let p = all(&mut d);
        assert_eq!(p.len(), 2);
        assert!(p[0].key && !p[1].key);
        assert_eq!(
            p[0].data,
            [
                0, 0, 0, 1, 0xaa, 0xbb, 0, 0, 0, 1, 0xcc, 0xdd, 0, 0, 0, 1, 0xee, // VPS, SPS, PPS
                0, 0, 0, 1, 9, 9, 0, 0, 0, 1, 7,
            ]
        );
        assert_eq!(p[1].data, [0, 0, 0, 1, 8]);
    }

    #[test]
    fn hvcc_length_prefixes_become_start_codes() {
        // A 23-byte hvcC header: byte 21 = 0xff (4-byte lengths), byte 22 = 3 arrays,
        // then one VPS (type 32), one SPS (type 33) and one PPS (type 34), one NAL each.
        let hvcc = [
            1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 3,
            32, 0, 1, 0, 2, 0xaa, 0xbb,
            33, 0, 1, 0, 2, 0xcc, 0xdd,
            34, 0, 1, 0, 1, 0xee,
        ];
        let data = [0, 0, 0, 2, 9, 9, 0, 0, 0, 1, 7];
        assert_eq!(annexb_from_hvcc(&data, &hvcc, false), [0, 0, 0, 1, 9, 9, 0, 0, 0, 1, 7]);
        let key = annexb_from_hvcc(&data, &hvcc, true);
        assert_eq!(
            key,
            [
                0, 0, 0, 1, 0xaa, 0xbb, // VPS
                0, 0, 0, 1, 0xcc, 0xdd, // SPS
                0, 0, 0, 1, 0xee, // PPS
                0, 0, 0, 1, 9, 9, 0, 0, 0, 1, 7,
            ]
        );
        // Byte 21 low bits = 1: 2-byte lengths.
        let mut hvcc2 = hvcc;
        hvcc2[21] = 0x01;
        let data2 = [0, 2, 9, 9, 0, 1, 7];
        assert_eq!(annexb_from_hvcc(&data2, &hvcc2, false), [0, 0, 0, 1, 9, 9, 0, 0, 0, 1, 7]);
        // Nonsense does not panic, and what came before is kept.
        assert_eq!(annexb_from_hvcc(&[0, 0, 0, 99, 1], &hvcc, false), Vec::<u8>::new());
        assert_eq!(annexb_from_hvcc(&data, &[1, 2], true), [0, 0, 0, 1, 9, 9, 0, 0, 0, 1, 7]);
        // Three arrays declared but the second is cut short: the VPS is still there.
        let mut cut = hvcc[..23 + 7 + 1].to_vec();
        cut[22] = 3;
        let key = annexb_from_hvcc(&data, &cut, true);
        assert!(key.starts_with(&[0, 0, 0, 1, 0xaa, 0xbb, 0, 0, 0, 1, 9, 9]));
    }

    #[test]
    fn rubbish_is_refused_and_cut_files_do_not_panic() {
        assert!(matches!(open(Cursor::new(b"hello world!!".to_vec())), Err(Error::Unknown)));
        assert!(matches!(open(Cursor::new(Vec::new())), Err(Error::Unknown)));
        for name in ["plain.mp4", "frag.mp4", "h264.mkv", "vp9.webm", "av1.ivf", "annexb.h264"] {
            let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
            let bytes = std::fs::read(path).unwrap();
            for n in (0..bytes.len()).step_by(bytes.len() / 80 + 1) {
                if let Ok(mut d) = open(Cursor::new(bytes[..n].to_vec())) {
                    for _ in 0..50 {
                        match d.next_packet() {
                            Ok(Some(_)) => {}
                            _ => break,
                        }
                    }
                    let _ = d.seek_to(500_000);
                }
            }
            // Flipped bytes either.
            let mut bad = bytes.clone();
            for i in (0..bad.len()).step_by(37) {
                bad[i] ^= 0x55;
            }
            if let Ok(mut d) = open(Cursor::new(bad)) {
                for _ in 0..50 {
                    match d.next_packet() {
                        Ok(Some(_)) => {}
                        _ => break,
                    }
                }
            }
        }
    }
}
