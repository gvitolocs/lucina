//! The three screens and the move between them. A screen owns its widgets and its backdrop (generated once per canvas size,
//! with the blurred copy its glass shows), draws itself at any time `now`, and takes pointer events in dp.

pub mod agenda;
pub mod build;
pub mod content;
pub mod notification;

use crate::anim::{Ease, Secs, phase};
use crate::canvas::{Canvas, Color, R};
use crate::text::Type;
use crate::tokens::{color, glass, motion};

pub use agenda::Agenda;
pub use build::Build;
pub use notification::Notification;

/// A pointer event, in dp.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Pointer {
    Down(f32, f32),
    Move(f32, f32),
    Up(f32, f32),
}

/// A screen of the concept.
pub trait Screen {
    fn name(&self) -> &'static str;
    /// Draw the whole screen at `now` onto `cv` (a phone-shaped canvas).
    fn draw(&mut self, cv: &mut Canvas, ty: &mut Type, now: Secs);
    /// A pointer event; true when something changed.
    fn pointer(&mut self, now: Secs, ev: Pointer) -> bool;
    /// Play the entrance animations again from `now`.
    fn enter(&mut self, now: Secs);
    /// Named demo actions for the gallery and the frame sequences ("swipe", "select-thu"...): true when known.
    fn act(&mut self, now: Secs, action: &str) -> bool;
}

/// A backdrop and its blurred copy, for one canvas size.
pub struct Backdrop {
    pub size: (u32, u32),
    pub image: Canvas,
    pub blurred: Canvas,
}

impl Backdrop {
    /// Make (or keep) the backdrop for `cv`'s size with `make(w, h, k)`.
    pub fn ensure(slot: &mut Option<Backdrop>, cv: &Canvas, make: fn(u32, u32, f32, u64) -> Canvas) {
        if slot.as_ref().is_some_and(|b| b.size == (cv.w, cv.h)) {
            return;
        }
        let image = make(cv.w, cv.h, cv.k, 7);
        let blurred = image.blurred((glass::BLUR * cv.k / 3.0).round().max(1.0) as u32);
        *slot = Some(Backdrop { size: (cv.w, cv.h), image, blurred });
    }
}

/// The home indicator at the bottom.
pub fn home_indicator(cv: &mut Canvas, ink: Color) {
    let b = cv.bounds();
    cv.fill_rrect(R::new(b.cx() - 67.0, b.bottom() - 9.0, 134.0, 5.0), 3.0, ink);
}

/// A move from one screen to the next.
#[derive(Clone, Copy, Debug)]
pub struct Transition {
    pub start: Secs,
    /// 1: the new screen comes from the right (forward); -1: from the left.
    pub dir: f32,
}

impl Transition {
    pub fn new(start: Secs, dir: f32) -> Transition {
        Transition { start, dir }
    }

    pub fn progress(&self, now: Secs) -> f32 {
        Ease::EMPHASIZED.at(phase(now, self.start, motion::LONG))
    }

    pub fn done(&self, now: Secs) -> bool {
        now >= self.start + motion::LONG
    }

    /// Compose `out` from the leaving screen `a` and the arriving `b` (both drawn at full size) at `now`: the leaving one
    /// sinks back (shrinks, dims, drifts a third of the way out), the arriving one slides in over it with rounded corners
    /// and a shadow on its leading edge, its corners squaring off as it lands.
    pub fn compose(&self, out: &mut Canvas, a: &Canvas, b: &Canvas, now: Secs) {
        let t = self.progress(now);
        let full = out.bounds();
        out.clear(color::VOID);
        let s = 1.0 - 0.08 * t;
        let back = full.scale_about_center(s).offset(-self.dir * full.w * 0.28 * t, 0.0);
        out.blit(a, back, 34.0 * t, 1.0);
        out.fill_rrect(back, 34.0 * t, Color::hexa(0x000000, (150.0 * t) as u8));
        let front = full.offset(self.dir * full.w * (1.0 - t), 0.0);
        let rad = 34.0 * (1.0 - t);
        out.fill_rrect_soft(front.inset(4.0), rad, 18.0, Color::hexa(0x000000, (140.0 * (1.0 - t)) as u8));
        out.blit(b, front, rad, 1.0);
    }
}
