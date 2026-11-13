//! App icons: one pack for every program, mapped from `/usr/share/lucina/icons.pack` like the fonts, so
//! a tile for an app that is still on the disk has its icon before the first frame, and no binary carries
//! pictures. Sources and the baking tool: `assets/icons/juno/`, `tools/lucina-icons`.
//!
//! The format is what drawing wants, so a draw decodes nothing into memory and allocates nothing. All numbers
//! little-endian:
//!
//! ```text
//! "LIP1"  count: u16  nsizes: u8  0: u8  sizes: [u8; nsizes], ascending
//! count entries, sorted by id:   id: [u8; 16], NUL padded    offsets: [u32; nsizes]
//! each icon at each size, at its offset:
//!     palette length - 1: u8     palette: ARGB u32 (straight alpha)
//!     rows, top to bottom, of runs that add up to the width:
//!       00nnnnnn              n+1 pixels left alone
//!       01nnnnnn idx          n+1 pixels of one colour
//!       10nnnnnn idx * (n+1)  n+1 pixels, each its own colour
//! ```

use std::sync::OnceLock;

use crate::gfx::{Face, Frame, Rect, Rgb, Text, load_font};
use crate::theme;

/// Where the pack is on the device.
pub const PACK_FILE: &str = "/usr/share/lucina/icons.pack";

const MAGIC: &[u8; 4] = b"LIP1";
const ID_LEN: usize = 16;
/// The plate's corner, as a fraction of its side: the same as the sources' `rx="12"` on 64.
const PLATE_RADIUS: f32 = 12.0 / 64.0;

/// A pack of icons, checked enough that a damaged file gives no icons rather than a panic.
#[derive(Clone, Copy, Debug)]
pub struct Pack<'a> {
    count: usize,
    sizes: &'a [u8],
    index: &'a [u8],
    bytes: &'a [u8],
}

/// One icon at one size, ready to draw.
#[derive(Clone, Copy, Debug)]
pub struct Icon<'a> {
    /// Its width and height in pixels.
    pub side: u32,
    data: &'a [u8],
}

impl<'a> Pack<'a> {
    pub fn parse(bytes: &'a [u8]) -> Option<Pack<'a>> {
        if bytes.get(..4)? != MAGIC {
            return None;
        }
        let count = u16::from_le_bytes([*bytes.get(4)?, *bytes.get(5)?]) as usize;
        let nsizes = *bytes.get(6)? as usize;
        let sizes = bytes.get(8..8 + nsizes)?;
        if nsizes == 0 || sizes.contains(&0) {
            return None;
        }
        let start = 8 + nsizes;
        let index = bytes.get(start..start + count * (ID_LEN + 4 * nsizes))?;
        Some(Pack {
            count,
            sizes,
            index,
            bytes,
        })
    }

    fn entry(&self, i: usize) -> &'a [u8] {
        let n = ID_LEN + 4 * self.sizes.len();
        &self.index[i * n..(i + 1) * n]
    }

    /// Every icon's name, in order.
    pub fn ids(&self) -> impl Iterator<Item = &'a str> + '_ {
        (0..self.count).filter_map(|i| {
            let id = &self.entry(i)[..ID_LEN];
            let len = id.iter().position(|&b| b == 0).unwrap_or(ID_LEN);
            std::str::from_utf8(&id[..len]).ok()
        })
    }

