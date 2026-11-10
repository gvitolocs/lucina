//! Fortuna, the DSi: two 256 x 192 screens at 1 px/dp, the bottom one touch. One picture holds both, top over bottom, with
//! the hinge between them.
//!
//! What changes for the hardware: 15-bit colour, a few MB to spare, a slow CPU.
//! - **Surfaces:** no glass, blur or photographs. Flat carbon panels with a hairline.
//! - **Colour:** every pixel ends in RGB555, through a 4 x 4 ordered dither, so gradients band evenly instead of
//!   posterising.
//! - **Type:** Light instead of Thin for the display word, since a 1 px thin stroke breaks up at this size. Mono at 8 px.
//! - **Motion is cut down:**
//!   - state changes snap; springs are read once they have settled;
//!   - the LIVE pill blinks once a second instead of breathing;
//!   - the timer's digits change without rolling, and its ring moves once a second;
//!   - no stagger and no count-up: values show at once.
//! - **Split:** the top screen shows (header with the phase, the timer); the bottom screen takes touch (days, media, the
//!   four actions, the latest notification with its count).

use crate::anim::Secs;
use crate::canvas::{Canvas, Color, R};
use crate::screens::content as c;
use crate::text::{Font, Type};
use crate::tokens::{color, radius};
use crate::Paint;

use super::blocks::Parts;
use super::Layout;

pub struct Fortuna {
    pub parts: Parts,
    panes: Vec<(R, R)>,
}

/// The two screens and the hinge between them, in px.
pub const TOP: R = R::new(0.0, 0.0, 256.0, 192.0);
pub const HINGE: f32 = 16.0;
pub const BOTTOM: R = R::new(0.0, 192.0 + HINGE, 256.0, 192.0);

impl Fortuna {
    pub fn new(now: Secs) -> Fortuna {
        let mut parts = Parts::new(now - 10.0, 4, 2);
        // No count-ups: the values are there from the start.
        parts.percent.dur = 0.0;
        parts.track = crate::widgets::ProgressTrack::new(3, 2, 0.76);
        Fortuna { parts, panes: Vec::new() }
    }

    /// A tap on the bottom screen at (`x`, `y`) px of that screen, applied as already settled.
    pub fn tap(&mut self, now: Secs, x: f32, y: f32) {
        let (x, y) = (x + BOTTOM.x, y + BOTTOM.y);
        let past = now - 10.0;
        if let Some(i) = self.parts.days.hit(x, y) {
            self.parts.days.select(past, i);
        } else if let Some(i) = self.parts.media.hit(x, y) {
            self.parts.media.press(past, i);
            self.parts.media.release(past, i);
        } else if let Some(i) = self.parts.pager.hit(x, y) {
            self.parts.pager.go(past, i);
        }
    }
}

