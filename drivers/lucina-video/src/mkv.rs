//! Matroska and WebM: read a cluster at a time, in order. The `Cues` make seeking quick when the file has them; without them a seek
//! reads forward through the clusters.

use std::io::{Read, Seek, SeekFrom};

use crate::demux::{Codec, Demuxer, Error, Packet, Track, annexb_from_avcc, annexb_from_hvcc, read_up_to};

pub(crate) const ID_SEGMENT: u32 = 0x1853_8067;
pub(crate) const ID_INFO: u32 = 0x1549_A966;
pub(crate) const ID_TRACKS: u32 = 0x1654_AE6B;
pub(crate) const ID_CUES: u32 = 0x1C53_BB6B;
pub(crate) const ID_CLUSTER: u32 = 0x1F43_B675;
pub(crate) const ID_TIMECODE_SCALE: u32 = 0x2A_D7B1;
pub(crate) const ID_DURATION: u32 = 0x4489;
pub(crate) const ID_TRACK_ENTRY: u32 = 0xAE;
pub(crate) const ID_TRACK_NUMBER: u32 = 0xD7;
pub(crate) const ID_TRACK_TYPE: u32 = 0x83;
pub(crate) const ID_CODEC_ID: u32 = 0x86;
pub(crate) const ID_CODEC_PRIVATE: u32 = 0x63A2;
pub(crate) const ID_VIDEO: u32 = 0xE0;
pub(crate) const ID_PIXEL_WIDTH: u32 = 0xB0;
pub(crate) const ID_PIXEL_HEIGHT: u32 = 0xBA;
pub(crate) const ID_TIMECODE: u32 = 0xE7;
pub(crate) const ID_SIMPLE_BLOCK: u32 = 0xA3;
pub(crate) const ID_BLOCK_GROUP: u32 = 0xA0;
pub(crate) const ID_BLOCK: u32 = 0xA1;
pub(crate) const ID_REFERENCE_BLOCK: u32 = 0xFB;
pub(crate) const ID_CUE_POINT: u32 = 0xBB;
pub(crate) const ID_CUE_TIME: u32 = 0xB3;
pub(crate) const ID_CUE_TRACK_POSITIONS: u32 = 0xB7;
pub(crate) const ID_CUE_CLUSTER_POSITION: u32 = 0xF1;

/// More than any real header element or frame. A file saying otherwise is damaged.
pub(crate) const MAX_ELEMENT: u64 = 32 << 20;

pub(crate) fn read_id(r: &mut impl Read) -> Result<Option<u32>, Error> {
    let mut b = [0u8; 1];
    if read_up_to(r, &mut b)? == 0 {
        return Ok(None);
    }
    let len = b[0].leading_zeros() as usize + 1;
    if len > 4 {
        return Err(Error::Bad("a Matroska element id"));
    }
    let mut v = b[0] as u32;
    let mut rest = [0u8; 3];
    if read_up_to(r, &mut rest[..len - 1])? < len - 1 {
        return Ok(None);
    }
    for x in &rest[..len - 1] {
        v = v << 8 | *x as u32;
    }
    Ok(Some(v))
}

/// A size. `None` for "unknown", which a stream may use for its segment and clusters.
pub(crate) fn read_size(r: &mut impl Read) -> Result<Option<Option<u64>>, Error> {
    let mut b = [0u8; 1];
    if read_up_to(r, &mut b)? == 0 {
        return Ok(None);
    }
    let len = b[0].leading_zeros() as usize + 1;
    if len > 8 {
        return Err(Error::Bad("a Matroska element size"));
    }
    let mut v = (b[0] as u64) & (0xff >> len);
    let mut unknown = v == (0xff >> len) as u64;
    let mut rest = [0u8; 7];
    if read_up_to(r, &mut rest[..len - 1])? < len - 1 {
        return Ok(None);
    }
    for x in &rest[..len - 1] {
        unknown &= *x == 0xff;
        v = v << 8 | *x as u64;
    }
    Ok(Some(if unknown { None } else { Some(v) }))
}

/// `(id, body)` of each element in a slice.
pub(crate) fn elements(mut b: &[u8]) -> impl Iterator<Item = (u32, &[u8])> {
    std::iter::from_fn(move || {
        let id = read_id(&mut b).ok()??;
        let size = read_size(&mut b).ok()??? as usize;
        let body = b.get(..size)?;
        b = &b[size..];
        Some((id, body))
    })
}

pub(crate) fn uint(b: &[u8]) -> u64 {
    b.iter().fold(0, |v, x| v << 8 | *x as u64)
}

