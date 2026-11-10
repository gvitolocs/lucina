//! The media controls: a glass pill of shuffle, previous, play/pause, next and repeat; a volume line; round icon buttons.
//! A pressed control shrinks a little and a soft disc lights behind it; play and pause cross-fade and turn a quarter of a
//! scale as they swap. Shuffle and repeat keep an accent dot while on.

use crate::anim::{Ease, Secs, Spring, Tween};
use crate::canvas::{Color, R};
use crate::icons::{self, Icon};
use crate::tokens::{color, motion, radius};
use crate::Paint;

use super::cell_x;

#[derive(Clone)]
pub struct MediaControls {
    pub playing: bool,
    pub shuffle: bool,
    pub repeat: bool,
    press: [Tween; 5],
    swap: Tween,
    last: R,
}

const ICONS: [Icon; 5] = [Icon::Shuffle, Icon::Prev, Icon::Play, Icon::Next, Icon::Repeat];

impl MediaControls {
    pub fn new(playing: bool) -> MediaControls {
        MediaControls {
            playing,
            shuffle: false,
            repeat: true,
            press: [Tween::at_rest(0.0); 5],
            swap: Tween::at_rest(1.0),
            last: R::default(),
        }
    }

    /// The control under (`x`, `y`) as last drawn: 0 to 4.
    pub fn hit(&self, x: f32, y: f32) -> Option<usize> {
        if !self.last.contains(x, y) {
            return None;
        }
        (0..5u32).find(|&i| {
            let (cx, cw) = cell_x(self.last.inset_xy(10.0, 0.0), 5, 4.0, i, 1);
            x >= cx && x < cx + cw
        }).map(|i| i as usize)
    }

    pub fn press(&mut self, now: Secs, i: usize) {
        if i < 5 {
            self.press[i] = Tween::new(self.press[i].at(now), 1.0, now, motion::PRESS, Ease::OutCubic);
        }
    }

    /// Release control `i`: it springs back and does what it does.
    pub fn release(&mut self, now: Secs, i: usize) {
        if i >= 5 {
            return;
        }
        self.press[i] = Tween::new(self.press[i].at(now), 0.0, now, motion::MEDIUM, Ease::OutBack(1.6));
        match i {
            0 => self.shuffle = !self.shuffle,
            2 => {
                self.playing = !self.playing;
                self.swap = Tween::new(0.0, 1.0, now, motion::SHORT, Ease::OutCubic);
            }
            4 => self.repeat = !self.repeat,
            _ => {}
        }
    }

    #[allow(clippy::needless_range_loop)]
    pub fn draw(&mut self, p: &mut Paint, r: R) {
        self.last = r;
        p.glass(r, radius::PILL, color::GLASS_TINT);
        let inner = r.inset_xy(10.0, 0.0);
        for i in 0..5usize {
            let (x, w) = cell_x(inner, 5, 4.0, i as u32, 1);
            let (cx, cy) = (x + w / 2.0, r.cy());
            let pr = self.press[i].at(p.now).clamp(-0.2, 1.2);
            let scale = 1.0 - 0.14 * pr;
            let size = if i == 2 { 22.0 } else { 17.0 } * scale;
            if pr > 0.01 {
                p.cv.fill_circle(cx, cy, 17.0, Color::hexa(0xffffff, (40.0 * pr.min(1.0)) as u8));
            }
            if i == 2 {
                p.cv.fill_circle(cx, cy, 19.0 * scale, color::INK);
                let s = self.swap.at(p.now).clamp(0.0, 1.0);
                let (now_icon, was_icon) = if self.playing { (Icon::Pause, Icon::Play) } else { (Icon::Play, Icon::Pause) };
                if s < 1.0 {
                    let o = p.cv.opacity;
                    p.cv.opacity = o * (1.0 - s);
                    icons::draw(p.cv, was_icon, cx, cy, size * (1.0 - 0.3 * s), color::PAPER_INK);
                    p.cv.opacity = o * s;
                    icons::draw(p.cv, now_icon, cx, cy, size * (0.7 + 0.3 * s), color::PAPER_INK);
                    p.cv.opacity = o;
                } else {
                    icons::draw(p.cv, now_icon, cx, cy, size, color::PAPER_INK);
                }
            } else {
                let on = (i == 0 && self.shuffle) || (i == 4 && self.repeat);
                let c = if on || i == 1 || i == 3 { color::INK } else { color::INK_2 };
                icons::draw(p.cv, ICONS[i], cx, cy, size, c);
                if on {
                    p.cv.fill_circle(cx, cy + 13.0, 1.6, color::ACCENT);
                }
            }
        }
    }
}

