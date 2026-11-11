//! A switch: on is the accent (selection and activity are the one accent). The thumb slides between the track's ends on a
//! tween of `motion::SHORT`; the track colour follows it. Where the thumb may be is [`browser_rules::thumb_ends`].

use crate::Paint;
use crate::anim::{Ease, Secs, Tween};
use crate::browser_rules;
use crate::canvas::R;
use crate::tokens::{color, motion, space};

use super::ink::{focus_ring, tap};
use super::lerp_dp;

/// Hundredths of a dp held to the range `browser_rules` takes, whatever the float (`f32::clamp` leaves a NaN as it is and the `as i32` that follows turns it into 0; the tests with NaN inputs pin this).
fn q(v: f32) -> i32 {
    (v * 100.0).round().clamp(
        -(browser_rules::MAX_LEN as f32),
        browser_rules::MAX_LEN as f32,
    ) as i32
}

/// The track, dp.
pub const W: f32 = 46.0;
pub const H: f32 = 28.0;
/// The thumb's diameter and its margin in the track.
const THUMB: f32 = 22.0;
const PAD: f32 = 3.0;

#[derive(Clone, Copy, Debug)]
pub struct Switch {
    on: bool,
    /// 0 off to 1 on.
    flip: Tween,
    pub focused: bool,
    pub min_hit: f32,
    last: R,
}

impl Switch {
    pub fn new(on: bool) -> Switch {
        Switch {
            on,
            flip: Tween::at_rest(if on { 1.0 } else { 0.0 }),
            focused: false,
            min_hit: space::TAP,
            last: R::default(),
        }
    }

    pub fn is_on(&self) -> bool {
        self.on
    }

    /// Turn it on or off at `now`.
    pub fn set(&mut self, now: Secs, on: bool) {
        if on != self.on {
            self.on = on;
            self.flip.ease = Ease::STANDARD;
            self.flip
                .retarget(now, if on { 1.0 } else { 0.0 }, motion::SHORT);
        }
    }

    pub fn toggle(&mut self, now: Secs) {
        self.set(now, !self.on);
    }

    pub fn settled(&self, now: Secs) -> bool {
        self.flip.done(now)
    }

    /// Draw it with its right edge at `right`, centred on `cy`.
    pub fn draw(&mut self, p: &mut Paint, right: f32, cy: f32) {
        let r = R::new(right - W, cy - H / 2.0, W, H);
        self.last = r;
        let t = self.flip.at(p.now).clamp(0.0, 1.0);
        let off = color::INK.alpha(0.16);
        p.cv.fill_rrect(r, H / 2.0, off.mix(color::ACCENT, t));
        if t < 1.0 {
            p.cv.stroke_rrect(r, H / 2.0, 0.8, color::HAIR.alpha(1.0 - t));
        }
        let (from, to) = browser_rules::thumb_ends(q(r.x), q(W), q(THUMB), q(PAD));
        let left = lerp_dp(from as f32 / 100.0, to as f32 / 100.0, t);
        let tx = left + THUMB / 2.0;
        p.cv.fill_rrect_soft(
            R::new(tx - 11.0, cy - 9.0, 22.0, 22.0),
            11.0,
            4.0,
            color::SHADOW,
        );
        p.cv.fill_circle(tx, cy, THUMB / 2.0, color::INK);
        if self.focused {
            focus_ring(p.cv, r, H / 2.0);
        }
    }

    pub fn hit(&self, x: f32, y: f32) -> bool {
        tap(self.last, self.min_hit, x, y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::{Canvas, Color};
    use crate::text::Type;

    fn frame(sw: &mut Switch, now: f64) -> Canvas {
        let mut ty = Type::new();
        let mut cv = Canvas::new(120, 60, 1.0);
        cv.clear(Color::hex(0));
        let mut p = Paint {
            cv: &mut cv,
            ty: &mut ty,
            now,
            backdrop: None,
        };
        sw.draw(&mut p, 100.0, 30.0);
        cv
    }

    /// The thumb's centre: the mean x of the white pixels on the centre line.
    fn thumb_cx(cv: &Canvas) -> f32 {
        let xs: Vec<usize> = (0..120)
            .filter(|&x| cv.px[30 * 120 + x] & 0xffffff == 0xffffff)
            .collect();
        assert!(!xs.is_empty(), "no thumb");
        xs.iter().sum::<usize>() as f32 / xs.len() as f32 + 0.5
    }

    #[test]
    fn rests_on_the_two_ends_and_slides_between() {
        let mut sw = Switch::new(false);
        let off = frame(&mut sw, 0.0);
        assert!(
            (thumb_cx(&off) - 68.0).abs() < 1.0,
            "off: thumb at the left, {}",
            thumb_cx(&off)
        );
        sw.toggle(1.0);
        assert!(!sw.settled(1.05));
        let early = thumb_cx(&frame(&mut sw, 1.0 + motion::SHORT * 0.1));
        assert!(early > 68.5 && early < 86.0, "mid-flip: {early}");
        let on = frame(&mut sw, 2.0);
        assert!((thumb_cx(&on) - 86.0).abs() < 1.0, "on: thumb at the right");
        assert!(sw.is_on() && sw.settled(2.0));
        assert_eq!(
            on.px[30 * 120 + 56] & 0xffffff,
            color::ACCENT.xrgb(),
            "on: the track is the accent"
        );
        // And back: it starts from where the thumb is.
        sw.toggle(3.0);
        assert!(thumb_cx(&frame(&mut sw, 3.0)) > 85.0);
        assert!((thumb_cx(&frame(&mut sw, 4.0)) - 68.0).abs() < 1.0);
    }

    #[test]
    fn far_off_screen_or_not_a_number_still_draws_without_overflow() {
        for right in [1.0e12, -1.0e12, f32::NAN, f32::INFINITY] {
            let mut sw = Switch::new(true);
            let mut ty = Type::new();
            let mut cv = Canvas::new(40, 40, 1.0);
            let mut p = Paint {
                cv: &mut cv,
                ty: &mut ty,
                now: 0.0,
                backdrop: None,
            };
            sw.draw(&mut p, right, 20.0);
        }
    }

    #[test]
    fn hit_and_focus() {
        let mut sw = Switch::new(true);
        let plain = frame(&mut sw, 0.0);
        assert!(
            sw.hit(100.0 - 23.0, 30.0) && sw.hit(80.0, 30.0 + 21.0) && sw.hit(80.0, 30.0 - 21.0),
            "the 46 x 28 track is 46 x 44 to hit"
        );
        assert!(!sw.hit(80.0, 30.0 + 22.5));
        assert!(!sw.hit(100.0 - 47.0, 30.0));
        sw.focused = true;
        let ringed = frame(&mut sw, 0.0);
        assert!(
            ringed.px.iter().zip(&plain.px).any(|(a, b)| a != b),
            "the focus ring draws"
        );
    }
}
