//! Where things are on the DSi's two 256x192 screens, and how the d-pad moves between them.
//!
//! One minimum for every control: [`TAP`] = 24 px in both directions (tabs, rows, dialog buttons, filter pills, the
//! keyboard's keys, where a key's hit cell runs up to its neighbours). The tiles and the Play button are larger. Only
//! the bottom screen takes touch; every screen also works from the buttons (d-pad moves the focus ring, A confirms, B goes
//! back, L/R switch tabs), so every focusable thing here is a rectangle in a list that [`nav`] walks.

use crate::tokens::radius;

pub const SCREEN_W: i32 = 256;
pub const SCREEN_H: i32 = 192;

/// The smallest control, both ways (24 px: about 6 mm on the DSi's 0.26 mm pixels).
pub const TAP: i32 = 24;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Rect {
        Rect { x, y, w, h }
    }
    pub const fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
    pub const fn cx(&self) -> i32 {
        self.x + self.w / 2
    }
    pub const fn cy(&self) -> i32 {
        self.y + self.h / 2
    }
    pub const fn right(&self) -> i32 {
        self.x + self.w
    }
    pub const fn bottom(&self) -> i32 {
        self.y + self.h
    }
    pub const fn inset(&self, d: i32) -> Rect {
        Rect::new(self.x + d, self.y + d, self.w - 2 * d, self.h - 2 * d)
    }
    pub const fn apart(&self, o: &Rect) -> bool {
        self.x >= o.x + o.w || o.x >= self.x + self.w || self.y >= o.y + o.h || o.y >= self.y + self.h
    }
}

/// Side margin and the gap between panels (Glass's Fortuna layout: 6 px).
pub const EDGE: i32 = 6;
pub const GAP: i32 = 6;
/// Panel corner radius on the DS (Glass's Fortuna panels use 10; the scaled card token is 12).
pub const PANEL_R: i32 = 10;
pub const TILE_R: i32 = radius::TILE as i32;

// ---- the bottom screen's tab bar: a segmented capsule ---------------------------------------------------------

pub const TABS: usize = 5;
pub const TAB_NAMES: [&str; TABS] = ["Home", "Games", "Files", "Lucina", "System"];
pub const TAB_HOME: usize = 0;
pub const TAB_GAMES: usize = 1;
pub const TAB_FILES: usize = 2;
/// Lucina: chat and agents (a "Chat | Agents" control inside it, on the bottom screen).
pub const TAB_LUCINA: usize = 3;
pub const TAB_SYSTEM: usize = 4;
pub const TAB_BAR: Rect = Rect::new(EDGE, SCREEN_H - 4 - (TAP + 4), SCREEN_W - 2 * EDGE, TAP + 4);
/// Where content ends above the tab bar.
pub const CONTENT_BOTTOM: i32 = TAB_BAR.y - 4;

/// The tab labels' widths in the tab face (skin::HEADLINE; a test holds them to the font). Five equal segments do not
/// fit five words in 244 px, so each segment is its label plus an equal share of what is left.
pub const TAB_LABEL_W: [i32; TABS] = [35, 41, 30, 41, 45];
const fn tab_pad() -> i32 {
    let (mut sum, mut i) = (0, 0);
    while i < TABS {
        sum += TAB_LABEL_W[i];
        i += 1;
    }
    (TAB_BAR.w - 4 - sum) / TABS as i32
}
pub const fn tab(i: usize) -> Rect {
    let pad = tab_pad();
    let (mut x, mut j) = (TAB_BAR.x + 2, 0);
    while j < i {
        x += TAB_LABEL_W[j] + pad;
        j += 1;
    }
    // the last segment takes the rounding, so the five tile the bar
    let w = if i == TABS - 1 { TAB_BAR.x + TAB_BAR.w - 2 - x } else { TAB_LABEL_W[i] + pad };
    Rect::new(x, TAB_BAR.y + 2, w, TAP)
}

