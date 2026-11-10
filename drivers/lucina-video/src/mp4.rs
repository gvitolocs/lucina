//! MP4: a sample table for a plain file, `moof` boxes read in turn for a fragmented one, which is what the server's HLS sends.

use std::io::{Read, Seek, SeekFrom};

use crate::demux::{Codec, Demuxer, Error, Packet, Track, annexb_from_avcc, annexb_from_hvcc, read_up_to};

/// More than any real `moov` or `moof`. A file saying otherwise is damaged.
pub(crate) const MAX_META: u64 = 64 << 20;
/// More than any real film has frames: over two days at 24 a second. Each costs 32 bytes in the table.
pub(crate) const MAX_SAMPLES: usize = 5_000_000;
/// A frame larger than any this CPU could decode.
pub(crate) const MAX_FRAME: u64 = 32 << 20;

#[derive(Clone, Copy, Debug)]
struct Sample {
    offset: u64,
    size: u32,
    pts_us: i64,
    key: bool,
}

pub(crate) fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

pub(crate) fn be64(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_be_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

/// The boxes inside a box's body: `(kind, body)`.
pub(crate) fn boxes(mut b: &[u8]) -> impl Iterator<Item = ([u8; 4], &[u8])> {
    std::iter::from_fn(move || {
        let size = be32(b, 0)? as usize;
        let kind: [u8; 4] = b.get(4..8)?.try_into().ok()?;
        let (head, size) = match size {
            0 => (8, b.len()),
            1 => (16, usize::try_from(be64(b, 8)?).ok()?),
            n => (8, n),
        };
        if size < head || size > b.len() {
            return None;
        }
        let body = &b[head..size];
        b = &b[size..];
        Some((kind, body))
    })
}

pub(crate) fn find<'a>(b: &'a [u8], kind: &[u8; 4]) -> Option<&'a [u8]> {
    boxes(b).find(|(k, _)| k == kind).map(|(_, v)| v)
}

/// What a `trak` holds that this needs.
pub(crate) struct Trak<'a> {
    pub(crate) timescale: u32,
    pub(crate) duration: u64,
    pub(crate) handler: [u8; 4],
    pub(crate) stsd: &'a [u8],
    pub(crate) stbl: &'a [u8],
    pub(crate) track_id: u32,
    pub(crate) elst_start: Option<i64>,
}

pub(crate) fn parse_trak(t: &[u8]) -> Option<Trak<'_>> {
    let mdia = find(t, b"mdia")?;
    let mdhd = find(mdia, b"mdhd")?;
    let (timescale, duration) = if mdhd.first()? == &1 { (be32(mdhd, 20)?, be64(mdhd, 24)?) } else { (be32(mdhd, 12)?, be32(mdhd, 16)? as u64) };
    let hdlr = find(mdia, b"hdlr")?;
    let handler: [u8; 4] = hdlr.get(8..12)?.try_into().ok()?;
    let stbl = find(find(mdia, b"minf")?, b"stbl")?;
    let stsd = find(stbl, b"stsd")?;
    let tkhd = find(t, b"tkhd")?;
    let track_id = if tkhd.first()? == &1 { be32(tkhd, 20)? } else { be32(tkhd, 12)? };
    // An edit list's start: B-frames push the first displayed frame's time past zero, and this takes it back.
    let elst_start = find(t, b"edts").and_then(|e| find(e, b"elst")).and_then(|e| {
        if e.first()? == &1 { Some(be64(e, 16)? as i64) } else { Some(be32(e, 12)? as i32 as i64) }
    });
    Some(Trak { timescale, duration, handler, stsd, stbl, track_id, elst_start })
}

