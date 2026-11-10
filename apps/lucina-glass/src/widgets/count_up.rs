//! A number that counts up to its value ("76%", "51%"): the shown value goes through [`rules::count_up`], so it never passes
//! the target and lands on it exactly. The digits are right-aligned, so the number grows leftwards without jitter.

use crate::anim::{Ease, Secs, phase, unit};
use crate::canvas::Color;
use crate::rules;
use crate::text::Font;
use crate::Paint;

#[derive(Clone)]
pub struct CountUp {
    pub target: u32,
    /// Where the count starts: 0 on entrance, the value shown before on an update.
    pub base: u32,
    pub start: Secs,
    pub dur: Secs,
    pub ease: Ease,
    /// Drawn after the number, smaller ("%").
    pub suffix: String,
}

impl CountUp {
    pub fn new(target: u32, start: Secs, suffix: &str) -> CountUp {
        CountUp { target, base: 0, start, dur: 1.2, ease: Ease::OutQuint, suffix: suffix.into() }
    }

    /// Count again from 0 to `target`, starting `now`.
    pub fn restart(&mut self, now: Secs, target: u32) {
        self.base = 0;
        self.target = target;
        self.start = now;
    }

    /// Count on from what is shown at `now` to `target` (down at once if it fell).
    pub fn retarget(&mut self, now: Secs, target: u32) {
        let shown = self.value(now);
        self.base = if target >= shown { shown } else { target };
        self.target = target;
        self.start = now;
    }

    /// Whether the count has landed.
    pub fn done(&self, now: Secs) -> bool {
        now >= self.start + self.dur
    }

    /// The value shown at `now`.
    pub fn value(&self, now: Secs) -> u32 {
        let base = self.base.min(self.target);
        base + rules::count_up(self.target - base, unit(self.ease.at(phase(now, self.start, self.dur))))
    }

    /// Draw ending at `right`, baseline `y`: the number in `font` at `size`, the suffix at `size * 0.5` beside its top.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(&self, p: &mut Paint, right: f32, y: f32, font: Font, size: f32, c: Color) {
        let v = self.value(p.now).to_string();
        let suf_size = (size * 0.42).max(9.0);
        let sw = if self.suffix.is_empty() { 0.0 } else { p.measure(Font::Light, (suf_size, 0.0), &self.suffix) + 2.0 };
        p.text_right(font, (size, -size * 0.03), right - sw, y, c, &v);
        if !self.suffix.is_empty() {
            let cap = p.ty.cap_height(font, size);
            let s = self.suffix.clone();
            p.text(Font::Light, (suf_size, 0.0), right - sw + 2.0, y - cap + p.ty.cap_height(Font::Light, suf_size), c, &s);
        }
    }
}
