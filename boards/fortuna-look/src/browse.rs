//! Files and Games. Files: the card as rows (the same row component as System), the breadcrumb and the folder's facts
//! on the top screen. Games: recently played first, platform filters, large covers fitted to each system's box shape
//! (a generated card with the title and the platform's glyph where there is no cover), the selection kept on the
//! second line, the selected game's card on the top screen with the round Play. Search narrows as he types, with the
//! touch keyboard. What a file is and how search matches come from `fortuna-library`; this module only draws.

use core::fmt::Write;

use fortuna_library::{stem, Ago, Day, Hit, Kind, Runner, Size};

use crate::canvas::Canvas;
use crate::icons::{self as ic, Icon};
use crate::layout::{self as l, Key, Rect, SCREEN_H, SCREEN_W};
use crate::screens::{self, Bar, Buf, Row};
use crate::skin::{self as s, ACCENT, ACCENT_FILL, INK, INK_2, INK_3};
use crate::text::Font;
use crate::tokens::glass as g;

// ---- models -------------------------------------------------------------------------------------------------------

/// The system a game runs on: its name, glyph and box shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum System {
    Ds,
    Gba,
    Gb,
    /// Scenes built into Fortuna (the opening teaser).
    Fortuna,
}

impl System {
    pub const fn from_runner(r: Runner) -> System {
        match r {
            Runner::Nds => System::Ds,
            Runner::Gba => System::Gba,
            Runner::GameBoy => System::Gb,
        }
    }
    pub const fn name(self) -> &'static str {
        match self {
            System::Ds => "DS",
            System::Gba => "GBA",
            System::Gb => "Game Boy",
            System::Fortuna => "Fortuna",
        }
    }
    pub const fn short(self) -> &'static str {
        match self {
            System::Gb => "GB",
            s => s.name(),
        }
    }
    pub fn icon(self, size: u8) -> &'static Icon {
        match (self, size) {
            (System::Ds, 12) => &ic::DS_12,
            (System::Ds, 16) => &ic::DS_16,
            (System::Ds, _) => &ic::DS_24,
            (System::Gba, 12) => &ic::GBA_12,
            (System::Gba, 16) => &ic::GBA_16,
            (System::Gba, _) => &ic::GBA_24,
            (System::Gb, 12) => &ic::GB_12,
            (System::Gb, 16) => &ic::GB_16,
            (System::Gb, _) => &ic::GB_24,
            (System::Fortuna, 12) => &ic::STAR_12,
            (System::Fortuna, 16) => &ic::STAR_16,
            (System::Fortuna, _) => &ic::STAR_24,
        }
    }
    /// The cover's shape, width : height: DS covers as TWiLight Menu++ and GameTDB scan them (128 x 115, so 10:9), GBA
    /// boxes about square, Game Boy boxes upright (about 7:8).
    pub const fn aspect(self) -> (i32, i32) {
        match self {
            System::Ds | System::Fortuna => (10, 9),
            System::Gba => (1, 1),
            System::Gb => (7, 8),
        }
    }
    /// The Games filter this system falls under (1 = DS, 2 = GBA, 3 = Game Boy; Fortuna's scenes under DS).
    pub const fn filter(self) -> usize {
        match self {
            System::Ds | System::Fortuna => 1,
            System::Gba => 2,
            System::Gb => 3,
        }
    }
}

/// One game.
#[derive(Clone, Copy, Debug)]
pub struct Game<'a> {
    /// The file name with its extension, or a Fortuna scene's title.
    pub name: &'a str,
    /// The folder it is in ("sd:/Games"), or "" for a scene.
    pub folder: &'a str,
    pub system: System,
    pub size: u64,
    /// Seconds since it was last played, if ever.
    pub played_ago: Option<u32>,
    /// The game's own icon (a DS banner), when it has one.
    pub art: Option<Art<'a>>,
}

/// A DS banner icon: 32x32 pixels as 16 OBJ tiles at 4 bits a pixel (4x4 tiles, row by row), and its palette
/// (BGR555; entry 0 is transparent). `fortuna_library::banner::parse` gives both.
#[derive(Clone, Copy, Debug)]
pub struct Art<'a> {
    pub tiles: &'a [u8; 512],
    pub palette: &'a [u16; 16],
}

