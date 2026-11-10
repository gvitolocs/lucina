//! The timer, 02:14, with an accent ring around an app icon. The ring sweeps continuously with the time left; at each new second the
//! digits that change roll up (the old one rises and fades, the new one rises in from below). Minutes and seconds come from
//! [`rules::mm_ss`], so the display never grows past five characters.

use crate::anim::{Ease, Secs, phase};
use crate::canvas::{Color, R};
use crate::icons::{self, Icon};
use crate::rules;
use crate::text::Font;
use crate::tokens::color;
use crate::Paint;

#[derive(Clone)]
pub struct TimerRing {
    /// The whole duration, seconds.
    pub total: u32,
    /// Seconds left when it (re)started.
    pub left_at_start: f64,
    pub started: Secs,
    /// Set while paused: the seconds left then.
    pub paused: Option<f64>,
    pub icon: Icon,
}

/// How long a digit takes to roll.
const ROLL: f64 = 0.32;

impl TimerRing {
    pub fn new(total: u32, left: f64, started: Secs) -> TimerRing {
        TimerRing { total: total.max(1), left_at_start: left, started, paused: None, icon: Icon::Upload }
    }

    /// Seconds left at `now`, never below 0.
    pub fn left(&self, now: Secs) -> f64 {
        match self.paused {
            Some(l) => l,
            None => (self.left_at_start - (now - self.started)).max(0.0),
        }
    }

    pub fn pause(&mut self, now: Secs) {
        if self.paused.is_none() {
            self.paused = Some(self.left(now));
        }
    }

    pub fn resume(&mut self, now: Secs) {
        if let Some(l) = self.paused.take() {
            self.left_at_start = l;
            self.started = now;
        }
    }

    /// The ring: track, sweep of the time left, a head dot; the app icon in the middle.
    pub fn draw_ring(&self, p: &mut Paint, cx: f32, cy: f32, rad: f32) {
        let left = self.left(p.now);
        let frac = rules::progress((left * 1000.0) as u64, self.total as u64 * 1000) as f32 / rules::UNIT as f32;
        p.cv.stroke_circle(cx, cy, rad, 3.0, color::HAIR);
        let sweep = frac * std::f32::consts::TAU;
        if sweep > 0.01 {
            p.cv.arc(cx, cy, rad, 3.0, 0.0, sweep, color::ACCENT);
            let (hx, hy) = (cx + rad * sweep.sin(), cy - rad * sweep.cos());
            p.cv.glow(hx, hy, 9.0, color::ACCENT.alpha(0.6));
            p.cv.fill_circle(hx, hy, 3.2, Color::hex(0xffffff));
        }
        let icon_r = R::new(cx - rad * 0.62, cy - rad * 0.62, rad * 1.24, rad * 1.24);
        p.cv.gradient_rrect(icon_r, rad * 0.34, Color::hex(0x2a2d35), Color::hex(0x15171c));
        p.cv.stroke_rrect(icon_r, rad * 0.34, 0.6, color::HAIR);
        icons::draw(p.cv, self.icon, cx, cy, rad * 0.7, color::INK);
    }

    /// The digits "MM:SS", baseline `y`, from `x`, Lato Thin at `size`; returns their width. Each digit sits in a fixed slot.
    pub fn draw_digits(&self, p: &mut Paint, x: f32, y: f32, size: f32, c: Color) -> f32 {
        let left = self.left(p.now);
        let shown = left.ceil() as u32;
        let (m, s) = rules::mm_ss(shown);
        let (pm, ps) = rules::mm_ss(shown + 1);
        // Time since the display last changed: the roll's clock.
        // Shown is `ceil(left)`, which changed when `left` crossed it from above.
        let since = if self.paused.is_some() || left <= 0.0 { ROLL } else { shown as f64 - left };
        let t = Ease::OutCubic.at(phase(since, 0.0, ROLL));
        let now_s = format!("{m:02}:{s:02}");
        let was_s = format!("{pm:02}:{ps:02}");
        let slot = p.measure(Font::Thin, (size, 0.0), "0") + size * 0.02;
        let colon = p.measure(Font::Thin, (size, 0.0), ":") + size * 0.04;
        let rise = size * 0.38;
        let mut cx = x;
        p.cv.push_clip(R::new(x - 2.0, y - size * 0.95, slot * 4.0 + colon + 8.0, size * 1.15), 0.0);
        for (a, b) in now_s.chars().zip(was_s.chars()) {
            let w = if a == ':' { colon } else { slot };
            let cw = p.measure(Font::Thin, (size, 0.0), &a.to_string());
            let gx = cx + (w - cw) / 2.0;
            if a != b && t < 1.0 && self.paused.is_none() {
                let bw = p.measure(Font::Thin, (size, 0.0), &b.to_string());
                p.text(Font::Thin, (size, 0.0), cx + (w - bw) / 2.0, y - rise * t, c.alpha(1.0 - t), &b.to_string());
                p.text(Font::Thin, (size, 0.0), gx, y + rise * (1.0 - t), c.alpha(t), &a.to_string());
            } else {
                p.text(Font::Thin, (size, 0.0), gx, y, c, &a.to_string());
            }
            cx += w;
        }
        p.cv.pop_clip();
        cx - x
    }
}
