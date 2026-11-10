//! What a file may ask of this machine. A file names its own picture size and a decoder believes it, so a hostile one costs as much memory
//! as it says: `filmcraft-vp9` 0.9.0 takes 16384 x 16384, about 800 MB a frame times its 8 references, and nothing the audit of it found would
//! stop that. So the size is read here, before the decoder sees the packet.

/// The most luma samples in a picture: 4096 x 2304, above 4K. Far past what plays here, so it refuses nothing real, and with a decoder's
/// references and the player's queue it stays in the hundreds of megabytes.
pub const MAX_LUMA_SAMPLES: u64 = 4096 * 2304;

/// The sentence for a person.
pub fn too_large(w: u64, h: u64) -> String {
    format!("this video's pictures are {w} x {h}, larger than this laptop can hold: it needs the server converting it")
}

struct Bits<'a> {
    b: &'a [u8],
    at: usize,
}

impl Bits<'_> {
    fn read(&mut self, n: usize) -> Option<u32> {
        let mut v = 0;
        for _ in 0..n {
            let byte = *self.b.get(self.at / 8)?;
            v = v << 1 | (byte >> (7 - self.at % 8) & 1) as u32;
            self.at += 1;
        }
        Some(v)
    }
}

fn color_config(r: &mut Bits, profile: u32) -> Option<()> {
    if profile >= 2 {
        r.read(1)?;
    }
    let space = r.read(3)?;
    if space != 7 {
        r.read(1)?;
        if profile == 1 || profile == 3 {
            r.read(3)?;
        }
    } else if profile == 1 || profile == 3 {
        r.read(1)?;
    }
    Some(())
}

/// The size one VP9 frame's uncompressed header names, if it names one: a frame may instead take its size from a reference, which was
/// checked when that one came. Damage answers `None`, and the decoder then says what is wrong.
fn vp9_one(frame: &[u8]) -> Option<(u32, u32)> {
    let mut r = Bits { b: frame, at: 0 };
    if r.read(2)? != 2 {
        return None;
    }
    let low = r.read(1)?;
    let high = r.read(1)?;
    let profile = low | high << 1;
    if profile == 3 {
        r.read(1)?;
    }
    if r.read(1)? == 1 {
        return None; // show_existing_frame
    }
    let key = r.read(1)? == 0;
    let show = r.read(1)? == 1;
    let error_resilient = r.read(1)? == 1;
    let explicit = if key {
        if r.read(24)? != 0x49_8342 {
            return None;
        }
        color_config(&mut r, profile)?;
        true
    } else {
        let intra_only = if show { false } else { r.read(1)? == 1 };
        if !error_resilient {
            r.read(2)?;
        }
        if intra_only {
            if r.read(24)? != 0x49_8342 {
                return None;
            }
            if profile > 0 {
                color_config(&mut r, profile)?;
            }
            r.read(8)?;
            true
        } else {
            r.read(8)?;
            for _ in 0..3 {
                r.read(4)?;
            }
            // Frame size with refs: a bit per reference, and the first one set gives the size.
            let mut from_ref = false;
            for _ in 0..3 {
                if r.read(1)? == 1 {
                    from_ref = true;
                    break;
                }
            }
            !from_ref
        }
    };
    if !explicit {
        return None;
    }
    let w = r.read(16)? + 1;
    let h = r.read(16)? + 1;
    Some((w, h))
}

/// Every picture size a VP9 packet names. A packet may hold several frames, listed in an index at its end (a superframe).
pub fn vp9_sizes(packet: &[u8]) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    let Some(&last) = packet.last() else { return out };
    if last & 0xe0 == 0xc0 {
        let frames = (last & 7) as usize + 1;
        let bytes = ((last >> 3) & 3) as usize + 1;
        let index = 2 + frames * bytes;
        if packet.len() >= index && packet[packet.len() - index] == last {
            let list = &packet[packet.len() - index + 1..packet.len() - 1];
            let mut at = 0usize;
            for size in list.chunks_exact(bytes) {
                let n = size.iter().rev().fold(0usize, |v, b| v << 8 | *b as usize);
                if let Some(f) = packet.get(at..at.saturating_add(n))
                    && let Some(s) = vp9_one(f)
                {
                    out.push(s);
                }
                at = at.saturating_add(n);
            }
            return out;
        }
    }
    out.extend(vp9_one(packet));
    out
}