/// Draws a banner icon with its top-left at (x, y), each pixel `scale` x `scale`.
pub fn draw_art(cv: &mut Canvas, a: &Art, x: i32, y: i32, scale: i32) {
    cv.art(a.tiles, a.palette, x, y, scale);
}

impl<'a> Game<'a> {
    /// The name as the lists show it: without the extension.
    pub fn name_shown(&self) -> &'a str {
        // a file name loses its extension; a title (a scene's, a banner's) shows as it is
        match fortuna_library::kind(self.name, false) {
            Kind::Game(_) => stem(self.name),
            _ => self.name,
        }
    }
}

/// "Played 2 h ago", "Played yesterday", "Not played yet".
pub fn played(out: &mut impl Write, ago: Option<u32>) {
    let _ = match ago {
        Some(s) => write!(out, "Played {}", Ago(s)),
        None => write!(out, "Not played yet"),
    };
}

/// One entry of a folder.
#[derive(Clone, Copy, Debug)]
pub struct Entry<'a> {
    pub name: &'a str,
    pub kind: Kind,
    /// Bytes for a file, entries for a folder.
    pub size: u64,
    /// FAT date of the last change.
    pub date: u16,
}

pub fn kind_icon(k: Kind) -> &'static Icon {
    match k {
        Kind::Folder => &ic::FOLDER_16,
        Kind::Game(r) => System::from_runner(r).icon(16),
        Kind::Picture => &ic::PICTURE_16,
        Kind::Text => &ic::DOC_16,
        Kind::Other => &ic::FILE_16,
    }
}

/// An entry's one line: "14 items", "GB · 2.0 MB", "72 KB · 24 Dec 2025".
pub fn entry_line(out: &mut impl Write, e: &Entry, this_year: u16) {
    let _ = match e.kind {
        // a folder's count is not read for every row on the DSi (0: unknown)
        Kind::Folder if e.size == 0 => Ok(()),
        Kind::Folder => write!(out, "{} items", e.size),
        // the row's glyph already says the system
        Kind::Game(_) => write!(out, "{}", Size(e.size)),
        _ if e.date == 0 => write!(out, "{}", Size(e.size)),
        _ => write!(out, "{} · {}", Size(e.size), Day { fat: e.date, this_year }),
    };
}

// ---- covers -------------------------------------------------------------------------------------------------------

/// A game's cover fitted into `bx` by its system's box shape, never cropped. With no cover art on the card it is a
/// generated card: the platform's glyph and the title.
pub fn cover(cv: &mut Canvas, bx: Rect, gm: &Game) -> Rect {
    let (w, h) = gm.system.aspect();
    let r = l::fit(bx, w, h);
    let big = r.h >= 60;
    s::cover_ground(cv, r.x, r.y, r.w, r.h, if big { 8 } else { 6 });
    match (gm.art, big) {
        // the game's own icon: twice its size on a big cover, as it is on a small one
        (Some(a), true) => draw_art(cv, &a, r.cx() - 32, r.cy() - 32, 2),
        (Some(a), false) => draw_art(cv, &a, r.cx() - 16, r.cy() - 16, 1),
        // a big cover sits next to its title, so it shows only the platform: each fact once
        (None, true) => cv.icon(gm.system.icon(24), r.cx() - 12, r.cy() - 12, INK_2),
        // no art: the generated card is the icon, its title drawn in (whole words, two lines at most)
        (None, false) => {
            cv.icon(gm.system.icon(12), r.x + 4, r.y + 4, INK_2);
            cv.text_wrap(s::FOOTNOTE, r.x + 4, r.y + 30, gm.name_shown(), r.w - 7, 2, 10, INK);
        }
    }
    r
}

// ---- Files --------------------------------------------------------------------------------------------------------

