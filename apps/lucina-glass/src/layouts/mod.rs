//! The concept on the other devices, from the same tokens and widgets as the phone (docs/design/phone-concept.md,
//! "Variants"; docs/design/rules.md). Device names are the product names (rule 6).
//!
//! - [`Juno`]: the laptop, 1920 x 1080 at 1.5 px/dp. The three phone screens merge into one dashboard, and the sheet becomes
//!   a dock. Pointer hover and the keyboard focus ring are shown.
//! - [`DianaTablet`]: Diana as a tablet, 2560 x 1600 at 2 px/dp, landscape or portrait.
//!   - Landscape: Build and the agenda side by side, Notification as a slide-over peeking from the right edge.
//!   - Portrait: the agenda on top; Build, and Notification with its stat cards, side by side under it.
//! - [`Fortuna`]: the DSi, two 256 x 192 screens at 1 px/dp, the bottom one touch.
//!   - No glass, blur or photographs; RGB555 with an ordered dither.
//!   - Animations are cut down: state changes snap, the LIVE pill blinks, the digits change without rolling.
//!
//! Every pane comes from [`rules::grid_box`], proved to stay inside its parent.

mod blocks;
mod diana_tablet;
mod fortuna;
mod juno;

pub use diana_tablet::DianaTablet;
pub use fortuna::Fortuna;
pub use juno::Juno;

use crate::anim::Secs;
use crate::canvas::{Canvas, R};
use crate::rules;
use crate::text::Type;

/// A device layout of the concept.
pub trait Layout {
    fn name(&self) -> &'static str;
    /// Pixel size and pixels per dp.
    fn size(&self) -> (u32, u32, f32);
    fn draw(&mut self, cv: &mut Canvas, ty: &mut Type, now: Secs);
    /// The panes it laid out last, in dp, with the area they had to stay in: for the tests.
    fn panes(&self) -> Vec<(R, R)>;
}

/// The pane at (`col`, `row`) spanning (`cs`, `rs`) of a `cols` x `rows` grid over `area`, `gap` dp apart, through the
/// proved [`rules::grid_box`] in hundredths of a dp.
#[allow(clippy::too_many_arguments)]
pub fn pane(area: R, cols: u32, rows: u32, gap: f32, col: u32, row: u32, cs: u32, rs: u32) -> R {
    let q = |v: f32| (v * 100.0).round() as i32;
    let b = rules::grid_box(q(area.x), q(area.y), q(area.w), q(area.h), cols, rows, q(gap), col, row, cs, rs);
    R::new(b.x as f32 / 100.0, b.y as f32 / 100.0, b.w as f32 / 100.0, b.h as f32 / 100.0)
}

/// Whether `inner` lies inside `outer`, by the proved [`rules::fits`].
pub fn fits(inner: R, outer: R) -> bool {
    let q = |v: f32| (v * 100.0).round() as i32;
    rules::fits(rules::Box2 { x: q(inner.x), y: q(inner.y), w: q(inner.w), h: q(inner.h) }, q(outer.x), q(outer.y), q(outer.w), q(outer.h))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_layout_keeps_its_panes_inside() {
        let mut ty = Type::new();
        let mut all: Vec<Box<dyn Layout>> = vec![Box::new(Juno::new(0.0)), Box::new(DianaTablet::new(0.0, false)), Box::new(DianaTablet::new(0.0, true)), Box::new(Fortuna::new(0.0))];
        for l in &mut all {
            let (w, h, k) = l.size();
            let mut cv = Canvas::new(w, h, k);
            l.draw(&mut cv, &mut ty, 3.0);
            let panes = l.panes();
            assert!(!panes.is_empty(), "{}", l.name());
            for (p, area) in panes {
                assert!(fits(p, area), "{}: {p:?} outside {area:?}", l.name());
                assert!(fits(area, cv.bounds()), "{}: {area:?} off the screen", l.name());
            }
            assert!(cv.px.iter().any(|&p| p != cv.px[0]), "{} drew nothing", l.name());
        }
    }

    #[test]
    fn fortuna_is_rgb555() {
        let mut d = Fortuna::new(0.0);
        let (w, h, k) = d.size();
        let mut cv = Canvas::new(w, h, k);
        d.draw(&mut cv, &mut Type::new(), 3.0);
        assert!(cv.px.iter().all(|&p| p & 0x0007_0707 == 0), "a colour outside RGB555");
    }
}
