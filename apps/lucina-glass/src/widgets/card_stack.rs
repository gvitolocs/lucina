//! The stacked notification card: the top card in full, two more peeking below it, each smaller and dimmer. A swipe throws
//! the top card sideways on a spring; as it leaves, the cards behind move up one place in proportion, and once it is gone it
//! goes to the back of the stack. A drag that does not go far enough springs back.

use crate::anim::{Secs, Spring};
use crate::canvas::{Color, R};
use crate::icons::{self, Icon};
use crate::text::Font;
use crate::tokens::{color, radius, space, ty};
use crate::Paint;

/// One notification: a fact and one short secondary line (docs/design/rules.md, rule 1).
#[derive(Clone)]
pub struct Card {
    pub fact: String,
    pub secondary: String,
    /// Urgent: the mark is red (rule 10); otherwise the accent.
    pub urgent: bool,
}

#[derive(Clone)]
pub struct CardStack {
    pub cards: Vec<Card>,
    pub top: usize,
    /// The top card's horizontal offset, dp.
    x: Spring,
    /// Where a throw is going (sign), while it flies.
    flying: Option<f32>,
    last: R,
}

/// How far each card behind sits below the one in front, and how much smaller.
const PEEK: f32 = 11.0;
const SHRINK: f32 = 0.055;

impl CardStack {
    pub fn new(cards: Vec<Card>) -> CardStack {
        CardStack { cards, top: 0, x: Spring::new(0.0, 260.0, 30.0), flying: None, last: R::default() }
    }

    pub fn hit(&self, x: f32, y: f32) -> bool {
        self.last.contains(x, y)
    }

    /// Follow a finger: the top card sits `dx` from its place.
    pub fn drag(&mut self, now: Secs, dx: f32) {
        self.x.snap(now, dx);
    }

    /// The finger lifted at `dx` moving at `vx` dp/s: throw the card, or let it spring back.
    pub fn release(&mut self, now: Secs, dx: f32, vx: f32) {
        let w = self.last.w.max(1.0);
        if dx.abs() > w * 0.33 || vx.abs() > 900.0 {
            self.throw(now, if dx + vx * 0.1 >= 0.0 { 1.0 } else { -1.0 }, vx);
        } else {
            self.x.set_target(now, 0.0);
            self.x.fling(now, vx);
        }
    }

    /// Throw the top card away to the right (`dir` 1) or left (-1).
    pub fn throw(&mut self, now: Secs, dir: f32, vx: f32) {
        let w = self.last.w.max(300.0);
        self.flying = Some(dir);
        self.x.set_target(now, dir * w * 1.25);
        self.x.fling(now, vx);
    }

    /// The cards behind move up as the top one leaves: 0 at rest, 1 once it is a card-width away.
    fn promote(&self, now: Secs) -> f32 {
        (self.x.at(now).abs() / self.last.w.max(1.0)).clamp(0.0, 1.0)
    }

    pub fn draw(&mut self, p: &mut Paint, r: R) {
        self.last = r;
        let n = self.cards.len();
        if n == 0 {
            return;
        }
        // A thrown card that is out of sight goes to the back.
        if let Some(dir) = self.flying {
            let x = self.x.at(p.now);
            if x * dir >= r.w * 1.1 {
                self.top = (self.top + 1) % n;
                self.flying = None;
                self.x.snap(p.now, 0.0);
            }
        }
        let promote = self.promote(p.now);
        let shown = n.min(3);
        for d in (0..shown).rev() {
            let idx = (self.top + d) % n;
            let depth = if d == 0 { 0.0 } else { d as f32 - promote };
            let s = 1.0 - SHRINK * depth;
            let cw = r.w * s;
            let mut cr = R::new(r.cx() - cw / 2.0, r.y + PEEK * depth * 1.0, cw, r.h * s);
            cr.y += r.h * (1.0 - s);
            let mut alpha = 1.0 - 0.28 * depth;
            if d == 0 {
                let x = self.x.at(p.now);
                cr = cr.offset(x, x.abs() * 0.06);
                alpha *= 1.0 - (x.abs() / (r.w * 1.2)).clamp(0.0, 1.0) * 0.8;
            }
            let o = p.cv.opacity;
            p.cv.opacity = o * alpha.clamp(0.0, 1.0);
            draw_card(p, &self.cards[idx], cr, d == 0);
            p.cv.opacity = o;
        }
    }
}

fn draw_card(p: &mut Paint, c: &Card, r: R, front: bool) {
    p.shadow(r, radius::LG, 14.0);
    p.cv.gradient_rrect(r, radius::LG, Color::hex(0x1d1f25), Color::hex(0x101115));
    p.cv.stroke_rrect(r, radius::LG, 0.7, Color::hexa(0xffffff, 26));
    if !front {
        return;
    }
    let mark = R::new(r.x + space::L, r.cy() - 18.0, 36.0, 36.0);
    p.cv.fill_rrect(mark, radius::SM, if c.urgent { color::LIVE } else { color::ACCENT_FILL });
    icons::draw(p.cv, Icon::Bell, mark.cx(), mark.cy(), 19.0, color::INK);
    let x = mark.right() + space::M;
    // The card's padding on the right too: long words end with "…" inside it.
    let room = r.right() - space::L - x;
    let fact = p.fit(Font::Regular, ty::HEADLINE, &c.fact, room);
    let line = p.fit(Font::Regular, ty::CAPTION, &c.secondary, room);
    p.text(Font::Regular, ty::HEADLINE, x, r.cy() - 3.0, color::INK, &fact);
    p.text(Font::Regular, ty::CAPTION, x, r.cy() + 16.0, color::INK_2, &line);
}