/// The codec and shape from `stsd`, and the bytes the codec needs.
pub(crate) fn parse_stsd(stsd: &[u8]) -> Option<(Codec, u32, u32, Vec<u8>)> {
    let entries = stsd.get(8..)?;
    let (kind, body) = boxes(entries).next()?;
    let codec = match &kind {
        b"avc1" | b"avc3" => Codec::H264,
        b"hvc1" | b"hev1" => Codec::Hevc,
        b"vp09" => Codec::Vp9,
        b"vp08" => Codec::Vp8,
        b"av01" => Codec::Av1,
        _ => Codec::Other,
    };
    let w = u16::from_be_bytes(body.get(24..26)?.try_into().ok()?) as u32;
    let h = u16::from_be_bytes(body.get(26..28)?.try_into().ok()?) as u32;
    let config = match &kind {
        b"avc1" | b"avc3" => body.get(78..).and_then(|rest| find(rest, b"avcC")).map(<[u8]>::to_vec).unwrap_or_default(),
        b"hvc1" | b"hev1" => body.get(78..).and_then(|rest| find(rest, b"hvcC")).map(<[u8]>::to_vec).unwrap_or_default(),
        _ => Vec::new(),
    };
    Some((codec, w, h, config))
}

fn audio_name(stsd: &[u8]) -> Option<String> {
    let (kind, _) = boxes(stsd.get(8..)?).next()?;
    Some(match &kind {
        b"mp4a" => "aac".to_owned(),
        b"Opus" => "opus".to_owned(),
        b"ac-3" => "ac3".to_owned(),
        b"ec-3" => "eac3".to_owned(),
        b"fLaC" => "flac".to_owned(),
        other => String::from_utf8_lossy(other).trim().to_owned(),
    })
}

/// The sample table of a plain file.
fn parse_samples(t: &Trak) -> Option<Vec<Sample>> {
    let s = t.stbl;
    let scale = t.timescale.max(1) as i128;
    let stsz = find(s, b"stsz")?;
    let fixed = be32(stsz, 4)?;
    let count = be32(stsz, 8)? as usize;
    if count > MAX_SAMPLES {
        return None;
    }
    let size_at = |i: usize| if fixed != 0 { Some(fixed) } else { be32(stsz, 12 + i * 4) };
    // Chunk offsets.
    let mut chunks: Vec<u64> = Vec::new();
    if let Some(stco) = find(s, b"stco") {
        for i in 0..be32(stco, 4)? as usize {
            chunks.push(be32(stco, 8 + i * 4)? as u64);
        }
    } else {
        let co64 = find(s, b"co64")?;
        for i in 0..be32(co64, 4)? as usize {
            chunks.push(be64(co64, 8 + i * 8)?);
        }
    }
    let stsc = find(s, b"stsc")?;
    let stsc_n = be32(stsc, 4)? as usize;
    let stts = find(s, b"stts")?;
    let stts_n = be32(stts, 4)? as usize;
    let ctts = find(s, b"ctts");
    let stss = find(s, b"stss");
    let mut keys: Vec<u32> = Vec::new();
    if let Some(ss) = stss {
        for i in 0..be32(ss, 4)? as usize {
            keys.push(be32(ss, 8 + i * 4)?);
        }
    }

    let mut out = Vec::with_capacity(count.min(1 << 16));
    let (mut i, mut stsc_at, mut dts) = (0usize, 0usize, 0i64);
    let (mut stts_at, mut stts_left, mut stts_delta) = (0usize, 0u32, 0i64);
    let (mut ctts_at, mut ctts_left, mut ctts_off) = (0usize, 0u32, 0i64);
    let mut next_key = 0usize;
    let shift = t.elst_start.unwrap_or(0);
    'chunks: for (c, &chunk_off) in chunks.iter().enumerate() {
        while stsc_at + 1 < stsc_n && (be32(stsc, 8 + (stsc_at + 1) * 12)? as usize) <= c + 1 {
            stsc_at += 1;
        }
        let per_chunk = be32(stsc, 8 + stsc_at * 12 + 4)? as usize;
        let mut offset = chunk_off;
        for _ in 0..per_chunk {
            if i >= count {
                break 'chunks;
            }
            let size = size_at(i)?;
            while stts_left == 0 && stts_at < stts_n {
                stts_left = be32(stts, 8 + stts_at * 8)?;
                stts_delta = be32(stts, 12 + stts_at * 8)? as i64;
                stts_at += 1;
            }
            let mut pts = dts;
            if let Some(c) = ctts {
                while ctts_left == 0 && ctts_at < be32(c, 4)? as usize {
                    ctts_left = be32(c, 8 + ctts_at * 8)?;
                    ctts_off = be32(c, 12 + ctts_at * 8)? as i32 as i64;
                    ctts_at += 1;
                }
                pts += ctts_off;
                ctts_left = ctts_left.saturating_sub(1);
            }
            let key = if stss.is_some() {
                while next_key < keys.len() && (keys[next_key] as usize) < i + 1 {
                    next_key += 1;
                }
                keys.get(next_key).is_some_and(|k| *k as usize == i + 1)
            } else {
                true
            };
            out.push(Sample { offset, size, pts_us: ((pts - shift) as i128 * 1_000_000 / scale) as i64, key });
            offset += size as u64;
            dts += stts_delta;
            stts_left = stts_left.saturating_sub(1);
            i += 1;
        }
    }
    Some(out)
}

