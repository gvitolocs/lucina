//! "Work ⊕ / Talks ⊕": a glass track with a brighter thumb that slides to the chosen segment on a spring. Past either end the
//! spring's overshoot squashes the thumb against the track instead of leaving it, and labels brighten as the thumb nears.

use crate::anim::{Secs, Spring};
use crate::canvas::{Color, R};
use crate::icons::{self, Icon};
use crate::text::Font;
use crate::tokens::{color, radius, ty};
use crate::Paint;

use super::{baseline_for, cell_x, lerp_dp};

#[derive(Clone)]
pub struct Segmented {
    pub labels: Vec<String>,
    pub selected: usize,
    /// The thumb's position as a segment index (fractional while moving).
    thumb: Spring,
    last: R,
}

const PAD: f32 = 4.0;

impl Segmented {
    pub fn new(labels: &[&str], selected: usize) -> Segmented {
        Segmented { labels: labels.iter().map(|s| s.to_string()).collect(), selected, thumb: Spring::snappy(selected as f32), last: R::default() }
    }

    pub fn select(&mut self, now: Secs, i: usize) {
        if i < self.labels.len() {
            self.selected = i;
            self.thumb.set_target(now, i as f32);
        }
    }

    /// The segment under (`x`, `y`) as last drawn.
    pub fn hit(&self, x: f32, y: f32) -> Option<usize> {
        if !self.last.contains(x, y) {
            return None;
        }
        let n = self.labels.len().max(1) as u32;
        let inner = self.last.inset(PAD);
        (0..n).find(|&i| {
            let (sx, sw) = cell_x(inner, n, 0.0, i, 1);
            x >= sx && x < sx + sw
        }).map(|i| i as usize)
    }

    pub fn settled(&self, now: Secs) -> bool {
        self.thumb.settled(now)
    }

    pub fn draw(&mut self, p: &mut Paint, r: R) {
        self.last = r;
        let n = self.labels.len().max(1) as u32;
        p.glass(r, radius::PILL, color::GLASS_TINT);
        let inner = r.inset(PAD);
        let pos = self.thumb.at(p.now);
        let last = (n - 1) as f32;
        let clamped = pos.clamp(0.0, last);
        let over = (pos - clamped).abs();
        let i0 = clamped.floor().min(last.max(0.0)) as u32;
        let i1 = (i0 + 1).min(n - 1);
        let (x0, w0) = cell_x(inner, n, 0.0, i0, 1);
        let (x1, _) = cell_x(inner, n, 0.0, i1, 1);
        let x = lerp_dp(x0, x1, clamped - i0 as f32);
        // Overshoot squashes against the track's end it went past.
        let squash = (over * 0.6).min(0.25);
        let tw = w0 * (1.0 - squash);
        let tx = if pos < 0.0 { x } else if pos > last { x + w0 - tw } else { x };
        let thumb = R::new(tx, inner.y, tw, inner.h);
        p.cv.fill_rrect_soft(thumb.offset(0.0, 2.0), radius::PILL, 6.0, Color::hexa(0x000000, 70));
        p.cv.gradient_rrect(thumb, radius::PILL, Color::hexa(0xffffff, 64), Color::hexa(0xffffff, 30));
        p.cv.stroke_rrect(thumb, radius::PILL, 0.7, Color::hexa(0xffffff, 90));
        for i in 0..n {
            let (sx, sw) = cell_x(inner, n, 0.0, i, 1);
            let near = (1.0 - (pos - i as f32).abs()).clamp(0.0, 1.0);
            let c = color::INK_2.mix(color::INK, near);
            let label = self.labels[i as usize].clone();
            let lw = p.measure(Font::Regular, ty::BODY, &label);
            let total = lw + 6.0 + 13.0;
            let lx = sx + (sw - total) / 2.0;
            let by = baseline_for(p, Font::Regular, ty::BODY.0, inner.cy());
            p.text(Font::Regular, ty::BODY, lx, by, c, &label);
            icons::draw(p.cv, Icon::PlusCircle, lx + lw + 6.0 + 6.5, inner.cy(), 13.0, c);
        }
    }
}