pub(crate) fn float(b: &[u8]) -> f64 {
    match b.len() {
        4 => f32::from_be_bytes(b.try_into().expect("four bytes")) as f64,
        8 => f64::from_be_bytes(b.try_into().expect("eight bytes")),
        _ => 0.0,
    }
}

pub struct Mkv<R> {
    r: R,
    track: Track,
    track_number: u64,
    /// Nanoseconds per unit of the file's times.
    scale: u64,
    segment_start: u64,
    first_cluster: u64,
    /// (time in microseconds, file position of the cluster).
    cues: Vec<(i64, u64)>,
    cluster_time: i64,
}

pub(crate) fn codec_of(id: &str) -> Codec {
    match id {
        "V_MPEG4/ISO/AVC" => Codec::H264,
        "V_MPEGH/ISO/HEVC" => Codec::Hevc,
        "V_VP9" => Codec::Vp9,
        "V_VP8" => Codec::Vp8,
        "V_AV1" => Codec::Av1,
        "V_MPEG2" => Codec::Mpeg2,
        _ => Codec::Other,
    }
}

impl<R: Read + Seek + Send> Mkv<R> {
    pub fn open(mut r: R) -> Result<Mkv<R>, Error> {
        // The EBML header, then the segment.
        let id = read_id(&mut r)?.ok_or(Error::Bad("an empty Matroska file"))?;
        if id != 0x1A45_DFA3 {
            return Err(Error::Unknown);
        }
        let size = read_size(&mut r)?.flatten().ok_or(Error::Bad("the Matroska header"))?;
        r.seek(SeekFrom::Current(size as i64))?;
        loop {
            let Some(id) = read_id(&mut r)? else { return Err(Error::Bad("a Matroska file with no segment")) };
            let Some(size) = read_size(&mut r)? else { return Err(Error::Bad("a Matroska file with no segment")) };
            if id == ID_SEGMENT {
                break;
            }
            let Some(size) = size else { return Err(Error::Bad("the Matroska header")) };
            r.seek(SeekFrom::Current(size as i64))?;
        }
        let segment_start = r.stream_position()?;
        let (mut scale, mut duration, mut track, mut number, mut cues_at) = (1_000_000u64, None, None, 0u64, None);
        let first_cluster;
        loop {
            let at = r.stream_position()?;
            let Some(id) = read_id(&mut r)? else { return Err(Error::Bad("a Matroska file with no pictures")) };
            let Some(size) = read_size(&mut r)? else { return Err(Error::Bad("a Matroska file cut short")) };
            if id == ID_CLUSTER {
                first_cluster = at;
                break;
            }
            let Some(size) = size else { return Err(Error::Bad("an element with no size")) };
            if size > MAX_ELEMENT {
                return Err(Error::Bad("an element larger than any real one"));
            }
            match id {
                ID_INFO | ID_TRACKS => {
                    let mut body = vec![0; size as usize];
                    if read_up_to(&mut r, &mut body)? < body.len() {
                        return Err(Error::Bad("a Matroska file cut short"));
                    }
                    if id == ID_INFO {
                        for (i, v) in elements(&body) {
                            match i {
                                ID_TIMECODE_SCALE => scale = uint(v).max(1),
                                ID_DURATION => duration = Some(float(v)),
                                _ => {}
                            }
                        }
                    } else {
                        (track, number) = parse_tracks(&body).ok_or(Error::Bad("a Matroska file with no video"))?;
                    }
                }
                ID_CUES => {
                    cues_at = Some(at);
                    r.seek(SeekFrom::Current(size as i64))?;
                }
                _ => {
                    r.seek(SeekFrom::Current(size as i64))?;
                }
            }
        }
        let mut track: Track = track.ok_or(Error::Bad("a Matroska file with no video"))?;
        track.duration_us = duration.map(|d| (d * scale as f64 / 1000.0) as i64);
        let mut m = Mkv { r, track, track_number: number, scale, segment_start, first_cluster, cues: Vec::new(), cluster_time: 0 };
        m.read_cues(cues_at)?;
        m.r.seek(SeekFrom::Start(first_cluster))?;
        Ok(m)
    }