pub fn tab_at(x: i32, y: i32) -> Option<usize> {
    (0..TABS).find(|&i| tab(i).contains(x, y))
}

// ---- Home ---------------------------------------------------------------------------------------------------

/// The last game played: its cover, title, when, and the big round Play.
pub const CONTINUE: Rect = Rect::new(EDGE, 6, SCREEN_W - 2 * EDGE, 86);
pub const CONTINUE_COVER: Rect = Rect::new(CONTINUE.x + 8, CONTINUE.y + 8, 66, 70);
pub const PLAY: Rect = Rect::new(CONTINUE.x + CONTINUE.w - 10 - 44, CONTINUE.y + (CONTINUE.h - 44) / 2, 44, 44);
/// Pinned under it on a dock: Fortuna's own games, then Lucina.
pub const PINS: usize = 3;
/// The pin that opens the Lucina tab.
pub const PIN_LUCINA: usize = 2;
pub const PIN_W: i32 = (SCREEN_W - 2 * EDGE - (PINS as i32 - 1) * 8) / PINS as i32;
pub const PIN_H: i32 = 56;
pub const fn pin(i: usize) -> Rect {
    Rect::new(EDGE + i as i32 * (PIN_W + 8), CONTINUE.y + CONTINUE.h + GAP, PIN_W, PIN_H)
}

// ---- lists (Files, System, search results): one row component -------------------------------------------------

pub const ROW_H: i32 = TAP;
pub const LIST_TOP: i32 = 6;
/// Whole rows above the tab bar.
pub const LIST_ROWS: i32 = (CONTENT_BOTTOM - LIST_TOP) / ROW_H;
pub const fn row(i: i32) -> Rect {
    Rect::new(EDGE, LIST_TOP + i * ROW_H, SCREEN_W - 2 * EDGE, ROW_H)
}
pub fn row_at(x: i32, y: i32, n: i32) -> Option<i32> {
    (0..n.min(LIST_ROWS)).find(|&i| row(i).contains(x, y))
}

/// The System screen: its rows, then the Restart button under them.
pub const SYSTEM_ROWS: i32 = 4;
pub const RESTART: Rect = Rect::new(EDGE, LIST_TOP + SYSTEM_ROWS * ROW_H + 4, SCREEN_W - 2 * EDGE, TAP);

// ---- Games: search, platform filters, the cover grid -----------------------------------------------------------

pub const FILTERS: usize = 4;
pub const FILTER_NAMES: [&str; FILTERS] = ["All", "DS", "GBA", "Game Boy"];
const FILTER_W: [i32; FILTERS] = [30, 30, 38, 62];
const FILTER_GAP: i32 = 4;
const fn filters_w() -> i32 {
    FILTER_W[0] + FILTER_W[1] + FILTER_W[2] + FILTER_W[3] + 3 * FILTER_GAP
}
/// The search field at the top left; it grows across the row while typing.
pub const SEARCH: Rect = Rect::new(EDGE, 4, SCREEN_W - 2 * EDGE - filters_w() - FILTER_GAP, TAP);
pub const SEARCH_TYPING: Rect = Rect::new(EDGE, 4, SCREEN_W - 2 * EDGE, TAP);
pub const fn filter(i: usize) -> Rect {
    let mut x = SEARCH.x + SEARCH.w + FILTER_GAP;
    let mut k = 0;
    while k < i {
        x += FILTER_W[k] + FILTER_GAP;
        k += 1;
    }
    Rect::new(x, 4, FILTER_W[i], TAP)
}

