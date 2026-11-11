//! A bottom sheet (the phone's popover): a glass panel resting 8 dp above the bottom edge with a grab handle, over a dimmed
//! page. It slides up from below the screen on a tween of `motion::MEDIUM`. Where it sits is [`browser_rules::sheet_top`], which
//! keeps it wholly on the screen.

use crate::Paint;
use crate::anim::{Ease, Secs, Tween};
use crate::browser_rules;
use crate::canvas::R;
use crate::tokens::{color, motion, radius};

use super::lerp_dp;
use super::popover::panel;

/// Hundredths of a dp held to the range `browser_rules` takes, whatever the float (`f32::clamp` leaves a NaN as it is and the `as i32` that follows turns it into 0; the tests with NaN inputs pin this).
fn q(v: f32) -> i32 {
    (v * 100.0).round().clamp(
        -(browser_rules::MAX_LEN as f32),
        browser_rules::MAX_LEN as f32,
    ) as i32
}

/// The room above the content (the handle's) and below it (the gesture inset's), dp.
const TOP: f32 = 24.0;
const BELOW: f32 = 12.0 + 30.0;
/// The gap between the sheet and the screen's edges, dp.
pub const MARGIN: f32 = 8.0;

#[derive(Clone, Copy, Debug)]
pub struct BottomSheet {
    /// 0 closed (below the screen) to 1 open.
    open: Tween,
    is_open: bool,
}

impl Default for BottomSheet {
    fn default() -> Self {
        BottomSheet::new()
    }
}

impl BottomSheet {
    pub fn new() -> BottomSheet {
        BottomSheet {
            open: Tween::at_rest(0.0),
            is_open: false,
        }
    }

    /// The sheet for `content_h` of content on a `screen_w` x `screen_h` screen.
    pub fn place(screen_w: f32, screen_h: f32, content_h: f32) -> R {
        let screen = q(screen_h).max(0);
        // The gap below the sheet is 8 dp, or the whole screen if it is smaller; the sheet is what is left, in hundredths.
        let margin = q(MARGIN).min(screen);
        let total = q(content_h + TOP + BELOW).max(0).min(screen - margin);
        let top = browser_rules::sheet_top(screen, margin, total);
        R::new(
            MARGIN,
            top as f32 / 100.0,
            (screen_w - 2.0 * MARGIN).max(0.0),
            total as f32 / 100.0,
        )
    }

    pub fn open(&mut self, now: Secs) {
        self.set(now, true);
    }

    pub fn close(&mut self, now: Secs) {
        self.set(now, false);
    }

    fn set(&mut self, now: Secs, open: bool) {
        if open != self.is_open {
            self.is_open = open;
            self.open.ease = Ease::STANDARD;
            self.open
                .retarget(now, if open { 1.0 } else { 0.0 }, motion::MEDIUM);
        }
    }

    pub fn is_open(&self) -> bool {
        self.is_open
    }

    /// 0 closed to 1 open at `now`: the dim behind the sheet follows it.
    pub fn progress(&self, now: Secs) -> f32 {
        self.open.at(now).clamp(0.0, 1.0)
    }

    pub fn visible(&self, now: Secs) -> bool {
        self.is_open || !self.open.done(now)
    }

    /// Draw the sheet `r` (from [`place`](Self::place)) slid by its move, with its handle; returns the content rectangle.
    pub fn draw(&self, p: &mut Paint, r: R, screen_h: f32) -> Option<R> {
        if !self.visible(p.now) {
            return None;
        }
        let t = self.progress(p.now);
        let hidden = screen_h - r.y;
        let r = r.offset(0.0, lerp_dp(hidden, 0.0, t));
        panel(p, r, radius::XL);
        p.cv.fill_rrect(
            R::new(r.cx() - 18.0, r.y + 8.0, 36.0, 5.0),
            2.5,
            color::HAIR,
        );
        Some(R::new(
            r.x + 16.0,
            r.y + TOP,
            (r.w - 32.0).max(0.0),
            (r.h - TOP - BELOW).max(0.0),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::{Canvas, Color};
    use crate::text::Type;

    #[test]
    fn rests_8_dp_above_the_bottom_and_never_leaves_the_screen() {
        let r = BottomSheet::place(360.0, 800.0, 300.0);
        assert_eq!((r.x, r.w, r.h), (8.0, 344.0, 366.0));
        assert_eq!(r.bottom(), 792.0);
        let tall = BottomSheet::place(360.0, 800.0, 5000.0);
        assert!(
            tall.y >= 0.0 && tall.bottom() <= 792.0 + 0.01,
            "a sheet taller than the screen is cut to it"
        );
        let tiny = BottomSheet::place(5.0, 5.0, 50.0);
        assert!(
            tiny.y >= 0.0 && tiny.h >= 0.0 && tiny.w >= 0.0 && tiny.bottom() <= 5.0 + 0.01,
            "a screen under 8 dp: {tiny:?}"
        );
        let none = BottomSheet::place(-1.0, f32::NAN, 10.0);
        assert!(none.y >= 0.0 && none.h == 0.0, "{none:?}");
        // f32 rounding of an awkward height never pushes the sheet past the margin.
        let odd = BottomSheet::place(360.0, 799.99, 300.004);
        assert!(odd.bottom() <= 799.99 - 8.0 + 0.0051, "{odd:?}");
    }

    #[test]
    fn slides_up_from_below() {
        let mut ty = Type::new();
        let mut sh = BottomSheet::new();
        let r = BottomSheet::place(120.0, 200.0, 60.0);
        let render = |sh: &BottomSheet, ty: &mut Type, now: f64| {
            let mut cv = Canvas::new(120, 200, 1.0);
            cv.clear(Color::hex(0));
            let mut p = Paint {
                cv: &mut cv,
                ty,
                now,
                backdrop: None,
            };
            (sh.draw(&mut p, r, 200.0), cv)
        };
        assert!(render(&sh, &mut ty, 0.0).0.is_none());
        sh.open(1.0);
        let (c0, _) = render(&sh, &mut ty, 1.0);
        assert!(c0.unwrap().y > 199.0 - 1.0, "starts below the screen");
        let (c, cv) = render(&sh, &mut ty, 2.0);
        let c = c.unwrap();
        assert_eq!((c.x, c.w, c.h), (24.0, 72.0, 60.0));
        assert_eq!(c.y, r.y + 24.0);
        assert!(cv.px.iter().any(|&v| v != 0));
        sh.close(3.0);
        assert!(!sh.visible(3.5));
    }
}
