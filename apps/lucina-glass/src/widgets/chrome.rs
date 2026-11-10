//! The quieter pieces: status bar, spinner, badge, avatar, profile chip, reminder card, stat card and the vertical slider.
//! Each draws from its arguments; the two with motion (spinner, slider) read the time.

use crate::anim::{Secs, Spring};
use crate::canvas::{Color, R};
use crate::icons::{self, Icon};
use crate::text::Font;
use crate::tokens::{color, radius, space, ty};
use crate::Paint;

use super::baseline_for;

/// "9:41", signal bars, Wi-Fi and battery across the top.
pub fn status_bar(p: &mut Paint, r: R, ink: Color) {
    let by = baseline_for(p, Font::Semibold, 15.0, r.cy());
    p.text(Font::Semibold, (15.0, -0.2), r.x + 26.0, by, ink, "9:41");
    let right = r.right() - 22.0;
    // Battery.
    let b = R::new(right - 25.0, r.cy() - 6.0, 23.0, 12.0);
    p.cv.stroke_rrect(b, 3.5, 1.0, ink.alpha(0.45));
    p.cv.fill_rrect(R::new(b.right() + 1.0, b.cy() - 2.0, 1.6, 4.0), 0.8, ink.alpha(0.45));
    p.cv.fill_rrect(b.inset(2.0), 2.0, ink);
    // Wi-Fi: three arcs and a dot.
    let (wx, wy) = (right - 40.0, r.cy() + 4.5);
    for (i, rad) in [3.5f32, 7.0, 10.5].iter().enumerate() {
        if i == 0 {
            p.cv.fill_circle(wx, wy - 1.5, 1.8, ink);
        } else {
            p.cv.arc(wx, wy, *rad, 1.9, -0.75, 1.5, ink);
        }
    }
    // Signal.
    for i in 0..4 {
        let h = 3.5 + i as f32 * 2.4;
        p.cv.fill_rrect(R::new(right - 70.0 + i as f32 * 4.6, r.cy() + 5.0 - h, 3.0, h), 1.0, ink);
    }
}

/// A small busy spinner: an arc that turns and breathes.
pub fn spinner(p: &mut Paint, cx: f32, cy: f32, rad: f32, c: Color) {
    let t = p.now as f32;
    let start = t * 5.2;
    let sweep = 1.4 + 1.0 * (t * 2.6).sin();
    p.cv.stroke_circle(cx, cy, rad, 1.6, c.alpha(0.18));
    p.cv.arc(cx, cy, rad, 1.6, start, sweep, c);
}

/// A round badge with a number ("1").
pub fn badge(p: &mut Paint, cx: f32, cy: f32, rad: f32, n: &str) {
    p.cv.fill_circle(cx, cy, rad, color::INK);
    let by = baseline_for(p, Font::Semibold, rad, cy);
    p.text_centered(Font::Semibold, (rad, 0.0), cx, by, color::PAPER_INK, n);
}

/// A generated avatar: a gradient disc with initials (no photograph).
pub fn avatar(p: &mut Paint, cx: f32, cy: f32, rad: f32, initials: &str) {
    p.cv.gradient_rrect(R::new(cx - rad, cy - rad, rad * 2.0, rad * 2.0), rad, Color::hex(0xff8a5c), Color::hex(0x7a3cff));
    let by = baseline_for(p, Font::Semibold, rad * 0.8, cy);
    p.text_centered(Font::Semibold, (rad * 0.8, 0.2), cx, by, color::INK, initials);
}

/// Avatar, name and one short line, in a glass pill.
pub fn profile_chip(p: &mut Paint, r: R, initials: &str, name: &str, line: &str) {
    p.glass(r, radius::PILL, color::MATERIAL);
    let rad = r.h / 2.0 - 5.0;
    avatar(p, r.x + 5.0 + rad, r.cy(), rad, initials);
    let x = r.x + 2.0 * rad + 16.0;
    p.text(Font::Semibold, ty::LEAD, x, r.cy() - 1.0, color::INK, name);
    p.text(Font::Regular, ty::CAPTION, x, r.cy() + 14.0, color::INK_2, line);
    icons::draw(p.cv, Icon::ArrowUpRight, r.right() - 22.0, r.cy(), 14.0, color::INK_2);
}

