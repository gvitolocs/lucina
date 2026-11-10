//! The game index the Games screen searches: every game file on the card, in RAM, in name order, with as-you-type
//! search that narrows the previous results instead of scanning again.
//!
//! The search follows [`crate::names::hit`] exactly: the same fold (case, Latin accents, `_ - .` as spaces), every
//! word of the query must occur, the worst word's rank counts (0 the title starts with it, 1 a word starts with it,
//! 2 inside a word), and results sort as [`crate::names::order`] does: recently played first, then rank, then title.
//! The index folds every title once, so a keystroke compares bytes only (`rules::rank`, proved).
//!
//! Memory: one arena of names and titles, one of folded keys, 28 bytes an entry. 2000 games with 40-byte names take
//! about 230 KB. Games are found by the kernel's directory walk ([`Library::add_dir`], [`Library::add`]) or loaded
//! from the cache file ([`crate::cache`]); this crate does no I/O.

use alloc::vec::Vec;

use crate::names;
use crate::platform::{self, Platform, PlatformMask};
use crate::rules;

/// The longest file name or title taken (a FAT long name is at most 255 bytes).
pub const MAX_NAME: usize = 255;
/// The longest query in bytes as typed; longer input is cut here (on a character boundary).
pub const MAX_QUERY: usize = 32;
/// The most games the index holds: ids are `u16`.
pub const MAX_GAMES: usize = 0xfffe;
/// The most folders the index records.
pub const MAX_DIRS: usize = 0xfffe;

/// One game.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Index into the folder table.
    pub dir: u16,
    pub platform: Platform,
    /// File size in bytes (saturating at 4 GiB - 1).
    pub size: u32,
    /// The file's FAT date (`day | month << 5 | (year - 1980) << 9`), 0 when unknown; see [`crate::names::Day`].
    pub date: u16,
    /// When it was last started, in seconds on the DSi clock; 0 = never.
    pub played: u32,
    name: u32,
    name_len: u8,
    title: u32,
    title_len: u8,
    key: u32,
    key_len: u16,
}

/// Every game on the card. Ids (`u16`) are positions in title order and change when the index is sorted again;
/// keep [`Library::key`] (stable) across changes instead.
pub struct Library {
    /// Raw folder paths, file names and titles.
    text: Vec<u8>,
    /// Folded titles (the search keys).
    keys: Vec<u8>,
    dirs: Vec<(u32, u16)>,
    entries: Vec<Entry>,
    sorted: bool,
    generation: u32,
}

impl Default for Library {
    fn default() -> Self {
        Self::new()
    }
}

impl Library {
    pub const fn new() -> Self {
        Library { text: Vec::new(), keys: Vec::new(), dirs: Vec::new(), entries: Vec::new(), sorted: true, generation: 0 }
    }

    /// Number of games.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Number of folders recorded.
    pub fn dir_count(&self) -> usize {
        self.dirs.len()
    }

    /// Folder `d`'s path.
    pub fn dir_path(&self, d: u16) -> &[u8] {
        let (off, len) = self.dirs[d as usize];
        &self.text[off as usize..off as usize + len as usize]
    }

    /// Changes whenever games, titles, play times or their order change: a [`Search`] made for an older generation
    /// starts over.
    pub fn generation(&self) -> u32 {
        self.generation
    }

    fn bump(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }

    /// Records a folder path (`/roms/gba`, no trailing slash) and returns its number for [`Library::add`].
    pub fn add_dir(&mut self, path: &[u8]) -> Option<u16> {
        if self.dirs.len() >= MAX_DIRS || path.len() > u16::MAX as usize {
            return None;
        }
        let off = u32::try_from(self.text.len()).ok()?;
        self.text.extend_from_slice(path);
        self.dirs.push((off, path.len() as u16));
        Some((self.dirs.len() - 1) as u16)
    }

