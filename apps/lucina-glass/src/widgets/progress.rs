//! "Phase 2 of 3": a pill-and-dot track. Phases done are full pills, the current one a pill filling up with a dot at its
//! head, the ones to come small dots. Moving to the next phase grows its dot into a pill while the fill runs on.

use crate::anim::{Ease, Secs, Tween};
use crate::canvas::R;
use crate::tokens::{color, motion, radius};
use crate::Paint;

use super::lerp_dp;

#[derive(Clone)]
pub struct ProgressTrack {
    pub phases: u32,
    /// 1-based.
    pub phase: u32,
    /// The current phase's fill, 0..1.
    pub fill: Tween,
    /// 0..1: how far the current phase's slot has grown from a dot into a pill.
    grow: Tween,
}

const DOT: f32 = 6.0;
const GAP: f32 = 5.0;

impl ProgressTrack {
    pub fn new(phases: u32, phase: u32, fill: f32) -> ProgressTrack {
        ProgressTrack { phases: phases.max(1), phase: phase.clamp(1, phases.max(1)), fill: Tween::at_rest(fill), grow: Tween::at_rest(1.0) }
    }

    /// Fill the current phase to `to` from wherever it is now.
    pub fn set_fill(&mut self, now: Secs, to: f32) {
        self.fill.ease = Ease::STANDARD;
        self.fill.retarget(now, to.clamp(0.0, 1.0), motion::MEDIUM * 2.0);
    }

    /// Go on to the next phase: the old one completes, the next dot grows into a pill.
    pub fn advance(&mut self, now: Secs) {
        if self.phase < self.phases {
            self.phase += 1;
            self.fill = Tween::new(0.0, 0.0, now, 0.0, Ease::Linear);
            self.grow = Tween::new(0.0, 1.0, now, motion::MEDIUM, Ease::OutBack(1.2));
        }
    }

    /// A replay of the entrance: fill from 0.
    pub fn replay(&mut self, now: Secs, to: f32) {
        self.fill = Tween::new(0.0, to, now, 1.1, Ease::STANDARD);
    }

    pub fn draw(&self, p: &mut Paint, r: R) {
        let now = p.now;
        let h = DOT;
        let y = r.cy() - h / 2.0;
        let dots_after = self.phases - self.phase;
        let grow = self.grow.at(now).max(0.0);
        // The current slot is a dot that grew; the pills share what the dots leave.
        let pills = self.phase as f32 - 1.0 + grow;
        let free = r.w - dots_after as f32 * (DOT + GAP) - (self.phase as f32 - 1.0) * GAP;
        let pill_w = if pills > 0.0 { (free / pills.max(1.0)).max(DOT) } else { DOT };
        let mut x = r.x;
        for i in 0..self.phases {
            if i + 1 < self.phase {
                p.cv.fill_rrect(R::new(x, y, pill_w, h), radius::PILL, color::INK);
                x += pill_w + GAP;
            } else if i + 1 == self.phase {
                let w = lerp_dp(DOT, pill_w, grow);
                let slot = R::new(x, y, w.max(DOT), h);
                p.cv.fill_rrect(slot, radius::PILL, color::HAIR);
                let f = self.fill.at(now).clamp(0.0, 1.0);
                let fw = lerp_dp(h, slot.w, f);
                p.cv.fill_rrect(R::new(x, y, fw, h), radius::PILL, color::INK);
                // The head: a brighter dot at the end of the fill.
                p.cv.fill_circle(x + fw - h / 2.0, y + h / 2.0, h / 2.0 + 1.6, color::INK);
                p.cv.fill_circle(x + fw - h / 2.0, y + h / 2.0, 1.6, color::ACCENT_FILL);
                x += slot.w + GAP;
            } else {
                p.cv.fill_circle(x + DOT / 2.0, y + h / 2.0, DOT / 2.0, color::INK_3);
                x += DOT + GAP;
            }
        }
    }
}