/// The Files list: an up row when inside a folder, then the entries from `first` of `total`; `focus` is a slot.
pub fn files(cv: &mut Canvas, parent: Option<&str>, entries: &[Entry], first: usize, total: usize, this_year: u16, focus: Option<i32>) {
    s::bottom_backdrop(cv);
    const N: usize = l::LIST_ROWS as usize;
    let up = parent.is_some() as usize;
    let shown = (N - up).min(entries.len());
    let mut vals: [Buf<32>; N] = core::array::from_fn(|_| Buf::new());
    for (i, e) in entries[..shown].iter().enumerate() {
        entry_line(&mut vals[up + i], e, this_year);
    }
    let blank = Row { icon: &ic::FILE_16, label: "", ok: None, value: "", opens: false };
    let mut rows = [blank; N];
    if let Some(p) = parent {
        rows[0] = Row { icon: &ic::BACK_16, label: p, ok: None, value: "", opens: false };
    }
    for (i, e) in entries[..shown].iter().enumerate() {
        rows[up + i] = Row { icon: kind_icon(e.kind), label: e.name, ok: None, value: vals[up + i].s(), opens: e.kind == Kind::Folder };
    }
    let n = up + shown;
    if n == 0 {
        cv.text_centre(s::BODY, SCREEN_W / 2, 60, "This folder is empty", INK_2);
    }
    screens::rows(cv, &rows[..n], 0, focus);
    // the scroll mark in the right margin
    if total > shown && n > 0 {
        let card = l::row(0);
        let (x, top, h) = (card.right() + 2, card.y + 4, n as i32 * l::ROW_H - 8);
        cv.pill(x, top, 2, h, s::HAIR);
        let len = (h * shown as i32 / total as i32).max(8);
        let pos = (h - len) * first as i32 / (total - shown).max(1) as i32;
        cv.pill(x, top + pos, 2, len, INK_3);
    }
    screens::tab_bar(cv, l::TAB_FILES, focus == Some(-1));
}

/// Files' top screen: where you are (the breadcrumb and the folder's name in the display face, its facts in one line),
/// and the focused entry.
#[allow(clippy::too_many_arguments)]
pub fn folder_top(cv: &mut Canvas, bar: &Bar, crumbs: &[&str], name: &str, facts: &str, sel: Option<&Entry>, this_year: u16) {
    s::top_backdrop(cv);
    screens::strip(cv, bar);
    let gx = l::EDGE;
    let head = Rect::new(gx, 18, SCREEN_W - 2 * gx, 76);
    s::material(cv, head.x, head.y, head.w, head.h, l::PANEL_R);
    let mut crumb = Buf::<96>::new();
    for c in crumbs {
        let _ = write!(crumb, "{c} › ");
    }
    if !crumbs.is_empty() {
        cv.text_fit(s::FOOTNOTE, head.x + 12, head.y + 16, crumb.s(), head.w - 24, INK_2);
    }
    cv.text_fit(s::DISPLAY, head.x + 10, head.y + 46, name, head.w - 20, INK);
    cv.text(s::FOOTNOTE, head.x + 12, head.y + 64, facts, INK_2);
    // the focused entry's card, for a file only: a folder's name is already the row
    if let Some(e) = sel.filter(|e| e.kind != Kind::Folder) {
        let card = Rect::new(gx, head.bottom() + gx, head.w, 44);
        s::material(cv, card.x, card.y, card.w, card.h, l::PANEL_R);
        let icon = match e.kind {
            Kind::Folder => &ic::FOLDER_24,
            Kind::Game(r) => System::from_runner(r).icon(24),
            Kind::Picture => &ic::PICTURE_24,
            Kind::Text => &ic::DOC_24,
            Kind::Other => &ic::FILE_24,
        };
        cv.icon(icon, card.x + 10, card.y + 10, INK_2);
        cv.text_fit(s::HEADLINE, card.x + 42, card.y + 19, e.name, card.w - 52, INK);
        let mut line = Buf::<48>::new();
        entry_line(&mut line, e, this_year);
        cv.text(s::FOOTNOTE, card.x + 42, card.y + 33, line.s(), INK_2);
    }
}

// ---- Games --------------------------------------------------------------------------------------------------------

