//! A download's progress bar: a thin hairline track with the accent filling it. The fill is
//! [`browser_rules::fill_width`] of the fraction [`browser_rules::fraction`] makes of bytes done and bytes expected, so it is
//! never wider than the track, empty at nothing and the whole track when done; it moves to a new value on a tween of
//! `motion::SHORT`.

use crate::Paint;
use crate::anim::{Ease, Secs, Tween, unit};
use crate::browser_rules;
use crate::canvas::R;
use crate::tokens::{color, motion};

#[derive(Clone, Copy, Debug)]
pub struct ProgressBar {
    frac: Tween,
}

impl ProgressBar {
    /// A bar at `done` of `total` bytes (an unknown total, 0, is an empty bar).
    pub fn new(done: u64, total: u64) -> ProgressBar {
        ProgressBar {
            frac: Tween::at_rest(
                browser_rules::fraction(done, total) as f32 / browser_rules::UNIT as f32,
            ),
        }
    }

    /// Move to `done` of `total` from wherever the fill is now.
    pub fn set(&mut self, now: Secs, done: u64, total: u64) {
        self.frac.ease = Ease::STANDARD;
        self.frac.retarget(
            now,
            browser_rules::fraction(done, total) as f32 / browser_rules::UNIT as f32,
            motion::SHORT,
        );
    }

    /// The fraction shown at `now`, 0..1.
    pub fn fraction(&self, now: Secs) -> f32 {
        self.frac.at(now).clamp(0.0, 1.0)
    }

    pub fn settled(&self, now: Secs) -> bool {
        self.frac.done(now)
    }

    /// Draw it in `r` (a few dp tall).
    pub fn draw(&self, p: &mut Paint, r: R) {
        let rad = r.h / 2.0;
        p.cv.fill_rrect(r, rad, color::HAIR);
        let w = browser_rules::fill_width(
            (r.w * 100.0).round().max(0.0) as u32,
            unit(self.fraction(p.now)),
        );
        if w > 0 {
            p.cv.fill_rrect(R::new(r.x, r.y, w as f32 / 100.0, r.h), rad, color::ACCENT);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::{Canvas, Color};
    use crate::text::Type;

    fn filled(bar: &ProgressBar, now: f64) -> usize {
        let mut ty = Type::new();
        let mut cv = Canvas::new(120, 20, 1.0);
        cv.clear(Color::hex(0));
        let mut p = Paint {
            cv: &mut cv,
            ty: &mut ty,
            now,
            backdrop: None,
        };
        bar.draw(&mut p, R::new(10.0, 8.0, 100.0, 4.0));
        (0..120)
            .filter(|&x| cv.px[10 * 120 + x] & 0xffffff == color::ACCENT.xrgb())
            .count()
    }

    #[test]
    fn fills_in_proportion_and_never_past_the_track() {
        // The rounded ends are anti-aliased, so the pure-accent pixels on the centre line are a pixel or two short of the width.
        assert_eq!(filled(&ProgressBar::new(0, 100), 0.0), 0);
        let half = filled(&ProgressBar::new(50, 100), 0.0);
        assert!((half as i32 - 50).abs() <= 2, "half: {half}");
        let full = filled(&ProgressBar::new(100, 100), 0.0);
        assert!((98..=100).contains(&full), "full: {full}");
        assert_eq!(
            filled(&ProgressBar::new(500, 100), 0.0),
            full,
            "more than expected is capped"
        );
        assert_eq!(
            filled(&ProgressBar::new(5, 0), 0.0),
            0,
            "an unknown total is empty"
        );
    }

    #[test]
    fn moves_to_a_new_value_over_short() {
        let mut b = ProgressBar::new(10, 100);
        b.set(1.0, 90, 100);
        assert_eq!(
            filled(&b, 1.0),
            filled(&ProgressBar::new(10, 100), 0.0),
            "starts where it was"
        );
        let mid = filled(&b, 1.0 + motion::SHORT * 0.3);
        assert!(mid > 10 && mid < 90, "{mid}");
        assert!(b.settled(1.3));
        assert!((filled(&b, 1.3) as i32 - 90).abs() <= 2);
    }
}
