//! The Games index as a small file on the card (`sd:/_fortuna/games.idx`), so the next boot has every game at once
//! and only re-walks the card in the background. The kernel writes it only when nothing streams from the SD
//! (fortuna/docs/PROGRAMMING.md); this module only turns a [`Library`] into bytes and back.
//!
//! Reading never panics and never trusts the file: it may be truncated, stale, half-written or damaged. [`read`]
//! checks the magic, version, CRC and every record before anything is used; any doubt is an [`Error`], and the
//! kernel then builds the index from the card instead.
//!
//! Format, all integers little-endian:
//! - header (12 bytes): magic `FIDX`, version `u16` = 1, folders `u16`, games `u32`;
//! - folders: path length `u16`, path bytes (`/roms/gba`, may be empty for the card's root);
//! - games: folder `u16`, platform `u8` ([`Platform`] as `u8`), size `u32`, FAT date `u16`, last played `u32`
//!   (0 = never), name length `u8` (1..=255), name bytes, title length `u8` (0 = the name's stem), title bytes;
//! - trailer: CRC-32 (zlib/PNG: reflected 0xEDB88320, init and final xor 0xFFFFFFFF) of every byte before it.

use crate::index::Library;
use crate::platform::{self, Platform};

pub const MAGIC: [u8; 4] = *b"FIDX";
pub const VERSION: u16 = 1;
pub const HEADER: usize = 12;
pub const TRAILER: usize = 4;
const GAME_FIXED: usize = 2 + 1 + 4 + 2 + 4 + 1 + 1;

/// Why a cache file was not taken (or could not be written).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// Shorter than a header and trailer.
    Short,
    BadMagic,
    BadVersion(u16),
    /// The CRC does not match: damaged or half-written.
    BadCrc,
    /// A record is out of bounds or not one [`Library`] could have written.
    BadRecord,
    /// The output buffer is too small ([`Library::saved_len`] says how big it must be).
    Full,
}

/// CRC-32 as zlib and PNG compute it, with a 16-entry table (64 bytes) for the DSi's small caches.
pub fn crc32(data: &[u8]) -> u32 {
    const T: [u32; 16] = {
        let mut t = [0u32; 16];
        let mut i = 0;
        while i < 16 {
            let mut c = i as u32;
            let mut k = 0;
            while k < 4 {
                c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
                k += 1;
            }
            t[i] = c;
            i += 1;
        }
        t
    };
    let mut c = 0xffff_ffffu32;
    for &b in data {
        c = T[((c ^ b as u32) & 15) as usize] ^ (c >> 4);
        c = T[((c ^ (b >> 4) as u32) & 15) as usize] ^ (c >> 4);
    }
    !c
}

impl Library {
    /// Bytes [`Library::save`] writes.
    pub fn saved_len(&self) -> usize {
        let dirs: usize = (0..self.dir_count() as u16).map(|d| 2 + self.dir_path(d).len()).sum();
        let games: usize = (0..self.len() as u16)
            .map(|id| GAME_FIXED + self.name(id).len() + if self.has_own_title(id) { self.title(id).len() } else { 0 })
            .sum();
        HEADER + dirs + games + TRAILER
    }

