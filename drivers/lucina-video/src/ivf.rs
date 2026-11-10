//! IVF, and raw H.264 in Annex B: the simplest containers, for test clips. Neither has an index, so seeking looks at every packet once.

use std::io::{Read, Seek, SeekFrom};

use crate::demux::{Codec, Demuxer, Error, Packet, Track, read_up_to};

pub struct Ivf<R> {
    r: R,
    track: Track,
    timebase: (u32, u32),
    start: u64,
    index: Vec<(u64, i64)>,
}

impl<R: Read + Seek + Send> Ivf<R> {
    pub fn open(mut r: R) -> Result<Ivf<R>, Error> {
        let mut h = [0u8; 32];
        if read_up_to(&mut r, &mut h)? < 32 {
            return Err(Error::Bad("IVF header cut short"));
        }
        let len = u16::from_le_bytes([h[6], h[7]]) as u64;
        let codec = match &h[8..12] {
            b"AV01" => Codec::Av1,
            b"VP90" => Codec::Vp9,
            b"VP80" => Codec::Vp8,
            b"H264" => Codec::H264,
            _ => Codec::Other,
        };
        let w = u16::from_le_bytes([h[12], h[13]]) as u32;
        let vh = u16::from_le_bytes([h[14], h[15]]) as u32;
        let den = u32::from_le_bytes(h[16..20].try_into().expect("four bytes"));
        let num = u32::from_le_bytes(h[20..24].try_into().expect("four bytes"));
        r.seek(SeekFrom::Start(len.max(32)))?;
        let track = Track { codec, width: w, height: vh, duration_us: None, config: Vec::new(), audio: None };
        Ok(Ivf { r, track, timebase: (num.max(1), den.max(1)), start: len.max(32), index: Vec::new() })
    }

    fn read_one(&mut self) -> Result<Option<Packet>, Error> {
        let at = self.r.stream_position()?;
        let mut h = [0u8; 12];
        if read_up_to(&mut self.r, &mut h)? < 12 {
            return Ok(None);
        }
        let n = u32::from_le_bytes(h[..4].try_into().expect("four bytes")) as usize;
        let ts = i64::from_le_bytes(h[4..12].try_into().expect("eight bytes"));
        if n > 32 << 20 {
            return Err(Error::Bad("IVF frame larger than any real one"));
        }
        let mut data = vec![0; n];
        if read_up_to(&mut self.r, &mut data)? < n {
            return Ok(None);
        }
        let pts_us = ts.saturating_mul(1_000_000).saturating_mul(self.timebase.0 as i64) / self.timebase.1 as i64;
        let key = is_key(self.track.codec, &data);
        if key && self.index.last().is_none_or(|l| l.0 < at) {
            self.index.push((at, pts_us));
        }
        Ok(Some(Packet { data, pts_us, key }))
    }
}

/// The first bytes of a packet say this for VP9 and AV1; for anything else every packet counts as one, which only makes seeking coarser.
fn is_key(codec: Codec, d: &[u8]) -> bool {
    match codec {
        Codec::Vp9 => d.first().is_some_and(|b| {
            // frame_marker, profile bits, show_existing_frame, frame_type. Profile 3 has one bit more.
            let profile = (b >> 5 & 1) | (b >> 4 & 1) << 1;
            let shift = if profile == 3 { 1 } else { 0 };
            b >> (3 - shift) & 1 == 0 && b >> (2 - shift) & 1 == 0
        }),
        // A sequence header OBU comes first in a key frame's temporal unit, after any delimiter.
        Codec::Av1 => {
            let mut at = 0;
            while let Some(&h) = d.get(at) {
                let kind = h >> 3 & 15;
                if kind == 1 {
                    return true;
                }
                if kind != 2 || h & 2 == 0 {
                    return false;
                }
                let Some(&size) = d.get(at + 1) else { return false };
                at += 2 + (size & 0x7f) as usize;
            }
            false
        }
        Codec::Vp8 => d.first().is_some_and(|b| b & 1 == 0),
        _ => true,
    }
}