/// `Err` with the sentence when this VP9 packet asks for too much.
pub fn check_vp9(packet: &[u8]) -> Result<(), String> {
    for (w, h) in vp9_sizes(packet) {
        if w as u64 * h as u64 > MAX_LUMA_SAMPLES {
            return Err(too_large(w as u64, h as u64));
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[derive(Default)]
    struct W {
        b: Vec<u8>,
        n: usize,
    }

    impl W {
        fn put(&mut self, v: u32, bits: usize) -> &mut W {
            for i in (0..bits).rev() {
                if self.n % 8 == 0 {
                    self.b.push(0);
                }
                let last = self.b.len() - 1;
                self.b[last] |= ((v as u64 >> i & 1) as u8) << (7 - self.n % 8);
                self.n += 1;
            }
            self
        }
    }

    /// A key frame's header naming this size, and no picture behind it: all a hostile file needs to get a decoder to allocate.
    pub(crate) fn hostile_key_frame(w: u32, h: u32) -> Vec<u8> {
        let mut f = W::default();
        f.put(2, 2).put(0, 2).put(0, 1).put(0, 1).put(1, 1).put(0, 1);
        f.put(0x49_8342, 24).put(1, 3).put(0, 1);
        f.put(w - 1, 16).put(h - 1, 16);
        f.put(0, 64);
        f.b
    }

    #[test]
    fn reads_the_size_from_a_key_frame() {
        assert_eq!(vp9_sizes(&hostile_key_frame(640, 360)), [(640, 360)]);
        assert_eq!(vp9_sizes(&hostile_key_frame(16384, 16384)), [(16384, 16384)]);
    }

    #[test]
    fn rejects_a_hostile_key_frame_and_lets_a_real_size_through() {
        assert!(check_vp9(&hostile_key_frame(16384, 16384)).unwrap_err().contains("16384 x 16384"));
        assert!(check_vp9(&hostile_key_frame(4097, 2304)).is_err());
        assert!(check_vp9(&hostile_key_frame(4096, 2304)).is_ok());
        assert!(check_vp9(&hostile_key_frame(3840, 2160)).is_ok());
        assert!(check_vp9(&hostile_key_frame(2160, 3840)).is_ok());
    }

    #[test]
    fn rejects_a_hostile_size_on_an_inter_frame_or_intra_only_frame() {
        // Inter frame: shown, not resilient, no reference gives the size, so it names one.
        let mut f = W::default();
        f.put(2, 2).put(0, 2).put(0, 1).put(1, 1).put(1, 1).put(0, 1);
        f.put(0, 2).put(0xff, 8).put(0, 12).put(0, 3);
        f.put(16383, 16).put(16383, 16).put(0, 32);
        assert!(check_vp9(&f.b).is_err());
        // With a reference giving it, there is nothing here to read.
        let mut f = W::default();
        f.put(2, 2).put(0, 2).put(0, 1).put(1, 1).put(1, 1).put(0, 1);
        f.put(0, 2).put(0xff, 8).put(0, 12).put(1, 1).put(0, 32);
        assert_eq!(vp9_sizes(&f.b), []);
        // Intra-only: not shown, profile 0, so no colour.
        let mut f = W::default();
        f.put(2, 2).put(0, 2).put(0, 1).put(1, 1).put(0, 1).put(0, 1);
        f.put(1, 1).put(0, 2).put(0x49_8342, 24).put(0xff, 8);
        f.put(16383, 16).put(16383, 16).put(0, 32);
        assert!(check_vp9(&f.b).is_err());
        // Another profile has more colour bits before the size.
        let mut f = W::default();
        f.put(2, 2).put(1, 1).put(1, 1).put(0, 1).put(0, 1).put(0, 1).put(1, 1).put(0, 1);
        f.put(0x49_8342, 24).put(1, 1).put(1, 3).put(0, 1).put(0, 3);
        f.put(16383, 16).put(16383, 16).put(0, 32);
        assert_eq!(vp9_sizes(&f.b), [(16384, 16384)]);
    }

    #[test]
    fn a_large_frame_cannot_hide_in_a_superframe() {
        let (small, large) = (hostile_key_frame(64, 48), hostile_key_frame(16384, 16384));
        let mut p = small.clone();
        p.extend_from_slice(&large);
        // Two frames, one byte per size.
        let marker = 0xc0 | 1;
        p.extend_from_slice(&[marker, small.len() as u8, large.len() as u8, marker]);
        assert_eq!(vp9_sizes(&p), [(64, 48), (16384, 16384)]);
        assert!(check_vp9(&p).is_err());
    }

    #[test]
    fn damage_is_left_for_the_decoder_and_never_panics() {
        assert_eq!(vp9_sizes(&[]), []);
        assert_eq!(vp9_sizes(&[0x00, 0x00]), []);
        let f = hostile_key_frame(16384, 16384);
        for n in 0..f.len() {
            let _ = vp9_sizes(&f[..n]);
        }
        // An index claiming more than the packet holds.
        assert_eq!(vp9_sizes(&[0xc1, 200, 200, 0xc1]), []);
        // A show-existing frame names nothing.
        assert_eq!(vp9_sizes(&[0b1000_1000, 0]), []);
    }

    #[test]
    fn real_files_read_as_their_size() {
        let path = format!("{}/tests/fixtures/vp9.webm", env!("CARGO_MANIFEST_DIR"));
        let mut d = crate::open(std::io::Cursor::new(std::fs::read(path).unwrap())).unwrap();
        let first = d.next_packet().unwrap().unwrap();
        assert_eq!(vp9_sizes(&first.data), [(64, 48)]);
    }
}