/// One line of the grid: a section name where a section starts, and up to four games.
#[derive(Clone, Copy, Debug)]
pub struct Line<'a> {
    pub head: Option<&'a str>,
    pub games: &'a [Game<'a>],
}

/// Where the Games screen's focus ring is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GamesFocus {
    Search,
    Filter(usize),
    /// A cover: its line in the whole grid and its column.
    Cell(usize, usize),
    Tabs,
}

fn search_glyph_field(cv: &mut Canvas, r: Rect, text: &str, ink: crate::canvas::Rgba, f: &'static Font) -> i32 {
    s::glass(cv, r.x, r.y, r.w, r.h, r.h / 2);
    cv.icon(&ic::SEARCH_12, r.x + 8, r.y + 6, INK_2);
    cv.text_fit(f, r.x + 24, r.y + 16, text, r.w - 30, ink)
}

/// The Games screen: search, the platform filters, two lines of the cover grid from `first`, the tab bar.
pub fn games(cv: &mut Canvas, filter: usize, lines: &[Line], first: usize, focus: Option<GamesFocus>, indexing: Option<u32>) {
    s::bottom_backdrop(cv);
    search_glyph_field(cv, l::SEARCH, "Search", INK_2, s::FOOTNOTE);
    if focus == Some(GamesFocus::Search) {
        let r = l::SEARCH;
        s::focus(cv, r.x, r.y, r.w, r.h, r.h / 2);
    }
    for i in 0..l::FILTERS {
        let r = l::filter(i);
        let ink = if i == filter {
            cv.pill(r.x, r.y, r.w, r.h, ACCENT_FILL);
            INK
        } else {
            s::glass(cv, r.x, r.y, r.w, r.h, r.h / 2);
            INK_2
        };
        cv.text_centre(s::HEADLINE, r.cx(), r.y + 16, l::FILTER_NAMES[i], ink);
        if focus == Some(GamesFocus::Filter(i)) {
            s::focus(cv, r.x, r.y, r.w, r.h, r.h / 2);
        }
    }
    let mut named: Option<(Rect, &str)> = None;
    // the line whose cover shows its name: its heading gives way to the label
    let labelled = match focus {
        Some(GamesFocus::Cell(a, _)) if a >= first && a < first + l::GRID_LINES => Some(a - first),
        _ => None,
    };
    for (slot, line) in lines.iter().skip(first).take(l::GRID_LINES).enumerate() {
        let hb = l::cover_box(slot, 0);
        if let (Some(h), false) = (line.head, labelled == Some(slot)) {
            cv.text(s::CAPTION, l::EDGE + 2, hb.y - 4, h, INK_2);
        }
        if slot == 0 && labelled != Some(0) {
            if let Some(n) = indexing {
                let mut t = Buf::<24>::new();
                let _ = write!(t, "Indexing · {n}");
                let w = cv.text_right(s::FOOTNOTE, SCREEN_W - l::EDGE - 2, hb.y - 4, t.s(), INK_2);
                let rx = (SCREEN_W - l::EDGE - 2 - w - 9) as f32;
                cv.ring(rx, (hb.y - 7) as f32, 4.5, 1.6, 0, 1, 3, ACCENT, s::HAIR);
            }
        }
        for (col, gm) in line.games.iter().take(l::GRID_COLS).enumerate() {
            let r = cover(cv, l::cover_box(slot, col), gm);
            if focus == Some(GamesFocus::Cell(first + slot, col)) {
                s::focus(cv, r.x, r.y, r.w, r.h, 6);
                named = Some((r, gm.name_shown()));
            }
        }
    }
    // the focused cover's name, above everything else
    if let Some((r, n)) = named {
        s::name_label(cv, r.x, r.y, r.w, n);
    }
    screens::tab_bar(cv, l::TAB_GAMES, focus == Some(GamesFocus::Tabs));
}