    /// Adds a file if it is a game (by extension); returns its id until the next sort, or `None` when it is not a
    /// game or does not fit. The index is unsorted until [`Library::finish`].
    pub fn add(&mut self, dir: u16, name: &[u8], size: u64, date: u16) -> Option<u16> {
        if dir as usize >= self.dirs.len() || name.is_empty() || name.len() > MAX_NAME || self.entries.len() >= MAX_GAMES {
            return None;
        }
        let (platform, stem_len) = platform::from_name(name)?;
        let name_off = u32::try_from(self.text.len()).ok()?;
        self.text.extend_from_slice(name);
        let mut e = Entry {
            dir,
            platform,
            size: size.min(u32::MAX as u64) as u32,
            date,
            played: 0,
            name: name_off,
            name_len: name.len() as u8,
            title: name_off,
            title_len: stem_len as u8,
            key: 0,
            key_len: 0,
        };
        let (k, kl) = self.push_key(&name[..stem_len])?;
        e.key = k;
        e.key_len = kl;
        self.entries.push(e);
        self.sorted = false;
        self.bump();
        Some((self.entries.len() - 1) as u16)
    }

    /// Sorts the games by title (folded, as [`crate::names::cmp_names`]), then platform, then file. Call after
    /// adding or retitling.
    pub fn finish(&mut self) {
        if self.sorted {
            return;
        }
        let keys = &self.keys;
        self.entries.sort_by(|a, b| {
            let ka = &keys[a.key as usize..a.key as usize + a.key_len as usize];
            let kb = &keys[b.key as usize..b.key as usize + b.key_len as usize];
            ka.cmp(kb).then(a.platform.cmp(&b.platform)).then(a.name.cmp(&b.name))
        });
        self.sorted = true;
        self.bump();
    }

    /// Replaces the title of game `id` (UTF-8, cut to 255 bytes on a character; e.g. a banner's first line).
    /// Unsorts the index.
    pub fn set_title(&mut self, id: u16, title: &[u8]) -> bool {
        if id as usize >= self.entries.len() {
            return false;
        }
        let title = &title[..floor_char(title, MAX_NAME)];
        if title.is_empty() {
            return false;
        }
        let Ok(off) = u32::try_from(self.text.len()) else { return false };
        let Some((k, kl)) = self.push_key(title) else { return false };
        self.text.extend_from_slice(title);
        let e = &mut self.entries[id as usize];
        e.title = off;
        e.title_len = title.len() as u8;
        e.key = k;
        e.key_len = kl;
        self.sorted = false;
        self.bump();
        true
    }

    /// Records that game `id` was started at `t` (seconds on the DSi clock).
    pub fn set_played(&mut self, id: u16, t: u32) -> bool {
        match self.entries.get_mut(id as usize) {
            Some(e) => {
                e.played = t;
                self.generation = self.generation.wrapping_add(1);
                true
            }
            None => false,
        }
    }

    pub fn entry(&self, id: u16) -> Option<&Entry> {
        self.entries.get(id as usize)
    }

    /// The file name of game `id` (`Golden Sun.gba`).
    pub fn name(&self, id: u16) -> &[u8] {
        let e = &self.entries[id as usize];
        &self.text[e.name as usize..e.name as usize + e.name_len as usize]
    }

    /// The folder of game `id` (`/roms/gba`).
    pub fn dir(&self, id: u16) -> &[u8] {
        self.dir_path(self.entries[id as usize].dir)
    }

    /// The title the user sees (as given, not folded): the banner's first line, or the file name's stem.
    pub fn title(&self, id: u16) -> &[u8] {
        let e = &self.entries[id as usize];
        &self.text[e.title as usize..e.title as usize + e.title_len as usize]
    }

    /// Whether the title was set ([`Library::set_title`]) rather than taken from the file name.
    pub fn has_own_title(&self, id: u16) -> bool {
        let e = &self.entries[id as usize];
        e.title != e.name
    }

