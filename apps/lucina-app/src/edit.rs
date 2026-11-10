//! What a text field holds: the text, the cursor and the selection, and every edit a key can make.
//! No drawing here, so all of it is tested on the host. Offsets are bytes on `char` boundaries.

use crate::keys::{self, Press};

/// What a key did to a [`TextEdit`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeyResult {
    /// The key belonged to the field.
    pub used: bool,
    /// The text changed.
    pub changed: bool,
    /// Enter in a single-line field, Ctrl+Enter in a multi-line one.
    pub submitted: bool,
}

/// The contents of a text field. Keep one per field in the app's state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TextEdit {
    pub text: String,
    /// Where the cursor is.
    pub cursor: usize,
    /// The other end of the selection; equal to `cursor` when nothing is selected.
    pub anchor: usize,
    /// Enter adds a line; Up and Down move between lines.
    pub multiline: bool,
    /// Draw every character as a bullet (the text itself stays readable to the app).
    pub password: bool,
    /// Shown in dim text while the field is empty.
    pub placeholder: String,
    /// Pixels scrolled, kept by the widget.
    pub(crate) scroll: i32,
    /// The x position Up and Down keep to, kept by the widget.
    pub(crate) goal_x: Option<i32>,
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

impl TextEdit {
    pub fn new() -> TextEdit {
        TextEdit::default()
    }

    /// A field with `text` in it and the cursor at the end.
    pub fn with_text(text: &str) -> TextEdit {
        TextEdit {
            cursor: text.len(),
            anchor: text.len(),
            text: text.to_string(),
            ..TextEdit::default()
        }
    }

    pub fn multiline() -> TextEdit {
        TextEdit {
            multiline: true,
            ..TextEdit::default()
        }
    }

    pub fn placeholder(mut self, s: &str) -> TextEdit {
        self.placeholder = s.to_string();
        self
    }

    /// Replace everything, with the cursor at the end.
    pub fn set(&mut self, text: &str) {
        self.text = text.to_string();
        self.cursor = text.len();
        self.anchor = text.len();
        self.scroll = 0;
    }

    pub fn clear(&mut self) {
        self.set("");
    }

    /// The selected bytes, in order.
    pub fn selection(&self) -> std::ops::Range<usize> {
        self.cursor.min(self.anchor)..self.cursor.max(self.anchor)
    }

    pub fn selected(&self) -> &str {
        &self.text[self.selection()]
    }

    /// Put in `s` instead of the selection. Line breaks become spaces in a single-line field.
    pub fn insert(&mut self, s: &str) {
        let r = self.selection();
        let at = r.start;
        if self.multiline {
            self.text.replace_range(r, s);
            self.cursor = at + s.len();
        } else {
            let flat: String = s
                .chars()
                .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
                .collect();
            self.text.replace_range(r, &flat);
            self.cursor = at + flat.len();
        }
        self.anchor = self.cursor;
    }

    fn before(&self, at: usize) -> usize {
        self.text[..at].char_indices().last().map_or(0, |(i, _)| i)
    }

    fn after(&self, at: usize) -> usize {
        self.text[at..]
            .chars()
            .next()
            .map_or(at, |c| at + c.len_utf8())
    }

    fn word_before(&self, at: usize) -> usize {
        let mut p = at;
        let mut in_word = false;
        for (i, c) in self.text[..at].char_indices().rev() {
            if is_word(c) {
                in_word = true;
            } else if in_word {
                break;
            }
            p = i;
        }
        p
    }

    fn word_after(&self, at: usize) -> usize {
        let mut in_word = false;
        for (i, c) in self.text[at..].char_indices() {
            if is_word(c) {
                in_word = true;
            } else if in_word {
                return at + i;
            }
        }
        self.text.len()
    }

    fn line_start(&self, at: usize) -> usize {
        self.text[..at].rfind('\n').map_or(0, |i| i + 1)
    }

    fn line_end(&self, at: usize) -> usize {
        self.text[at..]
            .find('\n')
            .map_or(self.text.len(), |i| at + i)
    }

    /// Move the cursor; with `select` the anchor stays where it was.
    pub fn move_to(&mut self, at: usize, select: bool) {
        self.cursor = at.min(self.text.len());
        if !select {
            self.anchor = self.cursor;
        }
        self.goal_x = None;
    }

    pub fn select_all(&mut self) {
        self.anchor = 0;
        self.cursor = self.text.len();
    }