/// A volume line with a knob that follows a spring.
#[derive(Clone)]
pub struct Volume {
    pub value: Spring,
    last: R,
}

impl Volume {
    pub fn new(v: f32) -> Volume {
        Volume { value: Spring::smooth(v), last: R::default() }
    }

    pub fn set(&mut self, now: Secs, v: f32) {
        self.value.set_target(now, v.clamp(0.0, 1.0));
    }

    /// The value for a pointer at `x`, if it is on the line as last drawn.
    pub fn value_at(&self, x: f32, y: f32) -> Option<f32> {
        self.last.inset_xy(0.0, -8.0).contains(x, y).then(|| ((x - self.last.x - 22.0) / (self.last.w - 22.0)).clamp(0.0, 1.0))
    }

    pub fn draw(&mut self, p: &mut Paint, r: R) {
        self.last = r;
        icons::draw(p.cv, Icon::Volume, r.x + 8.0, r.cy(), 15.0, color::INK_2);
        let line = R::new(r.x + 22.0, r.cy() - 1.5, r.w - 22.0, 3.0);
        p.cv.fill_rrect(line, radius::PILL, color::HAIR);
        let v = self.value.at(p.now).clamp(0.0, 1.0);
        let kx = super::lerp_dp(line.x + 1.5, line.right() - 1.5, v);
        p.cv.fill_rrect(R::new(line.x, line.y, kx - line.x, line.h), radius::PILL, color::INK);
        p.cv.fill_circle(kx, r.cy(), 5.5, color::INK);
    }
}

/// A round glass button with an icon (no label: a toggle whose icon says it; accent while on).
#[derive(Clone)]
pub struct RoundButton {
    pub icon: Icon,
    pub on: bool,
    press: Tween,
    last: R,
}

impl RoundButton {
    pub fn new(icon: Icon) -> RoundButton {
        RoundButton { icon, on: false, press: Tween::at_rest(0.0), last: R::default() }
    }

    pub fn hit(&self, x: f32, y: f32) -> bool {
        self.last.contains(x, y)
    }

    pub fn press(&mut self, now: Secs) {
        self.press = Tween::new(self.press.at(now), 1.0, now, motion::PRESS, Ease::OutCubic);
    }

    pub fn release(&mut self, now: Secs) {
        self.press = Tween::new(self.press.at(now), 0.0, now, motion::MEDIUM, Ease::OutBack(1.8));
        self.on = !self.on;
    }

    pub fn draw(&mut self, p: &mut Paint, cx: f32, cy: f32, rad: f32) {
        let pr = self.press.at(p.now).clamp(-0.3, 1.2);
        let rr = rad * (1.0 - 0.1 * pr);
        let r = R::new(cx - rr, cy - rr, rr * 2.0, rr * 2.0);
        self.last = R::new(cx - rad, cy - rad, rad * 2.0, rad * 2.0);
        if self.on {
            p.cv.fill_circle(cx, cy, rr, color::ACCENT_FILL);
        } else {
            p.glass(r, radius::PILL, color::GLASS_TINT);
        }
        icons::draw(p.cv, self.icon, cx, cy, rr * 0.9, color::INK);
    }
}
