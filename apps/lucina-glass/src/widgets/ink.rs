//! What the browser controls share: the three inks they draw with, text placed on a centre line, the keyboard focus ring and
//! order, and a control's hit area as last drawn (the proved rules of `browser_rules`).
//!
//! Hit testing and focus, for every control of this set:
//! - a control remembers the rectangle it was last drawn in; `hit(x, y)` answers for that rectangle grown to at least
//!   `min_hit` each way (the 44 dp tap target of Diana, `space::TAP`, by default) by [`browser_rules::tap`], left and top edges in,
//!   right and bottom out. A pointer layout whose neighbours touch sets `min_hit` to the drawn size;
//! - a control has `focused`: it draws the Home's focus ring (the accent, 1.6 dp, 3 dp outside) when set. Which control has it is
//!   a [`FocusOrder`] over the screen's controls, moved by [`browser_rules::focus_step`].
//!
//! Animation convention: a control that moves holds an [`crate::anim::Tween`] or [`crate::anim::Spring`] read at
//! [`Paint::now`] and changed by a call that takes the time it happens at, as every widget of the kit does; a rested control draws
//! exactly what a static one would.

use crate::Paint;
use crate::browser_rules;
use crate::canvas::{Canvas, Color, R};
use crate::text::Font;
use crate::tokens::color;

use super::baseline_for;

/// The three inks a control draws with: main, secondary and the faint one for disabled marks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    pub ink: Color,
    pub ink2: Color,
    pub ink3: Color,
}

impl Palette {
    /// The kit's inks on dark glass and the thick material.
    pub const STANDARD: Palette = Palette {
        ink: color::INK,
        ink2: color::INK_2,
        ink3: color::INK_3,
    };
}

/// Text left at `x`, centred on `cy`; returns its width.
#[allow(clippy::too_many_arguments)]
pub fn text_left(
    p: &mut Paint,
    f: Font,
    st: (f32, f32),
    x: f32,
    cy: f32,
    c: Color,
    s: &str,
) -> f32 {
    let by = baseline_for(p, f, st.0, cy);
    p.text(f, st, x, by, c, s)
}

/// Text centred on (`cx`, `cy`).
pub fn text_centre(p: &mut Paint, f: Font, st: (f32, f32), cx: f32, cy: f32, c: Color, s: &str) {
    let by = baseline_for(p, f, st.0, cy);
    p.text_centered(f, st, cx, by, c, s);
}

/// Text ending at `right`, centred on `cy`.
pub fn text_right(p: &mut Paint, f: Font, st: (f32, f32), right: f32, cy: f32, c: Color, s: &str) {
    let by = baseline_for(p, f, st.0, cy);
    p.text_right(f, st, right, by, c, s);
}

/// The keyboard focus ring of the Home: the accent, 1.6 dp, 3 dp outside `r`, whose corners have radius `rad`.
pub fn focus_ring(cv: &mut Canvas, r: R, rad: f32) {
    cv.stroke_rrect(r.inset(-3.0), rad + 3.0, 1.6, color::ACCENT);
}

/// A hairline from `x0` to `x1` at `y`.
pub fn divider(p: &mut Paint, x0: f32, x1: f32, y: f32) {
    p.cv.fill_rrect(R::new(x0, y, x1 - x0, 0.7), 0.0, color::HAIR);
}

/// Hundredths of a dp, held to the range the rules take.
fn q(v: f32) -> i32 {
    ((v * 100.0).round() as i64).clamp(
        -(browser_rules::MAX_LEN as i64),
        browser_rules::MAX_LEN as i64,
    ) as i32
}

/// Whether the point (`x`, `y`) lands on a control drawn in `drawn`, whose hit area is at least `min` each way. A rectangle with
/// no size (a control never drawn) is hit nowhere.
pub fn tap(drawn: R, min: f32, x: f32, y: f32) -> bool {
    if drawn.w <= 0.0 || drawn.h <= 0.0 {
        return false;
    }
    // A negative or NaN `min_hit` is no growth (`f32::max` returns the other side for a NaN).
    let min = min.max(0.0);
    browser_rules::tap(
        q(drawn.x),
        q(drawn.y),
        q(drawn.w),
        q(drawn.h),
        q(min).max(0),
        q(x),
        q(y),
    )
}