    /// Cues live before the first cluster, or are named by the seek head and sit at the end. This reads those before; the others are
    /// found by walking the top level, which costs little: it skips over each cluster by its size.
    fn read_cues(&mut self, at: Option<u64>) -> Result<(), Error> {
        let at = match at {
            Some(a) => Some(a),
            None => self.find_top_level(ID_CUES)?,
        };
        let Some(at) = at else { return Ok(()) };
        self.r.seek(SeekFrom::Start(at))?;
        let Some(ID_CUES) = read_id(&mut self.r)? else { return Ok(()) };
        let Some(Some(size)) = read_size(&mut self.r)? else { return Ok(()) };
        if size > MAX_ELEMENT {
            return Ok(());
        }
        let mut body = vec![0; size as usize];
        if read_up_to(&mut self.r, &mut body)? < body.len() {
            return Ok(());
        }
        for (id, point) in elements(&body) {
            if id != ID_CUE_POINT {
                continue;
            }
            let (mut time, mut pos) = (None, None);
            for (i, v) in elements(point) {
                match i {
                    ID_CUE_TIME => time = Some(uint(v)),
                    ID_CUE_TRACK_POSITIONS => {
                        for (j, w) in elements(v) {
                            if j == ID_CUE_CLUSTER_POSITION {
                                pos = Some(uint(w));
                            }
                        }
                    }
                    _ => {}
                }
            }
            if let (Some(t), Some(p)) = (time, pos) {
                self.cues.push(((t as i128 * self.scale as i128 / 1000) as i64, self.segment_start + p));
            }
        }
        self.cues.sort_unstable();
        Ok(())
    }

    fn find_top_level(&mut self, want: u32) -> Result<Option<u64>, Error> {
        self.r.seek(SeekFrom::Start(self.first_cluster))?;
        // A long film has thousands of clusters; give up rather than read the whole file's headers over a slow link.
        for _ in 0..100_000 {
            let at = self.r.stream_position()?;
            let Some(id) = read_id(&mut self.r)? else { return Ok(None) };
            let Some(size) = read_size(&mut self.r)? else { return Ok(None) };
            if id == want {
                return Ok(Some(at));
            }
            let Some(size) = size else { return Ok(None) };
            self.r.seek(SeekFrom::Current(size as i64))?;
        }
        Ok(None)
    }

    fn to_us(&self, units: i64) -> i64 {
        (units as i128 * self.scale as i128 / 1000) as i64
    }

    /// `(key, data)` from a block's body, when it belongs to the video track. Lacing is for audio: a video block with it is refused.
    fn block(&self, b: &[u8], simple: bool, has_reference: bool) -> Option<(i64, bool, Vec<u8>)> {
        let mut s = b;
        let track = read_size(&mut s).ok()???;
        if track != self.track_number {
            return None;
        }
        let rel = i16::from_be_bytes(s.get(..2)?.try_into().ok()?);
        let flags = *s.get(2)?;
        if flags & 0x06 != 0 {
            return None;
        }
        let key = if simple { flags & 0x80 != 0 } else { !has_reference };
        Some((self.to_us(self.cluster_time + rel as i64), key, s[3..].to_vec()))
    }

    fn packet(&self, (pts_us, key, data): (i64, bool, Vec<u8>)) -> Packet {
        // Without a CodecPrivate the frames are Annex B already; leave them alone.
        let data = match self.track.codec {
            Codec::H264 if !self.track.config.is_empty() => annexb_from_avcc(&data, &self.track.config, key),
            Codec::Hevc if !self.track.config.is_empty() => annexb_from_hvcc(&data, &self.track.config, key),
            _ => data,
        };
        Packet { data, pts_us, key }
    }
}

