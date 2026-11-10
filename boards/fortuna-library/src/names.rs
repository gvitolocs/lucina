//! What a file on Fortuna's SD card is, which runner opens a game, and how the Games search matches and orders names
//! (written for fortuna-look by the shell redesign, 2026-11-08; re-exported at the crate root). `hit` is the
//! reference for highlighting one visible row; [`crate::index::Search`] filters the whole library by the same rules
//! (same fold, every word must match, the worst word's rank, the same order).

use core::cmp::Ordering;
use core::fmt;

/// The program that runs a game. Every one of them is booted through Fortuna's own launch path (nds-bootstrap's ini
/// and the Unlaunch autoload block, `QUIT_PATH` back to `sd:/FORTUNA.NDS`), never a bare reboot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Runner {
    /// `.nds` / `.dsi`: nds-bootstrap in DSi mode.
    Nds,
    /// `.gba`: GBARunner2 through nds-bootstrap-hb (the same runner the GBA tiles start).
    Gba,
    /// `.gb` / `.gbc`: GameYob through nds-bootstrap-hb.
    GameBoy,
}

impl Runner {
    /// What the launch dialog calls it.
    pub const fn name(self) -> &'static str {
        match self {
            Runner::Nds => "nds-bootstrap",
            Runner::Gba => "GBARunner2",
            Runner::GameBoy => "GameYob",
        }
    }

    /// The short system name shown next to a game.
    pub const fn system(self) -> &'static str {
        match self {
            Runner::Nds => "DS",
            Runner::Gba => "GBA",
            Runner::GameBoy => "Game Boy",
        }
    }

    /// The system as a list row shows it.
    pub const fn short(self) -> &'static str {
        match self {
            Runner::Nds => "DS",
            Runner::Gba => "GBA",
            Runner::GameBoy => "GB",
        }
    }

    /// The files the runner needs besides the game, checked before the jump: a missing one is an error dialog, not
    /// a runner that stops with no way back (GBARunner2 halts at "Bios not found!").
    pub const fn needs(self) -> &'static [&'static str] {
        match self {
            Runner::Nds => &["sd:/_nds/nds-bootstrap-release.nds"],
            Runner::Gba => &["sd:/_nds/GBARunner2_arm7dldi_dsi.nds", "sd:/_gba/bios.bin"],
            Runner::GameBoy => &["sd:/_nds/TWiLightMenu/emulators/gameyob.nds"],
        }
    }
}

/// What a directory entry is, for its icon and for what a tap does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Folder,
    Game(Runner),
    Picture,
    Text,
    Other,
}

/// The extension after the last dot, or "" (a leading dot alone, as in `.hidden`, is not an extension).
pub fn ext(name: &str) -> &str {
    match name.rfind('.') {
        Some(i) if i > 0 => &name[i + 1..],
        _ => "",
    }
}

/// The name without its extension, as the Games list shows it.
pub fn stem(name: &str) -> &str {
    match name.rfind('.') {
        Some(i) if i > 0 => &name[..i],
        _ => name,
    }
}

/// The kind of an entry from its name (case-insensitive extension) and whether it is a directory.
pub fn kind(name: &str, dir: bool) -> Kind {
    if dir {
        return Kind::Folder;
    }
    let e = ext(name);
    let is = |list: &[&str]| list.iter().any(|x| e.eq_ignore_ascii_case(x));
    if is(&["nds", "dsi"]) {
        Kind::Game(Runner::Nds)
    } else if is(&["gba"]) {
        Kind::Game(Runner::Gba)
    } else if is(&["gb", "gbc"]) {
        Kind::Game(Runner::GameBoy)
    } else if is(&["png", "jpg", "jpeg", "bmp", "gif"]) {
        Kind::Picture
    } else if is(&["txt", "md", "ini", "log", "cfg", "json", "xml", "csv"]) {
        Kind::Text
    } else {
        Kind::Other
    }
}

