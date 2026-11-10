//! Pager dots 1-5: the active page is a wide accent pill with its number; moving to another page hands the width from the
//! leaving dot to the arriving one through [`rules::dot_widths`], so the row's total width never changes while they morph.

use crate::anim::{Ease, Secs, Tween};
use crate::canvas::R;
use crate::rules;
use crate::text::Font;
use crate::tokens::{color, motion, radius};
use crate::Paint;

use super::baseline_for;

#[derive(Clone)]
pub struct PagerDots {
    pub n: usize,
    pub active: usize,
    from: usize,
    t: Tween,
    last: R,
}

/// Widths in hundredths of a dp, for the proved rule.
const SMALL: u32 = 1800;
const BIG: u32 = 3800;
const GAP: f32 = 5.0;
const H: f32 = 18.0;

impl PagerDots {
    pub fn new(n: usize, active: usize) -> PagerDots {
        PagerDots { n: n.max(1), active, from: active, t: Tween::at_rest(1.0), last: R::default() }
    }

    pub fn go(&mut self, now: Secs, i: usize) {
        if i < self.n && i != self.active {
            self.from = self.active;
            self.active = i;
            self.t = Tween::new(0.0, 1.0, now, motion::MEDIUM, Ease::STANDARD);
        }
    }

    /// The row's width (constant).
    pub fn width(&self) -> f32 {
        ((self.n as u32 - 1) * SMALL + BIG) as f32 / 100.0 + GAP * (self.n as f32 - 1.0)
    }

    pub fn hit(&self, x: f32, y: f32) -> Option<usize> {
        // At least 48 dp tall to the finger.
        if !self.last.inset_xy(0.0, -15.0).contains(x, y) {
            return None;
        }
        let f = (x - self.last.x) / self.last.w * self.n as f32;
        Some((f as usize).min(self.n - 1))
    }

    pub fn draw(&mut self, p: &mut Paint, x: f32, cy: f32) {
        self.last = R::new(x, cy - H / 2.0, self.width(), H);
        let t = self.t.at(p.now).clamp(0.0, 1.0);
        let (leave, arrive) = rules::dot_widths(SMALL, BIG, crate::anim::unit(t));
        let mut px = x;
        for i in 0..self.n {
            let w = if i == self.active && i == self.from {
                BIG
            } else if i == self.active {
                arrive
            } else if i == self.from {
                leave
            } else {
                SMALL
            } as f32
                / 100.0;
            let heat = ((w * 100.0 - SMALL as f32) / (BIG - SMALL) as f32).clamp(0.0, 1.0);
            let r = R::new(px, cy - H / 2.0, w, H);
            p.cv.fill_rrect(r, radius::PILL, color::CARBON_HI.mix(color::ACCENT_FILL, heat));
            if heat < 0.5 {
                p.cv.stroke_rrect(r, radius::PILL, 0.6, color::HAIR);
            }
            let by = baseline_for(p, Font::Mono, 8.5, cy);
            p.text_centered(Font::Mono, (8.5, 0.0), r.cx(), by, color::INK_2.mix(color::INK, heat), &(i + 1).to_string());
            px += w + GAP;
        }
    }
}