pub struct Mp4<R> {
    r: R,
    track: Track,
    track_id: u32,
    timescale: u32,
    samples: Vec<Sample>,
    at: usize,
    /// Fragmented: where the next `moof` may start. `None` for a plain file.
    fragments: Option<u64>,
    file_len: u64,
    default_size: u32,
    default_duration: u32,
    default_flags: u32,
    next_dts: i64,
}

impl<R: Read + Seek + Send> Mp4<R> {
    pub fn open(mut r: R) -> Result<Mp4<R>, Error> {
        let file_len = r.seek(SeekFrom::End(0))?;
        r.seek(SeekFrom::Start(0))?;
        // Top-level boxes until the `moov`, which may be after the `mdat`.
        let mut pos = 0u64;
        let mut moov = None;
        let mut first_moof = None;
        while pos + 8 <= file_len {
            r.seek(SeekFrom::Start(pos))?;
            let mut h = [0u8; 16];
            let n = read_up_to(&mut r, &mut h)?;
            if n < 8 {
                break;
            }
            let (mut size, mut head) = (u32::from_be_bytes(h[..4].try_into().expect("four bytes")) as u64, 8u64);
            if size == 1 {
                if n < 16 {
                    break;
                }
                size = u64::from_be_bytes(h[8..16].try_into().expect("eight bytes"));
                head = 16;
            } else if size == 0 {
                size = file_len - pos;
            }
            if size < head {
                return Err(Error::Bad("an MP4 box too small to be one"));
            }
            match &h[4..8] {
                b"moov" => {
                    if size > MAX_META {
                        return Err(Error::Bad("an MP4 index larger than any real one"));
                    }
                    let mut body = vec![0; (size - head) as usize];
                    r.seek(SeekFrom::Start(pos + head))?;
                    if read_up_to(&mut r, &mut body)? < body.len() {
                        return Err(Error::Bad("the MP4 index is cut short"));
                    }
                    moov = Some(body);
                }
                b"moof" => {
                    first_moof = first_moof.or(Some(pos));
                    if moov.is_some() {
                        break;
                    }
                }
                _ => {}
            }
            pos = pos.saturating_add(size);
        }
        let moov = moov.ok_or(Error::Bad("an MP4 with no index"))?;
        let mut video = None;
        let mut audio = None;
        for (kind, body) in boxes(&moov) {
            if &kind == b"trak"
                && let Some(t) = parse_trak(body)
            {
                match &t.handler {
                    b"vide" if video.is_none() => video = Some(t),
                    b"soun" if audio.is_none() => audio = audio_name(t.stsd),
                    _ => {}
                }
            }
        }
        let t = video.ok_or(Error::Bad("an MP4 with no video"))?;
        let (codec, width, height, config) = parse_stsd(t.stsd).ok_or(Error::Bad("the MP4's video description"))?;
        let samples = parse_samples(&t).unwrap_or_default();
        let mvex = find(&moov, b"mvex");
        let trex = mvex.and_then(|m| boxes(m).find(|(k, v)| k == b"trex" && be32(v, 4) == Some(t.track_id)).map(|(_, v)| v));
        let fragments = (samples.is_empty() && mvex.is_some()).then_some(first_moof.unwrap_or(file_len));
        let scale = t.timescale.max(1) as i128;
        let mehd = mvex.and_then(|m| find(m, b"mehd")).and_then(|m| if m.first()? == &1 { be64(m, 4) } else { be32(m, 4).map(u64::from) });
        let units = if t.duration > 0 { Some(t.duration as i128 * 1_000_000 / scale) } else { None };
        let track = Track { codec, width, height, duration_us: units.map(|u| u as i64).or(mehd.map(|m| m as i64)), config, audio };
        if samples.is_empty() && fragments.is_none() {
            return Err(Error::Bad("an MP4 video with no frames"));
        }
        let mut m = Mp4 {
            r,
            track,
            track_id: t.track_id,
            timescale: t.timescale.max(1),
            samples,
            at: 0,
            fragments,
            file_len,
            default_size: trex.and_then(|v| be32(v, 16)).unwrap_or(0),
            default_duration: trex.and_then(|v| be32(v, 12)).unwrap_or(0),
            default_flags: trex.and_then(|v| be32(v, 20)).unwrap_or(0),
            next_dts: 0,
        };
        m.elst_shift(t.elst_start);
        Ok(m)
    }

