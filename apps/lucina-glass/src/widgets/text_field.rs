//! A text field with a caret: a pill with a search mark, the typed text, an accent caret that blinks and a clear button, ringed
//! in the Home's focus ring while it has the keys. The text scrolls to keep the caret in view
//! ([`browser_rules::scroll_for_caret`]), the caret keeps still while a key is being pressed and blinks after
//! ([`browser_rules::caret_visible`]), and the arrow keys stop at both ends of the text ([`browser_rules::caret_step`]).
//!
//! Text goes through the kit's [`crate::text::Type`] (`Paint::measure`, `Paint::text`): no second glyph cache. The field edits
//! characters, not bytes; composing input and selection are the shell's text service's job, not this widget's.

use crate::Paint;
use crate::anim::Secs;
use crate::browser_rules;
use crate::canvas::R;
use crate::icons::{self, Icon};
use crate::text::Font;
use crate::tokens::{color, radius, space, ty};

use super::bar::BarButton;
use super::ink::{Palette, focus_ring, tap, text_left};

/// Hundredths of a dp held to the range `browser_rules` takes, whatever the float (`f32::clamp` leaves a NaN as it is and the `as i32` that follows turns it into 0; the tests with NaN inputs pin this).
fn q(v: f32) -> i32 {
    (v * 100.0).round().clamp(
        -(browser_rules::MAX_LEN as f32),
        browser_rules::MAX_LEN as f32,
    ) as i32
}

/// The longest text offset `browser_rules::scroll_for_caret` takes, dp; beyond it the field scrolls with the same rule in floats.
const LIMIT_DP: f32 = browser_rules::MAX_LEN as f32 / 100.0;

/// How long the caret takes to blink once, ms.
pub const BLINK_MS: u64 = 1060;

/// Where the pieces of a field sit, dp.
#[derive(Clone, Copy, Debug)]
pub struct FieldMetrics {
    /// The search mark's centre from the field's left edge, and its size.
    pub mark_x: f32,
    pub mark: f32,
    /// The text's left edge from the field's left edge.
    pub text_x: f32,
    pub main: (f32, f32),
    /// The caret's width and height.
    pub caret_w: f32,
    pub caret_h: f32,
    /// The clear button's diameter and its centre from the field's right edge.
    pub clear_d: f32,
    pub clear_x: f32,
    /// Whether the field has its own faint fill and outline (a field in a bar), or is itself the bar's glass (the phone's pill).
    pub filled: bool,
}

impl FieldMetrics {
    /// A toolbar field with a control of diameter `ctl` (36 with a pointer, 44 on a touch screen).
    pub fn bar(ctl: f32, main: (f32, f32)) -> FieldMetrics {
        FieldMetrics {
            mark_x: ctl / 2.0 + 2.0,
            mark: 20.0,
            text_x: ctl + 6.0,
            main,
            caret_w: 1.6,
            caret_h: main.0 * 1.2,
            clear_d: ctl,
            clear_x: ctl / 2.0 + 2.0,
            filled: true,
        }
    }

