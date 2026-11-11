//! A popover: a glass panel that hangs under the control that opened it (Juno, tablet), with its content inside. It opens and
//! closes on a tween of `motion::SHORT`, sliding a few dp down from the control; [`Popover::progress`] is that move as a number
//! for callers that fade or scale something with it (the kit's glass has no global opacity, so the panel itself slides only).

use crate::Paint;
use crate::anim::{Ease, Secs, Tween};
use crate::browser_rules;
use crate::canvas::R;
use crate::tokens::{color, motion};

use super::ink::tap;
use super::lerp_dp;

/// The panel's corner radius, dp.
pub const RADIUS: f32 = 20.0;
/// The room round the content, dp.
pub const PAD: f32 = 10.0;
/// How far the panel slides while opening, dp.
pub const SLIDE: f32 = 8.0;

/// Hundredths of a dp held to the range `browser_rules` takes, whatever the float (`f32::clamp` leaves a NaN as it is and the `as i32` that follows turns it into 0; the tests with NaN inputs pin this).
fn q(v: f32) -> i32 {
    (v * 100.0).round().clamp(
        -(browser_rules::MAX_LEN as f32),
        browser_rules::MAX_LEN as f32,
    ) as i32
}

/// A floating glass panel with its shadow: popovers and sheets.
pub fn panel(p: &mut Paint, r: R, rad: f32) {
    p.shadow(r, rad, 18.0);
    p.glass(r, rad, color::SMOKE);
}

#[derive(Clone, Copy, Debug)]
pub struct Popover {
    /// 0 closed to 1 open.
    open: Tween,
    is_open: bool,
    last: R,
}

impl Default for Popover {
    fn default() -> Self {
        Popover::new()
    }
}

impl Popover {
    pub fn new() -> Popover {
        Popover {
            open: Tween::at_rest(0.0),
            is_open: false,
            last: R::default(),
        }
    }

    /// The panel for `content_h` of content, hanging right-aligned from a control whose right end is at `anchor_right`, `top` dp
    /// below it, `margin` inside the screen (`screen_w` wide): [`browser_rules::popover_x`] keeps it on the screen.
    pub fn place(
        anchor_right: f32,
        top: f32,
        w: f32,
        content_h: f32,
        margin: f32,
        screen_w: f32,
    ) -> R {
        let screen = q(screen_w).max(0);
        // A margin a screen cannot hold on both sides shrinks to half of it; the width is what is left, in hundredths.
        let margin = q(margin).max(0).min(screen / 2);
        let w = q(w).max(0).min(screen - 2 * margin);
        let x = browser_rules::popover_x(q(anchor_right), w, margin, screen);
        R::new(
            x as f32 / 100.0,
            top,
            w as f32 / 100.0,
            content_h + 2.0 * PAD,
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
                .retarget(now, if open { 1.0 } else { 0.0 }, motion::SHORT);
        }
    }

    pub fn is_open(&self) -> bool {
        self.is_open
    }

    /// 0 closed to 1 open at `now`.
    pub fn progress(&self, now: Secs) -> f32 {
        self.open.at(now).clamp(0.0, 1.0)
    }

    /// Whether anything of it shows at `now` (open, or still closing).
    pub fn visible(&self, now: Secs) -> bool {
        self.is_open || !self.open.done(now)
    }

    /// Draw the panel `r` at the open position, slid by the move; returns the content rectangle, or `None` once closed.
    pub fn draw(&mut self, p: &mut Paint, r: R) -> Option<R> {
        if !self.visible(p.now) {
            self.last = R::default();
            return None;
        }
        let t = self.progress(p.now);
        let r = r.offset(0.0, lerp_dp(-SLIDE, 0.0, t));
        self.last = r;
        panel(p, r, RADIUS);
        Some(r.inset(PAD))
    }

    /// Whether (`x`, `y`) is on the panel as last drawn (a tap outside closes it).
    pub fn hit(&self, x: f32, y: f32) -> bool {
        tap(self.last, 0.0, x, y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::{Canvas, Color};
    use crate::text::Type;

    #[test]
    fn stays_on_the_screen_and_right_aligned_where_it_can() {
        let r = Popover::place(1000.0, 100.0, 300.0, 200.0, 12.0, 1366.0);
        assert_eq!((r.right(), r.h), (1000.0, 220.0));
        let left = Popover::place(100.0, 100.0, 300.0, 200.0, 12.0, 1366.0);
        assert_eq!(left.x, 12.0, "kept inside the left margin");
        let right = Popover::place(1500.0, 100.0, 300.0, 200.0, 12.0, 1366.0);
        assert!(
            (right.right() - (1366.0 - 12.0)).abs() < 0.01,
            "kept inside the right margin"
        );
        let narrow = Popover::place(200.0, 0.0, 900.0, 10.0, 12.0, 360.0);
        assert!(
            narrow.x >= 12.0 && narrow.right() <= 348.0 + 0.01,
            "wider than the screen: shrunk to fit"
        );
        let tiny = Popover::place(5.0, 0.0, 300.0, 10.0, 12.0, 10.0);
        assert!(
            tiny.x >= 0.0 && tiny.right() <= 10.0 + 0.01 && tiny.w >= 0.0,
            "a screen under two margins: {tiny:?}"
        );
        let none = Popover::place(f32::NAN, 0.0, f32::NAN, 10.0, -5.0, -3.0);
        assert!(
            none.w == 0.0 && none.x == 0.0,
            "no screen, no panel: {none:?}"
        );
        let huge = Popover::place(1.0e12, 0.0, 300.0, 10.0, 12.0, 1.0e9);
        assert!(huge.x.is_finite() && huge.w == 300.0);
    }

    #[test]
    fn opens_slides_and_closes() {
        let mut ty = Type::new();
        let mut pop = Popover::new();
        let render = |pop: &mut Popover, ty: &mut Type, now: f64| {
            let mut cv = Canvas::new(300, 200, 1.0);
            cv.clear(Color::hex(0));
            let mut p = Paint {
                cv: &mut cv,
                ty,
                now,
                backdrop: None,
            };
            let content = pop.draw(&mut p, R::new(40.0, 40.0, 220.0, 120.0));
            (cv, content)
        };
        let (cv, content) = render(&mut pop, &mut ty, 0.0);
        assert!(
            content.is_none() && cv.px.iter().all(|&v| v == 0),
            "closed draws nothing"
        );
        pop.open(1.0);
        let (_, mid) = render(&mut pop, &mut ty, 1.0 + motion::SHORT * 0.05);
        let mid = mid.expect("opening is visible");
        assert!(mid.y < 50.0, "still above its place, {}", mid.y);
        let (cv, rest) = render(&mut pop, &mut ty, 2.0);
        assert_eq!(rest, Some(R::new(50.0, 50.0, 200.0, 100.0)));
        assert!(cv.px.iter().any(|&v| v != 0));
        assert!(pop.hit(100.0, 100.0) && !pop.hit(10.0, 10.0));
        pop.close(3.0);
        assert!(pop.visible(3.1) && !pop.visible(3.5));
        assert!(render(&mut pop, &mut ty, 4.0).1.is_none());
    }
}
