//! Adaptive glass: the bars (tab strip, toolbar, address pill, dock) take their tint, ink and rim from the page behind them, as
//! Apple's Liquid Glass switches between light and dark under its content and Fluent's acrylic takes its tint from what is
//! behind it (LucinaNet `docs/design/browser-glass.md`, "Adaptive chrome").
//!
//! - luminance `l` is the mean sRGB-encoded luma (0.2126 R + 0.7152 G + 0.0722 B, 0..1) of the blurred page in the rectangle
//!   behind the bar ([`region_luma`]);
//! - [`blend`] turns it into `t`: 0 at or under 0.60, 1 at or over 0.70, a smoothstep between. The band sits above 0.5 on
//!   purpose: the dark glass is itself dark (tint alpha 150), so white ink stays readable over a mid-grey page (a light page
//!   dimmed under a modal reads about 0.48); only a truly light page flips the bar;
//! - [`look`] is the appearance at `t`: dark glass (`GLASS_BAR_TINT`, white ink, white rim) to light glass (`GLASS_TINT_LIGHT`,
//!   `INK_ON_LIGHT`, `GLASS_RIM_LIGHT`), blended; the second and third inks are the ink at 62 % and 34 %.
//!
//! Popovers, sheets, dialogs and cards are the thick content material and stay dark: only the bars adapt. The flip animates over
//! `motion::SHORT`: [`Adaptive`] holds the `t` of one bar and moves it with a tween. The functions are float arithmetic, tested
//! here (ends, band, monotone) and not part of the Verus-checked rules.

use crate::Paint;
use crate::anim::{Ease, Secs, Tween};
use crate::canvas::{Canvas, Color, R};
use crate::tokens::{color, motion};

use super::ink::Palette;

/// How a bar looks over a page of some luminance.
#[derive(Clone, Copy, Debug)]
pub struct Adapt {
    pub tint: Color,
    pub ink: Color,
    pub ink2: Color,
    pub ink3: Color,
    pub rim: Color,
}

impl Adapt {
    /// The three inks, for the controls inside the bar.
    pub fn palette(&self) -> Palette {
        Palette {
            ink: self.ink,
            ink2: self.ink2,
            ink3: self.ink3,
        }
    }
}