/// 4 x 4 Bayer thresholds, 0..15.
const BAYER: [[u8; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];

/// Every pixel to RGB555 (each channel a multiple of 8), with an ordered dither.
pub fn to_rgb555(cv: &mut Canvas) {
    let w = cv.w as usize;
    for (i, p) in cv.px.iter_mut().enumerate() {
        let (x, y) = (i % w, i / w);
        let t = BAYER[y % 4][x % 4] as i32 / 2 - 4;
        let ch = |shift: u32| {
            let v = ((*p >> shift) & 0xff) as i32 + t;
            (v.clamp(0, 255) as u32 & 0xf8) << shift
        };
        *p = ch(16) | ch(8) | ch(0);
    }
}

impl Layout for Fortuna {
    fn name(&self) -> &'static str {
        "fortuna"
    }

    fn size(&self) -> (u32, u32, f32) {
        (256, 192 * 2 + HINGE as u32, 1.0)
    }

    fn panes(&self) -> Vec<(R, R)> {
        self.panes.clone()
    }

    fn draw(&mut self, cv: &mut Canvas, ty: &mut Type, now: Secs) {
        // Time moves in whole seconds here, at the middle of each: nothing is caught mid-move.
        let now = now.floor() + 0.5;
        cv.clear(Color::hex(0x2a2b30));
        let parts = &mut self.parts;
        let mut p = Paint { cv, ty, now, backdrop: None };
        let panel = |p: &mut Paint, r: R| {
            p.cv.fill_rrect(r, 10.0, color::CARBON_HI);
            p.cv.stroke_rrect(r, 10.0, 1.0, Color::hexa(0xffffff, 26));
        };

        // Top screen.
        p.cv.push_clip(TOP, 0.0);
        p.cv.gradient_rrect(TOP, 0.0, Color::hex(0x0b1630), Color::hex(0x020306));
        p.text(Font::Mono, (8.0, 0.4), 6.0, 10.0, color::INK_2, "9:41");
        p.cv.fill_rrect(R::new(232.0, 3.0, 17.0, 8.0), 2.0, color::INK_2);
        let head = R::new(6.0, 15.0, 244.0, 102.0);
        panel(&mut p, head);
        // LIVE blinks once a second: filled on even seconds, outlined on odd ones.
        if (now as i64) % 2 == 0 {
            let pill = R::new(head.right() - 44.0, head.y + 6.0, 36.0, 13.0);
            p.cv.fill_rrect(pill, radius::PILL, color::LIVE);
            p.text_centered(Font::Semibold, (8.0, 0.2), pill.cx(), pill.y + 9.5, color::INK, c::LIVE);
        } else {
            let pill = R::new(head.right() - 44.0, head.y + 6.0, 36.0, 13.0);
            p.cv.stroke_rrect(pill, radius::PILL, 1.0, color::LIVE);
            p.text_centered(Font::Semibold, (8.0, 0.2), pill.cx(), pill.y + 9.5, color::LIVE, c::LIVE);
        }
        p.text(Font::Light, (40.0, -0.8), head.x + 8.0, head.y + 60.0, color::INK, c::BUILD_TITLE);
        p.text(Font::Regular, (8.0, 0.0), head.x + 10.0, head.bottom() - 10.0, color::INK_2, c::PHASE);
        parts.track.draw(&mut p, R::new(head.x + 70.0, head.bottom() - 17.0, 110.0, 8.0));
        parts.percent.draw(&mut p, head.right() - 10.0, head.bottom() - 9.0, Font::Light, 20.0, color::INK);
        let timer = R::new(6.0, 124.0, 244.0, 62.0);
        panel(&mut p, timer);
        p.text(Font::Regular, (8.0, 0.0), timer.x + 10.0, timer.y + 14.0, color::INK_2, c::UPLOADING);
        parts.timer.draw_digits(&mut p, timer.x + 8.0, timer.bottom() - 10.0, 28.0, color::INK);
        parts.timer.draw_ring(&mut p, timer.right() - 30.0, timer.cy(), 20.0);
        p.cv.pop_clip();

        // The hinge.
        p.cv.fill_rrect(R::new(0.0, 192.0, 256.0, HINGE), 0.0, Color::hex(0x2a2b30));
        p.cv.fill_rrect(R::new(118.0, 192.0 + HINGE / 2.0 - 1.0, 20.0, 2.0), 1.0, Color::hex(0x55565c));

        // Bottom screen: touch.
        p.cv.push_clip(BOTTOM, 0.0);
        p.cv.fill_rrect(BOTTOM, 0.0, Color::hex(0x07080a));
        let b = BOTTOM;
        parts.days.draw(&mut p, R::new(b.x + 6.0, b.y + 6.0, 244.0, 24.0));
        parts.media.draw(&mut p, R::new(b.x + 6.0, b.y + 36.0, 244.0, 40.0));
        super::blocks::actions(&mut p, R::new(b.x + 6.0, b.y + 82.0, 244.0, 64.0), parts);
        // The latest notification: one fact, the red mark, the count.
        let card = R::new(b.x + 6.0, b.y + 152.0, 244.0, 34.0);
        panel(&mut p, card);
        p.cv.fill_circle(card.x + 14.0, card.cy(), 5.0, color::LIVE);
        p.text(Font::Regular, (9.0, 0.0), card.x + 26.0, card.cy() + 3.0, color::INK, c::CARDS[0].0);
        p.text_right(Font::Semibold, (9.0, 0.0), card.right() - 10.0, card.cy() + 3.0, color::LIVE, &c::PENDING.to_string());
        p.cv.pop_clip();

        to_rgb555(p.cv);
        self.panes = vec![(head, TOP), (timer, TOP), (card, BOTTOM)];
    }
}