/// The order the keyboard focus moves through a screen's controls: `Tab` and `Shift+Tab` wrap round it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FocusOrder {
    n: u32,
    cur: u32,
    /// Whether a control has the focus at all (nothing has it until the first key).
    active: bool,
}

impl FocusOrder {
    pub fn new(n: u32) -> FocusOrder {
        FocusOrder {
            n: n.min(browser_rules::MAX_FOCUS),
            cur: 0,
            active: false,
        }
    }

    /// The controls in the order changed (a tab closed): the focus stays on its control if it is still there.
    pub fn set_len(&mut self, n: u32) {
        self.n = n.min(browser_rules::MAX_FOCUS);
        self.cur = browser_rules::focus_clamp(self.cur, self.n);
        self.active &= self.n > 0;
    }

    /// Move on (or back); the first move lands on the first (or last) control.
    pub fn step(&mut self, back: bool) -> Option<u32> {
        if self.n == 0 {
            return None;
        }
        self.cur = if self.active {
            browser_rules::focus_step(self.cur, self.n, back)
        } else if back {
            self.n - 1
        } else {
            0
        };
        self.active = true;
        Some(self.cur)
    }

    pub fn set(&mut self, i: u32) {
        if i < self.n {
            self.cur = i;
            self.active = true;
        }
    }

    pub fn clear(&mut self) {
        self.active = false;
    }

    /// The focused control, if any.
    pub fn current(&self) -> Option<u32> {
        if self.active && self.n > 0 {
            Some(self.cur)
        } else {
            None
        }
    }

    pub fn is(&self, i: u32) -> bool {
        self.current() == Some(i)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tap_grows_small_controls_to_the_minimum() {
        let r = R::new(100.0, 100.0, 20.0, 20.0);
        assert!(tap(r, 44.0, 100.0, 100.0));
        assert!(
            tap(r, 44.0, 90.0, 130.0),
            "12 dp left of a 20 dp control is inside its 44 dp area"
        );
        assert!(!tap(r, 44.0, 87.9, 110.0), "past the 44 dp area");
        assert!(
            !tap(r, 20.0, 99.9, 110.0),
            "a pointer layout keeps the drawn size"
        );
        assert!(tap(r, 20.0, 119.9, 119.9));
        assert!(!tap(r, 20.0, 120.0, 110.0), "the right edge is out");
        assert!(
            !tap(R::default(), 44.0, 0.0, 0.0),
            "a control never drawn is hit nowhere"
        );
        // A negative or non-numeric `min_hit` is no growth: the drawn rectangle only.
        assert!(tap(r, -1.0, 110.0, 110.0) && !tap(r, -1.0, 99.9, 110.0));
        assert!(tap(r, f32::NAN, 100.0, 100.0) && !tap(r, f32::NAN, 99.9, 100.0));
    }

    #[test]
    fn focus_wraps_and_clamps() {
        let mut f = FocusOrder::new(3);
        assert_eq!(f.current(), None);
        assert_eq!(f.step(false), Some(0));
        assert_eq!(f.step(false), Some(1));
        assert_eq!(f.step(false), Some(2));
        assert_eq!(f.step(false), Some(0), "wraps forwards");
        assert_eq!(f.step(true), Some(2), "wraps backwards");
        f.set_len(2);
        assert_eq!(
            f.current(),
            Some(1),
            "the control that is gone hands the focus to the last"
        );
        f.set_len(0);
        assert_eq!(f.current(), None);
        assert_eq!(f.step(false), None);
        let mut b = FocusOrder::new(4);
        assert_eq!(
            b.step(true),
            Some(3),
            "the first Shift+Tab lands on the last control"
        );
        assert!(b.is(3));
        b.clear();
        assert!(!b.is(3));
    }
}