/// One character as the search compares it: lower case, Latin accents dropped, `_ - .` read as spaces.
pub fn fold(c: char) -> char {
    match c {
        'À'..='Å' | 'à'..='å' => 'a',
        'Ç' | 'ç' => 'c',
        'È'..='Ë' | 'è'..='ë' => 'e',
        'Ì'..='Ï' | 'ì'..='ï' => 'i',
        'Ñ' | 'ñ' => 'n',
        'Ò'..='Ö' | 'Ø' | 'ò'..='ö' | 'ø' => 'o',
        'Ù'..='Ü' | 'ù'..='ü' => 'u',
        'Ý' | 'ý' | 'ÿ' => 'y',
        '_' | '-' | '.' => ' ',
        c => c.to_ascii_lowercase(),
    }
}

/// Where a query matched a name, and how well.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hit {
    /// 0: the name starts with every word's match; 1: words match at word starts; 2: somewhere inside a word.
    /// The worst word decides.
    pub rank: u8,
    /// Byte range of the first word's match in the name, for highlighting (empty for an empty query).
    pub start: usize,
    pub end: usize,
}

/// End of `word` matched against `hay` at byte `start`, if it matches there.
fn match_at(hay: &str, start: usize, word: &str) -> Option<usize> {
    let mut h = hay[start..].char_indices();
    let mut end = start;
    for w in word.chars() {
        let (i, c) = h.next()?;
        if fold(c) != fold(w) {
            return None;
        }
        end = start + i + c.len_utf8();
    }
    Some(end)
}

/// The best place `word` matches in `hay`: the earliest of the best rank.
fn best(hay: &str, word: &str) -> Option<Hit> {
    let mut prev: Option<char> = None;
    let mut out: Option<Hit> = None;
    for (i, c) in hay.char_indices() {
        if let Some(end) = match_at(hay, i, word) {
            let rank = match prev {
                None => 0,
                Some(p) if !fold(p).is_alphanumeric() => 1,
                Some(_) => 2,
            };
            if out.map_or(true, |o| rank < o.rank) {
                out = Some(Hit { rank, start: i, end });
            }
            if rank == 0 {
                break;
            }
        }
        prev = Some(c);
    }
    out
}

/// Whether `name` matches `query`: every whitespace-separated word must occur in it (case and accents ignored).
/// An empty query matches everything with rank 0.
pub fn hit(query: &str, name: &str) -> Option<Hit> {
    let mut first: Option<Hit> = None;
    let mut worst = 0;
    for w in query.split_whitespace() {
        let h = best(name, w)?;
        worst = worst.max(h.rank);
        if first.is_none() {
            first = Some(h);
        }
    }
    Some(match first {
        None => Hit { rank: 0, start: 0, end: 0 },
        Some(h) => Hit { rank: worst, ..h },
    })
}

/// Two names compared the way the lists sort them: folded, then by length.
pub fn cmp_names(a: &str, b: &str) -> Ordering {
    a.chars().map(fold).cmp(b.chars().map(fold))
}

/// How a game sorts in the Games list.
#[derive(Clone, Copy, Debug)]
pub struct SortKey<'a> {
    /// When it was last played (seconds on the DSi clock); 0 = never.
    pub played: u32,
    /// The search rank (0 with no query).
    pub rank: u8,
    pub name: &'a str,
}

/// Recently played first (newest first), then by search rank, then by name.
pub fn order(a: &SortKey, b: &SortKey) -> Ordering {
    (a.played == 0)
        .cmp(&(b.played == 0))
        .then(b.played.cmp(&a.played))
        .then(a.rank.cmp(&b.rank))
        .then(cmp_names(a.name, b.name))
}

/// A file size the way the lists show it: `820 B`, `64 KB`, `4.1 MB`, `38 MB`, `1.9 GB`.
#[derive(Clone, Copy, Debug)]
pub struct Size(pub u64);