    /// The search key: the title folded.
    pub fn search_key(&self, id: u16) -> &[u8] {
        let e = &self.entries[id as usize];
        &self.keys[e.key as usize..e.key as usize + e.key_len as usize]
    }

    /// The full path of game `id` into `out` (`/roms/gba/Golden Sun.gba`); returns its length, or `None` if `out`
    /// is too small.
    pub fn path(&self, id: u16, out: &mut [u8]) -> Option<usize> {
        let d = self.dir(id);
        let n = self.name(id);
        let len = d.len() + 1 + n.len();
        if len > out.len() {
            return None;
        }
        out[..d.len()].copy_from_slice(d);
        out[d.len()] = b'/';
        out[d.len() + 1..len].copy_from_slice(n);
        Some(len)
    }

    /// A key that stays the same while the file keeps its name and folder, whatever else is added or sorted:
    /// FNV-1a over the folder, the name and the platform.
    pub fn key(&self, id: u16) -> u32 {
        let mut h = Fnv::new();
        h.bytes(self.dir(id));
        h.bytes(b"/");
        h.bytes(self.name(id));
        h.bytes(&[self.entries[id as usize].platform as u8]);
        h.finish()
    }

    /// The id of the game with this [`Library::key`], if it is still on the card.
    pub fn find_key(&self, key: u32) -> Option<u16> {
        (0..self.entries.len() as u16).find(|&id| self.key(id) == key)
    }

    /// The `n` games played most recently, newest first: the Games screen's "Recently played" row.
    pub fn recent(&self, n: usize, out: &mut Vec<u16>) {
        out.clear();
        out.extend((0..self.entries.len() as u16).filter(|&id| self.entries[id as usize].played != 0));
        out.sort_by(|&a, &b| self.entries[b as usize].played.cmp(&self.entries[a as usize].played).then(a.cmp(&b)));
        out.truncate(n);
    }

    fn push_key(&mut self, s: &[u8]) -> Option<(u32, u16)> {
        let off = u32::try_from(self.keys.len()).ok()?;
        fold_into(s, &mut self.keys);
        let len = self.keys.len() - off as usize;
        Some((off, len as u16))
    }
}

/// Appends `s` folded as [`crate::names::fold`] folds each character, as UTF-8. Bytes that are not UTF-8 are folded
/// as ASCII and kept.
pub fn fold_into(s: &[u8], out: &mut Vec<u8>) {
    let mut rest = s;
    while !rest.is_empty() {
        match core::str::from_utf8(rest) {
            Ok(t) => {
                push_folded(t, out);
                return;
            }
            Err(e) => {
                let (good, bad) = rest.split_at(e.valid_up_to());
                push_folded(unsafe_free_str(good), out);
                let skip = e.error_len().unwrap_or(bad.len()).max(1).min(bad.len());
                out.extend(bad[..skip].iter().map(|&b| rules::fold(b)));
                rest = &bad[skip..];
            }
        }
    }
}

fn unsafe_free_str(b: &[u8]) -> &str {
    // `valid_up_to` guarantees this prefix is UTF-8; the checked call cannot fail.
    core::str::from_utf8(b).unwrap_or("")
}

fn push_folded(t: &str, out: &mut Vec<u8>) {
    for c in t.chars() {
        let f = names::fold(c);
        let mut buf = [0u8; 4];
        out.extend_from_slice(f.encode_utf8(&mut buf).as_bytes());
    }
}

/// The largest length `<= max` that does not cut a UTF-8 character of `s`.
fn floor_char(s: &[u8], max: usize) -> usize {
    if s.len() <= max {
        return s.len();
    }
    let mut n = max;
    while n > 0 && (s[n] & 0xc0) == 0x80 {
        n -= 1;
    }
    n
}

/// The character that ends just before byte `i` of `key`, if `key` is UTF-8 there.
fn prev_char(key: &[u8], i: usize) -> Option<char> {
    let mut j = i.checked_sub(1)?;
    while j > 0 && i - j < 4 && (key[j] & 0xc0) == 0x80 {
        j -= 1;
    }
    core::str::from_utf8(&key[j..i]).ok()?.chars().next()
}

