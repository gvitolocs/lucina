//! Mon Tue Wed Thu Fri: a row of chips with one selected in the accent (rule 10: accent is what is selected). Choosing another morphs the selection across: its
//! leading edge runs ahead on a stiff spring and its trailing edge follows on a softer one, so the red pill stretches over the
//! chips in between and contracts on arrival. Labels under the pill turn white as it covers them.

use crate::anim::{Secs, Spring};
use crate::canvas::{Color, R};
use crate::text::Font;
use crate::tokens::{color, radius};
use crate::Paint;

use super::{baseline_for, cell_x};

#[derive(Clone)]
pub struct DayChips {
    pub labels: Vec<String>,
    pub selected: usize,
    /// The pill's two edges, in chip indices.
    lead: Spring,
    trail: Spring,
    last: R,
}

const GAP: f32 = 6.0;
const TYPE: (f32, f32) = (12.0, 0.1);

impl DayChips {
    pub fn new(labels: &[&str], selected: usize) -> DayChips {
        let s = selected as f32;
        DayChips {
            labels: labels.iter().map(|l| l.to_string()).collect(),
            selected,
            lead: Spring::new(s, 520.0, 38.0),
            trail: Spring::new(s, 210.0, 26.0),
            last: R::default(),
        }
    }

    pub fn select(&mut self, now: Secs, i: usize) {
        if i < self.labels.len() {
            self.selected = i;
            self.lead.set_target(now, i as f32);
            self.trail.set_target(now, i as f32);
        }
    }

    pub fn hit(&self, x: f32, y: f32) -> Option<usize> {
        // At least 48 dp tall to the finger, even where the chip is smaller.
        let grow = ((48.0 - self.last.h) / 2.0).max(0.0);
        if !self.last.inset_xy(0.0, -grow).contains(x, y) {
            return None;
        }
        let n = self.labels.len() as u32;
        (0..n).find(|&i| {
            let (cx, cw) = cell_x(self.last, n, GAP, i, 1);
            x >= cx && x < cx + cw
        }).map(|i| i as usize)
    }

    pub fn settled(&self, now: Secs) -> bool {
        self.lead.settled(now) && self.trail.settled(now)
    }

    pub fn draw(&mut self, p: &mut Paint, r: R) {
        self.last = r;
        let n = self.labels.len().max(1) as u32;
        // Chips.
        for i in 0..n {
            let (x, w) = cell_x(r, n, GAP, i, 1);
            let c = R::new(x, r.y, w, r.h);
            p.glass(c, radius::PILL, Color::hexa(0x0c0e14, 120));
        }
        // The pill: from the lower edge to the higher one, each edge interpolated between chip positions.
        let a = self.lead.at(p.now);
        let b = self.trail.at(p.now);
        let (lo, hi) = (a.min(b), a.max(b));
        let left = edge_x(r, n, lo, false);
        let right = edge_x(r, n, hi, true);
        let pill = R::new(left, r.y, (right - left).max(r.h), r.h);
        p.cv.fill_rrect_soft(pill.offset(0.0, 3.0), radius::PILL, 8.0, color::ACCENT.alpha(0.4));
        p.cv.gradient_rrect(pill, radius::PILL, color::ACCENT.mix(Color::hex(0xffffff), 0.06), color::ACCENT_FILL);
        for i in 0..n {
            let (x, w) = cell_x(r, n, GAP, i, 1);
            let label = self.labels[i as usize].clone();
            let inside = (x + w / 2.0 >= pill.x) && (x + w / 2.0 <= pill.right());
            let c = if inside { color::INK } else { color::INK_2 };
            let by = baseline_for(p, Font::Semibold, TYPE.0, r.cy());
            p.text_centered(Font::Semibold, TYPE, x + w / 2.0, by, c, &label);
        }
    }
}

/// The x of a pill edge at fractional chip index `f`: a chip's left (or right) edge, interpolated between neighbours.
fn edge_x(r: R, n: u32, f: f32, right: bool) -> f32 {
    let f = f.clamp(-0.4, n as f32 - 0.6);
    let i0 = f.floor().clamp(0.0, (n - 1) as f32) as u32;
    let i1 = (i0 + 1).min(n - 1);
    let t = (f - i0 as f32).clamp(-0.4, 1.0);
    let pick = |i: u32| {
        let (x, w) = cell_x(r, n, GAP, i, 1);
        if right { x + w } else { x }
    };
    let (a, b) = (pick(i0), pick(i1));
    a + (b - a) * t
}
