//! lucina-glass: the glass phone UI kit of docs/design/phone-concept.md.
//!
//! A concept to review and change before it is wired into the shell: three screens ("Build", the agenda, "Notification")
//! made of reusable widgets, each with its animations, drawn on a software [`canvas::Canvas`] in design units.
//!
//! - [`tokens`]: colours, radii, spacing, type scale, motion.
//! - [`canvas`]: anti-aliased shapes, glass, blur, images. [`text`]: the faces.
//! - [`anim`]: clock, easings, tweens and closed-form springs; frame-rate independent and pausable.
//! - [`rules`]: the integer rules every position goes through, proved by Verus (D-011).
//! - [`icons`], [`imagery`]: vector glyphs and generated space backdrops (no photographs, no third-party marks).
//! - [`widgets`]: the components. [`screens`]: the three screens and the transitions between them.
//! - [`layouts`]: the same concept on Juno (the laptop), Diana as a tablet (both orientations) and Fortuna (the DSi).

pub mod anim;
// browser_rules.rs is what Verus reads, as rules.rs is.
#[allow(clippy::manual_is_multiple_of, clippy::manual_div_ceil)]
pub mod browser_rules;
pub mod canvas;
pub mod home;
// home_rules.rs is what Verus reads, as rules.rs is.
#[allow(clippy::manual_is_multiple_of, clippy::manual_div_ceil)]
pub mod home_rules;
pub mod icons;
pub mod imagery;
pub mod layouts;
// rules.rs is what Verus reads, so it keeps to the arithmetic Verus models (`%`, `/`), not the std helpers clippy suggests.
#[allow(clippy::manual_is_multiple_of, clippy::manual_div_ceil)]
pub mod rules;
pub mod screens;
pub mod text;
pub mod tokens;
pub mod widgets;

pub use anim::{Clock, Ease, Secs, Spring, Tween};
pub use canvas::{Canvas, Color, R};
pub use text::{Font, Type};

/// Everything a widget draws with: the canvas, the type, and the time.
pub struct Paint<'a> {
    pub cv: &'a mut Canvas,
    pub ty: &'a mut Type,
    pub now: Secs,
    /// The blurred backdrop glass shows, the canvas's size; `None` on plain grounds.
    pub backdrop: Option<&'a Canvas>,
}

impl Paint<'_> {
    /// Text with its baseline at `y`.
    #[allow(clippy::too_many_arguments)]
    pub fn text(
        &mut self,
        f: Font,
        (size, tracking): (f32, f32),
        x: f32,
        y: f32,
        c: Color,
        s: &str,
    ) -> f32 {
        self.ty.draw(self.cv, f, size, tracking, x, y, c, s)
    }

    pub fn text_right(
        &mut self,
        f: Font,
        (size, tracking): (f32, f32),
        right: f32,
        y: f32,
        c: Color,
        s: &str,
    ) {
        self.ty
            .draw_right(self.cv, f, size, tracking, right, y, c, s)
    }

    pub fn text_centered(
        &mut self,
        f: Font,
        (size, tracking): (f32, f32),
        cx: f32,
        y: f32,
        c: Color,
        s: &str,
    ) {
        self.ty
            .draw_centered(self.cv, f, size, tracking, cx, y, c, s)
    }

    pub fn measure(&mut self, f: Font, (size, tracking): (f32, f32), s: &str) -> f32 {
        self.ty.measure(self.cv, f, size, tracking, s)
    }

    /// `s` as it fits in `max` dp: whole if it does, else cut at a character and ended with "…" (never wider than `max`).
    pub fn fit(&mut self, f: Font, st: (f32, f32), s: &str, max: f32) -> String {
        if self.measure(f, st, s) <= max {
            return s.to_string();
        }
        let chars: Vec<char> = s.chars().collect();
        let (mut lo, mut hi) = (0usize, chars.len());
        // The longest prefix that fits with the ellipsis, by halving.
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            let t: String = chars[..mid]
                .iter()
                .collect::<String>()
                .trim_end()
                .to_string()
                + "…";
            if self.measure(f, st, &t) <= max {
                lo = mid
            } else {
                hi = mid - 1
            }
        }
        chars[..lo]
            .iter()
            .collect::<String>()
            .trim_end()
            .to_string()
            + "…"
    }

    /// A glass panel: the backdrop through it when there is one, else a translucent fill.
    pub fn glass(&mut self, r: R, rad: f32, tint: Color) {
        match self.backdrop {
            Some(b) => self.cv.glass(r, rad, b, tint, tokens::color::GLASS_RIM),
            None => {
                self.cv.fill_rrect(r, rad, tint);
                self.cv.stroke_rrect(r, rad, 0.6, tokens::color::HAIR);
            }
        }
    }

    /// A soft shadow under a floating rectangle.
    pub fn shadow(&mut self, r: R, rad: f32, lift: f32) {
        self.cv
            .fill_rrect_soft(r.offset(0.0, lift * 0.6), rad, lift, tokens::color::SHADOW);
    }
}
