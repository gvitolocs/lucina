//! The device token file on the SD card (`sd:/_fortuna/secretary.tok`): the only place the DSi keeps its sign-in.
//!
//! The DSi has no secure storage, so the file is plain text with a CRC: anyone holding the SD card holds the sign-in
//! until the owner revokes "Fortuna (Nintendo DSi)" in Secretary's device list, which the next start notices
//! (`GET /v1/session` says not authenticated) and the file is removed. A damaged file is removed too: signed out,
//! never a crash. The token is never written anywhere else: not the log, not a screenshot, not the FTP listing's
//! names (the file is in `_fortuna/`, which the shell's library walk skips).

use alloc::string::String;
use alloc::vec::Vec;

use crate::rules::token_shape;

const MAGIC: &str = "fortuna-secretary 1";

/// Whether an FTP path is the sign-in's or the generator's file (`/_fortuna/secret*`, `/_fortuna/seed*`, any
/// case, `sd:` or not). Fortuna's FTP server has no login on the home network, so it serves neither: no RETR, SIZE,
/// STOR, rename or MD5 of them.
pub fn private_file(path: &str) -> bool {
    let p = path.strip_prefix("sd:").unwrap_or(path);
    let p = p.trim_start_matches('/');
    let Some((dir, name)) = p.split_once('/') else { return false };
    let name = name.trim_end_matches('/');
    dir.eq_ignore_ascii_case("_fortuna")
        && !name.contains('/')
        // `secret…` also covers FAT's 8.3 alias of the long name (SECRET~1.TOK), which FatFs opens too
        && (name.len() >= 6 && name.is_char_boundary(6) && name[..6].eq_ignore_ascii_case("secret")
            || name.len() >= 4 && name.is_char_boundary(4) && name[..4].eq_ignore_ascii_case("seed"))
}

/// Whether an FTP path is the `_fortuna` folder itself (it may be listed, not renamed or removed: renaming it would
/// move the private files out from under `private_file`).
pub fn private_folder(path: &str) -> bool {
    let p = path.strip_prefix("sd:").unwrap_or(path);
    p.trim_matches('/').eq_ignore_ascii_case("_fortuna")
}

pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

/// A device id as the server gives it: `dev_` and up to 64 URL-safe letters.
pub fn device_shape(d: &str) -> bool {
    d.strip_prefix("dev_")
        .is_some_and(|r| !r.is_empty() && r.len() <= 64 && r.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'))
}

/// What the file holds. No `Debug`: a token must never reach a log line by accident.
pub struct Saved {
    pub device_id: String,
    pub token: String,
}

pub fn encode(device_id: &str, token: &str) -> Option<Vec<u8>> {
    if !token_shape(token.as_bytes()) || !device_shape(device_id) {
        return None;
    }
    let mut v = Vec::new();
    v.extend_from_slice(MAGIC.as_bytes());
    v.extend_from_slice(b"\ndevice ");
    v.extend_from_slice(device_id.as_bytes());
    v.extend_from_slice(b"\ntoken ");
    v.extend_from_slice(token.as_bytes());
    v.push(b'\n');
    let crc = crc32(&v);
    v.extend_from_slice(b"crc ");
    for i in (0..8).rev() {
        v.push(b"0123456789abcdef"[((crc >> (i * 4)) & 15) as usize]);
    }
    v.push(b'\n');
    Some(v)
}

pub fn decode(bytes: &[u8]) -> Option<Saved> {
    if bytes.len() > 512 {
        return None;
    }
    let text = core::str::from_utf8(bytes).ok()?;
    let crc_at = text.rfind("crc ")?;
    let (body, crc_line) = text.split_at(crc_at);
    let crc_hex = crc_line.strip_prefix("crc ")?.strip_suffix('\n')?;
    if crc_hex.len() != 8 || u32::from_str_radix(crc_hex, 16).ok()? != crc32(body.as_bytes()) {
        return None;
    }
    let mut lines = body.lines();
    if lines.next()? != MAGIC {
        return None;
    }
    let device_id = lines.next()?.strip_prefix("device ")?;
    let token = lines.next()?.strip_prefix("token ")?;
    if lines.next().is_some() || !token_shape(token.as_bytes()) || !device_shape(device_id) {
        return None;
    }
    Some(Saved { device_id: device_id.into(), token: token.into() })
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: &str = "sec_abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNO-_";

    #[test]
    fn private_paths() {
        for p in ["/_fortuna/secretary.tok", "sd:/_FORTUNA/Secretary.TOK", "/_fortuna/secretary.tmp", "_fortuna/seed.bin", "/_fortuna/SEED.TMP", "/_fortuna/SECRET~1.TOK"] {
            assert!(private_file(p), "{p}");
        }
        for p in ["/_fortuna/games.idx", "/secretary.tok", "/x/_fortuna/secretary.tok", "/_fortuna", "/_fortuna/sub/secretary.tok"] {
            assert!(!private_file(p), "{p}");
        }
        assert!(private_folder("/_fortuna/") && private_folder("sd:/_Fortuna") && !private_folder("/_fortuna/games.idx"));
    }

    #[test]
    fn round_trip_and_damage() {
        assert_eq!(T.len(), 47);
        let f = encode("dev_123abc", T).unwrap();
        let s = decode(&f).unwrap();
        assert_eq!((s.device_id.as_str(), s.token.as_str()), ("dev_123abc", T));
        for i in 0..f.len() {
            let mut g = f.clone();
            g[i] ^= 0x01;
            assert!(decode(&g).is_none(), "flip at {i}");
            assert!(decode(&f[..i]).is_none(), "cut at {i}");
        }
        assert!(encode("dev_1", "sec_short").is_none());
        assert!(encode("device", T).is_none());
        assert!(encode("dev_a\nb", T).is_none());
    }
}