    /// The sizes each icon comes in, in pixels.
    pub fn sizes(&self) -> &'a [u8] {
        self.sizes
    }

    /// The icon called `id`, at the baked size nearest `side`.
    pub fn get(&self, id: &str, side: u32) -> Option<Icon<'a>> {
        if id.is_empty() || id.len() > ID_LEN {
            return None;
        }
        let mut key = [0u8; ID_LEN];
        key[..id.len()].copy_from_slice(id.as_bytes());
        let (mut lo, mut hi) = (0, self.count);
        while lo < hi {
            let mid = (lo + hi) / 2;
            let entry = self.entry(mid);
            match entry[..ID_LEN].cmp(&key) {
                std::cmp::Ordering::Equal => {
                    let which = (0..self.sizes.len())
                        .min_by_key(|&s| (self.sizes[s] as u32).abs_diff(side))?;
                    let o = ID_LEN + 4 * which;
                    let offset = u32::from_le_bytes(entry[o..o + 4].try_into().ok()?) as usize;
                    return Some(Icon {
                        side: self.sizes[which] as u32,
                        data: self.bytes.get(offset..)?,
                    });
                }
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
            }
        }
        None
    }
}

impl Icon<'_> {
    /// Draw with its top-left corner at (`x`, `y`), inside the frame's clip. Damaged data ends the
    /// drawing early and that is all.
    pub fn draw(&self, fr: &mut Frame, x: i32, y: i32) {
        let side = self.side as i32;
        let clip = fr.clip();
        let frame_w = fr.w as usize;
        let Some((&last, rest)) = self.data.split_first() else {
            return;
        };
        let n = last as usize + 1;
        let Some(palette) = rest.get(..n * 4) else {
            return;
        };
        let mut colours = [0u32; 256];
        for (c, b) in colours.iter_mut().zip(palette.as_chunks::<4>().0.iter()) {
            *c = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        }
        let ops = &rest[n * 4..];
        let mut at = 0;
        for row in 0..side {
            let py = y + row;
            let shown = py >= clip.y && py < clip.bottom();
            let line = py.max(0) as usize * frame_w;
            let mut col = 0;
            while col < side {
                let Some(&op) = ops.get(at) else { return };
                at += 1;
                let len = (op & 0x3f) as i32 + 1;
                let (from, to) = (x + col, x + col + len);
                let vis = from.max(clip.x)..to.min(clip.right());
                match op >> 6 {
                    0 => {}
                    1 => {
                        let Some(&idx) = ops.get(at) else { return };
                        at += 1;
                        let c = colours[idx as usize];
                        if shown && !vis.is_empty() {
                            let span =
                                &mut fr.px[line + vis.start as usize..line + vis.end as usize];
                            if c >> 24 == 255 {
                                span.fill(c & 0x00ff_ffff);
                            } else {
                                for d in span {
                                    *d = over(c, *d);
                                }
                            }
                        }
                    }
                    2 => {
                        let Some(idxs) = ops.get(at..at + len as usize) else {
                            return;
                        };
                        at += len as usize;
                        if shown {
                            for px in vis {
                                let c = colours[idxs[(px - from) as usize] as usize];
                                let d = &mut fr.px[line + px as usize];
                                *d = over(c, *d);
                            }
                        }
                    }
                    _ => return,
                }
                col += len;
            }
        }
    }
}

fn over(c: u32, under: u32) -> u32 {
    Rgb(c & 0x00ff_ffff).over(Rgb(under), (c >> 24) as u8).0
}

/// The device's pack, mapped once. `LUCINA_ICON_PACK` overrides it; off the device (tests, `--render`) it
/// is the source tree's. `None` when there is none: every caller then falls back to a letter.
pub fn pack() -> Option<&'static Pack<'static>> {
    static PACK: OnceLock<Option<Pack<'static>>> = OnceLock::new();
    PACK.get_or_init(|| {
        let mut files: Vec<std::path::PathBuf> = std::env::var_os("LUCINA_ICON_PACK")
            .map(Into::into)
            .into_iter()
            .collect();
        files.push(PACK_FILE.into());
        #[cfg(not(target_env = "musl"))]
        files.push(concat!(env!("CARGO_MANIFEST_DIR"), "/icons/juno.pack").into());
        files.iter().find_map(|f| {
            let bytes = load_font(f.parent()?, f.file_name()?.to_str()?)?;
            Pack::parse(bytes)
        })
    })
    .as_ref()
}