    /// Apply one key, except Up and Down in a multi-line field, which need the layout (the widget
    /// handles them). `clipboard` is what Ctrl+V pastes and Ctrl+C and Ctrl+X fill.
    pub fn key(&mut self, p: Press, clipboard: &mut String) -> KeyResult {
        let m = p.mods;
        let used = |changed| KeyResult {
            used: true,
            changed,
            submitted: false,
        };
        let sel_empty = self.cursor == self.anchor;
        if m.ctrl {
            match p.code {
                keys::KEY_A => {
                    self.select_all();
                    return used(false);
                }
                keys::KEY_C if !self.password => {
                    if !sel_empty {
                        *clipboard = self.selected().to_string();
                    }
                    return used(false);
                }
                keys::KEY_X if !self.password => {
                    if sel_empty {
                        return used(false);
                    }
                    *clipboard = self.selected().to_string();
                    self.insert("");
                    return used(true);
                }
                keys::KEY_V => {
                    let text = clipboard.clone();
                    self.insert(&text);
                    return used(!text.is_empty());
                }
                _ => {}
            }
        }
        match p.code {
            keys::KEY_ENTER | keys::KEY_KPENTER => {
                if self.multiline && !m.ctrl {
                    self.insert("\n");
                    return used(true);
                }
                KeyResult {
                    used: true,
                    changed: false,
                    submitted: true,
                }
            }
            keys::KEY_BACKSPACE => {
                if sel_empty {
                    if self.cursor == 0 {
                        return used(false);
                    }
                    self.anchor = if m.ctrl {
                        self.word_before(self.cursor)
                    } else {
                        self.before(self.cursor)
                    };
                }
                self.insert("");
                used(true)
            }
            keys::KEY_DELETE => {
                if sel_empty {
                    if self.cursor == self.text.len() {
                        return used(false);
                    }
                    self.anchor = if m.ctrl {
                        self.word_after(self.cursor)
                    } else {
                        self.after(self.cursor)
                    };
                }
                self.insert("");
                used(true)
            }
            keys::KEY_LEFT => {
                let to = if !sel_empty && !m.shift && !m.ctrl {
                    self.selection().start
                } else if m.ctrl {
                    self.word_before(self.cursor)
                } else {
                    self.before(self.cursor)
                };
                self.move_to(to, m.shift);
                used(false)
            }
            keys::KEY_RIGHT => {
                let to = if !sel_empty && !m.shift && !m.ctrl {
                    self.selection().end
                } else if m.ctrl {
                    self.word_after(self.cursor)
                } else {
                    self.after(self.cursor)
                };
                self.move_to(to, m.shift);
                used(false)
            }
            keys::KEY_HOME => {
                let to = if m.ctrl {
                    0
                } else {
                    self.line_start(self.cursor)
                };
                self.move_to(to, m.shift);
                used(false)
            }
            keys::KEY_END => {
                let to = if m.ctrl {
                    self.text.len()
                } else {
                    self.line_end(self.cursor)
                };
                self.move_to(to, m.shift);
                used(false)
            }
            code => {
                if m.ctrl || m.alt || m.meta {
                    return KeyResult::default();
                }
                match keys::char_for(code, m) {
                    Some(c) => {
                        let mut b = [0u8; 4];
                        self.insert(c.encode_utf8(&mut b));
                        used(true)
                    }
                    None => KeyResult::default(),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::Mods;

    fn press(e: &mut TextEdit, code: u16, m: Mods) -> KeyResult {
        let mut clip = String::new();
        e.key(Press { code, mods: m }, &mut clip)
    }

    const NONE: Mods = Mods {
        shift: false,
        ctrl: false,
        alt: false,
        meta: false,
        caps: false,
    };
    const SHIFT: Mods = Mods {
        shift: true,
        ..NONE
    };
    const CTRL: Mods = Mods { ctrl: true, ..NONE };

    #[test]
    fn types_and_deletes_across_multibyte_characters() {
        let mut e = TextEdit::with_text("åé");
        assert_eq!(e.cursor, 4);
        press(&mut e, keys::KEY_LEFT, NONE);
        assert_eq!(e.cursor, 2);
        let r = press(&mut e, 30, NONE);
        assert!(r.used && r.changed);
        assert_eq!(e.text, "åaé");
        press(&mut e, keys::KEY_BACKSPACE, NONE);
        press(&mut e, keys::KEY_BACKSPACE, NONE);
        assert_eq!(e.text, "é");
        assert_eq!(e.cursor, 0);
        // Nothing left of the cursor: used, but nothing changes.
        assert!(!press(&mut e, keys::KEY_BACKSPACE, NONE).changed);
        press(&mut e, keys::KEY_DELETE, NONE);
        assert_eq!(e.text, "");
    }

    #[test]
    fn selection_extends_and_is_replaced() {
        let mut e = TextEdit::with_text("hello world");
        press(&mut e, keys::KEY_LEFT, SHIFT);
        press(&mut e, keys::KEY_LEFT, SHIFT);
        assert_eq!(e.selected(), "ld");
        // Left without Shift goes to the selection's start and ends it.
        press(&mut e, keys::KEY_LEFT, NONE);
        assert_eq!((e.cursor, e.anchor), (9, 9));
        press(&mut e, keys::KEY_A, CTRL);
        assert_eq!(e.selected(), "hello world");
        press(&mut e, 30, SHIFT);
        assert_eq!(e.text, "A");
    }

    #[test]
    fn words_and_lines() {
        let mut e = TextEdit::with_text("one two_three, four");
        press(&mut e, keys::KEY_LEFT, CTRL);
        assert_eq!(&e.text[e.cursor..], "four");
        press(&mut e, keys::KEY_LEFT, CTRL);
        assert_eq!(&e.text[e.cursor..], "two_three, four");
        press(&mut e, keys::KEY_RIGHT, CTRL);
        assert_eq!(&e.text[e.cursor..], ", four");
        press(&mut e, keys::KEY_BACKSPACE, CTRL);
        assert_eq!(e.text, "one , four");
        press(&mut e, keys::KEY_HOME, NONE);
        assert_eq!(e.cursor, 0);
        press(&mut e, keys::KEY_DELETE, CTRL);
        assert_eq!(e.text, " , four");
        press(&mut e, keys::KEY_END, NONE);
        assert_eq!(e.cursor, e.text.len());

        let mut m = TextEdit::multiline();
        m.insert("ab\ncd");
        press(&mut m, keys::KEY_HOME, NONE);
        assert_eq!(m.cursor, 3);
        press(&mut m, keys::KEY_LEFT, NONE);
        press(&mut m, keys::KEY_HOME, NONE);
        assert_eq!(m.cursor, 0);
        press(&mut m, keys::KEY_END, NONE);
        assert_eq!(m.cursor, 2);
    }

    #[test]
    fn enter_submits_or_breaks_the_line() {
        let mut e = TextEdit::with_text("x");
        let r = press(&mut e, keys::KEY_ENTER, NONE);
        assert!(r.submitted && !r.changed);
        assert_eq!(e.text, "x");
        let mut m = TextEdit::multiline();
        assert!(!press(&mut m, keys::KEY_ENTER, NONE).submitted);
        assert_eq!(m.text, "\n");
        assert!(press(&mut m, keys::KEY_ENTER, CTRL).submitted);
    }

    #[test]
    fn clipboard() {
        let mut clip = String::new();
        let mut e = TextEdit::with_text("cut me");
        e.select_all();
        e.key(
            Press {
                code: keys::KEY_X,
                mods: CTRL,
            },
            &mut clip,
        );
        assert_eq!((e.text.as_str(), clip.as_str()), ("", "cut me"));
        clip = "a\nb".into();
        let r = e.key(
            Press {
                code: keys::KEY_V,
                mods: CTRL,
            },
            &mut clip,
        );
        assert!(r.changed);
        assert_eq!(e.text, "a b"); // No line break in a single-line field.

        let mut p = TextEdit::with_text("secret");
        p.password = true;
        p.select_all();
        clip.clear();
        p.key(
            Press {
                code: keys::KEY_C,
                mods: CTRL,
            },
            &mut clip,
        );
        assert_eq!(clip, "");
    }

    #[test]
    fn keys_that_are_not_for_the_field_pass_through() {
        let mut e = TextEdit::new();
        assert!(!press(&mut e, keys::KEY_TAB, NONE).used);
        assert!(!press(&mut e, keys::KEY_F2, NONE).used);
        assert!(!press(&mut e, keys::KEY_ESC, NONE).used);
        assert!(!press(&mut e, 30, CTRL).used || e.text.is_empty());
    }
}