/// `n / unit` with one decimal below 10, rounded.
fn tenths(f: &mut fmt::Formatter, n: u64, unit: u64, name: &str) -> fmt::Result {
    let t = (n * 10 + unit / 2) / unit;
    if t < 100 {
        write!(f, "{}.{} {name}", t / 10, t % 10)
    } else {
        write!(f, "{} {name}", (t + 5) / 10)
    }
}

impl fmt::Display for Size {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        const K: u64 = 1024;
        let b = self.0;
        if b < K {
            write!(f, "{b} B")
        } else if (b + K / 2) / K < K {
            write!(f, "{} KB", (b + K / 2) / K)
        } else if (b * 10 + K * K / 2) / (K * K) < 10 * K {
            tenths(f, b, K * K, "MB")
        } else {
            tenths(f, b, K * K * K, "GB")
        }
    }
}

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// A FAT directory date (`day | month << 5 | (year - 1980) << 9`): `8 Nov`, or `8 Nov 2025` outside `this_year`.
#[derive(Clone, Copy, Debug)]
pub struct Day {
    pub fat: u16,
    pub this_year: u16,
}

impl Day {
    pub const fn fat(year: u16, month: u16, day: u16) -> u16 {
        (year - 1980) << 9 | month << 5 | day
    }
}

impl fmt::Display for Day {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let (d, m, y) = (self.fat & 31, (self.fat >> 5) & 15, 1980 + (self.fat >> 9));
        if d == 0 || m == 0 || m > 12 {
            return f.write_str("—");
        }
        write!(f, "{d} {}", MONTHS[m as usize - 1])?;
        if y != self.this_year {
            write!(f, " {y}")?;
        }
        Ok(())
    }
}

/// How long ago, short: `just now`, `12 min ago`, `3 h ago`, `2 days ago`.
#[derive(Clone, Copy, Debug)]
pub struct Ago(pub u32);

