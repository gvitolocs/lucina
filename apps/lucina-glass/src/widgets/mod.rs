//! The concept's components. Each one is a small struct: its state, its animations (tweens and springs read at
//! [`crate::Paint::now`]), `draw` into a rectangle, and the calls that change it (`select`, `press`, `swipe`...), which take
//! the time they happen at. Nothing here counts frames, so any frame of any animation can be drawn on its own.

pub mod action;
pub mod calendar;
pub mod card_stack;
pub mod chrome;
pub mod count_up;
pub mod day_chips;
pub mod live_pill;
pub mod media;
pub mod pager;
pub mod progress;
pub mod segmented;
pub mod timer;

pub use action::ActionButton;
pub use calendar::{CalendarGrid, Tile, TileKind};
pub use card_stack::CardStack;
pub use count_up::CountUp;
pub use day_chips::DayChips;
pub use live_pill::LivePill;
pub use media::{MediaControls, RoundButton, Volume};
pub use pager::PagerDots;
pub use progress::ProgressTrack;
pub use segmented::Segmented;
pub use timer::TimerRing;

use crate::canvas::R;
use crate::rules;
use crate::text::Font;
use crate::Paint;

/// The baseline that centres capitals of `font` at `size` on `cy`.
pub fn baseline_for(p: &Paint, font: Font, size: f32, cy: f32) -> f32 {
    cy + p.ty.cap_height(font, size) / 2.0
}

/// A rectangle laid out by the proved row rule ([`rules::cell`]) in hundredths of a dp: cell `i` (spanning `span`) of `n`
/// across `r`, `gap` dp apart. Hundredths keep sub-pixel placement at 3x.
pub fn cell_x(r: R, n: u32, gap: f32, i: u32, span: u32) -> (f32, f32) {
    let q = |v: f32| (v * 100.0).round() as i32;
    let s = rules::cells(q(r.x), q(r.w).max(0), n, q(gap).min(q(r.w) / (n.max(2) as i32 - 1).max(1)), i, span);
    (s.start as f32 / 100.0, s.len as f32 / 100.0)
}

/// [`cell_x`] down a column.
pub fn cell_y(r: R, n: u32, gap: f32, i: u32, span: u32) -> (f32, f32) {
    cell_x(R::new(r.y, 0.0, r.h, 0.0), n, gap, i, span)
}

/// `a` to `b` (dp), `t` of the way through the proved interpolation (clamped to the two ends), in hundredths of a dp.
pub fn lerp_dp(a: f32, b: f32, t: f32) -> f32 {
    let q = |v: f32| (v * 100.0).round() as i32;
    rules::lerp(q(a), q(b), crate::anim::unit(t)) as f32 / 100.0
}
