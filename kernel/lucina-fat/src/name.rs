//! File names: validation, 8.3 short names (code page 437), the long-name checksum and case-insensitive comparison.
//!
//! The rules are Linux's `fs/fat/namei_vfat.c` with its default mount options (`shortname=mixed`, `iocharset=utf8`,
//! `nonumtail=0`): a name that is already 8.3 and all upper case is stored as a short entry only; a name that fits
//! 8.3 but has lower-case letters gets a long entry plus the upper-cased short name; any other name gets a long
//! entry plus a short name with a `~N` tail.

use alloc::{string::String, vec::Vec};

use crate::error::{Error, Result};

/// Longest name in UTF-16 units.
pub const MAX_NAME_UNITS: usize = 255;

/// Code page 437, the characters of bytes 0x80..=0xFF.
const CP437: [char; 128] = [
    'Ç', 'ü', 'é', 'â', 'ä', 'à', 'å', 'ç', 'ê', 'ë', 'è', 'ï', 'î', 'ì', 'Ä', 'Å', //
    'É', 'æ', 'Æ', 'ô', 'ö', 'ò', 'û', 'ù', 'ÿ', 'Ö', 'Ü', '¢', '£', '¥', '₧', 'ƒ', //
    'á', 'í', 'ó', 'ú', 'ñ', 'Ñ', 'ª', 'º', '¿', '⌐', '¬', '½', '¼', '¡', '«', '»', //
    '░', '▒', '▓', '│', '┤', '╡', '╢', '╖', '╕', '╣', '║', '╗', '╝', '╜', '╛', '┐', //
    '└', '┴', '┬', '├', '─', '┼', '╞', '╟', '╚', '╔', '╩', '╦', '╠', '═', '╬', '╧', //
    '╨', '╤', '╥', '╙', '╘', '╒', '╓', '╫', '╪', '┘', '┌', '█', '▄', '▌', '▐', '▀', //
    'α', 'ß', 'Γ', 'π', 'Σ', 'σ', 'µ', 'τ', 'Φ', 'Θ', 'Ω', 'δ', '∞', 'φ', 'ε', '∩', //
    '≡', '±', '≥', '≤', '⌠', '⌡', '÷', '≈', '°', '∙', '·', '√', 'ⁿ', '²', '■', '\u{a0}',
];

/// The checksum that ties long-name slots to their short entry.
pub fn checksum(short: &[u8; 11]) -> u8 {
    let mut sum = 0u8;
    for &b in short {
        sum = sum.rotate_right(1).wrapping_add(b);
    }
    sum
}

fn single_lower(c: char) -> char {
    let mut it = c.to_lowercase();
    match (it.next(), it.next()) {
        (Some(l), None) => l,
        _ => c,
    }
}

fn single_upper(c: char) -> char {
    let mut it = c.to_uppercase();
    match (it.next(), it.next()) {
        (Some(u), None) => u,
        _ => c,
    }
}

/// Whether two names are equal when case is ignored.
pub fn names_equal(a: &str, b: &str) -> bool {
    if a.is_ascii() && b.is_ascii() {
        return a.eq_ignore_ascii_case(b);
    }
    let mut x = a.chars().map(single_lower);
    let mut y = b.chars().map(single_lower);
    loop {
        match (x.next(), y.next()) {
            (None, None) => return true,
            (Some(p), Some(q)) if p == q => {}
            _ => return false,
        }
    }
}

/// Checks a name for creation and returns it as the UTF-16 units that go into the long-name slots.
/// Trailing dots are dropped (Windows and Linux do), a trailing space is refused.
pub fn long_units(name: &str) -> Result<(String, Vec<u16>)> {
    let trimmed = name.trim_end_matches('.');
    if trimmed.is_empty() {
        // "", "." and ".." (and "..." and the like) cannot be created.
        return Err(Error::Invalid);
    }
    if trimmed.ends_with(' ') {
        return Err(Error::Invalid);
    }
    for c in trimmed.chars() {
        if (c as u32) < 0x20 || matches!(c, '"' | '*' | ':' | '<' | '>' | '?' | '\\' | '|' | '/') {
            return Err(Error::Invalid);
        }
    }
    let units: Vec<u16> = trimmed.encode_utf16().collect();
    if units.len() > MAX_NAME_UNITS {
        return Err(Error::NameTooLong);
    }
    Ok((String::from(trimmed), units))
}