/// The top screen in Games and Files: the selected game, large, with the round Play (A plays it).
pub fn game_card(cv: &mut Canvas, bar: &Bar, gm: &Game) {
    s::top_backdrop(cv);
    screens::strip(cv, bar);
    let card = Rect::new(l::EDGE, 18, SCREEN_W - 2 * l::EDGE, SCREEN_H - 24);
    s::material(cv, card.x, card.y, card.w, card.h, l::PANEL_R);
    // content and status only: the top screen takes no touch, so no button here (a game starts with A or a tap on
    // its cover below). The cover is as tall as its shape needs, centred on the card, the title beside its top. It is
    // an overlay: its shape changes with the game, the card around it does not, so a d-pad move redraws the cover and
    // the text, not the whole top screen.
    let (aw, ah) = gm.system.aspect();
    let shape = l::fit(Rect::new(0, 0, CARD_COVER_W, card.h - 20), aw, ah);
    cv.begin_overlay();
    let c = cover(cv, Rect::new(card.x + 10, card.y + (card.h - shape.h) / 2, CARD_COVER_W, shape.h), gm);
    cv.end_overlay();
    let x = c.right() + 12;
    let w = card.right() - 10 - x;
    let n = cv.text_wrap(s::GAME_TITLE, x, c.y + 20, gm.name_shown(), w, 3, 18, INK);
    let y = c.y + 20 + (n - 1) * 18;
    // the platform as its glyph, then when it was last played
    cv.icon(gm.system.icon(12), x, y + 9, INK_2);
    let mut when = Buf::<24>::new();
    played(&mut when, gm.played_ago);
    cv.text(s::FOOTNOTE, x + 16, y + 18, when.s(), INK_2);
}

/// The width of the top card's cover.
const CARD_COVER_W: i32 = 112;

// ---- search -------------------------------------------------------------------------------------------------------

/// A name with the matched letters in the accent; plain and ellipsised when it does not fit.
#[allow(clippy::too_many_arguments)]
fn text_hit(cv: &mut Canvas, f: &'static Font, x: i32, y: i32, t: &str, h: Hit, max: i32) -> i32 {
    let ok = h.start < h.end && h.end <= t.len() && t.is_char_boundary(h.start) && t.is_char_boundary(h.end);
    if !ok || f.width(t) > max {
        return cv.text_fit(f, x, y, t, max, INK);
    }
    let mut p = x + cv.text(f, x, y, &t[..h.start], INK);
    p += cv.text(f, p, y, &t[h.start..h.end], ACCENT);
    p += cv.text(f, p, y, &t[h.end..], INK);
    p - x
}

/// Result rows (a material group): the platform glyph, the name with its match lit, when played or the size.
fn result_rows(cv: &mut Canvas, top: i32, results: &[(Game, Hit)], max_rows: usize) {
    // the panel and its separators are the same however many match, so typing redraws only the rows' contents
    let n = results.len().min(max_rows) as i32;
    s::material(cv, l::EDGE, top, SCREEN_W - 2 * l::EDGE, max_rows as i32 * l::ROW_H, l::PANEL_R);
    for i in 1..max_rows as i32 {
        cv.fill(l::EDGE + 30, top + i * l::ROW_H, SCREEN_W - 2 * l::EDGE - 30, 1, s::HAIR);
    }
    for (i, (gm, h)) in results.iter().take(n as usize).enumerate() {
        let b = Rect::new(l::EDGE, top + i as i32 * l::ROW_H, SCREEN_W - 2 * l::EDGE, l::ROW_H);
        cv.icon(gm.system.icon(16), b.x + 8, b.y + 4, INK_2);
        let mut meta = Buf::<24>::new();
        match gm.played_ago {
            Some(a) => {
                let _ = write!(meta, "{}", Ago(a));
            }
            None if gm.system == System::Fortuna => {
                let _ = write!(meta, "Fortuna");
            }
            None => {
                // the glyph says the system
                let _ = write!(meta, "{}", Size(gm.size));
            }
        }
        let right = b.right() - 9;
        let mw = cv.text_right(s::FOOTNOTE, right, b.y + 16, meta.s(), INK_2);
        text_hit(cv, s::BODY, b.x + 30, b.y + 16, gm.name_shown(), *h, right - mw - 6 - (b.x + 30));
    }
}