/// The icon called `id` from the device's pack, at the size nearest `side`.
pub fn get(id: &str, side: u32) -> Option<Icon<'static>> {
    pack()?.get(id, side)
}

/// Whether a manifest's `icon` value names an icon in a pack, rather than being characters to show.
pub fn is_name(s: &str) -> bool {
    s.len() >= 3
        && s.len() <= ID_LEN
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// What stands in for a missing icon: the same plate, with characters on it in gold.
pub fn draw_letter(fr: &mut Frame, t: &mut Text, r: Rect, letter: &str) {
    let rad = (r.w.min(r.h) as f32 * PLATE_RADIUS).round() as i32;
    fr.fill_round_rect(r, rad, theme::TERM_BG);
    let size = (r.h * 9 / 16).clamp(8, 255) as u16;
    let w = t.measure(Face::Bold, size, letter);
    let base = r.y + (r.h + t.ascent(Face::Bold, size) - t.descent(Face::Bold, size)) / 2;
    t.draw(
        fr,
        r.x + (r.w - w) / 2,
        base,
        Face::Bold,
        size,
        theme::GOLD,
        letter,
    );
}

/// The icon called `id` centred in `r`, or `letter` on a plate when there is no such icon.
pub fn draw_in(fr: &mut Frame, t: &mut Text, r: Rect, id: &str, letter: &str) {
    match get(id, r.w.min(r.h).max(1) as u32) {
        Some(icon) => {
            let side = icon.side as i32;
            icon.draw(fr, r.x + (r.w - side) / 2, r.y + (r.h - side) / 2);
        }
        None => draw_letter(fr, t, r, letter),
    }
}

/// The name of this app's icon: the shell says it in `LUCINA_APP_ICON` when it starts the app.
pub fn own_name() -> Option<&'static str> {
    static OWN: OnceLock<Option<String>> = OnceLock::new();
    OWN.get_or_init(|| std::env::var("LUCINA_APP_ICON").ok().filter(|s| is_name(s)))
        .as_deref()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pack of one 2x2 icon: a plain pixel, half-covered, then one left alone and one solid.
    fn tiny() -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(MAGIC);
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&[1, 0, 2]);
        let mut id = [0u8; ID_LEN];
        id[..3].copy_from_slice(b"one");
        b.extend_from_slice(&id);
        let offset = b.len() as u32 + 4;
        b.extend_from_slice(&offset.to_le_bytes());
        b.push(1);
        b.extend_from_slice(&0xff11_2233u32.to_le_bytes());
        b.extend_from_slice(&0x80ff_ffffu32.to_le_bytes());
        b.extend_from_slice(&[0b1000_0001, 0, 1]);
        b.extend_from_slice(&[0, 0b0100_0000, 0]);
        b
    }

    #[test]
    fn draws_opaque_blended_and_skipped_pixels() {
        let bytes = tiny();
        let pack = Pack::parse(&bytes).unwrap();
        assert_eq!(pack.ids().collect::<Vec<_>>(), ["one"]);
        let icon = pack.get("one", 64).unwrap();
        assert_eq!(icon.side, 2);
        let mut fr = Frame::new(4, 4);
        icon.draw(&mut fr, 1, 1);
        assert_eq!(fr.get(1, 1), Some(Rgb(0x112233)));
        assert_ne!(fr.get(2, 1), Some(Rgb(0)));
        assert_eq!(fr.get(1, 2), Some(Rgb(0)));
        assert_eq!(fr.get(2, 2), Some(Rgb(0x112233)));
    }

    #[test]
    fn stays_in_the_clip_and_off_the_edges() {
        let bytes = tiny();
        let icon = Pack::parse(&bytes).unwrap().get("one", 2).unwrap();
        let mut fr = Frame::new(2, 2);
        for (x, y) in [(-1, -1), (1, 1), (-5, 0), (0, 5), (1, -1), (-1, 1)] {
            icon.draw(&mut fr, x, y);
        }
        let mut fr = Frame::new(4, 4);
        fr.set_clip(Rect::new(1, 1, 1, 1));
        icon.draw(&mut fr, 0, 0);
        assert_eq!(fr.get(0, 0), Some(Rgb(0)));
        assert_eq!(fr.get(1, 1), Some(Rgb(0x112233)));
    }

    #[test]
    fn a_damaged_pack_gives_no_icons_and_no_panic() {
        assert!(Pack::parse(b"nope").is_none());
        let bytes = tiny();
        for cut in 0..bytes.len() {
            let Some(pack) = Pack::parse(&bytes[..cut]) else {
                continue;
            };
            let mut fr = Frame::new(4, 4);
            if let Some(icon) = pack.get("one", 2) {
                icon.draw(&mut fr, 0, 0);
            }
        }
        let mut bad = bytes.clone();
        let len = bad.len();
        bad[len - 2] = 0xff;
        let pack = Pack::parse(&bad).unwrap();
        pack.get("one", 2)
            .unwrap()
            .draw(&mut Frame::new(4, 4), 0, 0);
    }

    #[test]
    fn nearest_size_and_unknown_names() {
        let bytes = tiny();
        let pack = Pack::parse(&bytes).unwrap();
        assert!(pack.get("two", 2).is_none());
        assert!(pack.get("", 2).is_none());
        assert!(pack.get("a-name-longer-than-sixteen", 2).is_none());
    }

    #[test]
    fn names_are_told_from_characters_and_paths() {
        assert!(is_name("notes"));
        assert!(is_name("net"));
        assert!(!is_name("N"));
        assert!(!is_name("Ab"));
        assert!(!is_name("/usr/share/x.png"));
        assert!(!is_name("Note"));
    }

    #[test]
    fn a_missing_icon_gets_a_plate_with_its_letter() {
        let mut fr = Frame::new(64, 64);
        let mut t = Text::new();
        let r = Rect::new(0, 0, 64, 64);
        draw_in(&mut fr, &mut t, r, "no-such-icon", "Z");
        assert_eq!(fr.get(4, 32), Some(theme::TERM_BG));
        assert!((0..64).any(|x| fr.get(x, 32) == Some(theme::GOLD)));
    }

    /// Everything that ships: every app with a manifest naming an icon has it, at every size, and it
    /// draws cleanly from the source tree's pack.
    #[test]
    fn the_pack_covers_the_apps_and_draws() {
        let bytes = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/icons/juno.pack")).unwrap();
        let pack = Pack::parse(&bytes).unwrap();
        let crates = concat!(env!("CARGO_MANIFEST_DIR"), "/..");
        let mut seen = 0;
        for e in std::fs::read_dir(crates).unwrap().flatten() {
            let Ok(text) = std::fs::read_to_string(e.path().join("manifest.app")) else {
                continue;
            };
            let m = crate::Manifest::parse(&text).unwrap();
            if is_name(&m.icon) {
                assert!(pack.get(&m.icon, 64).is_some(), "{} has no icon", m.id);
                seen += 1;
            }
        }
        assert!(seen >= 2, "only {seen} manifests name an icon");
        for id in pack.ids().collect::<Vec<_>>() {
            for &s in pack.sizes() {
                let icon = pack.get(id, s as u32).unwrap();
                assert_eq!(icon.side, s as u32);
                let mut fr = Frame::new(s as u32, s as u32);
                fr.px.fill(0x00ab_cdef);
                icon.draw(&mut fr, 0, 0);
                // The plate: the centre of the top edge is the plate's colour, whatever the glyph. The pack's plate is
                // baked into the SVGs (`assets/icons/juno`, and `tools/lucina-icons` refuses one without it) at the
                // terminal ground as it was when they were drawn, so the expectation is that baked value: the icons
                // follow the theme when the pack is rebuilt, not before.
                assert_eq!(
                    fr.get(s as i32 / 2, 0),
                    Some(Rgb::hex(0x050608)),
                    "{id} at {s}"
                );
            }
        }
    }
}