/// The rank of one folded word in a folded key, as [`crate::names::hit`] ranks it: 0 the key starts with it, 1 it
/// starts a later word (the character before is not a letter or digit), 2 inside a word, `None` absent.
pub fn word_rank(key: &[u8], w: &[u8]) -> Option<u8> {
    match rules::rank(key, w) {
        rules::RANK_START => return Some(0),
        rules::RANK_NONE => return None,
        _ => {}
    }
    let mut from = 1;
    while let Some(i) = rules::find(key, w, from) {
        if prev_char(key, i).map_or(false, |p| !p.is_alphanumeric()) {
            return Some(1);
        }
        from = i + 1;
    }
    Some(2)
}

/// Folds a query as typed: split at whitespace (before folding, as [`crate::names::hit`] splits), each word folded,
/// words joined by a NUL byte (no name holds one). A typed `-` stays a word of its own that matches separators.
pub fn fold_query(raw: &[u8], out: &mut Vec<u8>) {
    out.clear();
    for w in raw.split(|b| b.is_ascii_whitespace()).filter(|w| !w.is_empty()) {
        if !out.is_empty() {
            out.push(0);
        }
        fold_into(w, out);
    }
}

/// The rank of a folded query ([`fold_query`]) in a folded key: every word must occur, the worst word decides; an
/// empty query matches with rank 0.
pub fn query_rank(key: &[u8], q: &[u8]) -> Option<u8> {
    let mut worst = 0;
    for w in q.split(|&b| b == 0).filter(|w| !w.is_empty()) {
        worst = worst.max(word_rank(key, w)?);
    }
    Some(worst)
}

/// As-you-type search over a [`Library`]. Keeps the previous folded query and its matches: when the new folded
/// query only adds bytes at the end (same filter, same library generation), it filters the previous matches. That
/// loses none: a longer last word matches only where its prefix does (`rules::lemma_narrow`), and a new word only
/// removes games.
pub struct Search {
    query: [u8; MAX_QUERY],
    qlen: usize,
    filter: PlatformMask,
    generation: u32,
    valid: bool,
    /// Ids that match the current query, ascending (= title order), with their rank.
    matched: Vec<(u16, u8)>,
    results: Vec<u16>,
    /// How many games the last [`Search::set`] had to check (for the sim's budget checks).
    pub last_checked: usize,
}

impl Default for Search {
    fn default() -> Self {
        Self::new()
    }
}

impl Search {
    pub const fn new() -> Self {
        Search {
            query: [0; MAX_QUERY],
            qlen: 0,
            filter: PlatformMask::ALL,
            generation: 0,
            valid: false,
            matched: Vec::new(),
            results: Vec::new(),
            last_checked: 0,
        }
    }

    /// The folded query in use.
    pub fn query(&self) -> &[u8] {
        &self.query[..self.qlen]
    }