    /// Writes the index into `buf`; returns the bytes written ([`Library::saved_len`]).
    pub fn save(&self, buf: &mut [u8]) -> Result<usize, Error> {
        let len = self.saved_len();
        if buf.len() < len {
            return Err(Error::Full);
        }
        let out = &mut buf[..len];
        let mut w = 0usize;
        let mut put = |b: &[u8]| {
            out[w..w + b.len()].copy_from_slice(b);
            w += b.len();
        };
        put(&MAGIC);
        put(&VERSION.to_le_bytes());
        put(&(self.dir_count() as u16).to_le_bytes());
        put(&(self.len() as u32).to_le_bytes());
        for d in 0..self.dir_count() as u16 {
            let p = self.dir_path(d);
            put(&(p.len() as u16).to_le_bytes());
            put(p);
        }
        for id in 0..self.len() as u16 {
            let e = *self.entry(id).expect("id below len");
            put(&e.dir.to_le_bytes());
            put(&[e.platform as u8]);
            put(&e.size.to_le_bytes());
            put(&e.date.to_le_bytes());
            put(&e.played.to_le_bytes());
            let n = self.name(id);
            put(&[n.len() as u8]);
            put(n);
            if self.has_own_title(id) {
                let t = self.title(id);
                put(&[t.len() as u8]);
                put(t);
            } else {
                put(&[0]);
            }
        }
        let crc = crc32(&buf[..len - TRAILER]);
        buf[len - TRAILER..len].copy_from_slice(&crc.to_le_bytes());
        Ok(len)
    }

    /// Builds a finished index from a cache file, or tells why the file cannot be used.
    pub fn load(data: &[u8]) -> Result<Library, Error> {
        let r = read(data)?;
        let mut lib = Library::new();
        for p in r.dirs() {
            lib.add_dir(p).ok_or(Error::BadRecord)?;
        }
        for g in r.games() {
            let id = lib.add(g.dir, g.name, g.size as u64, g.date).ok_or(Error::BadRecord)?;
            if lib.entry(id).map(|e| e.platform) != Some(g.platform) {
                return Err(Error::BadRecord);
            }
            if !g.title.is_empty() && !lib.set_title(id, g.title) {
                return Err(Error::BadRecord);
            }
            lib.set_played(id, g.played);
        }
        lib.finish();
        Ok(lib)
    }
}

/// One game record of a cache file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Game<'a> {
    pub dir: u16,
    pub platform: Platform,
    pub size: u32,
    pub date: u16,
    pub played: u32,
    pub name: &'a [u8],
    /// Empty when the title is the name's stem.
    pub title: &'a [u8],
}

/// A cache file [`read`] has checked from end to end.
#[derive(Clone, Copy, Debug)]
pub struct Reader<'a> {
    body: &'a [u8],
    dirs: u16,
    games: u32,
    games_at: usize,
}

struct Cursor<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        let end = self.at.checked_add(n).ok_or(Error::BadRecord)?;
        let s = self.b.get(self.at..end).ok_or(Error::BadRecord)?;
        self.at = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, Error> {
        let s = self.take(2)?;
        Ok(u16::from_le_bytes([s[0], s[1]]))
    }
    fn u32(&mut self) -> Result<u32, Error> {
        let s = self.take(4)?;
        Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }
}

fn dir_record<'a>(c: &mut Cursor<'a>) -> Result<&'a [u8], Error> {
    let n = c.u16()? as usize;
    c.take(n)
}

fn game_record<'a>(c: &mut Cursor<'a>, dirs: u16) -> Result<Game<'a>, Error> {
    let dir = c.u16()?;
    let platform = Platform::from_u8(c.u8()?).ok_or(Error::BadRecord)?;
    let size = c.u32()?;
    let date = c.u16()?;
    let played = c.u32()?;
    let nl = c.u8()? as usize;
    let name = c.take(nl)?;
    let tl = c.u8()? as usize;
    let title = c.take(tl)?;
    if dir >= dirs || name.is_empty() || platform::from_name(name).map(|(p, _)| p) != Some(platform) {
        return Err(Error::BadRecord);
    }
    if !title.is_empty() && core::str::from_utf8(title).is_err() {
        return Err(Error::BadRecord);
    }
    Ok(Game { dir, platform, size, date, played, name, title })
}

