//! The banner inside every `.nds`/`.dsi` file: a 32x32 icon, its 16-colour palette and the game's title in six
//! languages. Layout as GBATEK "DS Cartridge Icon/Title" describes it; the CRC is the BIOS CRC-16 (swiCRC16) that
//! `fortuna_launch::crc16` already implements.
//!
//! The icon is stored as 4x4 tiles of 8x8 pixels, 4 bits a pixel, row-major by tile: exactly the DS OBJ tile format
//! with 1D mapping. The kernel copies [`Banner::icon`] into OBJ VRAM and [`Banner::palette`] into an OBJ palette slot
//! and the game has its icon on screen, with no conversion. Palette entry 0 is transparent.

use crate::rules;

/// Header bytes the reader needs to find the banner and the header title.
pub const HEADER_NEEDED: usize = 0x6c;
/// Banner bytes [`parse`] needs: the part every banner version shares.
pub const BANNER_NEEDED: usize = 0x840;

const OFF_CRC: usize = 0x02;
const OFF_ICON: usize = 0x20;
const OFF_PALETTE: usize = 0x220;
const OFF_TITLE_EN: usize = 0x340;
const TITLE_UNITS: usize = 128;

/// Where the banner starts in the file, from the header (offset 0x68), or `None` for a header that is too short or
/// a game without a banner. Check the result against the file length with [`crate::rules::banner_span`].
pub fn banner_offset(header: &[u8]) -> Option<u32> {
    if header.len() < HEADER_NEEDED {
        return None;
    }
    let off = u32::from_le_bytes([header[0x68], header[0x69], header[0x6a], header[0x6b]]);
    if off < rules::HEADER_END {
        None
    } else {
        Some(off)
    }
}

/// The four-letter game code at 0x0C (`ASME` style): stable across file renames, the key "Recently played" uses for
/// DS games. `None` when the header is short or the code is not printable ASCII (homebrew often leaves it zero).
pub fn game_code(header: &[u8]) -> Option<[u8; 4]> {
    if header.len() < 0x10 {
        return None;
    }
    let c = [header[0x0c], header[0x0d], header[0x0e], header[0x0f]];
    if c.iter().all(|&b| (0x21..0x7f).contains(&b)) {
        Some(c)
    } else {
        None
    }
}

/// The 12-byte header title at 0x00 without its trailing NULs and spaces (`MARIOKARTDS`): the fallback when a game
/// has no banner title.
pub fn header_title(header: &[u8]) -> &[u8] {
    let t = &header[..header.len().min(12)];
    let mut end = t.len();
    while end > 0 && (t[end - 1] == 0 || t[end - 1] == b' ') {
        end -= 1;
    }
    let start = t.iter().position(|&b| b != 0 && b != b' ').unwrap_or(end);
    &t[start.min(end)..end]
}

/// Why a banner was not taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BannerError {
    /// Fewer than [`BANNER_NEEDED`] bytes.
    Short,
    /// A version this reader does not know (1, 2, 3 and 0x103 are the known ones).
    Version(u16),
    /// The CRC over the icon, palette and titles does not match: not a banner, or a damaged file.
    Crc { stored: u16, computed: u16 },
}

/// A game's banner: what the Games screen shows for it.
#[derive(Clone, PartialEq, Eq)]
pub struct Banner {
    pub version: u16,
    /// 32x32 pixels as 16 OBJ tiles, 4 bits a pixel.
    pub icon: [u8; 512],
    /// BGR555, bit 15 clear; entry 0 is transparent.
    pub palette: [u16; 16],
    /// The English title as UTF-16, up to 128 units, NUL-padded; lines are separated by '\n'
    /// (title, subtitle, publisher).
    pub title: [u16; TITLE_UNITS],
}

impl core::fmt::Debug for Banner {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Banner").field("version", &self.version).finish_non_exhaustive()
    }
}

/// Reads a banner from its first [`BANNER_NEEDED`] bytes (more are ignored), checking its version and CRC.
pub fn parse(b: &[u8]) -> Result<Banner, BannerError> {
    if b.len() < BANNER_NEEDED {
        return Err(BannerError::Short);
    }
    let version = u16::from_le_bytes([b[0], b[1]]);
    if !matches!(version, 0x0001 | 0x0002 | 0x0003 | 0x0103) {
        return Err(BannerError::Version(version));
    }
    let stored = u16::from_le_bytes([b[OFF_CRC], b[OFF_CRC + 1]]);
    let computed = fortuna_launch::crc16(0xffff, &b[OFF_ICON..BANNER_NEEDED]);
    if stored != computed {
        return Err(BannerError::Crc { stored, computed });
    }
    let mut icon = [0u8; 512];
    icon.copy_from_slice(&b[OFF_ICON..OFF_PALETTE]);
    let mut palette = [0u16; 16];
    for (i, p) in palette.iter_mut().enumerate() {
        let o = OFF_PALETTE + 2 * i;
        *p = u16::from_le_bytes([b[o], b[o + 1]]) & 0x7fff;
    }
    let mut title = [0u16; TITLE_UNITS];
    for (i, t) in title.iter_mut().enumerate() {
        let o = OFF_TITLE_EN + 2 * i;
        *t = u16::from_le_bytes([b[o], b[o + 1]]);
    }
    Ok(Banner { version, icon, palette, title })
}

