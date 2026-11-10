//! The decoders behind one face. Which one handles each codec comes from `docs/research/juno-media.md`: measured on the laptop, and each
//! checked frame by frame against FFmpeg.

use std::collections::BTreeMap;

use lucina_video::limits::{self, MAX_LUMA_SAMPLES};
use lucina_video::{Codec, Packet, Planes};
use rav1d::Rav1dError;
use rav1d::rust_api::{Decoder as Rav1d, PlanarImageComponent, Picture as Rav1dPicture, Settings};

/// A VP9 picture into the common form. `None` above 8 bits: the screen path reads bytes.
fn from_filmcraft(p: filmcraft_vp9::Picture) -> Option<Picture> {
    let (filmcraft_vp9::Plane::U8(y), filmcraft_vp9::Plane::U8(u), filmcraft_vp9::Plane::U8(v)) = (p.y, p.u, p.v) else {
        return None;
    };
    Some(Picture::Owned { pts_us: p.pts, w: p.width as usize, h: p.height as usize, y, u, v, y_stride: p.y_stride, uv_stride: p.uv_stride })
}

/// A picture on its way from the decoding thread to the screen's.
pub struct Av1Picture(Rav1dPicture);

// SAFETY: rav1d's `Picture` is not `Send` only because its C-shaped fields hold raw pointers. It is a counted reference to memory the decoder
// no longer writes once it hands the picture out (dav1d's contract, which rav1d keeps: a frame other frames refer to is read-only), and its
// count is an `Arc`. So this moves one reference to one other thread, which only reads through it and drops it.
unsafe impl Send for Av1Picture {}

/// One decoded picture, with its display time.
pub enum Picture {
    Owned { pts_us: i64, w: usize, h: usize, y: Vec<u8>, u: Vec<u8>, v: Vec<u8>, y_stride: usize, uv_stride: usize },
    Av1 { pts_us: i64, pic: Av1Picture },
    /// The video block's picture as it laid it out (`crate::venus`), for the GPU to take whole.
    Nv12(Nv12Picture),
}

/// Linear NV12: Y lines `stride` apart from 0, Cb and Cr interleaved (Cb first) from `uv_offset`, the picture being `w` x `h` at
/// `crop` inside it.
pub struct Nv12Picture {
    pub pts_us: i64,
    pub w: usize,
    pub h: usize,
    pub crop: (usize, usize),
    pub stride: usize,
    pub uv_offset: usize,
    pub data: Vec<u8>,
}

impl Nv12Picture {
    /// The planar form the CPU path converts from.
    pub fn to_i420(&self) -> Picture {
        let l = crate::venus::Layout {
            width: self.w,
            height: self.h,
            stride: self.stride,
            uv_offset: self.uv_offset,
            bytes: self.data.len(),
            crop: [self.crop.0, self.crop.1, self.w, self.h],
        };
        let (y, u, v) = crate::venus::nv12_to_i420(&self.data, &l, self.w, self.h);
        Picture::Owned { pts_us: self.pts_us, w: self.w, h: self.h, y, u, v, y_stride: self.w, uv_stride: self.w.div_ceil(2) }
    }

    /// The picture's luma lines, for a checksum.
    pub fn luma_rows(&self) -> impl Iterator<Item = &[u8]> {
        (0..self.h).map(|r| {
            let at = (self.crop.1 + r) * self.stride + self.crop.0;
            &self.data[at..at + self.w]
        })
    }
}

impl Picture {
    pub fn pts_us(&self) -> i64 {
        match self {
            Picture::Owned { pts_us, .. } | Picture::Av1 { pts_us, .. } => *pts_us,
            Picture::Nv12(n) => n.pts_us,
        }
    }

    /// `None` for a form the screen path cannot read as planes: a bit depth above 8, or NV12 (see [`Nv12Picture::to_i420`]).
    pub fn planes(&self) -> Option<Planes<'_>> {
        match self {
            Picture::Nv12(_) => None,
            Picture::Owned { w, h, y, u, v, y_stride, uv_stride, .. } => Some(Planes { w: *w, h: *h, y, u, v, y_stride: *y_stride, uv_stride: *uv_stride }),
            Picture::Av1 { pic: Av1Picture(pic), .. } => (pic.bit_depth() == 8).then(|| Planes {
                w: pic.width() as usize,
                h: pic.height() as usize,
                y: pic.plane(PlanarImageComponent::Y),
                u: pic.plane(PlanarImageComponent::U),
                v: pic.plane(PlanarImageComponent::V),
                y_stride: pic.stride(PlanarImageComponent::Y) as usize,
                uv_stride: pic.stride(PlanarImageComponent::U) as usize,
            }),
        }
    }
}