/// Checks a cache file: magic, version, CRC, and that its folders and games exactly fill it, every record valid.
pub fn read(data: &[u8]) -> Result<Reader<'_>, Error> {
    if data.len() < HEADER + TRAILER {
        return Err(Error::Short);
    }
    if data[..4] != MAGIC {
        return Err(Error::BadMagic);
    }
    let version = u16::from_le_bytes([data[4], data[5]]);
    if version != VERSION {
        return Err(Error::BadVersion(version));
    }
    let end = data.len() - TRAILER;
    let stored = u32::from_le_bytes([data[end], data[end + 1], data[end + 2], data[end + 3]]);
    if crc32(&data[..end]) != stored {
        return Err(Error::BadCrc);
    }
    let dirs = u16::from_le_bytes([data[6], data[7]]);
    let games = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);
    if dirs as usize > crate::index::MAX_DIRS || games as usize > crate::index::MAX_GAMES {
        return Err(Error::BadRecord);
    }
    let body = &data[..end];
    let mut c = Cursor { b: body, at: HEADER };
    for _ in 0..dirs {
        dir_record(&mut c)?;
    }
    let games_at = c.at;
    for _ in 0..games {
        game_record(&mut c, dirs)?;
    }
    if c.at != body.len() {
        return Err(Error::BadRecord);
    }
    Ok(Reader { body, dirs, games, games_at })
}

impl<'a> Reader<'a> {
    pub fn dir_count(&self) -> u16 {
        self.dirs
    }

    pub fn game_count(&self) -> u32 {
        self.games
    }

    /// The folder paths in file order.
    pub fn dirs(&self) -> impl Iterator<Item = &'a [u8]> + 'a {
        let mut c = Cursor { b: self.body, at: HEADER };
        // `read` walked these records already: none can fail.
        (0..self.dirs).filter_map(move |_| dir_record(&mut c).ok())
    }