    fn elst_shift(&mut self, shift: Option<i64>) {
        // A fragmented file's edit list applies to its fragments' times too.
        if self.fragments.is_some() {
            self.next_dts = -shift.unwrap_or(0);
        }
    }

    /// Reads the next `moof` into the table. False at the end of the file.
    fn read_fragment(&mut self) -> Result<bool, Error> {
        let Some(mut pos) = self.fragments else { return Ok(false) };
        while pos + 8 <= self.file_len {
            self.r.seek(SeekFrom::Start(pos))?;
            let mut h = [0u8; 16];
            let n = read_up_to(&mut self.r, &mut h)?;
            if n < 8 {
                break;
            }
            let (mut size, mut head) = (u32::from_be_bytes(h[..4].try_into().expect("four bytes")) as u64, 8u64);
            if size == 1 && n == 16 {
                size = u64::from_be_bytes(h[8..16].try_into().expect("eight bytes"));
                head = 16;
            } else if size == 0 {
                size = self.file_len - pos;
            }
            if size < head {
                return Err(Error::Bad("an MP4 box too small to be one"));
            }
            if &h[4..8] == b"moof" {
                if size > MAX_META {
                    return Err(Error::Bad("an MP4 fragment index larger than any real one"));
                }
                let mut body = vec![0; (size - head) as usize];
                self.r.seek(SeekFrom::Start(pos + head))?;
                if read_up_to(&mut self.r, &mut body)? < body.len() {
                    self.fragments = Some(self.file_len);
                    return Ok(false);
                }
                self.parse_moof(&body, pos);
                self.fragments = Some(pos + size);
                return Ok(true);
            }
            pos = pos.saturating_add(size);
        }
        self.fragments = Some(self.file_len);
        Ok(false)
    }

