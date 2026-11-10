//! An action: a round glass button with an icon over a one-word label (docs/design/rules.md, rule 2; the Custom Cast
//! study's "icon over a short label"). Hovered, its rim brightens and it lifts a little; pressed, it shrinks and fills with
//! the accent, then eases back on release. The button is 44 dp, the smallest tap target.

use crate::anim::{Ease, Secs, Tween};
use crate::canvas::{Color, R};
use crate::icons::{self, Icon};
use crate::text::Font;
use crate::tokens::{color, motion, radius, space, ty};
use crate::Paint;

#[derive(Clone)]
pub struct ActionButton {
    pub icon: Icon,
    pub label: String,
    hover: Tween,
    press: Tween,
    last: R,
}

impl ActionButton {
    pub fn new(icon: Icon, label: &str) -> ActionButton {
        ActionButton { icon, label: label.into(), hover: Tween::at_rest(0.0), press: Tween::at_rest(0.0), last: R::default() }
    }

    pub fn set_hover(&mut self, now: Secs, on: bool) {
        let to = if on { 1.0 } else { 0.0 };
        if self.hover.to != to {
            self.hover.ease = Ease::STANDARD;
            self.hover.retarget(now, to, motion::SHORT);
        }
    }

    pub fn press(&mut self, now: Secs) {
        self.press = Tween::new(self.press.at(now), 1.0, now, motion::PRESS, Ease::OutCubic);
    }

    pub fn release(&mut self, now: Secs) {
        self.press = Tween::new(self.press.at(now), 0.0, now, motion::MEDIUM, Ease::OutBack(1.6));
    }

    pub fn hit(&self, x: f32, y: f32) -> bool {
        self.last.contains(x, y)
    }

    /// Its size: the button and the label under it.
    pub const fn size() -> (f32, f32) {
        (space::TAP + 20.0, space::TAP + 20.0)
    }

    /// Draw centred on `cx`, the button's top at `y`; returns the area it answers to.
    pub fn draw(&mut self, p: &mut Paint, cx: f32, y: f32) -> R {
        let (w, h) = Self::size();
        self.last = R::new(cx - w / 2.0, y, w, h);
        let hv = self.hover.at(p.now).clamp(0.0, 1.0);
        let pr = self.press.at(p.now).clamp(-0.3, 1.2);
        let rad = space::TAP / 2.0 * (1.0 + 0.04 * hv - 0.08 * pr);
        let cy = y + space::TAP / 2.0;
        let r = R::new(cx - rad, cy - rad, rad * 2.0, rad * 2.0);
        p.glass(r, radius::PILL, color::GLASS_TINT.mix(Color::hexa(0xffffff, 60), 0.4 * hv));
        if pr > 0.01 {
            p.cv.fill_circle(cx, cy, rad, color::ACCENT_FILL.alpha(pr.min(1.0)));
        }
        if hv > 0.01 {
            p.cv.stroke_circle(cx, cy, rad - 0.4, 0.8, color::INK.alpha(0.5 * hv));
        }
        icons::draw(p.cv, self.icon, cx, cy, 19.0, color::INK);
        let label = self.label.clone();
        p.text_centered(Font::Semibold, ty::SMALL, cx, y + space::TAP + 15.0, color::INK, &label);
        self.last
    }
}