    /// Searches `lib` for `q` (UTF-8 as typed, cut at [`MAX_QUERY`] bytes) among the platforms in `filter`, and
    /// returns the results in [`crate::names::order`]: played games first (newest first), then rank, then title.
    /// An empty query lists every game in the filter. The library must be [`Library::finish`]ed.
    pub fn set(&mut self, lib: &Library, q: &[u8], filter: PlatformMask) -> &[u16] {
        let raw = &q[..floor_char(q, MAX_QUERY)];
        let mut folded = Vec::with_capacity(raw.len());
        fold_query(raw, &mut folded);
        let mut nq = [0u8; MAX_QUERY];
        let n = folded.len().min(MAX_QUERY); // folding and joining never make text longer
        nq[..n].copy_from_slice(&folded[..n]);
        let new_q = &nq[..n];

        let narrowing = self.valid
            && self.generation == lib.generation()
            && self.filter == filter
            && n >= self.qlen
            && nq[..self.qlen] == self.query[..self.qlen];
        let mut kept: Vec<(u16, u8)> = Vec::with_capacity(if narrowing { self.matched.len() } else { lib.len() });
        let mut checked = 0usize;
        let mut consider = |id: u16| {
            checked += 1;
            if !filter.has(lib.entries[id as usize].platform) {
                return None;
            }
            query_rank(lib.search_key(id), new_q).map(|r| (id, r))
        };
        if narrowing {
            for &(id, _) in &self.matched {
                if let Some(m) = consider(id) {
                    kept.push(m);
                }
            }
        } else {
            for id in 0..lib.len() as u16 {
                if let Some(m) = consider(id) {
                    kept.push(m);
                }
            }
        }
        self.last_checked = checked;
        self.matched = kept;
        self.query = nq;
        self.qlen = n;
        self.filter = filter;
        self.generation = lib.generation();
        self.valid = true;

        self.results.clear();
        let mut order: Vec<(u16, u8)> = self.matched.clone();
        order.sort_by(|&(a, ra), &(b, rb)| {
            let pa = lib.entries[a as usize].played;
            let pb = lib.entries[b as usize].played;
            (pa == 0).cmp(&(pb == 0)).then(pb.cmp(&pa)).then(ra.cmp(&rb)).then(a.cmp(&b))
        });
        self.results.extend(order.iter().map(|&(id, _)| id));
        &self.results
    }

    /// The results of the last [`Search::set`].
    pub fn results(&self) -> &[u16] {
        &self.results
    }

    /// Forgets the previous query, so the next [`Search::set`] scans the whole library.
    pub fn reset(&mut self) {
        self.valid = false;
    }
}

struct Fnv(u32);