/// A reminder: one fact in headline, one secondary line, and a bar in `mark` (warning for a due date).
pub fn reminder_card(p: &mut Paint, r: R, (fact, secondary): (&str, &str), mark: Color) {
    p.glass(r, radius::LG, color::MATERIAL);
    p.cv.fill_rrect(R::new(r.x + space::L, r.y + space::L, 3.0, r.h - 2.0 * space::L), radius::PILL, mark);
    let x = r.x + space::L + 13.0;
    let room = r.right() - space::L - x;
    let (fact, secondary) = (p.fit(Font::Regular, ty::HEADLINE, fact, room), p.fit(Font::Regular, ty::CAPTION, secondary, room));
    p.text(Font::Regular, ty::HEADLINE, x, r.cy() - 3.0, color::INK, &fact);
    p.text(Font::Regular, ty::CAPTION, x, r.cy() + 16.0, color::INK_2, &secondary);
}

/// A stat card (the house style's `StatCard`): a caption with its icon, over one value in the thin numeral face.
pub fn stat_tile(p: &mut Paint, r: R, icon: Icon, caption: &str, value: &str) {
    p.glass(r, radius::LG, color::MATERIAL);
    icons::draw(p.cv, icon, r.x + space::L + 7.0, r.y + space::L + 7.0, 15.0, color::ACCENT);
    let caption = p.fit(Font::Semibold, ty::CAPTION, caption, r.w - 2.0 * space::L - 20.0);
    p.text(Font::Semibold, ty::CAPTION, r.x + space::L + 20.0, r.y + space::L + 11.0, color::INK_2, &caption);
    let size = ty::NUMERAL.0.min(r.h - 40.0).max(ty::TITLE2.0);
    p.text(Font::Thin, (size, ty::NUMERAL.1), r.x + space::L - 4.0, r.bottom() - space::L + 2.0, color::INK, value);
}

/// The left rail's vertical slider: up, a track with a thumb on a spring, down.
#[derive(Clone)]
pub struct VSlider {
    pub value: Spring,
    pub steps: u32,
    last: R,
}

impl VSlider {
    pub fn new(steps: u32, at: u32) -> VSlider {
        VSlider { value: Spring::snappy(at as f32), steps: steps.max(2), last: R::default() }
    }

    /// One step up (`d` = -1) or down (1).
    pub fn step(&mut self, now: Secs, d: i32) {
        let t = (self.value.target + d as f32).clamp(0.0, (self.steps - 1) as f32);
        self.value.set_target(now, t);
    }

    /// -1 for the up arrow, 1 for the down arrow, as last drawn.
    pub fn hit(&self, x: f32, y: f32) -> Option<i32> {
        if !self.last.contains(x, y) {
            return None;
        }
        if y < self.last.y + 34.0 {
            Some(-1)
        } else if y > self.last.bottom() - 34.0 {
            Some(1)
        } else {
            None
        }
    }

    pub fn draw(&mut self, p: &mut Paint, r: R) {
        self.last = r;
        p.glass(r, radius::PILL, color::SMOKE);
        icons::draw(p.cv, Icon::ChevronUp, r.cx(), r.y + 18.0, 14.0, color::INK);
        icons::draw(p.cv, Icon::ChevronDown, r.cx(), r.bottom() - 18.0, 14.0, color::INK);
        let track = R::new(r.cx() - 1.0, r.y + 38.0, 2.0, r.h - 76.0);
        p.cv.fill_rrect(track, 1.0, color::HAIR);
        for i in 0..self.steps {
            let y = track.y + track.h * i as f32 / (self.steps - 1) as f32;
            p.cv.fill_circle(r.cx(), y, 1.4, color::INK_3);
        }
        let v = self.value.at(p.now) / (self.steps - 1) as f32;
        let ty_ = track.y + track.h * v;
        p.cv.glow(r.cx(), ty_, 12.0, color::ACCENT.alpha(0.5));
        p.cv.fill_circle(r.cx(), ty_, 5.0, color::ACCENT);
        p.cv.fill_circle(r.cx(), ty_, 1.8, color::INK);
    }
}