/// Decodes a short name (8 + 3 bytes, space padded) for display. `lcase` carries the NT case bits:
/// 0x08 base lower case, 0x10 extension lower case.
pub fn decode_short(raw: &[u8; 11], lcase: u8) -> String {
    let mut out = String::new();
    let conv = |b: u8, lower: bool| -> char {
        let c = if b < 0x80 {
            b as char
        } else {
            CP437[(b - 0x80) as usize]
        };
        if lower { single_lower(c) } else { c }
    };
    let base_end = raw[..8]
        .iter()
        .rposition(|&b| b != b' ')
        .map_or(0, |p| p + 1);
    for (i, &b) in raw[..base_end].iter().enumerate() {
        // 0x05 stands for a real 0xE5 in the first byte.
        let b = if i == 0 && b == 0x05 { 0xE5 } else { b };
        out.push(conv(b, lcase & 0x08 != 0));
    }
    let ext_end = raw[8..]
        .iter()
        .rposition(|&b| b != b' ')
        .map_or(0, |p| p + 1);
    if ext_end > 0 {
        out.push('.');
        for &b in &raw[8..8 + ext_end] {
            out.push(conv(b, lcase & 0x10 != 0));
        }
    }
    out
}

fn is_legal_short_ascii(b: u8) -> bool {
    matches!(b, b'A'..=b'Z' | b'0'..=b'9')
        || matches!(
            b,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'('
                | b')'
                | b'-'
                | b'@'
                | b'^'
                | b'_'
                | b'`'
                | b'{'
                | b'}'
                | b'~'
        )
}

fn to_oem(c: char) -> Option<u8> {
    if c.is_ascii() {
        let b = c as u8;
        return is_legal_short_ascii(b).then_some(b);
    }
    CP437.iter().position(|&x| x == c).map(|p| p as u8 + 0x80)
}

/// Converts one part (base or extension) of a long name to short-name bytes.
fn convert_part(src: &[char], max: usize, lossy: &mut bool, has_lower: &mut bool) -> Vec<u8> {
    let mut out = Vec::new();
    for &c in src {
        if c == ' ' || c == '.' {
            *lossy = true;
            continue;
        }
        let up = single_upper(c);
        if up != c {
            *has_lower = true;
        }
        match to_oem(up) {
            Some(b) => out.push(b),
            None => {
                out.push(b'_');
                *lossy = true;
            }
        }
    }
    if out.len() > max {
        out.truncate(max);
        *lossy = true;
    }
    out
}

fn pad(base: &[u8], ext: &[u8]) -> [u8; 11] {
    let mut raw = [b' '; 11];
    raw[..base.len()].copy_from_slice(base);
    raw[8..8 + ext.len()].copy_from_slice(ext);
    // A first byte of 0xE5 would read as "deleted".
    if raw[0] == 0xE5 {
        raw[0] = 0x05;
    }
    raw
}

/// A planned short name.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ShortPlan {
    pub raw: [u8; 11],
    /// Whether the entry needs long-name slots to carry the real name.
    pub needs_lfn: bool,
}