/// The grid: lines of four covers. Each line has a 14 px header slot (a section name where a section starts) over a
/// 48 px cover band; two lines show at a time.
pub const GRID_COLS: usize = 4;
pub const GRID_TOP: i32 = SEARCH.y + SEARCH.h + 2;
pub const LINE_HEAD: i32 = 14;
pub const COVER_H: i32 = 48;
pub const LINE_H: i32 = LINE_HEAD + COVER_H + 2;
pub const GRID_LINES: usize = 2;
pub const CELL_W: i32 = (SCREEN_W - 2 * EDGE - (GRID_COLS as i32 - 1) * 4) / GRID_COLS as i32;
/// The box a cover is fitted into in cell `col` of on-screen line `line`.
pub const fn cover_box(line: usize, col: usize) -> Rect {
    Rect::new(EDGE + col as i32 * (CELL_W + 4) + 2, GRID_TOP + line as i32 * LINE_H + LINE_HEAD, CELL_W - 4, COVER_H)
}
pub fn cell_at(x: i32, y: i32) -> Option<(usize, usize)> {
    for line in 0..GRID_LINES {
        for col in 0..GRID_COLS {
            let b = cover_box(line, col);
            let cell = Rect::new(b.x - 2, b.y - LINE_HEAD, CELL_W, LINE_H);
            if cell.contains(x, y) {
                return Some((line, col));
            }
        }
    }
    None
}

/// The first line on screen for the selected line `sel` of `total`: the selection sits on the second visible line (so
/// the grid does not jump on the first press), except at the very top and bottom.
/// The rule is in rules.rs, checked by Verus.
pub fn first_line(sel: usize, total: usize) -> usize {
    crate::rules::first_line(sel, total, GRID_LINES)
}

/// A cover's size inside `b` for a box of shape `w`:`h` (never cropped: fitted and centred).
pub fn fit(b: Rect, w: i32, h: i32) -> Rect {
    let (cw, ch) = crate::rules::fit_dims(b.w, b.h, w, h);
    Rect::new(b.x + (b.w - cw) / 2, b.y + (b.h - ch), cw, ch)
}

// ---- search: results above the keyboard --------------------------------------------------------------------------

pub const RESULTS_TOP: i32 = SEARCH_TYPING.y + SEARCH_TYPING.h + 4;
pub const RESULT_ROWS: i32 = 2;
pub const fn result_row(i: i32) -> Rect {
    Rect::new(EDGE, RESULTS_TOP + i * ROW_H, SCREEN_W - 2 * EDGE, ROW_H)
}

/// The touch keyboard: four rows of hit cells 26 px high, from here to the bottom edge.
pub const KB_TOP: i32 = SCREEN_H - 4 * KEY_PITCH_Y;
pub const KEY_PITCH_Y: i32 = 26;
pub const KB_LETTERS: [&str; 3] = ["qwertyuiop", "asdfghjkl", "zxcvbnm"];
pub const KEYS: usize = 10 + 9 + 9 + 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Char(u8),
    Shift,
    Back,
    Symbols,
    Space,
    Done,
}

/// Key `i`: what it is and its hit cell (the drawn key is the cell inset by 1 px). Cells tile each row edge to edge.
pub const fn key(i: usize) -> (Key, Rect) {
    // x positions in 1/10 px: ten cells of 25.6 px across 256
    const fn cell(c10: i32, w10: i32, row: i32) -> Rect {
        Rect::new(c10 / 10, KB_TOP + row * KEY_PITCH_Y, (c10 + w10) / 10 - c10 / 10, KEY_PITCH_Y)
    }
    let i = i as i32;
    if i < 10 {
        (Key::Char(KB_LETTERS[0].as_bytes()[i as usize]), cell(i * 256, 256, 0))
    } else if i < 19 {
        let j = i - 10;
        (Key::Char(KB_LETTERS[1].as_bytes()[j as usize]), cell(128 + j * 256, 256, 1))
    } else if i < 28 {
        let j = i - 19;
        if j == 0 {
            (Key::Shift, cell(0, 384, 2))
        } else if j <= 7 {
            (Key::Char(KB_LETTERS[2].as_bytes()[(j - 1) as usize]), cell(384 + (j - 1) * 256, 256, 2))
        } else {
            (Key::Back, cell(384 + 7 * 256, 2560 - 384 - 7 * 256, 2))
        }
    } else {
        match i - 28 {
            0 => (Key::Symbols, cell(0, 512, 3)),
            1 => (Key::Space, cell(512, 1280, 3)),
            _ => (Key::Done, cell(1792, 768, 3)),
        }
    }
}