/// 0 under `a`, 1 over `b`, smooth between.
fn smooth(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// How light the glass is for a page of luminance `lum`: 0 dark, 1 light.
pub fn blend(lum: f32) -> f32 {
    smooth(0.60, 0.70, lum)
}

/// The appearance at `t` (0 dark to 1 light).
pub fn look(t: f32) -> Adapt {
    let ink = color::INK.mix(color::INK_ON_LIGHT, t);
    Adapt {
        tint: color::GLASS_BAR_TINT.mix(color::GLASS_TINT_LIGHT, t),
        ink,
        ink2: ink.alpha(0.62),
        ink3: ink.alpha(0.34),
        rim: color::GLASS_RIM.mix(color::GLASS_RIM_LIGHT, t),
    }
}

/// The appearance for a page of luminance `lum` (0..1) behind the bar.
pub fn adapt(lum: f32) -> Adapt {
    look(blend(lum))
}

/// Mean luma (0..1) of `b` (the blurred page) inside `r` (dp), every third pixel.
pub fn region_luma(b: &Canvas, r: R) -> f32 {
    let k = b.k;
    let (x0, y0) = ((r.x * k).max(0.0) as u32, (r.y * k).max(0.0) as u32);
    let (x1, y1) = (
        ((r.right() * k) as u32).min(b.w),
        ((r.bottom() * k) as u32).min(b.h),
    );
    let (mut sum, mut n) = (0.0f32, 0u32);
    let step = 3;
    let mut y = y0;
    while y < y1 {
        let mut x = x0;
        while x < x1 {
            let p = b.px[(y * b.w + x) as usize];
            let (r_, g_, b_) = (
                ((p >> 16) & 255) as f32,
                ((p >> 8) & 255) as f32,
                (p & 255) as f32,
            );
            sum += (0.2126 * r_ + 0.7152 * g_ + 0.0722 * b_) / 255.0;
            n += 1;
            x += step;
        }
        y += step;
    }
    if n == 0 { 0.0 } else { sum / n as f32 }
}

/// A glass bar whose look follows the page behind it, at once (no animation): its shadow, then glass tinted by [`adapt`].
pub fn adaptive_bar(p: &mut Paint, r: R, rad: f32) -> Adapt {
    let lum = p.backdrop.map(|b| region_luma(b, r)).unwrap_or(0.0);
    let a = adapt(lum);
    draw_bar(p, r, rad, &a);
    a
}

fn draw_bar(p: &mut Paint, r: R, rad: f32, a: &Adapt) {
    p.shadow(r, rad, 7.0);
    match p.backdrop {
        Some(b) => p.cv.glass(r, rad, b, a.tint, a.rim),
        None => p.cv.fill_rrect(r, rad, a.tint),
    }
}

/// One bar's adaptive state: the flip between dark and light animates over `motion::SHORT` when the page under it changes.
#[derive(Clone, Copy, Debug)]
pub struct Adaptive {
    t: Option<Tween>,
}

impl Default for Adaptive {
    fn default() -> Self {
        Adaptive::new()
    }
}

impl Adaptive {
    pub fn new() -> Adaptive {
        Adaptive { t: None }
    }

    /// The page behind the bar now has luminance `lum`: head for its look (the first time, be there at once).
    pub fn set_luma(&mut self, now: Secs, lum: f32) {
        let target = blend(lum);
        match &mut self.t {
            None => self.t = Some(Tween::at_rest(target)),
            Some(t) if (t.to - target).abs() > 0.001 => {
                t.ease = Ease::STANDARD;
                t.retarget(now, target, motion::SHORT);
            }
            Some(_) => {}
        }
    }

    /// The look at `now`.
    pub fn look(&self, now: Secs) -> Adapt {
        look(self.t.map(|t| t.at(now)).unwrap_or(0.0))
    }

    /// Whether the bar is still changing.
    pub fn settled(&self, now: Secs) -> bool {
        self.t.is_none_or(|t| t.done(now))
    }

    /// Measure the page behind `r`, move toward its look and draw the bar in the look of `p.now`.
    pub fn bar(&mut self, p: &mut Paint, r: R, rad: f32) -> Adapt {
        let lum = p.backdrop.map(|b| region_luma(b, r)).unwrap_or(0.0);
        self.set_luma(p.now, lum);
        let a = self.look(p.now);
        draw_bar(p, r, rad, &a);
        a
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::Type;

    #[test]
    fn dark_under_dark_light_under_light() {
        assert_eq!(adapt(0.1).ink, color::INK);
        assert_eq!(adapt(0.9).ink, color::INK_ON_LIGHT);
        assert_eq!(adapt(0.1).tint, color::GLASS_BAR_TINT);
        assert_eq!(adapt(0.9).tint, color::GLASS_TINT_LIGHT);
        assert_eq!(adapt(0.9).rim, color::GLASS_RIM_LIGHT);
        let mid = adapt(0.65);
        assert!(mid.ink.r > 40 && mid.ink.r < 255);
    }

    #[test]
    fn the_band_is_narrow_and_monotone_and_a_dimmed_light_page_stays_dark() {
        assert_eq!(
            blend(0.48),
            0.0,
            "a light page dimmed under a modal keeps the dark glass"
        );
        assert_eq!(blend(0.60), 0.0);
        assert_eq!(blend(0.70), 1.0);
        let mut last = 0.0;
        for i in 0..=100 {
            let t = blend(i as f32 / 100.0);
            assert!(t >= last && (0.0..=1.0).contains(&t));
            last = t;
        }
        let a = adapt(0.1);
        assert_eq!(a.ink2.a, (255.0f32 * 0.62).round() as u8);
        assert_eq!(a.ink3.a, (255.0f32 * 0.34).round() as u8);
    }

    #[test]
    fn region_luma_reads_the_region_only() {
        let mut cv = Canvas::new(60, 30, 1.0);
        cv.clear(Color::hex(0x000000));
        cv.fill_rrect(R::new(30.0, 0.0, 30.0, 30.0), 0.0, Color::hex(0xffffff));
        assert!(region_luma(&cv, R::new(0.0, 0.0, 30.0, 30.0)) < 0.01);
        assert!(region_luma(&cv, R::new(30.0, 0.0, 30.0, 30.0)) > 0.99);
        assert_eq!(
            region_luma(&cv, R::new(100.0, 0.0, 10.0, 10.0)),
            0.0,
            "outside the canvas reads as dark"
        );
    }

    #[test]
    fn the_flip_animates_over_short_and_starts_where_it_is() {
        let mut a = Adaptive::new();
        a.set_luma(0.0, 0.9);
        assert_eq!(
            a.look(0.0).ink,
            color::INK_ON_LIGHT,
            "the first measure is there at once"
        );
        a.set_luma(1.0, 0.1);
        assert_eq!(
            a.look(1.0).ink,
            color::INK_ON_LIGHT,
            "the flip starts from the look it was in"
        );
        assert!(!a.settled(1.1));
        let mid = a.look(1.0 + motion::SHORT / 2.0).ink;
        assert!(mid.r > color::INK_ON_LIGHT.r && mid.r < color::INK.r);
        assert!(a.settled(1.0 + motion::SHORT + 0.01));
        assert_eq!(a.look(2.0).ink, color::INK);
    }

    #[test]
    fn a_bar_draws_over_a_page_and_without_one() {
        let mut ty = Type::new();
        let mut page = Canvas::new(120, 60, 1.0);
        page.clear(Color::hex(0xffffff));
        let blurred = page.blurred(2);
        let mut cv = Canvas::new(120, 60, 1.0);
        cv.clear(Color::hex(0xffffff));
        let mut p = Paint {
            cv: &mut cv,
            ty: &mut ty,
            now: 0.0,
            backdrop: Some(&blurred),
        };
        let a = adaptive_bar(&mut p, R::new(10.0, 10.0, 100.0, 40.0), 20.0);
        assert_eq!(a.ink, color::INK_ON_LIGHT, "a white page gives light glass");
        let mut cv2 = Canvas::new(120, 60, 1.0);
        cv2.clear(Color::hex(0));
        let mut p2 = Paint {
            cv: &mut cv2,
            ty: &mut ty,
            now: 0.0,
            backdrop: None,
        };
        Adaptive::new().bar(&mut p2, R::new(10.0, 10.0, 100.0, 40.0), 20.0);
        assert!(cv2.px.iter().any(|&v| v != 0));
    }
}
