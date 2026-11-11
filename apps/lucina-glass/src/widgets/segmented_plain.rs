//! A segmented control without the plus icon of the kit's [`super::Segmented`]: a glass pill whose chosen segment is the accent
//! (soft fill, outline) and slides to a new choice on a tween of `motion::SHORT`. Segments are the proved equal cells of
//! [`crate::rules::cells`].

use crate::Paint;
use crate::anim::{Ease, Secs, Tween};
use crate::canvas::R;
use crate::text::Font;
use crate::tokens::{color, motion, radius, space, ty};

use super::ink::{focus_ring, tap, text_centre};
use super::{cell_x, lerp_dp};

const PAD: f32 = 4.0;

#[derive(Clone, Debug)]
pub struct SegmentedPlain {
    pub labels: Vec<String>,
    selected: usize,
    /// The chosen segment's position as an index (fractional while it moves).
    thumb: Tween,
    /// The segment with the keyboard focus.
    pub focused: Option<usize>,
    pub min_hit: f32,
    last: R,
}

impl SegmentedPlain {
    pub fn new(labels: &[&str], selected: usize) -> SegmentedPlain {
        let selected = selected.min(labels.len().saturating_sub(1));
        SegmentedPlain {
            labels: labels.iter().map(|s| s.to_string()).collect(),
            selected,
            thumb: Tween::at_rest(selected as f32),
            focused: None,
            min_hit: space::TAP,
            last: R::default(),
        }
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn select(&mut self, now: Secs, i: usize) {
        if i < self.labels.len() && i != self.selected {
            self.selected = i;
            self.thumb.ease = Ease::STANDARD;
            self.thumb.retarget(now, i as f32, motion::SHORT);
        }
    }

    pub fn settled(&self, now: Secs) -> bool {
        self.thumb.done(now)
    }

    pub fn draw(&mut self, p: &mut Paint, r: R) {
        self.last = r;
        let n = self.labels.len().max(1) as u32;
        p.glass(r, radius::PILL, color::GLASS_TINT);
        let inner = r.inset(PAD);
        let pos = self.thumb.at(p.now).clamp(0.0, (n - 1) as f32);
        let i0 = pos.floor() as u32;
        let i1 = (i0 + 1).min(n - 1);
        let (x0, w0) = cell_x(inner, n, 0.0, i0, 1);
        let (x1, w1) = cell_x(inner, n, 0.0, i1, 1);
        let t = pos - i0 as f32;
        let thumb = R::new(lerp_dp(x0, x1, t), inner.y, lerp_dp(w0, w1, t), inner.h);
        p.cv.fill_rrect(thumb, radius::PILL, color::ACCENT_SOFT);
        p.cv.stroke_rrect(thumb, radius::PILL, 0.8, color::ACCENT);
        for i in 0..n {
            let (sx, sw) = cell_x(inner, n, 0.0, i, 1);
            let near = (1.0 - (pos - i as f32).abs()).clamp(0.0, 1.0);
            let c = color::INK_2.mix(color::INK, near);
            text_centre(
                p,
                Font::Semibold,
                ty::LEAD,
                sx + sw / 2.0,
                inner.cy(),
                c,
                &self.labels[i as usize],
            );
            if self.focused == Some(i as usize) {
                focus_ring(p.cv, R::new(sx, inner.y, sw, inner.h), radius::PILL);
            }
        }
    }

    /// The segment under (`x`, `y`) as last drawn.
    pub fn hit(&self, x: f32, y: f32) -> Option<usize> {
        if !tap(self.last, self.min_hit, x, y) {
            return None;
        }
        let n = self.labels.len().max(1) as u32;
        let inner = self.last.inset(PAD);
        // A point in the hit margin outside the pill belongs to the nearest segment.
        let x = x.clamp(inner.x, inner.right() - 0.01);
        (0..n)
            .find(|&i| {
                let (sx, sw) = cell_x(inner, n, 0.0, i, 1);
                x >= sx && x < sx + sw
            })
            .map(|i| i as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::{Canvas, Color};
    use crate::text::Type;

    fn render(s: &mut SegmentedPlain, now: f64) -> Canvas {
        let mut ty = Type::new();
        let mut cv = Canvas::new(300, 80, 1.0);
        cv.clear(Color::hex(0));
        let mut p = Paint {
            cv: &mut cv,
            ty: &mut ty,
            now,
            backdrop: None,
        };
        s.draw(&mut p, R::new(20.0, 20.0, 240.0, 44.0));
        cv
    }

    #[test]
    fn the_choice_slides_and_has_no_plus() {
        let mut s = SegmentedPlain::new(&["Tabs", "Private"], 0);
        let a = render(&mut s, 0.0);
        s.select(1.0, 1);
        assert_eq!(s.selected(), 1);
        assert!(!s.settled(1.05));
        let mid = render(&mut s, 1.0 + motion::SHORT * 0.1);
        let end = render(&mut s, 2.0);
        assert!(a.px != mid.px && mid.px != end.px && a.px != end.px);
        // The accent fill is on the left segment at first and on the right one when done.
        let blue = |cv: &Canvas, xs: std::ops::Range<usize>| {
            (24..60usize)
                .flat_map(|y| xs.clone().map(move |x| (x, y)))
                .filter(|&(x, y)| {
                    let v = cv.px[y * 300 + x];
                    (v & 0xff) as i32 - ((v >> 16) & 0xff) as i32 > 12
                })
                .count()
        };
        assert!(blue(&a, 24..140) > 2 * blue(&a, 142..256));
        assert!(blue(&end, 142..256) > 2 * blue(&end, 24..140));
        s.select(3.0, 9);
        assert_eq!(s.selected(), 1, "an index past the end is ignored");
    }

    #[test]
    fn hit_names_the_segment_and_margins_go_to_the_nearest() {
        let mut s = SegmentedPlain::new(&["A", "B", "C"], 1);
        render(&mut s, 0.0);
        assert_eq!(s.hit(40.0, 42.0), Some(0));
        assert_eq!(s.hit(140.0, 42.0), Some(1));
        assert_eq!(s.hit(250.0, 42.0), Some(2));
        assert_eq!(s.hit(15.0, 42.0), None, "left of the pill");
        assert_eq!(s.hit(150.0, 100.0), None);
    }
}