/// Plans the short name for `name` (already checked by [`long_units`]). `taken` says whether a short name is in
/// use in the directory.
pub fn plan_short(name: &str, taken: &mut dyn FnMut(&[u8; 11]) -> bool) -> Result<ShortPlan> {
    let chars: Vec<char> = name.chars().collect();
    let mut lossy = false;
    let lead = chars.iter().take_while(|&&c| c == '.').count();
    if lead > 0 {
        lossy = true;
    }
    let body = &chars[lead..];
    let (base_src, ext_src): (&[char], &[char]) = match body.iter().rposition(|&c| c == '.') {
        Some(p) => (&body[..p], &body[p + 1..]),
        None => (body, &[]),
    };
    let mut base_lower = false;
    let mut ext_lower = false;
    let mut base = convert_part(base_src, 8, &mut lossy, &mut base_lower);
    let ext = convert_part(ext_src, 3, &mut lossy, &mut ext_lower);
    if base.is_empty() {
        base.push(b'_');
        lossy = true;
    }
    if !lossy {
        let raw = pad(&base, &ext);
        if !taken(&raw) {
            return Ok(ShortPlan {
                raw,
                needs_lfn: base_lower || ext_lower,
            });
        }
        if !base_lower && !ext_lower {
            // The very same short name is in use by another entry.
            return Err(Error::Exists);
        }
    }
    let mut digits = [0u8; 10];
    for n in 1u32..=999_999 {
        let mut len = 0;
        let mut v = n;
        while v > 0 {
            digits[len] = b'0' + (v % 10) as u8;
            v /= 10;
            len += 1;
        }
        let keep = (8 - 1 - len).min(base.len());
        let mut b = base[..keep].to_vec();
        b.push(b'~');
        for i in (0..len).rev() {
            b.push(digits[i]);
        }
        let raw = pad(&b, &ext);
        if !taken(&raw) {
            return Ok(ShortPlan {
                raw,
                needs_lfn: true,
            });
        }
    }
    Err(Error::NoSpace)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(name: &str, taken: &[&[u8; 11]]) -> ShortPlan {
        plan_short(name, &mut |r| taken.contains(&r)).unwrap()
    }

    #[test]
    fn checksum_of_known_names() {
        // The example of the specification: "FOO     BAR" -> 0xC9? computed with the specified algorithm.
        let mut sum = 0u8;
        for &b in b"FOO     BAR" {
            sum = ((sum & 1) << 7).wrapping_add(sum >> 1).wrapping_add(b);
        }
        assert_eq!(checksum(b"FOO     BAR"), sum);
    }

    #[test]
    fn short_name_rules() {
        assert_eq!(plan("README", &[]).raw, *b"README     ");
        assert!(!plan("README", &[]).needs_lfn);
        assert_eq!(plan("readme.txt", &[]).raw, *b"README  TXT");
        assert!(plan("readme.txt", &[]).needs_lfn);
        assert_eq!(plan("Long file name.text", &[]).raw, *b"LONGFI~1TEX");
        assert_eq!(plan("a+b.c", &[]).raw, *b"A_B~1   C  ");
        assert_eq!(plan("x", &[b"X          "]).raw, *b"X~1        ");
        assert_eq!(plan("ÜBER", &[]).raw[0], 0x9A);
        assert_eq!(plan(".bashrc", &[]).raw, *b"BASHRC~1   ");
    }

    #[test]
    fn same_short_name_is_refused() {
        assert_eq!(
            plan_short("README", &mut |r| r == b"README     ").err(),
            Some(Error::Exists)
        );
    }

    #[test]
    fn decode_uses_the_case_bits() {
        assert_eq!(decode_short(b"FOO     BAR", 0), "FOO.BAR");
        assert_eq!(decode_short(b"FOO     BAR", 0x08), "foo.BAR");
        assert_eq!(decode_short(b"FOO     BAR", 0x18), "foo.bar");
        assert_eq!(decode_short(b"NOEXT      ", 0x10), "NOEXT");
        assert_eq!(
            decode_short(
                &[
                    0x05, b'A', b' ', b' ', b' ', b' ', b' ', b' ', b' ', b' ', b' '
                ],
                0
            ),
            "σA"
        );
    }

    #[test]
    fn comparison_ignores_case() {
        assert!(names_equal("Hello", "hELLO"));
        assert!(names_equal("ÜBER", "über"));
        assert!(!names_equal("a", "b"));
        assert!(!names_equal("a", "ab"));
    }
}