impl Banner {
    /// Line `line` of the English title (0 = the title, 1 = subtitle or publisher, ...) as UTF-8 into `out`; returns
    /// the bytes written. Stops at the end of the line, at NUL, or when `out` is full (never inside a character).
    /// Unpaired surrogates become U+FFFD; trailing spaces are dropped.
    pub fn title_line(&self, line: usize, out: &mut [u8]) -> usize {
        let mut cur = 0usize;
        let mut n = 0usize;
        for r in char::decode_utf16(self.title.iter().copied()) {
            let c = r.unwrap_or(char::REPLACEMENT_CHARACTER);
            if c == '\0' {
                break;
            }
            if c == '\n' {
                if cur == line {
                    break;
                }
                cur += 1;
                continue;
            }
            if c == '\r' || cur != line {
                continue;
            }
            let mut buf = [0u8; 4];
            let s = c.encode_utf8(&mut buf);
            if n + s.len() > out.len() {
                break;
            }
            out[n..n + s.len()].copy_from_slice(s.as_bytes());
            n += s.len();
        }
        while n > 0 && out[n - 1] == b' ' {
            n -= 1;
        }
        n
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A valid version-1 banner with a given English title, an icon of value `fill` and a ramp palette.
    pub(crate) fn make(title: &str, fill: u8) -> [u8; BANNER_NEEDED] {
        let mut b = [0u8; BANNER_NEEDED];
        b[0] = 1;
        for v in &mut b[OFF_ICON..OFF_PALETTE] {
            *v = fill;
        }
        for i in 0..16 {
            let c = (i as u16 * 2) | 0x8000; // bit 15 set on purpose: the reader clears it
            b[OFF_PALETTE + 2 * i..OFF_PALETTE + 2 * i + 2].copy_from_slice(&c.to_le_bytes());
        }
        for (i, u) in title.encode_utf16().take(TITLE_UNITS).enumerate() {
            b[OFF_TITLE_EN + 2 * i..OFF_TITLE_EN + 2 * i + 2].copy_from_slice(&u.to_le_bytes());
        }
        let crc = fortuna_launch::crc16(0xffff, &b[OFF_ICON..BANNER_NEEDED]);
        b[OFF_CRC..OFF_CRC + 2].copy_from_slice(&crc.to_le_bytes());
        b
    }

    fn line(b: &Banner, l: usize) -> std::string::String {
        let mut out = [0u8; 256];
        let n = b.title_line(l, &mut out);
        std::string::String::from_utf8(out[..n].to_vec()).unwrap()
    }

    #[test]
    fn reads_a_banner() {
        let raw = make("Sample Game\nA homebrew title\nLucina", 0x21);
        let b = parse(&raw).unwrap();
        assert_eq!(b.version, 1);
        assert!(b.icon.iter().all(|&v| v == 0x21));
        assert_eq!(b.palette[3], 6);
        assert!(b.palette.iter().all(|&c| c & 0x8000 == 0));
        assert_eq!(line(&b, 0), "Sample Game");
        assert_eq!(line(&b, 1), "A homebrew title");
        assert_eq!(line(&b, 2), "Lucina");
        assert_eq!(line(&b, 3), "");
    }

    #[test]
    fn titles_in_utf8() {
        let b = parse(&make("Café über\r\nÆble", 0)).unwrap();
        assert_eq!(line(&b, 0), "Café über");
        assert_eq!(line(&b, 1), "Æble");
        // never cut inside a character
        let mut out = [0u8; 4];
        let n = b.title_line(0, &mut out);
        assert_eq!(&out[..n], b"Caf");
    }

    #[test]
    fn rejects_bad_banners() {
        let mut raw = make("X", 1);
        assert_eq!(parse(&raw[..BANNER_NEEDED - 1]), Err(BannerError::Short));
        raw[OFF_ICON + 5] ^= 0xff;
        assert!(matches!(parse(&raw), Err(BannerError::Crc { .. })));
        let mut raw = make("X", 1);
        raw[0] = 9;
        assert_eq!(parse(&raw), Err(BannerError::Version(9)));
    }

    #[test]
    fn header_fields() {
        let mut h = [0u8; 0x200];
        h[..11].copy_from_slice(b"MARIOKARTDS");
        h[0x0c..0x10].copy_from_slice(b"AMCE");
        h[0x68..0x6c].copy_from_slice(&0x0010_0000u32.to_le_bytes());
        assert_eq!(header_title(&h), b"MARIOKARTDS");
        assert_eq!(game_code(&h), Some(*b"AMCE"));
        assert_eq!(banner_offset(&h), Some(0x10_0000));
        h[0x68..0x6c].copy_from_slice(&0u32.to_le_bytes());
        assert_eq!(banner_offset(&h), None);
        h[0x0c..0x10].copy_from_slice(&[0, 0, 0, 0]);
        assert_eq!(game_code(&h), None);
        assert_eq!(banner_offset(&h[..0x60]), None);
        assert_eq!(header_title(&[0u8; 12]), b"");
    }

    /// A real ROM, when one is given: `FORTUNA_LIBRARY_NDS=<file.nds> cargo test -p fortuna-library real_rom`.
    /// ROMs stay out of git, so without the variable there is nothing to read and the test checks nothing.
    #[test]
    fn real_rom() {
        let Ok(path) = std::env::var("FORTUNA_LIBRARY_NDS") else { return };
        let data = std::fs::read(&path).unwrap();
        let off = banner_offset(&data).expect("banner offset");
        let (s, e) = crate::rules::banner_span(data.len() as u64, off).expect("banner inside the file");
        let b = parse(&data[s as usize..e as usize]).expect("valid banner");
        std::println!("{path}: v{} title {:?} code {:?}", b.version, line(&b, 0), game_code(&data));
        assert!(!line(&b, 0).is_empty());
    }
}