pub enum Decoder {
    /// The laptop's video block (`crate::venus`).
    Venus(Box<crate::venus::Venus>),
    H264(rusty_h264_decoder::Decoder),
    Vp9(filmcraft_vp9::Decoder),
    Av1(Rav1d),
}

/// Threads for a decoder that can use them: what this machine has, up to where more stop helping. One on today's one-CPU Juno.
fn threads() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get()).min(4)
}

impl Decoder {
    /// The video block when it is there and takes the stream, the CPU decoders otherwise.
    pub fn for_track(codec: Codec, width: u32, height: u32) -> Result<Decoder, String> {
        match crate::venus::Venus::open(codec, width, height) {
            Some(Ok(v)) => {
                eprintln!("video: decoding {} {width}x{height} on the video block", codec.name());
                return Ok(Decoder::Venus(Box::new(v)));
            }
            Some(Err(e)) => eprintln!("video: the video block did not take it ({e}): the CPU decodes"),
            None => {}
        }
        Decoder::new(codec)
    }

    /// `Err` says what cannot be played here, for a person to read.
    pub fn new(codec: Codec) -> Result<Decoder, String> {
        match codec {
            Codec::H264 => Ok(Decoder::H264(rusty_h264_decoder::Decoder::new())),
            Codec::Vp9 => Ok(Decoder::Vp9(filmcraft_vp9::Decoder::with_threads(threads()))),
            Codec::Av1 => {
                let mut s = Settings::new();
                // rav1d has the limit built in, and refuses at the header.
                s.set_frame_size_limit(MAX_LUMA_SAMPLES as u32);
                s.set_n_threads(threads() as u32);
                // Frame threads hold frames back, which the queue already does.
                s.set_max_frame_delay(1);
                Rav1d::with_settings(&s).map(Decoder::Av1).map_err(|e| format!("the AV1 decoder did not start: {e:?}"))
            }
            other => Err(format!("{} video is not playable on this laptop without the server converting it", other.name())),
        }
    }

    /// Decode one packet and add what came out to `out`.
    pub fn decode(&mut self, p: &Packet, out: &mut Vec<Picture>) -> Result<(), String> {
        match self {
            Decoder::Venus(v) => {
                v.send(&p.data, p.pts_us).map_err(|e| e.to_string())?;
                while let Some(f) = v.take().map_err(|e| e.to_string())? {
                    out.push(venus_picture(v, &f)?);
                }
            }
            Decoder::H264(d) => {
                // One picture per access unit, in decode order: the player's queue puts them right by time.
                if let Some(f) = d.decode(&p.data).map_err(|e| format!("{e:?}"))? {
                    let (w, h) = (f.width, f.height);
                    // The decoder refuses above about 38 MP by itself, so one picture costs at most 57 MB before this stops it.
                    if w as u64 * h as u64 > MAX_LUMA_SAMPLES {
                        return Err(limits::too_large(w as u64, h as u64));
                    }
                    out.push(Picture::Owned { pts_us: p.pts_us, w, h, y: f.y, u: f.u, v: f.v, y_stride: w, uv_stride: w / 2 });
                }
            }
            Decoder::Vp9(d) => {
                // Before the decoder: `filmcraft-vp9` 0.9.0 allocates whatever the header says, up to 16384 x 16384.
                limits::check_vp9(&p.data)?;
                for f in d.decode(&p.data, p.pts_us).map_err(|e| format!("{e:?}"))? {
                    out.push(from_filmcraft(f).ok_or("VP9 above 8 bits is not playable on this laptop without the server converting it")?);
                }
            }
            Decoder::Av1(d) => {
                let mut sent = d.send_data(p.data.clone().into_boxed_slice(), None, Some(p.pts_us), None);
                loop {
                    loop {
                        match d.get_picture() {
                            Ok(pic) => out.push(Picture::Av1 { pts_us: pic.timestamp().unwrap_or(p.pts_us), pic: Av1Picture(pic) }),
                            Err(Rav1dError::TryAgain) => break,
                            Err(e) => return Err(format!("{e:?}")),
                        }
                    }
                    match sent {
                        Ok(()) => break,
                        Err(Rav1dError::TryAgain) => sent = d.send_pending_data(),
                        Err(e) => return Err(format!("{e:?}")),
                    }
                }
            }
        }
        Ok(())
    }
}