fn parse_tracks(b: &[u8]) -> Option<(Option<Track>, u64)> {
    let mut audio = None;
    let mut video = None;
    for (id, entry) in elements(b) {
        if id != ID_TRACK_ENTRY {
            continue;
        }
        let (mut number, mut kind, mut codec, mut config, mut w, mut h) = (0, 0, String::new(), Vec::new(), 0, 0);
        for (i, v) in elements(entry) {
            match i {
                ID_TRACK_NUMBER => number = uint(v),
                ID_TRACK_TYPE => kind = uint(v),
                ID_CODEC_ID => codec = String::from_utf8_lossy(v).into_owned(),
                ID_CODEC_PRIVATE => config = v.to_vec(),
                ID_VIDEO => {
                    for (j, x) in elements(v) {
                        match j {
                            ID_PIXEL_WIDTH => w = uint(x) as u32,
                            ID_PIXEL_HEIGHT => h = uint(x) as u32,
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        match kind {
            1 if video.is_none() => video = Some((number, codec, config, w, h)),
            2 if audio.is_none() => audio = Some(codec.trim_start_matches("A_").to_ascii_lowercase()),
            _ => {}
        }
    }
    let (number, codec, config, width, height) = video?;
    let track = Track { codec: codec_of(&codec), width, height, duration_us: None, config, audio };
    Some((Some(track), number))
}

impl<R: Read + Seek + Send> Demuxer for Mkv<R> {
    fn track(&self) -> &Track {
        &self.track
    }

    fn next_packet(&mut self) -> Result<Option<Packet>, Error> {
        loop {
            let Some(id) = read_id(&mut self.r)? else { return Ok(None) };
            let Some(size) = read_size(&mut self.r)? else { return Ok(None) };
            match id {
                // Into a cluster, known size or not: its children are read as they come.
                ID_CLUSTER => {}
                ID_TIMECODE => {
                    let Some(size) = size else { return Err(Error::Bad("an element with no size")) };
                    let mut b = [0u8; 8];
                    if size > 8 {
                        return Err(Error::Bad("a cluster time"));
                    }
                    if read_up_to(&mut self.r, &mut b[..size as usize])? < size as usize {
                        return Ok(None);
                    }
                    self.cluster_time = uint(&b[..size as usize]) as i64;
                }
                ID_SIMPLE_BLOCK | ID_BLOCK_GROUP => {
                    let Some(size) = size else { return Err(Error::Bad("an element with no size")) };
                    if size > MAX_ELEMENT {
                        return Err(Error::Bad("a block larger than any real one"));
                    }
                    let mut body = vec![0; size as usize];
                    if read_up_to(&mut self.r, &mut body)? < body.len() {
                        return Ok(None);
                    }
                    let got = if id == ID_SIMPLE_BLOCK {
                        self.block(&body, true, false)
                    } else {
                        let has_reference = elements(&body).any(|(i, _)| i == ID_REFERENCE_BLOCK);
                        elements(&body).find(|(i, _)| *i == ID_BLOCK).and_then(|(_, b)| self.block(b, false, has_reference))
                    };
                    if let Some(g) = got {
                        return Ok(Some(self.packet(g)));
                    }
                }
                _ => {
                    let Some(size) = size else { return Err(Error::Bad("an element with no size")) };
                    self.r.seek(SeekFrom::Current(size as i64))?;
                }
            }
        }
    }

    fn seek_to(&mut self, pts_us: i64) -> Result<i64, Error> {
        // From the last cluster that starts at or before the time.
        let start = self.cues.iter().rev().find(|c| c.0 <= pts_us).map_or(self.first_cluster, |c| c.1);
        self.r.seek(SeekFrom::Start(start))?;
        let mut best: Option<(u64, i64)> = None;
        loop {
            let at = self.r.stream_position()?;
            let Some(p) = self.next_packet()? else { break };
            if p.pts_us > pts_us && best.is_some() && p.key {
                break;
            }
            if p.key && p.pts_us <= pts_us {
                best = Some((at, p.pts_us));
            }
        }
        match best {
            Some((at, pts)) => {
                // The time was read from the cluster the block is in; `at` is inside it, and reading goes on from there with that time.
                self.r.seek(SeekFrom::Start(self.first_cluster))?;
                self.scan_to(at)?;
                Ok(pts)
            }
            None => {
                self.r.seek(SeekFrom::Start(self.first_cluster))?;
                Ok(0)
            }
        }
    }
}

impl<R: Read + Seek + Send> Mkv<R> {
    /// Stands at `at`, a block's start, with its cluster's time read.
    fn scan_to(&mut self, at: u64) -> Result<(), Error> {
        let start = self.cues.iter().rev().find(|c| c.1 <= at).map_or(self.first_cluster, |c| c.1);
        self.r.seek(SeekFrom::Start(start))?;
        while self.r.stream_position()? < at {
            if self.next_packet()?.is_none() {
                break;
            }
        }
        self.r.seek(SeekFrom::Start(at))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn mkv_with(codec: Codec, config: Vec<u8>) -> Mkv<Cursor<Vec<u8>>> {
        Mkv {
            r: Cursor::new(Vec::new()),
            track: Track { codec, width: 64, height: 48, duration_us: None, config, audio: None },
            track_number: 1,
            scale: 1_000_000,
            segment_start: 0,
            first_cluster: 0,
            cues: Vec::new(),
            cluster_time: 0,
        }
    }

    #[test]
    fn annexb_frames_without_codec_private_pass_through() {
        // A Matroska track without CodecPrivate is Annex B already.
        let data = vec![0, 0, 0, 1, 9, 9];
        for codec in [Codec::H264, Codec::Hevc] {
            let m = mkv_with(codec, Vec::new());
            assert_eq!(m.packet((0, true, data.clone())).data, data);
        }
    }
}
