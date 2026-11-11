//! A stepper: a label, then minus, the value and plus ("Zoom  -  110%  +"). Each step is [`browser_rules::step_value`]; a button
//! is off (faint) at the end of the range it has reached.

use crate::Paint;
use crate::browser_rules;
use crate::canvas::R;
use crate::icons::{self, Icon};
use crate::text::Font;
use crate::tokens::{color, space};

use super::ink::{focus_ring, tap, text_centre, text_left};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepperPart {
    Minus,
    Plus,
}

#[derive(Clone, Debug)]
pub struct Stepper {
    pub label: String,
    value: i32,
    lo: i32,
    hi: i32,
    step: i32,
    /// Written after the value ("%").
    pub unit: &'static str,
    /// Which button has the keyboard focus.
    pub focused: Option<StepperPart>,
    pub min_hit: f32,
    minus: R,
    plus: R,
}

impl Stepper {
    pub fn new(
        label: &str,
        value: i32,
        lo: i32,
        hi: i32,
        step: i32,
        unit: &'static str,
    ) -> Stepper {
        let (lo, hi) = (lo.min(hi), lo.max(hi));
        Stepper {
            label: label.to_string(),
            value: value.clamp(lo, hi),
            lo,
            hi,
            step: step.max(0),
            unit,
            focused: None,
            min_hit: space::TAP,
            minus: R::default(),
            plus: R::default(),
        }
    }

    pub fn value(&self) -> i32 {
        self.value
    }

    /// One step up or down; the value stays in its range.
    pub fn press(&mut self, part: StepperPart) {
        self.value = browser_rules::step_value(
            self.value,
            self.lo,
            self.hi,
            self.step,
            part == StepperPart::Plus,
        );
    }

    pub fn can(&self, part: StepperPart) -> bool {
        browser_rules::can_step(self.value, self.lo, self.hi, part == StepperPart::Plus)
    }

    /// Draw it in the row `row`; the buttons are discs of diameter `d`, the text `main`.
    pub fn draw(&mut self, p: &mut Paint, row: R, main: (f32, f32), d: f32) {
        text_left(
            p,
            Font::Regular,
            main,
            row.x + 12.0,
            row.cy(),
            color::INK,
            &self.label,
        );
        let plus = row.right() - 4.0 - d / 2.0;
        let val = plus - d / 2.0 - 34.0;
        let minus = val - 34.0 - d / 2.0;
        for (cx, part, icon) in [
            (minus, StepperPart::Minus, Icon::Minus),
            (plus, StepperPart::Plus, Icon::Plus),
        ] {
            let disc = R::new(cx - d / 2.0, row.cy() - d / 2.0, d, d);
            match part {
                StepperPart::Minus => self.minus = disc,
                StepperPart::Plus => self.plus = disc,
            }
            let live = self.can(part);
            p.cv.fill_circle(
                cx,
                row.cy(),
                d / 2.0,
                color::INK.alpha(if live { 0.10 } else { 0.05 }),
            );
            icons::draw(
                p.cv,
                icon,
                cx,
                row.cy(),
                16.0,
                if live { color::INK } else { color::INK_3 },
            );
            if self.focused == Some(part) {
                focus_ring(p.cv, disc, d / 2.0);
            }
        }
        text_centre(
            p,
            Font::Semibold,
            main,
            val,
            row.cy(),
            color::INK,
            &format!("{}{}", self.value, self.unit),
        );
    }

    /// The button under (`x`, `y`) as last drawn; an off button still answers (the caller's press then changes nothing).
    pub fn hit(&self, x: f32, y: f32) -> Option<StepperPart> {
        if tap(self.plus, self.min_hit, x, y) {
            Some(StepperPart::Plus)
        } else if tap(self.minus, self.min_hit, x, y) {
            Some(StepperPart::Minus)
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

    #[test]
    fn steps_stay_in_range_and_buttons_go_off_at_the_ends() {
        let mut s = Stepper::new("Zoom", 100, 50, 150, 10, "%");
        s.press(StepperPart::Plus);
        assert_eq!(s.value(), 110);
        for _ in 0..20 {
            s.press(StepperPart::Plus);
        }
        assert_eq!(s.value(), 150);
        assert!(!s.can(StepperPart::Plus) && s.can(StepperPart::Minus));
        for _ in 0..20 {
            s.press(StepperPart::Minus);
        }
        assert_eq!(s.value(), 50);
        assert!(!s.can(StepperPart::Minus));
        assert_eq!(
            Stepper::new("Zoom", 900, 50, 150, 10, "%").value(),
            150,
            "an out-of-range start is clamped"
        );
    }

    #[test]
    fn draws_and_tells_its_buttons_apart() {
        let mut ty = Type::new();
        let mut cv = Canvas::new(300, 80, 1.0);
        cv.clear(Color::hex(0));
        let mut p = Paint {
            cv: &mut cv,
            ty: &mut ty,
            now: 0.0,
            backdrop: None,
        };
        let mut s = Stepper::new("Zoom", 110, 50, 300, 10, "%");
        s.draw(&mut p, R::new(10.0, 20.0, 280.0, 40.0), (15.0, 0.0), 36.0);
        assert!(cv.px.iter().any(|&v| v != 0));
        let plus_cx = 10.0 + 280.0 - 4.0 - 18.0;
        assert_eq!(s.hit(plus_cx, 40.0), Some(StepperPart::Plus));
        assert_eq!(
            s.hit(plus_cx - 34.0 - 34.0 - 36.0, 40.0),
            Some(StepperPart::Minus)
        );
        assert_eq!(s.hit(20.0, 40.0), None);
    }
}