impl Decoder {
    /// Behind time: leave out what can be left out. For VP9, frames nothing refers to go without the loop filter, a little softer. H.264's
    /// pictures nothing refers to are not even sent to it (`h264_droppable`). AV1 has no such switch here.
    pub fn set_hurrying(&mut self, on: bool) {
        if let Decoder::Vp9(d) = self {
            d.set_draft(on);
        }
    }

    /// The end of the file: what is still inside.
    pub fn flush(&mut self) -> Vec<Picture> {
        match self {
            Decoder::Venus(v) => {
                let mut out = Vec::new();
                if v.end().is_ok() {
                    while let Ok(Some(f)) = v.take_waiting(std::time::Duration::from_secs(1)) {
                        match venus_picture(v, &f) {
                            Ok(p) => out.push(p),
                            Err(_) => break,
                        }
                    }
                }
                out
            }
            Decoder::Vp9(d) => d.flush().into_iter().filter_map(from_filmcraft).collect(),
            Decoder::Av1(d) => std::iter::from_fn(|| d.get_picture().ok()).map(|pic| Picture::Av1 { pts_us: pic.timestamp().unwrap_or(0), pic: Av1Picture(pic) }).collect(),
            Decoder::H264(_) => Vec::new(),
        }
    }
}

/// One picture of the video block, copied out of its buffer as it is (one copy, NV12) and the buffer given back.
fn venus_picture(v: &mut crate::venus::Venus, f: &crate::venus::Frame) -> Result<Picture, String> {
    let l = v.layout.ok_or("a picture before the video block said its layout")?;
    let (w, h) = (l.crop[2], l.crop[3]);
    if w as u64 * h as u64 > MAX_LUMA_SAMPLES {
        return Err(limits::too_large(w as u64, h as u64));
    }
    let chroma_end = l.uv_offset + (l.crop[1] / 2 + h.div_ceil(2)) * l.stride;
    if l.crop[0] + w > l.stride || chroma_end > f.len {
        return Err(format!("a picture of {w}x{h} at {:?} does not fit its {} bytes", (l.crop[0], l.crop[1]), f.len));
    }
    let data = v.bytes(f).to_vec();
    v.give_back(f).map_err(|e| e.to_string())?;
    Ok(Picture::Nv12(Nv12Picture { pts_us: f.pts_us, w, h, crop: (l.crop[0], l.crop[1]), stride: l.stride, uv_offset: l.uv_offset, data }))
}

/// How many pictures H.264's decode order can run ahead of display order, with the streams people actually have.
pub const H264_REORDER: usize = 4;

/// Pictures in display order, from a decoder that may hand them over in decode order.
#[derive(Default)]
pub struct Queue {
    frames: BTreeMap<i64, Picture>,
}

impl Queue {
    pub fn push(&mut self, p: Picture) {
        self.frames.insert(p.pts_us(), p);
    }

    pub fn len(&self) -> usize {
        self.frames.len()
    }

    pub fn clear(&mut self) {
        self.frames.clear();
    }

    pub fn first_pts(&self) -> Option<i64> {
        self.frames.first_key_value().map(|(k, _)| *k)
    }

    pub fn pop(&mut self) -> Option<Picture> {
        self.frames.pop_first().map(|(_, v)| v)
    }
}

