//! A modal dialog card: the page's origin as the title ("shop.example.net says"), the page's own words as the message, and two
//! buttons (Cancel, OK as the engine answers), on the thick material over a dimmed page. The origin is the title so that the
//! message, which is the page's, cannot pass for the browser's own words.

use crate::Paint;
use crate::browser_rules;
use crate::canvas::R;
use crate::text::Font;
use crate::tokens::{color, radius, ty};

use super::button::PushButton;
use super::ink::{FocusOrder, text_left};

/// Hundredths of a dp held to the range `browser_rules` takes, whatever the float (`f32::clamp` leaves a NaN as it is and the `as i32` that follows turns it into 0; the tests with NaN inputs pin this).
fn q(v: f32) -> i32 {
    (v * 100.0).round().clamp(
        -(browser_rules::MAX_LEN as f32),
        browser_rules::MAX_LEN as f32,
    ) as i32
}

/// The card's height: padding, title, message and the button row, dp.
pub const HEIGHT: f32 = 24.0 + 16.0 + 12.0 + 26.0 + 22.0 + 48.0 + 16.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialogPart {
    Cancel,
    Ok,
}

#[derive(Clone, Debug)]
pub struct DialogCard {
    pub origin: String,
    pub message: String,
    cancel: PushButton,
    ok: PushButton,
    /// Tab moves between the two buttons.
    pub focus: FocusOrder,
}

impl DialogCard {
    pub fn new(origin: &str, message: &str, cancel: &str, ok: &str) -> DialogCard {
        DialogCard {
            origin: origin.to_string(),
            message: message.to_string(),
            cancel: PushButton::new(cancel, false),
            ok: PushButton::new(ok, true),
            focus: FocusOrder::new(2),
        }
    }

    /// The card `w` wide centred in `area` (the page below the bars), `lift` dp (not below: a negative one is none) above the middle: wholly inside it
    /// ([`browser_rules::centre`]).
    pub fn place(area: R, w: f32, lift: f32) -> R {
        // Lengths in hundredths, the card no longer than the area, so `centre`'s `len <= outer` holds for every input.
        let (ow, oh) = (q(area.w).max(0), q(area.h).max(0));
        let (w, h) = (q(w).max(0).min(ow), q(HEIGHT).min(oh));
        let x = browser_rules::centre(q(area.x), ow, w);
        let y = browser_rules::centre(q(area.y), oh, h);
        R::new(
            x as f32 / 100.0,
            (y as f32 / 100.0 - lift.max(0.0)).max(area.y),
            w as f32 / 100.0,
            h as f32 / 100.0,
        )
    }

    pub fn draw(&mut self, p: &mut Paint, r: R) {
        self.cancel.focused = self.focus.is(0);
        self.ok.focused = self.focus.is(1);
        p.shadow(r, radius::XL, 24.0);
        p.glass(r, radius::XL, color::MATERIAL);
        text_left(
            p,
            Font::Semibold,
            ty::CAPTION,
            r.x + 24.0,
            r.y + 32.0,
            color::INK_2,
            &self.origin,
        );
        text_left(
            p,
            Font::Semibold,
            ty::HEADLINE,
            r.x + 24.0,
            r.y + 72.0,
            color::INK,
            &self.message,
        );
        let by = r.bottom() - 16.0 - 48.0;
        let half = (r.w - 48.0 - 10.0) / 2.0;
        self.cancel.draw(p, R::new(r.x + 24.0, by, half, 48.0));
        self.ok
            .draw(p, R::new(r.x + 24.0 + half + 10.0, by, half, 48.0));
    }

    /// The button under (`x`, `y`) as last drawn.
    pub fn hit(&self, x: f32, y: f32) -> Option<DialogPart> {
        if self.ok.hit(x, y) {
            Some(DialogPart::Ok)
        } else if self.cancel.hit(x, y) {
            Some(DialogPart::Cancel)
        } else {
            None
        }
    }

    /// The button the keyboard focus is on.
    pub fn focused(&self) -> Option<DialogPart> {
        match self.focus.current() {
            Some(0) => Some(DialogPart::Cancel),
            Some(1) => Some(DialogPart::Ok),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::{Canvas, Color};
    use crate::text::Type;

    #[test]
    fn sits_in_the_middle_of_the_page() {
        let area = R::new(0.0, 100.0, 1000.0, 600.0);
        let r = DialogCard::place(area, 360.0, 0.0);
        assert_eq!((r.cx(), r.cy()), (500.0, 400.0));
        let small = DialogCard::place(R::new(0.0, 0.0, 200.0, 100.0), 360.0, 50.0);
        assert!(
            small.x >= 0.0 && small.right() <= 200.0 && small.y >= 0.0 && small.bottom() <= 100.0,
            "a small window holds it"
        );
        let none = DialogCard::place(R::new(f32::NAN, 1.0e12, -5.0, f32::NAN), 360.0, 0.0);
        assert!(
            none.w == 0.0 && none.h == 0.0,
            "an area with no size holds a card with none: {none:?}"
        );
        let down = DialogCard::place(area, 360.0, -500.0);
        assert_eq!(
            down,
            DialogCard::place(area, 360.0, 0.0),
            "a negative lift is none, so the card stays inside its area"
        );
        let huge = DialogCard::place(R::new(0.0, 0.0, 1.0e9, 1.0e9), 360.0, 0.0);
        assert!(huge.x.is_finite() && huge.w == 360.0);
    }

    #[test]
    fn draws_buttons_that_hit_and_a_focus_that_moves() {
        let mut ty = Type::new();
        let mut d = DialogCard::new(
            "shop.example.net says",
            "Delete this address?",
            "Cancel",
            "OK",
        );
        let r = R::new(20.0, 20.0, 320.0, HEIGHT);
        let draw = |d: &mut DialogCard, ty: &mut Type| {
            let mut cv = Canvas::new(360, 220, 1.0);
            cv.clear(Color::hex(0));
            let mut p = Paint {
                cv: &mut cv,
                ty,
                now: 0.0,
                backdrop: None,
            };
            d.draw(&mut p, r);
            cv
        };
        let a = draw(&mut d, &mut ty);
        assert!(a.px.iter().any(|&v| v != 0));
        let by = r.bottom() - 16.0 - 24.0;
        assert_eq!(d.hit(r.x + 24.0 + 10.0, by), Some(DialogPart::Cancel));
        assert_eq!(d.hit(r.right() - 24.0 - 10.0, by), Some(DialogPart::Ok));
        assert_eq!(d.hit(r.x + 24.0 + 10.0, r.y + 30.0), None);
        assert_eq!(d.focused(), None);
        d.focus.step(false);
        d.focus.step(false);
        assert_eq!(d.focused(), Some(DialogPart::Ok));
        let b = draw(&mut d, &mut ty);
        assert_ne!(a.px, b.px, "the focus ring draws");
    }
}