/// The keyboard: a glass panel with keys; `pressed` lights one in the accent.
pub fn keyboard(cv: &mut Canvas, shift: bool, pressed: Option<Key>) {
    let top = l::KB_TOP - 2;
    s::glass(cv, -2, top, SCREEN_W + 4, SCREEN_H - top + 4, 12);
    for i in 0..l::KEYS {
        let (k, cell) = l::key(i);
        let r = Rect::new(cell.x + 1, cell.y + 2, cell.w - 2, cell.h - 3);
        let down = pressed == Some(k);
        let fill = match k {
            Key::Done => ACCENT_FILL,
            Key::Char(_) | Key::Space => g::CARBON_HI,
            _ => g::CARBON,
        };
        cv.round_rect(r.x, r.y, r.w, r.h, 5, fill);
        if down {
            // the pressed key over it, as an overlay: typing does not repaint the keyboard
            cv.begin_overlay();
            cv.round_rect(r.x, r.y, r.w, r.h, 5, ACCENT);
            cv.end_overlay();
        }
        let mut one = [0u8; 4];
        let (label, icon): (&str, Option<&Icon>) = match k {
            Key::Char(ch) => (((if shift { ch.to_ascii_uppercase() } else { ch }) as char).encode_utf8(&mut one), None),
            Key::Shift => ("", Some(&ic::SHIFT_12)),
            Key::Back => ("", Some(&ic::ERASE_16)),
            Key::Symbols => ("123", None),
            Key::Space => ("space", None),
            Key::Done => ("Done", None),
        };
        match icon {
            Some(i) => {
                let sz = i.size as i32;
                cv.icon(i, r.cx() - sz / 2, r.cy() - sz / 2, if shift && k == Key::Shift { ACCENT } else { INK });
            }
            None => {
                let f = if matches!(k, Key::Char(_)) { s::HEADLINE } else { s::FOOTNOTE };
                cv.text_centre(f, r.cx(), r.y + 15, label, if down { s::ON_LIGHT } else { INK });
            }
        }
    }
}

/// Searching: the field across the top with the query and the count, the two best results, the keyboard.
pub fn search(cv: &mut Canvas, query: &str, results: &[(Game, Hit)], found: u32, shift: bool, pressed: Option<Key>) {
    s::bottom_backdrop(cv);
    let r = l::SEARCH_TYPING;
    let w = search_glyph_field(cv, r, query, INK, s::BODY);
    // the caret: an overlay, so typing redraws only the field
    cv.begin_overlay();
    cv.fill(r.x + 24 + w + 1, r.y + 6, 1, 13, ACCENT);
    cv.end_overlay();
    s::focus(cv, r.x, r.y, r.w, r.h, r.h / 2);
    let mut n = Buf::<16>::new();
    let _ = write!(n, "{found} found");
    cv.icon(&ic::CROSS_12, r.right() - 20, r.y + 6, INK_2);
    cv.text_right(s::FOOTNOTE, r.right() - 26, r.y + 16, n.s(), INK_2);
    if results.is_empty() {
        cv.text_centre(s::BODY, SCREEN_W / 2, l::RESULTS_TOP + 28, "No game matches", INK_2);
    }
    result_rows(cv, l::RESULTS_TOP, results, l::RESULT_ROWS as usize);
    keyboard(cv, shift, pressed);
}

/// The top screen while searching: every result so far, the matched letters lit.
pub fn results_top(cv: &mut Canvas, bar: &Bar, query: &str, results: &[(Game, Hit)], found: u32) {
    s::top_backdrop(cv);
    screens::strip(cv, bar);
    let mut t = Buf::<48>::new();
    let _ = write!(t, "“{query}”");
    let w = cv.text(s::VALUE, l::EDGE + 4, 38, t.s(), INK);
    let mut n = Buf::<16>::new();
    let _ = write!(n, "{found} found");
    cv.text(s::FOOTNOTE, l::EDGE + 4 + w + 8, 38, n.s(), INK_2);
    let rows = ((SCREEN_H - 48 - 6) / l::ROW_H) as usize;
    result_rows(cv, 48, results, rows);
}