    fn parse_moof(&mut self, moof: &[u8], moof_start: u64) {
        let scale = self.timescale as i128;
        for (kind, traf) in boxes(moof) {
            if &kind != b"traf" {
                continue;
            }
            let Some(tfhd) = find(traf, b"tfhd") else { continue };
            let Some(flags) = be32(tfhd, 0).map(|v| v & 0xff_ffff) else { continue };
            if be32(tfhd, 4) != Some(self.track_id) {
                continue;
            }
            let mut at = 8;
            let mut base = moof_start;
            if flags & 1 != 0 {
                base = be64(tfhd, at).unwrap_or(base);
                at += 8;
            }
            if flags & 2 != 0 {
                at += 4;
            }
            let mut d_duration = self.default_duration;
            if flags & 8 != 0 {
                d_duration = be32(tfhd, at).unwrap_or(d_duration);
                at += 4;
            }
            let mut d_size = self.default_size;
            if flags & 0x10 != 0 {
                d_size = be32(tfhd, at).unwrap_or(d_size);
                at += 4;
            }
            let mut d_flags = self.default_flags;
            if flags & 0x20 != 0 {
                d_flags = be32(tfhd, at).unwrap_or(d_flags);
            }
            if let Some(tfdt) = find(traf, b"tfdt") {
                let t = if tfdt.first() == Some(&1) { be64(tfdt, 4).map(|v| v as i64) } else { be32(tfdt, 4).map(i64::from) };
                if let Some(t) = t {
                    // The edit-list shift stays in `next_dts` only until the first fragment says where it is.
                    self.next_dts = t;
                }
            }
            let mut offset = base;
            for (k, trun) in boxes(traf) {
                if &k != b"trun" {
                    continue;
                }
                let Some(tf) = be32(trun, 0).map(|v| v & 0xff_ffff) else { continue };
                let Some(count) = be32(trun, 4) else { continue };
                let mut p = 8;
                if tf & 1 != 0 {
                    if let Some(d) = be32(trun, p) {
                        offset = base.wrapping_add(d as i32 as i64 as u64);
                    }
                    p += 4;
                }
                let mut first_flags = None;
                if tf & 4 != 0 {
                    first_flags = be32(trun, p);
                    p += 4;
                }
                for i in 0..count {
                    let (mut dur, mut size, mut fl, mut cts) = (d_duration, d_size, d_flags, 0i64);
                    if tf & 0x100 != 0 {
                        let Some(v) = be32(trun, p) else { break };
                        dur = v;
                        p += 4;
                    }
                    if tf & 0x200 != 0 {
                        let Some(v) = be32(trun, p) else { break };
                        size = v;
                        p += 4;
                    }
                    if tf & 0x400 != 0 {
                        let Some(v) = be32(trun, p) else { break };
                        fl = v;
                        p += 4;
                    } else if i == 0
                        && let Some(f) = first_flags
                    {
                        fl = f;
                    }
                    if tf & 0x800 != 0 {
                        let Some(v) = be32(trun, p) else { break };
                        cts = v as i32 as i64;
                        p += 4;
                    }
                    let key = fl & 0x0001_0000 == 0;
                    let pts_us = ((self.next_dts + cts) as i128 * 1_000_000 / scale) as i64;
                    if self.samples.len() >= MAX_SAMPLES {
                        return;
                    }
                    self.samples.push(Sample { offset, size, pts_us, key });
                    offset = offset.wrapping_add(size as u64);
                    self.next_dts += dur as i64;
                }
            }
        }
    }
}

impl<R: Read + Seek + Send> Demuxer for Mp4<R> {
    fn track(&self) -> &Track {
        &self.track
    }

    fn next_packet(&mut self) -> Result<Option<Packet>, Error> {
        while self.at >= self.samples.len() {
            if !self.read_fragment()? {
                return Ok(None);
            }
        }
        let s = self.samples[self.at];
        self.at += 1;
        if s.size as u64 > MAX_FRAME {
            return Err(Error::Bad("a frame larger than any real one"));
        }
        self.r.seek(SeekFrom::Start(s.offset))?;
        let mut data = vec![0; s.size as usize];
        if read_up_to(&mut self.r, &mut data)? < data.len() {
            return Ok(None);
        }
        let data = match self.track.codec {
            Codec::H264 => annexb_from_avcc(&data, &self.track.config, s.key),
            Codec::Hevc if !self.track.config.is_empty() => annexb_from_hvcc(&data, &self.track.config, s.key),
            _ => data,
        };
        Ok(Some(Packet { data, pts_us: s.pts_us, key: s.key }))
    }

    fn seek_to(&mut self, pts_us: i64) -> Result<i64, Error> {
        // A fragmented file's table grows as it is read: read until it covers the time.
        while self.fragments.is_some() && self.samples.last().is_none_or(|s| s.pts_us <= pts_us) && self.read_fragment()? {}
        let mut best = None;
        for (i, s) in self.samples.iter().enumerate() {
            if s.key && s.pts_us <= pts_us && best.is_none_or(|b: usize| s.pts_us >= self.samples[b].pts_us) {
                best = Some(i);
            }
        }
        let i = best.or(self.samples.iter().position(|s| s.key)).unwrap_or(0);
        self.at = i;
        Ok(self.samples.get(i).map_or(0, |s| s.pts_us))
    }
}