impl<R: Read + Seek + Send> Demuxer for Ivf<R> {
    fn track(&self) -> &Track {
        &self.track
    }

    fn next_packet(&mut self) -> Result<Option<Packet>, Error> {
        self.read_one()
    }

    fn seek_to(&mut self, pts_us: i64) -> Result<i64, Error> {
        // Read forward from the last key frame known, until past the time.
        let mut from = self.index.iter().rev().find(|k| k.1 <= pts_us).copied();
        let resume = self.index.last().map_or(self.start, |k| k.0);
        self.r.seek(SeekFrom::Start(resume))?;
        while let Some(p) = self.read_one()? {
            if p.pts_us > pts_us {
                break;
            }
        }
        if let Some(better) = self.index.iter().rev().find(|k| k.1 <= pts_us) {
            from = Some(*better);
        }
        let (at, pts) = from.unwrap_or((self.start, 0));
        self.r.seek(SeekFrom::Start(at))?;
        Ok(pts)
    }
}

/// Raw H.264: pictures found by their first slice. No times in the file, so 24 a second.
pub struct AnnexB {
    bytes: Vec<u8>,
    at: usize,
    track: Track,
    n: i64,
    keys: Vec<(usize, i64)>,
}

const RAW_FRAME_US: i64 = 1_000_000 / 24;

impl AnnexB {
    pub fn open<R: Read>(mut r: R) -> Result<AnnexB, Error> {
        let mut bytes = Vec::new();
        r.read_to_end(&mut bytes)?;
        let track = Track { codec: Codec::H264, width: 0, height: 0, duration_us: None, config: Vec::new(), audio: None };
        Ok(AnnexB { bytes, at: 0, track, n: 0, keys: Vec::new() })
    }
}

/// Where the next start code after `from` begins.
fn next_start(b: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    while i + 3 <= b.len() {
        if b[i] == 0 && b[i + 1] == 0 && b[i + 2] == 1 {
            return Some(if i > from && b[i - 1] == 0 { i - 1 } else { i });
        }
        i += 1;
    }
    None
}

impl Demuxer for AnnexB {
    fn track(&self) -> &Track {
        &self.track
    }

    fn next_packet(&mut self) -> Result<Option<Packet>, Error> {
        if self.at >= self.bytes.len() {
            return Ok(None);
        }
        let Some(mut nal) = next_start(&self.bytes, self.at) else {
            self.at = self.bytes.len();
            return Ok(None);
        };
        let begin = nal;
        let (mut seen_slice, mut key) = (false, false);
        let end = loop {
            let code = nal + if self.bytes[nal + 2] == 1 { 3 } else { 4 };
            let Some(&h) = self.bytes.get(code) else { break self.bytes.len() };
            let kind = h & 0x1f;
            let first_mb_zero = self.bytes.get(code + 1).is_some_and(|v| v & 0x80 != 0);
            let begins_picture = matches!(kind, 6..=9) || (matches!(kind, 1 | 5) && first_mb_zero);
            if seen_slice && begins_picture {
                break nal;
            }
            if matches!(kind, 1 | 5) {
                seen_slice = true;
                key |= kind == 5;
            }
            match next_start(&self.bytes, code) {
                Some(n) => nal = n,
                None => break self.bytes.len(),
            }
        };
        self.at = end;
        let pts_us = self.n * RAW_FRAME_US;
        self.n += 1;
        if key && self.keys.last().is_none_or(|k| k.0 < begin) {
            self.keys.push((begin, pts_us));
        }
        Ok(Some(Packet { data: self.bytes[begin..end].to_vec(), pts_us, key }))
    }

    fn seek_to(&mut self, pts_us: i64) -> Result<i64, Error> {
        while self.n * RAW_FRAME_US <= pts_us && self.next_packet()?.is_some() {}
        let (at, pts) = self.keys.iter().rev().find(|k| k.1 <= pts_us).copied().unwrap_or((0, 0));
        self.at = at;
        self.n = pts / RAW_FRAME_US;
        Ok(pts)
    }
}