/// Whether nothing else needs this H.264 access unit: every slice in it has `nal_ref_idc` 0. When the CPU has fallen behind, such a picture
/// can be left undecoded with no harm to any other.
pub fn h264_droppable(annex_b: &[u8]) -> bool {
    let mut any = false;
    let mut i = 0;
    while i + 3 < annex_b.len() {
        if annex_b[i] == 0 && annex_b[i + 1] == 0 && annex_b[i + 2] == 1 {
            let h = annex_b[i + 3];
            if matches!(h & 0x1f, 1 | 5) {
                any = true;
                if h >> 5 & 3 != 0 {
                    return false;
                }
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    any
}

#[cfg(test)]
mod tests {

    #[test]
    fn an_nv12_picture_with_a_crop_splits_like_the_planes() {
        // 4 x 2 picture at (2, 0) in a 6-wide frame of stride 8; CbCr from 16.
        let mut data = vec![0u8; 24];
        for (i, b) in data[..16].iter_mut().enumerate() {
            *b = i as u8;
        }
        data[16..24].copy_from_slice(&[0, 0, 100, 200, 101, 201, 0, 0]);
        let n = Nv12Picture { pts_us: 7, w: 4, h: 2, crop: (2, 0), stride: 8, uv_offset: 16, data };
        let rows: Vec<&[u8]> = n.luma_rows().collect();
        assert_eq!(rows, [&[2u8, 3, 4, 5][..], &[10, 11, 12, 13][..]]);
        let Picture::Owned { y, u, v, w, h, pts_us, .. } = n.to_i420() else { panic!("planar") };
        assert_eq!((w, h, pts_us), (4, 2, 7));
        assert_eq!(y, [2, 3, 4, 5, 10, 11, 12, 13]);
        assert_eq!((u, v), (vec![100, 101], vec![200, 201]));
    }
    use super::*;

    fn owned(pts_us: i64) -> Picture {
        Picture::Owned { pts_us, w: 2, h: 2, y: vec![0; 4], u: vec![0], v: vec![0], y_stride: 2, uv_stride: 1 }
    }

    #[test]
    fn the_queue_gives_display_order() {
        let mut q = Queue::default();
        for t in [0, 300, 100, 200] {
            q.push(owned(t));
        }
        assert_eq!(q.first_pts(), Some(0));
        let order: Vec<i64> = std::iter::from_fn(|| q.pop().map(|p| p.pts_us())).collect();
        assert_eq!(order, [0, 100, 200, 300]);
    }

    #[test]
    fn only_a_picture_nobody_refers_to_can_be_dropped() {
        // nal_ref_idc 0, slice.
        assert!(h264_droppable(&[0, 0, 0, 1, 0x01, 0x80]));
        // nal_ref_idc 2, slice; an IDR.
        assert!(!h264_droppable(&[0, 0, 0, 1, 0x41, 0x80]));
        assert!(!h264_droppable(&[0, 0, 0, 1, 0x65, 0x88]));
        // Two slices, one of them referenced.
        assert!(!h264_droppable(&[0, 0, 1, 0x01, 0x80, 0, 0, 1, 0x21, 0x00]));
        // Parameter sets only: no picture to drop, and they must be read.
        assert!(!h264_droppable(&[0, 0, 1, 0x67, 0x42, 0, 0, 1, 0x68, 0x01]));
        assert!(!h264_droppable(&[]));
    }

    #[test]
    fn rejects_a_damaged_packet_with_an_error_not_a_panic() {
        let packet = |data: &[u8]| Packet { data: data.to_vec(), pts_us: 0, key: true };
        let mut out = Vec::new();
        // A VP9 frame marker followed by nothing it can read.
        let mut vp9 = Decoder::new(Codec::Vp9).unwrap();
        assert!(vp9.decode(&packet(&[0x82, 0x49, 0x83, 0x42, 0xff, 0xff]), &mut out).is_err());
        assert!(out.is_empty());
        // The others: whatever they answer, they answer.
        for codec in [Codec::H264, Codec::Av1] {
            let mut d = Decoder::new(codec).unwrap();
            let _ = d.decode(&packet(&[0, 0, 1, 0x65, 0xff, 0xff, 0xff, 0x12, 0x00, 0x0a]), &mut out);
        }
    }

    fn packet(data: Vec<u8>) -> Packet {
        Packet { data, pts_us: 0, key: true }
    }

    /// A key frame's header naming 16384 x 16384, as `limits` builds it. Past the decoder this would ask for gigabytes.
    #[test]
    fn rejects_a_hostile_vp9_size_before_the_decoder_allocates() {
        // Key frame, shown; the sync code; four bits of colour; then width and height minus one, 16 bits each: 0x3fff, 0x3fff.
        let mut bits = vec![0b1000_0010u8, 0x49, 0x83, 0x42, 0x23, 0xff, 0xf3, 0xff, 0xf0];
        bits.extend_from_slice(&[0; 16]);
        let mut d = Decoder::new(Codec::Vp9).unwrap();
        let mut out = Vec::new();
        let e = d.decode(&packet(bits), &mut out).unwrap_err();
        assert!(e.contains("16384 x 16384"), "{e}");
        assert!(out.is_empty());
    }

    fn big_fixture(name: &str) -> Vec<Packet> {
        let path = format!("{}/../../drivers/lucina-video/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
        let mut d = lucina_video::open(std::io::Cursor::new(std::fs::read(path).unwrap())).unwrap();
        std::iter::from_fn(|| d.next_packet().unwrap()).collect()
    }

    /// Real files of 5000 x 4000: 20 MP, under what each decoder itself accepts for H.264 and over Juno's limit.
    #[test]
    fn rejects_real_files_above_the_limit_with_every_decoder() {
        for (name, codec) in [("too-large.mp4", Codec::H264), ("too-large.webm", Codec::Vp9), ("too-large-av1.mp4", Codec::Av1)] {
            let mut d = Decoder::new(codec).unwrap();
            let mut out = Vec::new();
            let mut failed = None;
            for p in big_fixture(name) {
                if let Err(e) = d.decode(&p, &mut out) {
                    failed = Some(e);
                    break;
                }
            }
            assert!(failed.is_some(), "{name} decoded");
            assert!(out.is_empty(), "{name} gave a picture");
        }
    }

    #[test]
    fn what_this_cannot_play_says_so() {
        for c in [Codec::Hevc, Codec::Mpeg2, Codec::Other] {
            let e = Decoder::new(c).err().unwrap();
            assert!(e.contains("server"), "{e}");
        }
    }

    fn decode_fixture(name: &str) -> Vec<(i64, usize, usize)> {
        let path = format!("{}/../../drivers/lucina-video/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
        let mut d = lucina_video::open(std::io::Cursor::new(std::fs::read(path).unwrap())).unwrap();
        let mut dec = Decoder::new(d.track().codec).unwrap();
        let mut out = Vec::new();
        while let Some(p) = d.next_packet().unwrap() {
            dec.decode(&p, &mut out).unwrap();
        }
        out.extend(dec.flush());
        let mut seen: Vec<(i64, usize, usize)> = out.iter().map(|p| { let s = p.planes().unwrap(); (p.pts_us(), s.w, s.h) }).collect();
        seen.sort_unstable();
        seen
    }

    /// Every picture has the time of a packet, in the order a screen shows them: a wrong time would hold the player up.
    #[test]
    fn pictures_carry_the_times_of_their_packets() {
        for name in std::env::var("LUCINA_VIDEO_CHECK").map(|p| vec![p]).unwrap_or_default().into_iter().chain(["plain.mp4", "vp9.webm", "av1.mp4"].map(|n| format!("{}/../../drivers/lucina-video/tests/fixtures/{n}", env!("CARGO_MANIFEST_DIR")))) {
            let mut d = lucina_video::open(std::io::Cursor::new(std::fs::read(&name).unwrap())).unwrap();
            let mut dec = Decoder::new(d.track().codec).unwrap();
            let mut packets = Vec::new();
            let mut arrived = Vec::new();
            let mut out = Vec::new();
            while let Some(p) = d.next_packet().unwrap() {
                packets.push(p.pts_us);
                dec.decode(&p, &mut out).unwrap();
                arrived.extend(out.drain(..).map(|p| p.pts_us()));
            }
            arrived.extend(dec.flush().iter().map(|p| p.pts_us()));
            println!("{name}: packets {packets:?}\narrived {arrived:?}");
            let mut sorted = packets.clone();
            sorted.sort_unstable();
            let mut got = arrived.clone();
            got.sort_unstable();
            assert_eq!(got, sorted, "{name}");
        }
    }

    #[test]
    fn each_codec_comes_out_as_pictures_with_their_times() {
        let h = decode_fixture("plain.mp4");
        assert_eq!(h.len(), 12);
        assert_eq!((h[0].1, h[0].2), (64, 48));
        assert_eq!(h[3].0 / 100_000, 3);
        assert_eq!(decode_fixture("h264.mkv").len(), 12);
        assert_eq!(decode_fixture("frag.mp4").len(), 12);
        let v = decode_fixture("vp9.webm");
        assert_eq!(v.len(), 12);
        assert_eq!(v[5].0 / 100_000, 5);
        let a = decode_fixture("av1.mp4");
        assert_eq!(a.len(), 12);
        assert_eq!((a[0].1, a[0].2), (64, 64));
        assert_eq!(a[5].0 / 100_000, 5);
    }
}
