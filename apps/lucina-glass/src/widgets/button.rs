//! A text button: a verb. The primary one is the kit's light `PAPER` button (the accent stays for selection), the secondary one
//! a quiet fill.

use crate::Paint;
use crate::canvas::R;
use crate::text::Font;
use crate::tokens::{color, radius, space, ty};

use super::ink::{focus_ring, tap, text_centre};

#[derive(Clone, Debug)]
pub struct PushButton {
    pub label: String,
    pub primary: bool,
    pub focused: bool,
    pub min_hit: f32,
    last: R,
}

impl PushButton {
    pub fn new(label: &str, primary: bool) -> PushButton {
        PushButton {
            label: label.to_string(),
            primary,
            focused: false,
            min_hit: space::TAP,
            last: R::default(),
        }
    }

    pub fn draw(&mut self, p: &mut Paint, r: R) {
        self.last = r;
        if self.primary {
            p.cv.fill_rrect(r, radius::MD, color::PAPER);
            text_centre(
                p,
                Font::Semibold,
                ty::LEAD,
                r.cx(),
                r.cy(),
                color::PAPER_INK,
                &self.label,
            );
        } else {
            p.cv.fill_rrect(r, radius::MD, color::INK.alpha(0.12));
            p.cv.stroke_rrect(r, radius::MD, 0.6, color::HAIR);
            text_centre(
                p,
                Font::Semibold,
                ty::LEAD,
                r.cx(),
                r.cy(),
                color::INK,
                &self.label,
            );
        }
        if self.focused {
            focus_ring(p.cv, r, radius::MD);
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

    #[test]
    fn both_kinds_draw_and_the_focus_ring_adds_ink_outside() {
        let mut ty = Type::new();
        let mut count = [0usize; 3];
        for (i, (primary, focused)) in [(true, false), (false, false), (true, true)]
            .into_iter()
            .enumerate()
        {
            let mut cv = Canvas::new(200, 100, 1.0);
            cv.clear(Color::hex(0));
            let mut p = Paint {
                cv: &mut cv,
                ty: &mut ty,
                now: 0.0,
                backdrop: None,
            };
            let mut b = PushButton::new("Allow", primary);
            b.focused = focused;
            b.draw(&mut p, R::new(40.0, 30.0, 120.0, 40.0));
            count[i] = cv.px.iter().filter(|&&v| v != 0).count();
            assert!(b.hit(100.0, 50.0) && !b.hit(10.0, 50.0));
        }
        assert!(count.iter().all(|&n| n > 0));
        assert!(
            count[2] > count[0],
            "the focus ring draws outside the button"
        );
    }
}
