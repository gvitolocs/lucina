//! The terminal's screen model: a grid of cells driven by `vte`'s escape-sequence parser.
//!
//! It covers what BusyBox's shell, its line editor, `top`, `less` and `vi` send (the Linux console's
//! and xterm's common subset): cursor movement, erasing, inserting and deleting characters and
//! lines, scroll regions, SGR colours (16, 256 and true colour), the alternate screen, cursor
//! visibility, application cursor keys, and replies to the status and attribute queries. Rows that
//! changed are marked dirty so the window only redraws those. Rows scrolled off the top are kept
//! (without their trailing blanks) so the window can look back at them.

use std::collections::VecDeque;

use vte::{Params, Parser, Perform};

/// Rows kept after they scroll off the top of the main screen.
const HISTORY_ROWS: usize = 1000;

/// A colour as the application asked for it; the window resolves it with its palette.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Color {
    Default,
    Indexed(u8),
    Rgb(u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Attr {
    pub fg: Color,
    pub bg: Color,
    pub bold: bool,
    pub inverse: bool,
    pub underline: bool,
}

impl Default for Attr {
    fn default() -> Self {
        Attr {
            fg: Color::Default,
            bg: Color::Default,
            bold: false,
            inverse: false,
            underline: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub attr: Attr,
}

impl Default for Cell {
    fn default() -> Self {
        Cell {
            ch: ' ',
            attr: Attr::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Saved {
    row: usize,
    col: usize,
    attr: Attr,
}

/// Everything but the parser, so that the parser can drive it (`Parser::advance` borrows both).
pub struct Screen {
    pub cols: usize,
    pub rows: usize,
    cells: Vec<Cell>,
    /// The main screen while the alternate one (`CSI ? 1049 h`) is shown.
    main_saved: Option<Vec<Cell>>,
    /// Rows scrolled off the top of the main screen, oldest first.
    history: VecDeque<Vec<Cell>>,
    /// How many rows back the window looks; 0 is the live screen.
    view_back: usize,
    pub row: usize,
    pub col: usize,
    /// The cursor is past the last column; the next printed character wraps first (DECAWM).
    wrap_pending: bool,
    attr: Attr,
    saved: Saved,
    top: usize,
    bottom: usize,
    pub cursor_visible: bool,
    /// DECCKM: arrow keys send `ESC O A` instead of `ESC [ A`.
    pub app_cursor: bool,
    autowrap: bool,
    dirty: Vec<bool>,
    /// Bytes to send back to the program (status and attribute replies).
    pub replies: Vec<u8>,
    /// Lines completed by a line feed, kept when tracing (`--trace`).
    pub trace: Option<Vec<String>>,
}

pub struct Terminal {
    parser: Parser,
    pub screen: Screen,
}

impl Terminal {
    pub fn new(cols: usize, rows: usize) -> Terminal {
        Terminal {
            parser: Parser::new(),
            screen: Screen::new(cols.max(1), rows.max(1)),
        }
    }

    /// Feed output from the program.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.screen, bytes);
    }
}

impl Screen {
    fn new(cols: usize, rows: usize) -> Screen {
        Screen {
            cols,
            rows,
            cells: vec![Cell::default(); cols * rows],
            main_saved: None,
            history: VecDeque::new(),
            view_back: 0,
            row: 0,
            col: 0,
            wrap_pending: false,
            attr: Attr::default(),
            saved: Saved::default(),
            top: 0,
            bottom: rows - 1,
            cursor_visible: true,
            app_cursor: false,
            autowrap: true,
            dirty: vec![true; rows],
            replies: Vec::new(),
            trace: None,
        }
    }

    pub fn cell(&self, row: usize, col: usize) -> Cell {
        self.cells[row * self.cols + col]
    }

    /// The cell at a row of the window, which may look back into the history.
    pub fn view_cell(&self, row: usize, col: usize) -> Cell {
        if row < self.view_back {
            let old = &self.history[self.history.len() - self.view_back + row];
            old.get(col).copied().unwrap_or_default()
        } else {
            self.cell(row - self.view_back, col)
        }
    }

    /// How many rows back the window looks.
    pub fn view_back(&self) -> usize {
        self.view_back
    }

    /// Moves the window by `rows` (positive = older), within the history. Returns whether it moved.
    pub fn scroll_view(&mut self, rows: isize) -> bool {
        let back = self
            .view_back
            .saturating_add_signed(rows)
            .min(self.history.len());
        let moved = back != self.view_back;
        self.view_back = back;
        if moved {
            self.mark_all_dirty();
        }
        moved
    }

    /// Keeps a row that is about to scroll off the top.
    fn keep_row(&mut self, row: usize) {
        let cells = &self.cells[row * self.cols..(row + 1) * self.cols];
        let used = cells
            .iter()
            .rposition(|c| *c != Cell::default())
            .map_or(0, |i| i + 1);
        if self.history.len() == HISTORY_ROWS {
            self.history.pop_front();
        }
        self.history.push_back(cells[..used].to_vec());
        // A window looking back keeps showing the same rows.
        if self.view_back > 0 {
            self.view_back = (self.view_back + 1).min(self.history.len());
        }
    }

    /// The text of a row, trailing blanks removed.
    #[cfg(test)]
    pub fn row_text(&self, row: usize) -> String {
        let s: String = self.cells[row * self.cols..(row + 1) * self.cols]
            .iter()
            .map(|c| c.ch)
            .collect();
        s.trim_end().to_string()
    }

    /// The rows changed since the last call, cleared as they are returned.
    pub fn take_dirty(&mut self) -> Vec<usize> {
        let rows = self
            .dirty
            .iter()
            .enumerate()
            .filter(|(_, d)| **d)
            .map(|(i, _)| i)
            .collect();
        self.dirty.iter_mut().for_each(|d| *d = false);
        rows
    }

    pub fn mark_all_dirty(&mut self) {
        self.dirty.iter_mut().for_each(|d| *d = true);
    }

    fn blank(&self) -> Cell {
        // Erased cells keep the current background, like xterm and the Linux console.
        Cell {
            ch: ' ',
            attr: Attr {
                bg: self.attr.bg,
                ..Attr::default()
            },
        }
    }

    fn clear_cells(&mut self, row: usize, from: usize, to: usize) {
        let b = self.blank();
        let base = row * self.cols;
        self.cells[base + from.min(self.cols)..base + to.min(self.cols)].fill(b);
        self.dirty[row] = true;
    }

    /// Move rows `top..=bottom` up by `n`, blank rows coming in at the bottom.
    fn scroll_up(&mut self, top: usize, bottom: usize, n: usize) {
        let n = n.min(bottom + 1 - top);
        let c = self.cols;
        if top == 0 && self.main_saved.is_none() {
            for r in 0..n {
                self.keep_row(r);
            }
        }
        self.cells
            .copy_within((top + n) * c..(bottom + 1) * c, top * c);
        for r in bottom + 1 - n..=bottom {
            self.clear_cells(r, 0, c);
        }
        for r in top..=bottom {
            self.dirty[r] = true;
        }
    }

    /// Move rows `top..=bottom` down by `n`, blank rows coming in at the top.
    fn scroll_down(&mut self, top: usize, bottom: usize, n: usize) {
        let n = n.min(bottom + 1 - top);
        let c = self.cols;
        self.cells
            .copy_within(top * c..(bottom + 1 - n) * c, (top + n) * c);
        for r in top..top + n {
            self.clear_cells(r, 0, c);
        }
        for r in top..=bottom {
            self.dirty[r] = true;
        }
    }

    fn line_feed(&mut self) {
        if let Some(t) = self.trace.as_mut() {
            let s: String = self.cells[self.row * self.cols..(self.row + 1) * self.cols]
                .iter()
                .map(|c| c.ch)
                .collect();
            t.push(s.trim_end().to_string());
        }
        if self.row == self.bottom {
            self.scroll_up(self.top, self.bottom, 1);
        } else if self.row + 1 < self.rows {
            self.row += 1;
        }
        self.wrap_pending = false;
    }

    fn reverse_index(&mut self) {
        if self.row == self.top {
            self.scroll_down(self.top, self.bottom, 1);
        } else if self.row > 0 {
            self.row -= 1;
        }
        self.wrap_pending = false;
    }

    fn goto(&mut self, row: usize, col: usize) {
        self.dirty[self.row] = true;
        self.row = row.min(self.rows - 1);
        self.col = col.min(self.cols - 1);
        self.wrap_pending = false;
        self.dirty[self.row] = true;
    }

    fn set_cursor_row(&mut self, row: usize) {
        let col = self.col;
        self.goto(row, col);
    }

    fn set_cursor_col(&mut self, col: usize) {
        let row = self.row;
        self.goto(row, col);
    }

    fn sgr(&mut self, params: &Params) {
        let mut it = params
            .iter()
            .map(|p| p.first().copied().unwrap_or(0))
            .peekable();
        if it.peek().is_none() {
            self.attr = Attr::default();
            return;
        }
        while let Some(p) = it.next() {
            match p {
                0 => self.attr = Attr::default(),
                1 => self.attr.bold = true,
                4 => self.attr.underline = true,
                7 => self.attr.inverse = true,
                22 => self.attr.bold = false,
                24 => self.attr.underline = false,
                27 => self.attr.inverse = false,
                30..=37 => self.attr.fg = Color::Indexed((p - 30) as u8),
                39 => self.attr.fg = Color::Default,
                40..=47 => self.attr.bg = Color::Indexed((p - 40) as u8),
                49 => self.attr.bg = Color::Default,
                90..=97 => self.attr.fg = Color::Indexed((p - 90 + 8) as u8),
                100..=107 => self.attr.bg = Color::Indexed((p - 100 + 8) as u8),
                38 | 48 => {
                    let c = match it.next() {
                        Some(5) => it.next().map(|n| Color::Indexed(n.min(255) as u8)),
                        Some(2) => {
                            let r = it.next().unwrap_or(0).min(255) as u32;
                            let g = it.next().unwrap_or(0).min(255) as u32;
                            let b = it.next().unwrap_or(0).min(255) as u32;
                            Some(Color::Rgb(r << 16 | g << 8 | b))
                        }
                        _ => None,
                    };
                    if let Some(c) = c {
                        if p == 38 {
                            self.attr.fg = c;
                        } else {
                            self.attr.bg = c;
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn private_mode(&mut self, mode: u16, on: bool) {
        match mode {
            1 => self.app_cursor = on,
            7 => self.autowrap = on,
            25 => {
                self.cursor_visible = on;
                self.dirty[self.row] = true;
            }
            47 | 1047 | 1049 => self.alternate_screen(on),
            _ => {}
        }
    }

    fn alternate_screen(&mut self, on: bool) {
        if on && self.main_saved.is_none() {
            self.saved = Saved {
                row: self.row,
                col: self.col,
                attr: self.attr,
            };
            self.view_back = 0;
            let blank = vec![Cell::default(); self.cols * self.rows];
            self.main_saved = Some(std::mem::replace(&mut self.cells, blank));
        } else if !on && let Some(main) = self.main_saved.take() {
            self.cells = main;
            let s = self.saved;
            self.attr = s.attr;
            self.goto(s.row, s.col);
        }
        self.mark_all_dirty();
    }

    fn reset(&mut self) {
        let (cols, rows, trace) = (self.cols, self.rows, self.trace.take());
        *self = Screen::new(cols, rows);
        self.trace = trace;
    }
}

/// The first parameter, with 0 or missing read as `default` (how CSI counts work).
fn arg(params: &Params, i: usize, default: u16) -> usize {
    match params.iter().nth(i).and_then(|p| p.first().copied()) {
        None | Some(0) => default as usize,
        Some(v) => v as usize,
    }
}

impl Perform for Screen {
    fn print(&mut self, c: char) {
        if self.wrap_pending {
            if self.autowrap {
                self.col = 0;
                self.line_feed();
            }
            self.wrap_pending = false;
        }
        let i = self.row * self.cols + self.col;
        self.cells[i] = Cell {
            ch: c,
            attr: self.attr,
        };
        self.dirty[self.row] = true;
        if self.col + 1 < self.cols {
            self.col += 1;
        } else {
            self.wrap_pending = true;
        }
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\n' | 0x0b | 0x0c => self.line_feed(),
            b'\r' => self.set_cursor_col(0),
            0x08 => {
                if self.col > 0 {
                    self.set_cursor_col(self.col - 1);
                }
            }
            b'\t' => {
                let next = ((self.col / 8) + 1) * 8;
                self.set_cursor_col(next.min(self.cols - 1));
            }
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        if ignore {
            return;
        }
        let private = intermediates.first() == Some(&b'?');
        if private {
            if matches!(action, 'h' | 'l') {
                for p in params.iter() {
                    self.private_mode(p.first().copied().unwrap_or(0), action == 'h');
                }
            }
            return;
        }
        let n = arg(params, 0, 1);
        match action {
            'A' => self.set_cursor_row(self.row.saturating_sub(n).max(if self.row >= self.top {
                self.top
            } else {
                0
            })),
            'B' | 'e' => {
                let limit = if self.row <= self.bottom {
                    self.bottom
                } else {
                    self.rows - 1
                };
                self.set_cursor_row((self.row + n).min(limit))
            }
            'C' | 'a' => self.set_cursor_col(self.col + n),
            'D' => self.set_cursor_col(self.col.saturating_sub(n)),
            'E' => {
                self.goto((self.row + n).min(self.bottom), 0);
            }
            'F' => {
                self.goto(self.row.saturating_sub(n).max(self.top), 0);
            }
            'G' | '`' => self.set_cursor_col(n - 1),
            'd' => self.set_cursor_row(n - 1),
            'H' | 'f' => self.goto(arg(params, 0, 1) - 1, arg(params, 1, 1) - 1),
            'J' => {
                let (row, col) = (self.row, self.col);
                match arg(params, 0, 0) {
                    0 => {
                        self.clear_cells(row, col, self.cols);
                        for r in row + 1..self.rows {
                            self.clear_cells(r, 0, self.cols);
                        }
                    }
                    1 => {
                        for r in 0..row {
                            self.clear_cells(r, 0, self.cols);
                        }
                        self.clear_cells(row, 0, col + 1);
                    }
                    // Only the history, as on the Linux console and xterm.
                    3 => {
                        self.history.clear();
                        self.view_back = 0;
                        self.mark_all_dirty();
                    }
                    _ => {
                        for r in 0..self.rows {
                            self.clear_cells(r, 0, self.cols);
                        }
                    }
                }
            }
            'K' => {
                let (row, col) = (self.row, self.col);
                match arg(params, 0, 0) {
                    0 => self.clear_cells(row, col, self.cols),
                    1 => self.clear_cells(row, 0, col + 1),
                    _ => self.clear_cells(row, 0, self.cols),
                }
            }
            'X' => {
                let (row, col) = (self.row, self.col);
                self.clear_cells(row, col, col + n);
            }
            '@' => {
                let base = self.row * self.cols;
                let n = n.min(self.cols - self.col);
                self.cells
                    .copy_within(base + self.col..base + self.cols - n, base + self.col + n);
                let (row, col) = (self.row, self.col);
                self.clear_cells(row, col, col + n);
            }
            'P' => {
                let base = self.row * self.cols;
                let n = n.min(self.cols - self.col);
                self.cells
                    .copy_within(base + self.col + n..base + self.cols, base + self.col);
                let row = self.row;
                self.clear_cells(row, self.cols - n, self.cols);
            }
            'L' => {
                if (self.top..=self.bottom).contains(&self.row) {
                    self.scroll_down(self.row, self.bottom, n);
                }
            }
            'M' => {
                if (self.top..=self.bottom).contains(&self.row) {
                    self.scroll_up(self.row, self.bottom, n);
                }
            }
            'S' => self.scroll_up(self.top, self.bottom, n),
            'T' => self.scroll_down(self.top, self.bottom, n),
            'm' => self.sgr(params),
            'r' => {
                let top = arg(params, 0, 1) - 1;
                let bottom = arg(params, 1, self.rows as u16) - 1;
                if top < bottom && bottom < self.rows {
                    self.top = top;
                    self.bottom = bottom;
                    self.goto(0, 0);
                }
            }
            's' => {
                self.saved = Saved {
                    row: self.row,
                    col: self.col,
                    attr: self.attr,
                }
            }
            'u' => {
                let s = self.saved;
                self.attr = s.attr;
                self.goto(s.row, s.col);
            }
            'n' => match arg(params, 0, 0) {
                5 => self.replies.extend_from_slice(b"\x1b[0n"),
                6 => self.replies.extend_from_slice(
                    format!("\x1b[{};{}R", self.row + 1, self.col + 1).as_bytes(),
                ),
                _ => {}
            },
            // "VT102": what the Linux console answers.
            'c' if intermediates.is_empty() => self.replies.extend_from_slice(b"\x1b[?6c"),
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, byte: u8) {
        if !intermediates.is_empty() {
            return; // Character set selection (ESC ( B and friends): only one set here.
        }
        match byte {
            b'7' => {
                self.saved = Saved {
                    row: self.row,
                    col: self.col,
                    attr: self.attr,
                }
            }
            b'8' => {
                let s = self.saved;
                self.attr = s.attr;
                self.goto(s.row, s.col);
            }
            b'D' => self.line_feed(),
            b'E' => {
                self.set_cursor_col(0);
                self.line_feed();
            }
            b'M' => self.reverse_index(),
            b'c' => self.reset(),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn term(cols: usize, rows: usize, input: &str) -> Terminal {
        let mut t = Terminal::new(cols, rows);
        t.feed(input.as_bytes());
        t
    }

    #[test]
    fn prints_and_wraps() {
        let t = term(5, 3, "hello world");
        assert_eq!(t.screen.row_text(0), "hello");
        assert_eq!(t.screen.row_text(1), " worl");
        assert_eq!(t.screen.row_text(2), "d");
        assert_eq!((t.screen.row, t.screen.col), (2, 1));
    }

    #[test]
    fn the_last_column_waits_before_wrapping() {
        // A program that writes exactly a full line and then CR LF must not get an empty line.
        let t = term(5, 3, "abcde\r\nx");
        assert_eq!(t.screen.row_text(0), "abcde");
        assert_eq!(t.screen.row_text(1), "x");
    }

    #[test]
    fn scrolls_at_the_bottom() {
        let t = term(4, 2, "one\r\ntwo\r\nsix");
        assert_eq!(t.screen.row_text(0), "two");
        assert_eq!(t.screen.row_text(1), "six");
    }

    #[test]
    fn busybox_line_editing() {
        // What ash's line editor sends after a backspace in "echo hi" -> "echo h": BS, then
        // erase to the end of the line.
        let t = term(20, 2, "~ # echo hi\x08\x1b[K");
        assert_eq!(t.screen.row_text(0), "~ # echo h");
        assert_eq!(t.screen.col, 10);
        // Moving left three and inserting.
        let t = term(20, 2, "abcdef\x1b[3DX\x1b[@");
        assert_eq!(t.screen.row_text(0), "abcX ef");
    }

    #[test]
    fn cursor_addressing_and_erase() {
        let t = term(
            10,
            4,
            "xxxxxxxxxx\r\nyyyyyyyyyy\x1b[1;3H\x1b[1K\x1b[2;5H\x1b[0K",
        );
        assert_eq!(t.screen.row_text(0), "   xxxxxxx");
        assert_eq!(t.screen.row_text(1), "yyyy");
        let t = term(10, 3, "aaa\r\nbbb\r\nccc\x1b[2J");
        assert!((0..3).all(|r| t.screen.row_text(r).is_empty()));
    }

    #[test]
    fn delete_characters_and_lines() {
        let t = term(10, 3, "abcdef\x1b[1;2H\x1b[2P");
        assert_eq!(t.screen.row_text(0), "adef");
        let t = term(5, 3, "1\r\n2\r\n3\x1b[1;1H\x1b[M");
        assert_eq!(t.screen.row_text(0), "2");
        assert_eq!(t.screen.row_text(1), "3");
        assert_eq!(t.screen.row_text(2), "");
        let t = term(5, 3, "1\r\n2\r\n3\x1b[1;1H\x1b[L");
        assert_eq!(t.screen.row_text(0), "");
        assert_eq!(t.screen.row_text(1), "1");
    }

    #[test]
    fn scroll_region() {
        // top-like: rows 2..3 scroll, row 1 stays.
        let t = term(6, 4, "head\x1b[2;3r\x1b[2;1Ha\r\nb\r\nc");
        assert_eq!(t.screen.row_text(0), "head");
        assert_eq!(t.screen.row_text(1), "b");
        assert_eq!(t.screen.row_text(2), "c");
    }

    #[test]
    fn colours() {
        let t = term(
            10,
            1,
            "\x1b[1;31mR\x1b[0m\x1b[38;5;200mP\x1b[48;2;1;2;3mT\x1b[mN",
        );
        let s = &t.screen;
        assert_eq!(s.cell(0, 0).attr.fg, Color::Indexed(1));
        assert!(s.cell(0, 0).attr.bold);
        assert_eq!(s.cell(0, 1).attr.fg, Color::Indexed(200));
        assert!(!s.cell(0, 1).attr.bold);
        assert_eq!(s.cell(0, 2).attr.bg, Color::Rgb(0x010203));
        assert_eq!(s.cell(0, 3).attr, Attr::default());
    }

    #[test]
    fn alternate_screen_restores_the_main_one() {
        let mut t = term(8, 2, "prompt");
        t.feed(b"\x1b[?1049h\x1b[Hvi stuff");
        assert_eq!(t.screen.row_text(0), "vi stuff");
        t.feed(b"\x1b[?1049l");
        assert_eq!(t.screen.row_text(0), "prompt");
        assert_eq!(t.screen.col, 6);
    }

    #[test]
    fn answers_queries() {
        let mut t = term(10, 5, "ab\x1b[6n\x1b[c");
        assert_eq!(t.screen.replies, b"\x1b[1;3R\x1b[?6c");
        t.screen.replies.clear();
        t.feed(b"\x1b[?25l\x1b[?1h");
        assert!(!t.screen.cursor_visible);
        assert!(t.screen.app_cursor);
    }

    #[test]
    fn dirty_rows() {
        let mut t = term(5, 4, "");
        t.screen.take_dirty();
        t.feed(b"\x1b[3;1Hx");
        assert_eq!(t.screen.take_dirty(), vec![0, 2]);
        assert!(t.screen.take_dirty().is_empty());
    }

    #[test]
    fn traces_completed_lines() {
        let mut t = Terminal::new(20, 3);
        t.screen.trace = Some(Vec::new());
        t.feed(b"~ # echo hi\r\nhi\r\n~ # ");
        assert_eq!(
            t.screen.trace.as_deref(),
            Some(&["~ # echo hi".to_string(), "hi".to_string()][..])
        );
    }

    #[test]
    fn scrolled_rows_stay_in_the_history() {
        let mut t = term(4, 2, "one\r\ntwo\r\nsix\r\nten");
        let s = &mut t.screen;
        assert_eq!(s.view_back(), 0);
        assert_eq!(s.view_cell(0, 0).ch, 's');
        assert!(s.scroll_view(1));
        assert_eq!(s.view_cell(0, 0).ch, 't'); // "two"
        assert_eq!(s.view_cell(1, 0).ch, 's');
        assert!(s.scroll_view(100));
        assert_eq!(s.view_back(), 2);
        assert_eq!(s.view_cell(0, 0).ch, 'o');
        assert_eq!(s.view_cell(0, 3).ch, ' '); // past the kept part of the row
        assert!(!s.scroll_view(1));
        assert!(s.scroll_view(-100));
        assert_eq!(s.view_cell(0, 0).ch, 's');
    }

    #[test]
    fn a_window_looking_back_stays_on_its_rows() {
        let mut t = term(4, 2, "one\r\ntwo\r\nsix");
        t.screen.scroll_view(1);
        assert_eq!(t.screen.view_cell(0, 0).ch, 'o');
        t.feed(b"\r\nten");
        assert_eq!(t.screen.view_back(), 2);
        assert_eq!(t.screen.view_cell(0, 0).ch, 'o');
    }

    #[test]
    fn the_history_is_bounded_and_clearable() {
        let mut t = Terminal::new(4, 1);
        for _ in 0..HISTORY_ROWS + 5 {
            t.feed(b"x\r\n");
        }
        t.screen.scroll_view(isize::MAX);
        assert_eq!(t.screen.view_back(), HISTORY_ROWS);
        t.feed(b"\x1b[3J");
        assert_eq!(t.screen.view_back(), 0);
        assert!(!t.screen.scroll_view(1));
    }

    #[test]
    fn scroll_regions_and_the_alternate_screen_keep_no_history() {
        let mut t = term(4, 3, "a\x1b[2;3r\x1b[3;1Hb\r\nc\r\nd");
        assert!(!t.screen.scroll_view(1));
        let mut t = term(4, 2, "\x1b[?1049hx\r\ny\r\nz");
        assert!(!t.screen.scroll_view(1));
        t.feed(b"\x1b[?1049l");
        assert!(!t.screen.scroll_view(1));
    }

    #[test]
    fn utf8_and_tabs() {
        let t = term(20, 1, "a\tb ─│é");
        assert_eq!(t.screen.row_text(0), "a       b ─│é");
    }
}