impl fmt::Display for Ago {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let s = self.0;
        match s {
            0..=59 => f.write_str("just now"),
            60..=3599 => write!(f, "{} min ago", s / 60),
            3600..=86399 => write!(f, "{} h ago", s / 3600),
            86400..=172799 => f.write_str("yesterday"),
            _ => write!(f, "{} days ago", s / 86400),
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::format;

    #[test]
    fn kinds_and_runners() {
        assert_eq!(kind("Mario Kart DS.nds", false), Kind::Game(Runner::Nds));
        assert_eq!(kind("hb.DSI", false), Kind::Game(Runner::Nds));
        assert_eq!(kind("example.gba", false), Kind::Game(Runner::Gba));
        assert_eq!(kind("fireda-engine.GBA", false), Kind::Game(Runner::Gba));
        assert_eq!(kind("Tetris.gb", false), Kind::Game(Runner::GameBoy));
        assert_eq!(kind("Crystal.GBC", false), Kind::Game(Runner::GameBoy));
        assert_eq!(kind("cover.PNG", false), Kind::Picture);
        assert_eq!(kind("FORTUNA.TXT", false), Kind::Text);
        assert_eq!(kind("INTRO.FMV", false), Kind::Other);
        assert_eq!(kind("game.nds", true), Kind::Folder);
        assert_eq!(kind(".nds", false), Kind::Other, "a dot file is not a game");
        assert_eq!(kind("nds", false), Kind::Other);
        assert_eq!(stem("Advance Wars.gba"), "Advance Wars");
        assert_eq!(stem(".hidden"), ".hidden");
        assert_eq!(stem("a.b.gb"), "a.b");
        assert!(Runner::Gba.needs().contains(&"sd:/_gba/bios.bin"));
    }

    #[test]
    fn search_ignores_case_accents_and_separators() {
        let h = hit("poke", "Example Game").unwrap();
        assert_eq!((h.rank, h.start, h.end), (0, 0, 5), "é is two bytes");
        let h = hit("pokemon", "Example Game").unwrap();
        assert_eq!(&"Example Game"[h.start..h.end], "Pokémon");
        assert!(hit("pokemon x", "Pokémon_X").is_some());
        assert!(hit("POKÉ", "example").is_some());
        assert!(hit("zelda", "Example Game").is_none());
    }

    #[test]
    fn rank_prefers_starts() {
        assert_eq!(hit("war", "Warioware").unwrap().rank, 0);
        assert_eq!(hit("war", "Advance Wars").unwrap().rank, 1);
        let h = hit("war", "Software").unwrap();
        assert_eq!((h.rank, h.start), (2, 4));
        // the earliest of the best rank wins, even after a worse one
        let h = hit("ar", "Bar Arena").unwrap();
        assert_eq!((h.rank, h.start), (1, 4));
        // every word must match; the worst word decides the rank
        assert_eq!(hit("adv wars", "Advance Wars").unwrap().rank, 1);
        assert_eq!(hit("adv ars", "Advance Wars").unwrap().rank, 2);
        assert!(hit("adv zelda", "Advance Wars").is_none());
    }

    #[test]
    fn empty_query_matches_all() {
        assert_eq!(hit("", "x"), Some(Hit { rank: 0, start: 0, end: 0 }));
        assert_eq!(hit("   ", "x"), Some(Hit { rank: 0, start: 0, end: 0 }));
        assert_eq!(hit("x", ""), None);
    }

    #[test]
    fn match_never_splits_a_character() {
        for name in ["Pokémon", "éé", "aé", "Ñandú"] {
            for q in ["e", "é", "n", "o", "a", "u"] {
                if let Some(h) = hit(q, name) {
                    assert!(name.is_char_boundary(h.start) && name.is_char_boundary(h.end), "{q} in {name}");
                }
            }
        }
    }

    #[test]
    fn order_puts_recent_first() {
        let k = |played, rank, name| SortKey { played, rank, name };
        let mut v = [k(0, 0, "Tetris"), k(500, 2, "Zelda"), k(0, 0, "advance"), k(900, 0, "Mario"), k(0, 1, "Bomber")];
        v.sort_by(order);
        let names: std::vec::Vec<_> = v.iter().map(|x| x.name).collect();
        assert_eq!(names, ["Mario", "Zelda", "advance", "Tetris", "Bomber"]);
        assert_eq!(cmp_names("Éclair", "eclair"), Ordering::Equal);
        assert_eq!(cmp_names("ab", "abc"), Ordering::Less);
    }

    #[test]
    fn sizes() {
        let s = |n| format!("{}", Size(n));
        assert_eq!(s(0), "0 B");
        assert_eq!(s(1023), "1023 B");
        assert_eq!(s(1024), "1 KB");
        assert_eq!(s(65536), "64 KB");
        assert_eq!(s(1024 * 1024 - 1), "1.0 MB");
        assert_eq!(s(4_300_000), "4.1 MB");
        assert_eq!(s(10 * 1024 * 1024 - 1), "10 MB");
        assert_eq!(s(38 * 1024 * 1024), "38 MB");
        assert_eq!(s(1023 * 1024 * 1024 + 900 * 1024 * 1024), "1.9 GB");
        assert_eq!(s(4 * 1024 * 1024 * 1024 - 1), "4.0 GB");
    }

    #[test]
    fn days_and_ago() {
        let d = |fat, this_year| format!("{}", Day { fat, this_year });
        assert_eq!(d(Day::fat(2026, 11, 8), 2026), "8 Nov");
        assert_eq!(d(Day::fat(2025, 1, 31), 2026), "31 Jan 2025");
        assert_eq!(d(0, 2026), "—");
        assert_eq!(d(Day::fat(2026, 13, 1), 2026), "—");
        let a = |s| format!("{}", Ago(s));
        assert_eq!(a(5), "just now");
        assert_eq!(a(720), "12 min ago");
        assert_eq!(a(3 * 3600 + 5), "3 h ago");
        assert_eq!(a(90_000), "yesterday");
        assert_eq!(a(3 * 86400), "3 days ago");
    }
}