pub fn key_at(x: i32, y: i32) -> Option<Key> {
    (0..KEYS).map(key).find(|(_, r)| r.contains(x, y)).map(|(k, _)| k)
}

// ---- dialogs ------------------------------------------------------------------------------------------------------

pub const ALERT_W: i32 = 216;
pub const fn alert(h: i32) -> Rect {
    Rect::new((SCREEN_W - ALERT_W) / 2, (SCREEN_H - h) / 2 - 6, ALERT_W, h)
}
pub const fn alert_button(a: Rect, i: i32, n: i32) -> Rect {
    let (pad, gap) = (10, 6);
    let w = (a.w - 2 * pad - (n - 1) * gap) / n;
    Rect::new(a.x + pad + i * (w + gap), a.y + a.h - pad - TAP, w, TAP)
}

// ---- the d-pad ---------------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Up,
    Down,
    Left,
    Right,
}

/// The focusable rectangle the d-pad moves to from `from` in `dir`: the nearest whose centre lies in that direction
/// (distance along the direction, plus twice the sideways offset, so straight lines win), or `from` if none.
pub fn nav(rects: &[Rect], from: usize, dir: Dir) -> usize {
    let Some(f) = rects.get(from) else { return from };
    let (fx, fy) = (f.cx(), f.cy());
    let mut best: Option<(i32, usize)> = None;
    for (i, r) in rects.iter().enumerate() {
        if i == from {
            continue;
        }
        let (dx, dy) = (r.cx() - fx, r.cy() - fy);
        let (along, side) = match dir {
            Dir::Up => (-dy, dx.abs()),
            Dir::Down => (dy, dx.abs()),
            Dir::Left => (-dx, dy.abs()),
            Dir::Right => (dx, dy.abs()),
        };
        if along <= 0 {
            continue;
        }
        let score = along + 2 * side;
        if best.is_none_or(|(s, _)| score < s) {
            best = Some((score, i));
        }
    }
    best.map_or(from, |(_, i)| i)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tappable(r: Rect, what: &str) {
        assert!(r.w >= TAP && r.h >= TAP, "{what} {r:?} is under {TAP} px");
        assert!(r.x >= 0 && r.y >= 0 && r.right() <= SCREEN_W && r.bottom() <= SCREEN_H, "{what} {r:?} is off screen");
    }

    #[test]
    fn tab_labels_fit_their_segments() {
        for (i, n) in TAB_NAMES.iter().enumerate() {
            assert_eq!(crate::skin::HEADLINE.width(n), TAB_LABEL_W[i], "TAB_LABEL_W[{i}] is not the width of {n:?}");
            assert!(tab(i).w >= TAB_LABEL_W[i] + 8, "{n:?} has under 4 px each side in {:?}", tab(i));
            if i > 0 {
                assert_eq!(tab(i).x, tab(i - 1).right(), "tabs {} and {i} do not touch", i - 1);
            }
        }
        assert_eq!(tab(TABS - 1).right(), TAB_BAR.right() - 2);
    }

    #[test]
    fn every_control_meets_the_one_minimum() {
        for i in 0..TABS {
            tappable(tab(i), "tab");
        }
        tappable(PLAY, "play");
        for i in 0..PINS {
            tappable(pin(i), "pin");
            assert!(pin(i).bottom() <= CONTENT_BOTTOM);
        }
        for i in 0..LIST_ROWS {
            tappable(row(i), "row");
        }
        assert!(row(LIST_ROWS - 1).bottom() <= CONTENT_BOTTOM);
        tappable(RESTART, "restart");
        assert!(RESTART.bottom() <= CONTENT_BOTTOM);
        tappable(SEARCH, "search");
        for i in 0..FILTERS {
            tappable(filter(i), "filter");
            assert!(filter(i).right() <= SCREEN_W - EDGE, "filter {i} {:?}", filter(i));
            assert!(filter(i).apart(&SEARCH));
        }
        for i in 0..RESULT_ROWS {
            tappable(result_row(i), "result");
            assert!(result_row(i).bottom() <= KB_TOP);
        }
        let a = alert(96);
        for n in 1..=2 {
            for i in 0..n {
                tappable(alert_button(a, i, n), "alert button");
            }
        }
        for line in 0..GRID_LINES {
            for col in 0..GRID_COLS {
                tappable(cover_box(line, col), "cover");
                assert!(cover_box(line, col).bottom() <= CONTENT_BOTTOM);
            }
        }
    }

    #[test]
    fn keys_tile_each_row_and_hit_where_drawn() {
        let mut letters = 0;
        for i in 0..KEYS {
            let (k, r) = key(i);
            tappable(r, "key");
            if let Key::Char(c) = k {
                assert!(c.is_ascii_lowercase());
                letters += 1;
            }
            for j in 0..i {
                assert!(r.apart(&key(j).1), "keys {j} and {i} overlap");
            }
            assert_eq!(key_at(r.cx(), r.cy()), Some(k));
        }
        assert_eq!(letters, 26);
        // rows 0, 2 and 3 run edge to edge
        for y in [KB_TOP + 1, KB_TOP + 2 * KEY_PITCH_Y + 1, KB_TOP + 3 * KEY_PITCH_Y + 1] {
            for x in 0..SCREEN_W {
                assert!(key_at(x, y).is_some(), "gap at ({x},{y})");
            }
        }
        assert_eq!(key_at(0, 0), None);
    }

    #[test]
    fn hit_tests_match_rectangles() {
        for y in 0..SCREEN_H {
            for x in 0..SCREEN_W {
                let hit = tab_at(x, y);
                for i in 0..TABS {
                    assert_eq!(hit == Some(i), tab(i).contains(x, y));
                }
                let r = row_at(x, y, 99);
                for i in 0..LIST_ROWS {
                    assert_eq!(r == Some(i), row(i).contains(x, y));
                }
            }
        }
    }

    #[test]
    fn selection_stays_on_the_second_line() {
        assert_eq!(first_line(0, 10), 0);
        assert_eq!(first_line(1, 10), 0);
        assert_eq!(first_line(2, 10), 1);
        assert_eq!(first_line(5, 10), 4);
        assert_eq!(first_line(9, 10), 8);
        assert_eq!(first_line(3, 2), 0);
        assert_eq!(first_line(0, 0), 0);
        for total in 0..12 {
            for sel in 0..total {
                let f = first_line(sel, total);
                assert!(sel >= f && sel < f + GRID_LINES, "sel {sel} of {total} off screen at {f}");
            }
        }
    }

    #[test]
    fn covers_fit_without_cropping() {
        let b = cover_box(0, 0);
        for (w, h) in [(9, 10), (1, 1), (7, 8), (16, 9)] {
            let c = fit(b, w, h);
            assert!(c.w <= b.w && c.h <= b.h && c.x >= b.x && c.right() <= b.right() && c.bottom() == b.bottom());
            assert!((c.w * h - c.h * w).abs() <= w.max(h), "{w}:{h} -> {c:?}");
        }
    }

    #[test]
    fn dpad_moves_to_the_nearest_in_line() {
        let grid: [Rect; 8] = core::array::from_fn(|i| cover_box(i / 4, i % 4));
        assert_eq!(nav(&grid, 0, Dir::Right), 1);
        assert_eq!(nav(&grid, 1, Dir::Down), 5);
        assert_eq!(nav(&grid, 5, Dir::Up), 1);
        assert_eq!(nav(&grid, 4, Dir::Left), 4, "nothing to the left stays put");
        assert_eq!(nav(&grid, 3, Dir::Right), 3);
        assert_eq!(nav(&grid, 99, Dir::Up), 99);
    }
}
