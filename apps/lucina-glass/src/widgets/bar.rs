//! A glass bar and the quiet round control that sits in it (the browser's tab strip, toolbar, address pill and dock).

use crate::Paint;
use crate::canvas::{Color, R};
use crate::icons::{self, Icon};
use crate::tokens::{color, space};

use super::ink::{focus_ring, tap};

/// A glass bar: a short, tight shadow so stacked bars stay clean.
pub fn bar(p: &mut Paint, r: R, rad: f32) {
    p.shadow(r, rad, 7.0);
    p.glass(r, rad, color::SMOKE);
}

/// A round control inside a bar: no glass of its own, an icon, a soft accent fill while it is active.
#[derive(Clone, Copy, Debug)]
pub struct BarButton {
    pub icon: Icon,
    /// Pressed or switched on (the menu while its popover is open).
    pub active: bool,
    pub focused: bool,
    /// The smallest hit area each way, dp.
    pub min_hit: f32,
    last: R,
}

impl BarButton {
    pub fn new(icon: Icon) -> BarButton {
        BarButton {
            icon,
            active: false,
            focused: false,
            min_hit: space::TAP,
            last: R::default(),
        }
    }

    /// Draw it as a disc of diameter `d` centred on (`cx`, `cy`), the icon 0.58 of that in `ink`.
    pub fn draw(&mut self, p: &mut Paint, cx: f32, cy: f32, d: f32, ink: Color) {
        self.last = R::new(cx - d / 2.0, cy - d / 2.0, d, d);
        if self.active {
            p.cv.fill_circle(cx, cy, d / 2.0, color::ACCENT_SOFT);
        }
        if self.focused {
            focus_ring(p.cv, self.last, d / 2.0);
        }
        icons::draw(p.cv, self.icon, cx, cy, d * 0.58, ink);
    }

    pub fn hit(&self, x: f32, y: f32) -> bool {
        tap(self.last, self.min_hit, x, y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::Canvas;
    use crate::text::Type;

    #[test]
    fn draws_and_is_hit_in_its_44_dp_area() {
        let mut ty = Type::new();
        let mut cv = Canvas::new(120, 120, 1.0);
        cv.clear(Color::hex(0));
        let mut p = Paint {
            cv: &mut cv,
            ty: &mut ty,
            now: 0.0,
            backdrop: None,
        };
        let mut b = BarButton::new(Icon::Back);
        assert!(!b.hit(60.0, 60.0), "not before it was drawn");
        b.draw(&mut p, 60.0, 60.0, 36.0, color::INK);
        assert!(cv.px.iter().any(|&v| v != 0));
        assert!(b.hit(60.0, 60.0));
        assert!(b.hit(60.0 - 21.9, 60.0), "a 36 dp control is 44 dp to hit");
        assert!(!b.hit(60.0 - 22.1, 60.0));
        b.min_hit = 36.0;
        assert!(
            !b.hit(60.0 - 21.0, 60.0),
            "a pointer layout keeps the drawn size"
        );
    }
}
