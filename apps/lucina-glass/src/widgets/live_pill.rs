//! The red "LIVE!" / "NOW!" pill: a breathing dot inside and a halo that swells and fades behind it, once per period.

use crate::anim::{Ease, Secs, phase, pulse};
use crate::canvas::{Color, R};
use crate::text::Font;
use crate::tokens::{color, motion, radius};
use crate::Paint;

use super::baseline_for;

#[derive(Clone)]
pub struct LivePill {
    pub label: String,
    pub color: Color,
    /// When it went live: the pulse runs from here.
    pub since: Secs,
    /// Whether it breathes (off: a still pill, for "pending" states).
    pub live: bool,
}

const TYPE: (f32, f32) = (11.0, 0.2);
const H: f32 = 20.0;

impl LivePill {
    pub fn new(label: &str, since: Secs) -> LivePill {
        LivePill { label: label.into(), color: color::LIVE, since, live: true }
    }

    /// Its size in dp.
    pub fn size(&self, p: &mut Paint) -> (f32, f32) {
        let tw = p.measure(Font::Semibold, TYPE, &self.label);
        (tw + 26.0, H)
    }

    /// Draw with its top-left at (`x`, `y`); returns its rectangle.
    pub fn draw(&self, p: &mut Paint, x: f32, y: f32) -> R {
        let (w, h) = self.size(p);
        let r = R::new(x, y, w, h);
        let t = (p.now - self.since).max(0.0);
        if self.live {
            // The halo: swells 7 dp and fades over the first 70 % of each period.
            let cycle = (t / motion::PULSE).fract() * motion::PULSE;
            let k = phase(cycle, 0.0, motion::PULSE * 0.7);
            let grow = Ease::OutCubic.at(k) * 7.0;
            p.cv.fill_rrect(r.inset(-grow), radius::PILL, self.color.alpha(0.42 * (1.0 - k)));
        }
        p.cv.gradient_rrect(r, radius::PILL, self.color.mix(Color::hex(0xffffff), 0.08), self.color.mix(color::LIVE_DEEP, 0.6));
        let breath = if self.live { pulse(t, motion::PULSE) } else { 1.0 };
        p.cv.fill_circle(x + 10.0, y + h / 2.0, 2.6 + 0.5 * breath, color::INK.alpha(0.55 + 0.45 * breath));
        let by = baseline_for(p, Font::Semibold, TYPE.0, y + h / 2.0);
        p.text(Font::Semibold, TYPE, x + 17.0, by, color::INK, &self.label.clone());
        r
    }
}