impl Fnv {
    const fn new() -> Self {
        Fnv(0x811c_9dc5)
    }
    fn bytes(&mut self, b: &[u8]) {
        for &x in b {
            self.0 = (self.0 ^ x as u32).wrapping_mul(0x0100_0193);
        }
    }
    fn finish(&self) -> u32 {
        self.0
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::string::String;
    use std::vec;

    pub(crate) fn lib() -> Library {
        let mut l = Library::new();
        let nds = l.add_dir(b"/roms/nds").unwrap();
        let gba = l.add_dir(b"/roms/gba").unwrap();
        let gb = l.add_dir(b"/roms/gb").unwrap();
        assert!(l.add(nds, b"Mario Kart DS.nds", 64 << 20, 0x5968).is_some());
        assert!(l.add(nds, b"New Super Mario Bros.nds", 32 << 20, 0).is_some());
        assert!(l.add(gba, b"Golden Sun.gba", 8 << 20, 0).is_some());
        assert!(l.add(gba, b"Mario & Luigi - Superstar Saga.GBA", 16 << 20, 0).is_some());
        assert!(l.add(gba, b"example.gba", 16 << 20, 0).is_some());
        assert!(l.add(gb, b"Tetris.gb", 32 << 10, 0).is_some());
        assert!(l.add(gb, "Example Game.gbc".as_bytes(), 2 << 20, 0).is_some());
        assert!(l.add(gb, b"Zelda - Link's Awakening DX.gbc", 1 << 20, 0).is_some());
        assert!(l.add(gb, b"Tetris.sav", 8 << 10, 0).is_none());
        assert!(l.add(9, b"x.gb", 1, 0).is_none());
        l.finish();
        l
    }

    pub(crate) fn titles(l: &Library, ids: &[u16]) -> std::vec::Vec<String> {
        ids.iter().map(|&i| String::from_utf8(l.title(i).to_vec()).unwrap()).collect()
    }

    fn id_of(l: &Library, title: &str) -> u16 {
        (0..l.len() as u16).find(|&i| l.title(i) == title.as_bytes()).unwrap()
    }

    #[test]
    fn sorted_by_folded_title_and_paths() {
        let l = lib();
        let all: std::vec::Vec<u16> = (0..l.len() as u16).collect();
        assert_eq!(
            titles(&l, &all),
            vec![
                "Golden Sun",
                "Mario & Luigi - Superstar Saga",
                "Mario Kart DS",
                "New Super Mario Bros",
                "example",
                "Example Game",
                "Tetris",
                "Zelda - Link's Awakening DX"
            ]
        );
        let mut buf = [0u8; 64];
        let n = l.path(0, &mut buf).unwrap();
        assert_eq!(&buf[..n], b"/roms/gba/Golden Sun.gba");
        assert_eq!(l.path(0, &mut [0u8; 5]), None);
        assert_eq!(l.search_key(id_of(&l, "Example Game")), b"pokemon crystal");
        assert_eq!(l.search_key(id_of(&l, "Zelda - Link's Awakening DX")), b"zelda   link's awakening dx");
    }

    #[test]
    fn same_rules_as_names_hit() {
        // For every query and every title, the index's rank is names::hit's rank.
        let l = lib();
        let queries = ["", "m", "mar", "io", "poke", "pokemon", "POKÉ", "pokemon c", "adv", "link", "zelda dx", "x",
            "ar", "sun gold", "-", "saga mario", "é", "tet", "qqq", "  mario   kart ", "'s", "a-", "s aw", "&"];
        for q in queries {
            for id in 0..l.len() as u16 {
                let title = core::str::from_utf8(l.title(id)).unwrap();
                let mut fq = std::vec::Vec::new();
                fold_query(q.as_bytes(), &mut fq);
                let ours = query_rank(l.search_key(id), &fq);
                let theirs = names::hit(q, title).map(|h| h.rank);
                assert_eq!(ours, theirs, "query {q:?} title {title:?}");
            }
        }
    }

    #[test]
    fn results_in_names_order() {
        let mut l = lib();
        let crystal = id_of(&l, "Example Game");
        let fire = id_of(&l, "example");
        l.set_played(crystal, 1000);
        let mut s = Search::new();
        let r = s.set(&l, b"poke", PlatformMask::ALL).to_vec();
        assert_eq!(r, vec![crystal, fire], "played first");
        let r = s.set(&l, b"mar", PlatformMask::ALL).to_vec();
        assert_eq!(titles(&l, &r), vec!["Mario & Luigi - Superstar Saga", "Mario Kart DS", "New Super Mario Bros"]);
        let r = s.set(&l, b"", PlatformMask::GBA).to_vec();
        assert_eq!(titles(&l, &r), vec!["Golden Sun", "Mario & Luigi - Superstar Saga", "example"]);
        assert!(s.set(&l, b"qqq", PlatformMask::ALL).is_empty());
        // The same order names::order gives.
        let r = s.set(&l, b"o", PlatformMask::ALL).to_vec();
        let mut keys: std::vec::Vec<(u16, names::SortKey)> = (0..l.len() as u16)
            .filter_map(|id| {
                let t = core::str::from_utf8(l.title(id)).unwrap();
                names::hit("o", t).map(|h| (id, names::SortKey { played: l.entry(id).unwrap().played, rank: h.rank, name: t }))
            })
            .collect();
        keys.sort_by(|a, b| names::order(&a.1, &b.1));
        assert_eq!(r, keys.iter().map(|k| k.0).collect::<std::vec::Vec<_>>());
    }

    #[test]
    fn typing_narrows_without_rescanning() {
        let l = lib();
        let mut s = Search::new();
        s.set(&l, b"m", PlatformMask::ALL);
        assert_eq!(s.last_checked, l.len());
        let before = s.results().len();
        s.set(&l, b"ma", PlatformMask::ALL);
        assert_eq!(s.last_checked, before, "only the previous matches are checked");
        s.set(&l, b"mar", PlatformMask::ALL);
        s.set(&l, b"ma", PlatformMask::ALL); // backspace: a full scan again
        assert_eq!(s.last_checked, l.len());
        s.set(&l, b"ma", PlatformMask::DS); // other filter: full scan
        assert_eq!(s.last_checked, l.len());
    }

    #[test]
    fn narrowing_matches_full_scan_on_every_prefix() {
        // The incremental path must give exactly what a fresh search gives, for every prefix of every title (as typed,
        // with accents and capitals), and a second word too.
        let l = lib();
        for id in 0..l.len() as u16 {
            let title = core::str::from_utf8(l.title(id)).unwrap().to_owned() + " " + "x";
            let mut inc = Search::new();
            for (end, _) in title.char_indices().chain([(title.len(), ' ')]) {
                let q = &title.as_bytes()[..end.min(MAX_QUERY)];
                let a = inc.set(&l, q, PlatformMask::ALL).to_vec();
                let b = Search::new().set(&l, q, PlatformMask::ALL).to_vec();
                assert_eq!(a, b, "prefix {:?}", String::from_utf8_lossy(q));
            }
        }
    }

    use std::borrow::ToOwned;

    #[test]
    fn retitle_restarts_search_and_resorts() {
        let mut l = lib();
        let mut s = Search::new();
        let tetris = id_of(&l, "Tetris");
        s.set(&l, b"te", PlatformMask::ALL);
        assert_eq!(s.results(), &[tetris]);
        let key = l.key(tetris);
        assert!(l.set_title(tetris, "Ætris Classic".as_bytes()));
        assert!(l.has_own_title(tetris));
        l.finish();
        let t = l.find_key(key).unwrap();
        assert_eq!(l.title(t), "Ætris Classic".as_bytes());
        assert!(s.set(&l, b"tet", PlatformMask::ALL).is_empty(), "the old title is gone");
        assert_eq!(s.last_checked, l.len(), "a new generation scans again");
        let r = s.set(&l, b"classic", PlatformMask::ALL).to_vec();
        assert_eq!(titles(&l, &r), vec!["Ætris Classic"]);
        assert!(!l.set_title(t, b""));
        assert!(!l.set_title(999, b"x"));
    }

    #[test]
    fn cuts_on_characters() {
        let mut l = lib();
        let long = "é".repeat(200); // 400 bytes
        assert!(l.set_title(0, long.as_bytes()));
        l.finish();
        assert!((0..l.len() as u16).any(|i| l.title(i).len() == 254 && core::str::from_utf8(l.title(i)).is_ok()));
        let mut s = Search::new();
        s.set(&l, "ø".repeat(20).as_bytes(), PlatformMask::ALL); // 40 bytes typed
        assert!(s.query().len() <= MAX_QUERY);
        assert!(s.query().iter().all(|&b| b == b'o'));
    }

    #[test]
    fn not_utf8_is_folded_as_ascii() {
        let mut out = std::vec::Vec::new();
        fold_into(b"AB\xffC\xe9D", &mut out);
        assert_eq!(out, b"ab\xffc\xe9d");
    }

    #[test]
    fn recently_played() {
        let mut l = lib();
        let mut out = std::vec::Vec::new();
        l.recent(5, &mut out);
        assert!(out.is_empty());
        let (a, b, c) = (id_of(&l, "Tetris"), id_of(&l, "Golden Sun"), id_of(&l, "Mario Kart DS"));
        l.set_played(a, 10);
        l.set_played(b, 30);
        l.set_played(c, 20);
        l.recent(2, &mut out);
        assert_eq!(out, vec![b, c]);
        assert!(!l.set_played(999, 1));
        assert_eq!(l.entry(c).unwrap().date, 0x5968);
    }

    #[test]
    fn keys_are_stable_across_sorting() {
        let mut l = lib();
        let before: std::vec::Vec<u32> = (0..l.len() as u16).map(|i| l.key(i)).collect();
        assert!(l.add(0, b"Animal Crossing.nds", 1, 0).is_some());
        l.finish();
        for k in before {
            assert!(l.find_key(k).is_some());
        }
    }
}