    /// The games in file order.
    pub fn games(&self) -> impl Iterator<Item = Game<'a>> + 'a {
        let mut c = Cursor { b: self.body, at: self.games_at };
        let dirs = self.dirs;
        (0..self.games).filter_map(move |_| game_record(&mut c, dirs).ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::tests::{lib, titles};
    use crate::PlatformMask;
    use std::vec;
    use std::vec::Vec;

    fn saved(l: &Library) -> Vec<u8> {
        let mut buf = vec![0u8; l.saved_len()];
        assert_eq!(l.save(&mut buf), Ok(buf.len()));
        buf
    }

    #[test]
    fn crc32_is_zlibs() {
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
        assert_eq!(crc32(b"The quick brown fox jumps over the lazy dog"), 0x414f_a339);
    }

    #[test]
    fn round_trip() {
        let mut l = lib();
        l.set_played(2, 12345);
        assert!(l.set_title(0, "Golden Sun: L'Âge Perdu".as_bytes()));
        l.finish();
        let data = saved(&l);
        let back = Library::load(&data).unwrap();
        assert_eq!(back.len(), l.len());
        let all: Vec<u16> = (0..l.len() as u16).collect();
        assert_eq!(titles(&back, &all), titles(&l, &all));
        for id in 0..l.len() as u16 {
            assert_eq!(back.entry(id).unwrap().played, l.entry(id).unwrap().played);
            assert_eq!(back.entry(id).unwrap().date, l.entry(id).unwrap().date);
            assert_eq!(back.key(id), l.key(id));
            assert_eq!(back.has_own_title(id), l.has_own_title(id));
        }
        // Searching the loaded index gives the same answers.
        let (mut a, mut b) = (crate::Search::new(), crate::Search::new());
        for q in ["", "poke", "mario k", "âge", "zelda"] {
            assert_eq!(a.set(&l, q.as_bytes(), PlatformMask::ALL), b.set(&back, q.as_bytes(), PlatformMask::ALL));
        }
        // An empty library, and the root folder as "".
        let mut e = Library::new();
        assert!(Library::load(&saved(&e)).unwrap().is_empty());
        let root = e.add_dir(b"").unwrap();
        e.add(root, b"x.nds", 1, 0).unwrap();
        e.finish();
        let back = Library::load(&saved(&e)).unwrap();
        let mut p = [0u8; 16];
        let n = back.path(0, &mut p).unwrap();
        assert_eq!(&p[..n], b"/x.nds");
    }

    #[test]
    fn many_games_and_long_names() {
        let mut l = Library::new();
        let d = l.add_dir(b"/Games").unwrap();
        let long = std::format!("{}.gba", "x".repeat(251));
        assert_eq!(long.len(), 255);
        assert!(l.add(d, long.as_bytes(), 1, 0).is_some());
        for i in 0..300 {
            assert!(l.add(d, std::format!("Game {i}.nds").as_bytes(), i, (i % 31) as u16).is_some());
        }
        l.finish();
        let data = saved(&l);
        let back = Library::load(&data).unwrap();
        assert_eq!(back.len(), 301);
        assert_eq!(read(&data).unwrap().games().count(), 301);
    }

    #[test]
    fn short_buffer_is_full() {
        let l = lib();
        let mut buf = vec![0u8; l.saved_len() - 1];
        assert_eq!(l.save(&mut buf), Err(Error::Full));
    }

    #[test]
    fn every_truncation_and_every_byte_flip_is_refused() {
        let data = saved(&lib());
        for n in 0..data.len() {
            assert!(Library::load(&data[..n]).is_err(), "truncated to {n}");
        }
        for i in 0..data.len() {
            for bit in 0..8 {
                let mut d = data.clone();
                d[i] ^= 1 << bit;
                assert!(Library::load(&d).is_err(), "byte {i} bit {bit}");
            }
        }
        let mut longer = data.clone();
        longer.push(0);
        assert!(Library::load(&longer).is_err());
    }

    /// Records with a valid CRC but wrong contents: every one is refused, none panics.
    #[test]
    fn bad_records_with_a_good_crc() {
        let data = saved(&lib());
        let reseal = |mut d: Vec<u8>| {
            let end = d.len() - TRAILER;
            let c = crc32(&d[..end]);
            d[end..].copy_from_slice(&c.to_le_bytes());
            d
        };
        let r = read(&data).unwrap();
        let first_game = r.games_at;
        let cases: Vec<(usize, u8)> = vec![
            (first_game, 0xff),     // folder number past the folders
            (first_game + 2, 9),    // unknown platform
            (first_game + 2, 0),    // platform that does not fit the extension (a .gba file named DS)
            (first_game + 13, 0),   // empty name
            (first_game + 13, 200), // name length running into the next record
            (6, 0xff),              // folder count
            (8, 0xff),              // game count
            (10, 0x01),             // game count above the limit
        ];
        for (at, v) in cases {
            let mut d = data.clone();
            d[at] = v;
            let d = reseal(d);
            assert_eq!(Library::load(&d).err(), Some(Error::BadRecord), "byte {at} = {v}");
        }
        let mut d = data.clone();
        d[4] = 2;
        assert_eq!(read(&reseal(d)).err(), Some(Error::BadVersion(2)));
        let mut d = data.clone();
        d[0] = b'X';
        assert_eq!(read(&d).err(), Some(Error::BadMagic));
    }

    /// Pseudo-random garbage with a valid CRC: never a panic.
    #[test]
    fn garbage_never_panics() {
        let mut x = 0x1234_5678u32;
        let mut rnd = || {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x
        };
        for round in 0..2000 {
            let len = HEADER + (rnd() % 200) as usize;
            let mut d: Vec<u8> = (0..len).map(|_| rnd() as u8).collect();
            d[..4].copy_from_slice(&MAGIC);
            d[4..6].copy_from_slice(&VERSION.to_le_bytes());
            if round % 2 == 0 {
                d[6..8].copy_from_slice(&((rnd() % 4) as u16).to_le_bytes());
                d[8..12].copy_from_slice(&(rnd() % 4).to_le_bytes());
            }
            let c = crc32(&d);
            d.extend_from_slice(&c.to_le_bytes());
            let _ = Library::load(&d);
        }
    }
}