    /// The phone's address pill.
    pub fn pill(main: (f32, f32)) -> FieldMetrics {
        FieldMetrics {
            mark_x: 26.0,
            mark: 22.0,
            text_x: 50.0,
            main,
            caret_w: 1.8,
            caret_h: 20.0,
            clear_d: space::TAP,
            clear_x: 26.0,
            filled: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldPart {
    Clear,
    Field,
}

#[derive(Clone, Debug)]
pub struct TextField {
    text: String,
    /// In characters, 0 to the text's length.
    caret: usize,
    /// How far the text is scrolled left, dp.
    scroll: f32,
    pub focused: bool,
    last_input: Secs,
    pub min_hit: f32,
    last: R,
    text_area: R,
    main: (f32, f32),
    clear: BarButton,
}

impl TextField {
    pub fn new(text: &str) -> TextField {
        TextField {
            text: text.to_string(),
            caret: text.chars().count(),
            scroll: 0.0,
            focused: false,
            last_input: 0.0,
            min_hit: space::TAP,
            last: R::default(),
            text_area: R::default(),
            main: ty::LEAD,
            clear: BarButton::new(Icon::Close),
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// The caret's place, in characters.
    pub fn caret(&self) -> usize {
        self.caret
    }

    fn len(&self) -> usize {
        self.text.chars().count()
    }

    fn byte(&self, chars: usize) -> usize {
        self.text
            .char_indices()
            .nth(chars)
            .map(|(i, _)| i)
            .unwrap_or(self.text.len())
    }

    /// Give the field the keys: the caret shows at once.
    pub fn focus(&mut self, now: Secs) {
        self.focused = true;
        self.last_input = now;
    }

    pub fn blur(&mut self) {
        self.focused = false;
    }

    /// Type `s` at the caret.
    pub fn insert(&mut self, now: Secs, s: &str) {
        let at = self.byte(self.caret);
        self.text.insert_str(at, s);
        self.caret += s.chars().count();
        self.last_input = now;
    }

    /// Delete the character before the caret.
    pub fn backspace(&mut self, now: Secs) {
        if self.caret > 0 {
            let (a, b) = (self.byte(self.caret - 1), self.byte(self.caret));
            self.text.replace_range(a..b, "");
            self.caret -= 1;
        }
        self.last_input = now;
    }

    /// Delete the character after the caret.
    pub fn delete(&mut self, now: Secs) {
        if self.caret < self.len() {
            let (a, b) = (self.byte(self.caret), self.byte(self.caret + 1));
            self.text.replace_range(a..b, "");
        }
        self.last_input = now;
    }

    /// An arrow key: one character left or right.
    pub fn move_caret(&mut self, now: Secs, back: bool) {
        self.caret = browser_rules::caret_step(self.caret as u32, self.len() as u32, back) as usize;
        self.last_input = now;
    }

    /// Empty the field (the clear button).
    pub fn clear_text(&mut self, now: Secs) {
        self.text.clear();
        self.caret = 0;
        self.scroll = 0.0;
        self.last_input = now;
    }

    /// Put the caret where the point at `x` (dp, on screen) is, as last drawn: the character edge nearest to it.
    pub fn click(&mut self, p: &mut Paint, now: Secs, x: f32) {
        let rel = x - self.text_area.x + self.scroll;
        let mut best = (0usize, f32::MAX);
        for i in 0..=self.len() {
            let prefix: String = self.text.chars().take(i).collect();
            let w = p.measure(Font::Regular, self.main, &prefix);
            if (w - rel).abs() < best.1 {
                best = (i, (w - rel).abs());
            }
        }
        self.caret = best.0;
        self.last_input = now;
    }

    /// Whether the caret is lit at `now`.
    pub fn caret_lit(&self, now: Secs) -> bool {
        let ms = ((now - self.last_input).max(0.0) * 1000.0) as u64;
        browser_rules::caret_visible(ms, BLINK_MS)
    }

    /// Draw it in `r`: `pal` for the inks, `m` for the metrics.
    pub fn draw(&mut self, p: &mut Paint, r: R, m: &FieldMetrics, pal: &Palette) {
        self.last = r;
        self.main = m.main;
        let cy = r.cy();
        if m.filled {
            p.cv.fill_rrect(r, radius::PILL, pal.ink.alpha(0.09));
        }
        if self.focused {
            focus_ring(p.cv, r, r.h / 2.0);
        } else if m.filled {
            p.cv.stroke_rrect(r, radius::PILL, 0.7, pal.ink.alpha(0.16));
        }
        icons::draw(p.cv, Icon::Search, r.x + m.mark_x, cy, m.mark, pal.ink2);
        let tx = r.x + m.text_x;
        let clear_cx = r.right() - m.clear_x;
        let room_right = if self.focused || !self.text.is_empty() {
            clear_cx - m.clear_d / 2.0
        } else {
            r.right() - 8.0
        };
        self.text_area = R::new(tx, r.y, (room_right - tx).max(1.0), r.h);
        // Keep the caret in view: the rule works in hundredths of a dp.
        let before: String = self.text.chars().take(self.caret).collect();
        let caret_x = p.measure(Font::Regular, m.main, &before);
        let view = self.text_area.w - 2.0;
        self.scroll = if caret_x < LIMIT_DP && self.scroll < LIMIT_DP {
            let s = browser_rules::scroll_for_caret(
                q(self.scroll).max(0),
                q(caret_x).max(0),
                q(view).max(1),
            );
            s as f32 / 100.0
        } else {
            // Past the length the proved rule takes (10,486 dp, some 800 characters), the same rule in floats.
            if caret_x < self.scroll {
                caret_x
            } else if caret_x > self.scroll + view {
                caret_x - view
            } else {
                self.scroll
            }
        };
        p.cv.push_clip(self.text_area, 0.0);
        text_left(
            p,
            Font::Regular,
            m.main,
            tx - self.scroll,
            cy,
            pal.ink,
            &self.text,
        );
        if self.focused && self.caret_lit(p.now) {
            let cx = tx - self.scroll + caret_x + 1.0;
            p.cv.fill_rrect(
                R::new(cx, cy - m.caret_h / 2.0, m.caret_w, m.caret_h),
                m.caret_w / 2.0,
                color::ACCENT,
            );
        }
        p.cv.pop_clip();
        if self.focused || !self.text.is_empty() {
            self.clear.min_hit = self.min_hit;
            self.clear.draw(p, clear_cx, cy, m.clear_d, pal.ink2);
        }
    }

    /// What is under (`x`, `y`) as last drawn: the clear button (when shown), else the field.
    pub fn hit(&self, x: f32, y: f32) -> Option<FieldPart> {
        if (self.focused || !self.text.is_empty()) && self.clear.hit(x, y) {
            Some(FieldPart::Clear)
        } else if tap(self.last, self.min_hit, x, y) {
            Some(FieldPart::Field)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::{Canvas, Color};
    use crate::text::Type;

    fn draw(f: &mut TextField, ty: &mut Type, now: f64, w: f32) -> Canvas {
        let mut cv = Canvas::new(400, 80, 1.0);
        cv.clear(Color::hex(0));
        let mut p = Paint {
            cv: &mut cv,
            ty,
            now,
            backdrop: None,
        };
        f.draw(
            &mut p,
            R::new(20.0, 20.0, w, 40.0),
            &FieldMetrics::bar(40.0, ty::LEAD),
            &Palette::STANDARD,
        );
        cv
    }

    #[test]
    fn editing_moves_the_caret_by_characters() {
        let mut f = TextField::new("lucna");
        assert_eq!(f.caret(), 5);
        f.move_caret(0.0, true);
        f.move_caret(0.0, true);
        assert_eq!(f.caret(), 3);
        f.insert(0.0, "i");
        assert_eq!((f.text(), f.caret()), ("lucina", 4));
        f.backspace(0.0);
        assert_eq!((f.text(), f.caret()), ("lucna", 3));
        f.delete(0.0);
        assert_eq!(f.text(), "luca");
        for _ in 0..10 {
            f.move_caret(0.0, true);
        }
        assert_eq!(f.caret(), 0, "stops at the start");
        f.backspace(0.0);
        assert_eq!(f.text(), "luca", "nothing before the first character");
        for _ in 0..10 {
            f.move_caret(0.0, false);
        }
        assert_eq!(f.caret(), 4, "stops at the end");
        f.delete(0.0);
        assert_eq!(f.text(), "luca");
        // Multibyte characters count as one.
        let mut g = TextField::new("æøå");
        g.move_caret(0.0, true);
        g.backspace(0.0);
        assert_eq!((g.text(), g.caret()), ("æå", 1));
        g.insert(0.0, "日本");
        assert_eq!((g.text(), g.caret()), ("æ日本å", 3));
        g.clear_text(1.0);
        assert_eq!((g.text(), g.caret()), ("", 0));
    }

    #[test]
    fn caret_shows_after_a_key_then_blinks() {
        let mut f = TextField::new("a");
        f.focus(1.0);
        assert!(f.caret_lit(1.0));
        assert!(f.caret_lit(1.5));
        assert!(!f.caret_lit(1.0 + 0.531), "dark in the second half");
        assert!(f.caret_lit(1.0 + 1.06), "lit again after a period");
        f.insert(3.0, "b");
        assert!(f.caret_lit(3.0), "a key press shows it");
    }

    #[test]
    fn draws_the_caret_only_while_focused_and_the_ring_around() {
        let mut ty = Type::new();
        let mut f = TextField::new("lucina");
        let idle = draw(&mut f, &mut ty, 0.0, 300.0);
        f.focus(0.0);
        let focused = draw(&mut f, &mut ty, 0.0, 300.0);
        assert_ne!(idle.px, focused.px);
        // The accent caret is somewhere in the text area.
        let accent = color::ACCENT.xrgb();
        assert!(
            focused.px.contains(&accent),
            "an accent pixel: ring or caret"
        );
        let dark = draw(&mut f, &mut ty, 0.8, 300.0);
        assert_ne!(
            focused.px, dark.px,
            "the caret is out in the second half of the blink"
        );
    }

    #[test]
    fn long_text_scrolls_to_keep_the_caret_in_the_field() {
        let mut ty = Type::new();
        let mut f = TextField::new(&"lucina design notes ".repeat(6));
        f.focus(0.0);
        let cv = draw(&mut f, &mut ty, 0.0, 200.0);
        assert!(f.scroll > 0.0, "scrolled by {}", f.scroll);
        // Nothing of the text spills beyond the field's right end.
        assert!(
            (0..80).all(|y| (226..400).all(|x| cv.px[y * 400 + x] == 0)),
            "spill"
        );
        // The caret moves to the start: the scroll comes back.
        for _ in 0..200 {
            f.move_caret(0.0, true);
        }
        let _ = draw(&mut f, &mut ty, 0.0, 200.0);
        assert_eq!(f.scroll, 0.0);
    }

    #[test]
    fn text_longer_than_the_rules_range_still_keeps_the_caret_in_view() {
        let mut ty = Type::new();
        let mut f = TextField::new(&"W".repeat(1500));
        f.focus(0.0);
        let _ = draw(&mut f, &mut ty, 0.0, 200.0);
        let end = {
            let mut cv = Canvas::new(10, 10, 1.0);
            let mut p = Paint {
                cv: &mut cv,
                ty: &mut ty,
                now: 0.0,
                backdrop: None,
            };
            p.measure(Font::Regular, ty::LEAD, &"W".repeat(1500))
        };
        assert!(end > LIMIT_DP, "the test text is {end} dp, past the range");
        assert!(
            f.scroll.is_finite() && f.scroll > LIMIT_DP && f.scroll <= end,
            "scroll {}",
            f.scroll
        );
        assert!(
            end - f.scroll <= f.text_area.w,
            "the caret at {end} is in the {} dp window from {}",
            f.text_area.w,
            f.scroll
        );
    }

    #[test]
    fn clicking_places_the_caret_and_the_clear_button_is_a_part() {
        let mut ty = Type::new();
        let mut f = TextField::new("lucina");
        f.focus(0.0);
        let mut cv = Canvas::new(400, 80, 1.0);
        cv.clear(Color::hex(0));
        {
            let mut p = Paint {
                cv: &mut cv,
                ty: &mut ty,
                now: 0.0,
                backdrop: None,
            };
            f.draw(
                &mut p,
                R::new(20.0, 20.0, 300.0, 40.0),
                &FieldMetrics::bar(40.0, ty::LEAD),
                &Palette::STANDARD,
            );
            f.click(&mut p, 0.0, f.text_area.x + 0.5);
            assert_eq!(f.caret(), 0);
            f.click(&mut p, 0.0, 400.0);
            assert_eq!(f.caret(), 6);
        }
        assert_eq!(f.hit(20.0 + 300.0 - 22.0, 40.0), Some(FieldPart::Clear));
        assert_eq!(f.hit(120.0, 40.0), Some(FieldPart::Field));
        assert_eq!(f.hit(10.0, 5.0), None);
        let empty = TextField::new("");
        assert_eq!(empty.hit(100.0, 40.0), None, "never drawn");
    }

    #[test]
    fn the_pill_has_no_fill_of_its_own() {
        let mut ty = Type::new();
        let draw_with = |m: FieldMetrics, ty: &mut Type| {
            let mut cv = Canvas::new(400, 80, 1.0);
            cv.clear(Color::hex(0));
            let mut p = Paint {
                cv: &mut cv,
                ty,
                now: 0.0,
                backdrop: None,
            };
            TextField::new("").draw(
                &mut p,
                R::new(20.0, 20.0, 300.0, 48.0),
                &m,
                &Palette::STANDARD,
            );
            cv.px.iter().filter(|&&v| v != 0).count()
        };
        let bar = draw_with(FieldMetrics::bar(40.0, ty::LEAD), &mut ty);
        let pill = draw_with(FieldMetrics::pill(ty::LEAD), &mut ty);
        assert!(
            pill > 0 && pill * 10 < bar,
            "the pill draws only its search mark: {pill} against {bar}"
        );
    }
}
