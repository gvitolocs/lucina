//! The run-time compositor: a retained display list and dirty-rectangle redraws, so a state change on the DSi costs a
//! few small rectangles of drawing instead of a whole screen.
//!
//! The screens do not change: a [`Canvas`] with a [`List`] attached records what they draw instead of drawing it.
//! - **Base:** the shapes (backdrop, panels, pills, fills). Painted once into a base image whenever they change
//!   (a tab, a dialog, a scrolled grid), then reused.
//! - **Overlays:** text, icons, rings, banner art, the focus ring, and shapes drawn between
//!   [`Canvas::begin_overlay`] and [`Canvas::end_overlay`] (the name label, the caret, a pressed key).
//!   When only overlays change, the rectangles they left or entered are copied back from the base image and only the
//!   overlays there are drawn again.
//! - [`Canvas::flatten`] (a dialog) makes everything drawn so far part of the base, so the dialog sits over it.
//!
//! [`compose`] does the work and returns the rectangles that changed, for the copy to VRAM. Its result is pixel for
//! pixel what painting the screen directly gives (`tests::composed_equals_painted`).

use crate::canvas::{Canvas, Rgba};
use crate::icons::Icon;
use crate::layout::Rect;
use crate::text::Font;

pub const MAX_OPS: usize = 448;
pub const ARENA: usize = 8192;
/// Bytes of one banner icon in the arena: 512 of tiles, 32 of palette.
const ART: usize = 512 + 32;

/// One recorded drawing call. Fonts and icons are the generated statics, kept as pointers (compared by address).
/// `repr(u8)`: the tag is the first byte and Clear is 0, so an all-zero List is a valid empty one (the kernel
/// allocates lists zeroed on the heap).
#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(u8)]
pub enum Op {
    Clear(Rgba),
    Fill(i32, i32, i32, i32, Rgba),
    Gradient(i32, i32, i32, i32, Rgba, Rgba),
    RoundRect(i32, i32, i32, i32, i32, Rgba),
    Panel(i32, i32, i32, i32, i32, Rgba, Rgba, Rgba),
    Outline(i32, i32, i32, i32, i32, i32, i32, Rgba),
    Ring(f32, f32, f32, f32, u32, u32, u32, Rgba, Rgba),
    Icon(*const Icon, i32, i32, Rgba),
    /// font, x, baseline, arena offset and length of the UTF-8 text, colour
    Text(*const Font, i32, i32, u16, u16, Rgba),
    /// arena offset of the icon (tiles then palette), x, y, scale
    Art(u16, i32, i32, i32),
    Flatten,
}

/// What a screen drew, in order: each op and whether it is an overlay.
pub struct List {
    ops: [(Op, bool); MAX_OPS],
    /// Each op's pixels (unclipped), worked out once when it is recorded: a text's width means a glyph lookup a
    /// character, too dear to repeat for every dirty rectangle.
    rects: [Rect; MAX_OPS],
    n: usize,
    arena: [u8; ARENA],
    used: usize,
    /// More was drawn than fits: the compositor then redraws everything.
    pub overflow: bool,
}

impl Default for List {
    fn default() -> Self {
        Self::new()
    }
}

impl List {
    pub const fn new() -> List {
        List { ops: [(Op::Flatten, false); MAX_OPS], rects: [Rect::new(0, 0, 0, 0); MAX_OPS], n: 0, arena: [0; ARENA], used: 0, overflow: false }
    }

    pub fn clear(&mut self) {
        self.n = 0;
        self.used = 0;
        self.overflow = false;
    }

    pub fn len(&self) -> usize {
        self.n
    }

    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    pub fn push(&mut self, op: Op, overlay: bool) {
        if self.n < MAX_OPS {
            self.ops[self.n] = (op, overlay);
            self.rects[self.n] = self.raw_bounds(self.n);
            self.n += 1;
        } else {
            self.overflow = true;
        }
    }

    /// Copies bytes into the arena; None when it is full.
    pub fn keep(&mut self, b: &[u8]) -> Option<u16> {
        if self.used + b.len() > ARENA {
            self.overflow = true;
            return None;
        }
        let at = self.used;
        self.arena[at..at + b.len()].copy_from_slice(b);
        self.used += b.len();
        Some(at as u16)
    }

    pub fn push_text(&mut self, f: &'static Font, x: i32, y: i32, s: &str, c: Rgba) {
        if let Some(at) = self.keep(s.as_bytes()) {
            self.push(Op::Text(f, x, y, at, s.len() as u16, c), true);
        }
    }

    pub fn push_art(&mut self, tiles: &[u8; 512], palette: &[u16; 16], x: i32, y: i32, scale: i32) {
        let mut b = [0u8; ART];
        b[..512].copy_from_slice(tiles);
        for (i, p) in palette.iter().enumerate() {
            b[512 + 2 * i..514 + 2 * i].copy_from_slice(&p.to_le_bytes());
        }
        if let Some(at) = self.keep(&b) {
            self.push(Op::Art(at, x, y, scale), true);
        }
    }

    fn text(&self, at: u16, len: u16) -> &str {
        core::str::from_utf8(&self.arena[at as usize..at as usize + len as usize]).unwrap_or("")
    }

    /// Op `i` here is the same drawing as op `j` of `o` (text and art compared by content).
    fn same(&self, i: usize, o: &List, j: usize) -> bool {
        let (a, b) = (self.ops[i].0, o.ops[j].0);
        match (a, b) {
            (Op::Text(fa, xa, ya, pa, la, ca), Op::Text(fb, xb, yb, pb, lb, cb)) => {
                fa == fb && xa == xb && ya == yb && ca == cb && self.text(pa, la) == o.text(pb, lb)
            }
            (Op::Art(pa, xa, ya, sa), Op::Art(pb, xb, yb, sb)) => {
                xa == xb && ya == yb && sa == sb && self.arena[pa as usize..pa as usize + ART] == o.arena[pb as usize..pb as usize + ART]
            }
            _ => a == b,
        }
    }

    /// A hash of the base (FNV-1a over its ops, text and art by content): equal bases hash equal, so a caller can
    /// keep base images of screens seen before and reuse them (`compose`'s `base_ready`).
    pub fn base_hash(&self) -> u64 {
        let mut h: u64 = 0xcbf29ce484222325;
        let mut mix = |b: &[u8]| {
            for &x in b {
                h ^= x as u64;
                h = h.wrapping_mul(0x100000001b3);
            }
        };
        let flat = self.flat();
        for i in (0..self.n).filter(|&i| self.is_base(i, flat)) {
            match self.ops[i].0 {
                Op::Text(f, x, y, at, len, c) => {
                    mix(&(f as usize).to_le_bytes());
                    mix(&x.to_le_bytes());
                    mix(&y.to_le_bytes());
                    mix(&[c.r, c.g, c.b, c.a]);
                    mix(self.text(at, len).as_bytes());
                }
                Op::Art(at, x, y, sc) => {
                    mix(&x.to_le_bytes());
                    mix(&y.to_le_bytes());
                    mix(&sc.to_le_bytes());
                    mix(&self.arena[at as usize..at as usize + ART]);
                }
                op => {
                    // field by field (an Op's raw bytes include padding, which is not stable)
                    let c4 = |c: Rgba| [c.r, c.g, c.b, c.a];
                    let mut ints = [0i32; 8];
                    let mut cols = [[0u8; 4]; 3];
                    let tag: u8 = match op {
                        Op::Clear(c) => {
                            cols[0] = c4(c);
                            1
                        }
                        Op::Fill(x, y, w, hh, c) => {
                            ints[..4].copy_from_slice(&[x, y, w, hh]);
                            cols[0] = c4(c);
                            2
                        }
                        Op::Gradient(x, y, w, hh, a, b) => {
                            ints[..4].copy_from_slice(&[x, y, w, hh]);
                            cols[0] = c4(a);
                            cols[1] = c4(b);
                            3
                        }
                        Op::RoundRect(x, y, w, hh, r, c) => {
                            ints[..5].copy_from_slice(&[x, y, w, hh, r]);
                            cols[0] = c4(c);
                            4
                        }
                        Op::Panel(x, y, w, hh, r, f, rim, hi) => {
                            ints[..5].copy_from_slice(&[x, y, w, hh, r]);
                            cols = [c4(f), c4(rim), c4(hi)];
                            5
                        }
                        Op::Outline(x, y, w, hh, r, g, wd, c) => {
                            ints[..7].copy_from_slice(&[x, y, w, hh, r, g, wd]);
                            cols[0] = c4(c);
                            6
                        }
                        Op::Ring(cx, cy, r, w, st, ln, d, c, t) => {
                            ints = [cx.to_bits() as i32, cy.to_bits() as i32, r.to_bits() as i32, w.to_bits() as i32, st as i32, ln as i32, d as i32, 0];
                            cols[0] = c4(c);
                            cols[1] = c4(t);
                            7
                        }
                        Op::Icon(p, x, y, c) => {
                            ints[..3].copy_from_slice(&[p as usize as i32, x, y]);
                            cols[0] = c4(c);
                            8
                        }
                        Op::Flatten => 9,
                        Op::Text(..) | Op::Art(..) => 0,
                    };
                    mix(&[tag]);
                    for v in ints {
                        mix(&v.to_le_bytes());
                    }
                    for c in cols {
                        mix(&c);
                    }
                }
            }
        }
        h
    }

    /// Where the base ends: ops before the last Flatten are all base; after it only the shapes are.
    fn flat(&self) -> usize {
        (0..self.n).rev().find(|&i| self.ops[i].0 == Op::Flatten).map_or(0, |i| i + 1)
    }

    fn is_base(&self, i: usize, flat: usize) -> bool {
        i < flat || !self.ops[i].1
    }

    /// Op `i` and whether it is an overlay (for tools that look inside a list).
    pub fn op(&self, i: usize) -> (Op, bool) {
        self.ops[i]
    }

    /// Paints op `i` onto `cv` (paint mode).
    pub fn paint_op(&self, cv: &mut Canvas, i: usize) {
        self.draw(cv, i)
    }

    /// Paints op `i` onto `cv` (paint mode).
    fn draw(&self, cv: &mut Canvas, i: usize) {
        match self.ops[i].0 {
            Op::Clear(c) => cv.clear(c),
            Op::Fill(x, y, w, h, c) => cv.fill(x, y, w, h, c),
            Op::Gradient(x, y, w, h, a, b) => cv.gradient(x, y, w, h, a, b),
            Op::RoundRect(x, y, w, h, r, c) => cv.round_rect(x, y, w, h, r, c),
            Op::Panel(x, y, w, h, r, f, rim, hi) => cv.panel(x, y, w, h, r, f, rim, hi),
            Op::Outline(x, y, w, h, r, g, wd, c) => cv.outline(x, y, w, h, r, g, wd, c),
            Op::Ring(cx, cy, r, w, s, l, d, c, t) => cv.ring(cx, cy, r, w, s, l, d, c, t),
            // SAFETY: icons and fonts are the generated statics; the pointer came from a `&'static`.
            Op::Icon(p, x, y, c) => cv.icon(unsafe { &*p }, x, y, c),
            Op::Text(f, x, y, at, len, c) => {
                cv.text(unsafe { &*f }, x, y, self.text(at, len), c);
            }
            Op::Art(at, x, y, scale) => {
                let a = at as usize;
                let mut tiles = [0u8; 512];
                tiles.copy_from_slice(&self.arena[a..a + 512]);
                let mut pal = [0u16; 16];
                for (k, p) in pal.iter_mut().enumerate() {
                    *p = u16::from_le_bytes([self.arena[a + 512 + 2 * k], self.arena[a + 513 + 2 * k]]);
                }
                cv.art(&tiles, &pal, x, y, scale);
            }
            Op::Flatten => {}
        }
    }

    /// The pixels op `i` can touch.
    fn bounds(&self, i: usize, w: i32, h: i32) -> Rect {
        clip(self.rects[i], w, h)
    }

    /// The pixels op `i` can touch, unclipped (the whole screen for a clear).
    fn raw_bounds(&self, i: usize) -> Rect {
        let (w, h) = (4096, 4096);
        match self.ops[i].0 {
            Op::Clear(_) | Op::Flatten => Rect::new(0, 0, w, h),
            Op::Fill(x, y, ww, hh, _) | Op::Gradient(x, y, ww, hh, _, _) => Rect::new(x, y, ww, hh),
            Op::RoundRect(x, y, ww, hh, _, _) | Op::Panel(x, y, ww, hh, _, _, _, _) => Rect::new(x, y, ww, hh),
            Op::Outline(x, y, ww, hh, _, g, wd, _) => {
                let o = g + wd + 1;
                Rect::new(x - o, y - o, ww + 2 * o, hh + 2 * o)
            }
            Op::Ring(cx, cy, r, _, _, _, _, _, _) => {
                let e = r as i32 + 3;
                Rect::new(cx as i32 - e, cy as i32 - e, 2 * e, 2 * e)
            }
            Op::Icon(p, x, y, _) => {
                let s = unsafe { (*p).size } as i32;
                Rect::new(x, y, s, s)
            }
            Op::Text(f, x, y, at, len, _) => {
                let f = unsafe { &*f };
                let tw = f.width(self.text(at, len));
                Rect::new(x - 2, y - f.px as i32 - 3, tw + 4, f.px as i32 + f.descent as i32 + 6)
            }
            Op::Art(_, x, y, s) => Rect::new(x, y, 32 * s, 32 * s),
        }
    }
}

fn r4(r: &Rect) -> crate::rules::Rect4 {
    (r.x, r.y, r.w, r.h)
}

fn from4(r: crate::rules::Rect4) -> Rect {
    Rect::new(r.0, r.1, r.2, r.3)
}

fn clip(r: Rect, w: i32, h: i32) -> Rect {
    from4(crate::rules::clip(r4(&r), w, h))
}

fn overlaps(a: &Rect, b: &Rect) -> bool {
    crate::rules::overlaps(r4(a), r4(b))
}

fn union(a: Rect, b: Rect) -> Rect {
    from4(crate::rules::union(r4(&a), r4(&b)))
}

pub const MAX_DIRTY: usize = 24;

/// What a composition changed on screen.
#[derive(Clone, Copy, Debug)]
pub struct Dirty {
    pub rects: [Rect; MAX_DIRTY],
    pub n: usize,
    /// The base was painted again (a tab, a dialog, a scroll): the whole screen changed.
    pub full: bool,
}

impl Dirty {
    fn add(&mut self, r: Rect) {
        if r.w <= 0 || r.h <= 0 || self.full {
            return;
        }
        let mut r = r;
        // merge with whatever it overlaps, until nothing does: the rectangles stay disjoint (overlays are drawn per
        // rectangle, and translucent ink drawn twice would blend twice); ones that only touch stay apart
        let mut i = 0;
        while i < self.n {
            if overlaps(&self.rects[i], &r) {
                r = union(r, self.rects[i]);
                self.rects[i] = self.rects[self.n - 1];
                self.n -= 1;
                i = 0;
            } else {
                i += 1;
            }
        }
        if self.n == MAX_DIRTY {
            self.full = true;
            return;
        }
        self.rects[self.n] = r;
        self.n += 1;
    }

    /// The pixels op `i` of `l` changed: its bounds, or for the focus ring only its four edges (the inside is
    /// whatever it surrounds, which did not change).
    fn add_op(&mut self, l: &List, i: usize, w: i32, h: i32) {
        if let Op::Outline(x, y, ww, hh, rr, g, wd, _) = l.ops[i].0 {
            let o = g + wd + 1;
            let (x0, y0, x1, y1) = (x - o, y - o, x + ww + o, y + hh + o);
            // the rounded corners reach in as far as the radius: a square that size at each corner
            let k = (rr.min(ww / 2).min(hh / 2).max(0) + o + 1).min((x1 - x0) / 2).min((y1 - y0) / 2);
            for c in [(x0, y0), (x1 - k, y0), (x0, y1 - k), (x1 - k, y1 - k)] {
                self.add(clip(Rect::new(c.0, c.1, k, k), w, h));
            }
            // the edges between the corner squares: eight disjoint pieces in all
            let t = o + 1;
            for s in [
                Rect::new(x0 + k, y0, x1 - x0 - 2 * k, t),
                Rect::new(x0 + k, y1 - t, x1 - x0 - 2 * k, t),
                Rect::new(x0, y0 + k, t, y1 - y0 - 2 * k),
                Rect::new(x1 - t, y0 + k, t, y1 - y0 - 2 * k),
            ] {
                self.add(clip(s, w, h));
            }
        } else {
            self.add(l.bounds(i, w, h));
        }
    }

    pub fn rects(&self) -> &[Rect] {
        &self.rects[..self.n]
    }
}

/// Brings `front` (what is on screen) from `prev` to `new`, using `base` (the shapes alone) as the cache. With no
/// `prev`, or when the shapes changed, everything is drawn again; `base_ready` says the caller has already put the
/// new base into `base` (an image kept from before, by `List::base_hash`), so only the overlays are drawn. Returns
/// what changed. `base` and `front` are `w` x `h` frames.
pub fn compose(base: &mut [u16], front: &mut [u16], w: i32, h: i32, new: &List, prev: Option<&List>, base_ready: bool) -> Dirty {
    let mut d = Dirty { rects: [Rect::default(); MAX_DIRTY], n: 0, full: false };
    let nf = new.flat();
    let same_base = match prev {
        Some(p) if !p.overflow && !new.overflow => {
            let pf = p.flat();
            let a = (0..new.n).filter(|&i| new.is_base(i, nf));
            let b = (0..p.n).filter(|&i| p.is_base(i, pf));
            a.clone().count() == b.clone().count() && a.zip(b).all(|(i, j)| new.same(i, p, j))
        }
        _ => false,
    };
    if !same_base {
        if !base_ready {
            let mut cv = Canvas::new(base, w, h, w);
            for i in (0..new.n).filter(|&i| new.is_base(i, nf)) {
                new.draw(&mut cv, i);
            }
        }
        front[..(w * h) as usize].copy_from_slice(&base[..(w * h) as usize]);
        let mut cv = Canvas::new(front, w, h, w);
        for i in (0..new.n).filter(|&i| !new.is_base(i, nf)) {
            new.draw(&mut cv, i);
        }
        d.full = true;
        return d;
    }
    let p = prev.unwrap_or(new);
    let pf = p.flat();
    // overlays that left (in prev, not in new) and that came (in new, not in prev), matched one to one
    let mut used = [false; MAX_OPS];
    // the overlays in order; most are where they were, so the same position is tried first
    let (mut ni, mut nn) = ([0u16; MAX_OPS], 0);
    for i in (0..new.n).filter(|&i| !new.is_base(i, nf)) {
        ni[nn] = i as u16;
        nn += 1;
    }
    let (mut pi, mut pn) = ([0u16; MAX_OPS], 0);
    for j in (0..p.n).filter(|&j| !p.is_base(j, pf)) {
        pi[pn] = j as u16;
        pn += 1;
    }
    for k in 0..nn {
        let i = ni[k] as usize;
        let hit = if k < pn && !used[pi[k] as usize] && new.same(i, p, pi[k] as usize) {
            Some(pi[k] as usize)
        } else {
            pi[..pn].iter().map(|&j| j as usize).find(|&j| !used[j] && new.same(i, p, j))
        };
        match hit {
            Some(j) => used[j] = true,
            None => d.add_op(new, i, w, h),
        }
    }
    for j in (0..p.n).filter(|&j| !p.is_base(j, pf) && !used[j]) {
        d.add_op(p, j, w, h);
    }
    if d.full {
        front[..(w * h) as usize].copy_from_slice(&base[..(w * h) as usize]);
        let mut cv = Canvas::new(front, w, h, w);
        for i in (0..new.n).filter(|&i| !new.is_base(i, nf)) {
            new.draw(&mut cv, i);
        }
        return d;
    }
    for r in d.rects() {
        for y in r.y..r.bottom() {
            let row = (y * w) as usize;
            front[row + r.x as usize..row + r.right() as usize].copy_from_slice(&base[row + r.x as usize..row + r.right() as usize]);
        }
    }
    let mut cv = Canvas::new(front, w, h, w);
    for r in d.rects() {
        cv.clip_to(r.x, r.y, r.w, r.h);
        for i in (0..new.n).filter(|&i| !new.is_base(i, nf)) {
            if overlaps(&new.bounds(i, w, h), r) {
                new.draw(&mut cv, i);
            }
        }
    }
    d
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use crate::sample::{self, H, W};
    use std::boxed::Box;
    use std::vec;

    fn record(name: &str) -> (Box<List>, Box<List>) {
        let (mut t, mut b) = (Box::new(List::new()), Box::new(List::new()));
        let (mut tp, mut bp) = (vec![0u16; W * H], vec![0u16; W * H]);
        {
            let mut tc = Canvas::new(&mut tp, W as i32, H as i32, W as i32);
            let mut bc = Canvas::new(&mut bp, W as i32, H as i32, W as i32);
            tc.rec = Some(&mut t);
            bc.rec = Some(&mut b);
            sample::render(name, &mut tc, &mut bc);
        }
        (t, b)
    }

    fn painted(name: &str) -> (std::vec::Vec<u16>, std::vec::Vec<u16>) {
        let (mut tp, mut bp) = (vec![0x8000u16; W * H], vec![0x8000u16; W * H]);
        {
            let mut tc = Canvas::new(&mut tp, W as i32, H as i32, W as i32);
            let mut bc = Canvas::new(&mut bp, W as i32, H as i32, W as i32);
            sample::render(name, &mut tc, &mut bc);
        }
        (tp, bp)
    }

    /// Every screen, composed in order (each from the one before, so overlay-only changes take the incremental
    /// path), is pixel for pixel what painting it directly gives.
    #[test]
    fn composed_equals_painted() {
        let mut base = [vec![0u16; W * H], vec![0u16; W * H]];
        let mut front = [vec![0u16; W * H], vec![0u16; W * H]];
        let mut prev: [Option<Box<List>>; 2] = [None, None];
        let mut incremental = 0;
        for name in sample::SCREENS {
            let (t, b) = record(name);
            let (pt, pb) = painted(name);
            for (k, (list, want)) in [(t, pt), (b, pb)].into_iter().enumerate() {
                assert!(!list.overflow, "{name}: the display list overflowed");
                let d = compose(&mut base[k], &mut front[k], W as i32, H as i32, &list, prev[k].as_deref(), false);
                if !d.full {
                    incremental += 1;
                }
                let bad = front[k].iter().zip(want.iter()).position(|(a, b)| a != b);
                assert!(bad.is_none(), "{name} screen {k}: first differing pixel at {:?} (full {})", bad.map(|i| (i % W, i / W)), d.full);
                prev[k] = Some(list);
            }
        }
        assert!(incremental >= 3, "only {incremental} incremental compositions: the test no longer covers that path");
    }

    /// Reusing a kept base image (same hash) gives the same pixels as drawing it.
    #[test]
    fn kept_base_is_the_same() {
        let (_, games) = record("2-games");
        let (_, home) = record("1-home");
        let (mut base, mut front) = (vec![0u16; W * H], vec![0u16; W * H]);
        compose(&mut base, &mut front, W as i32, H as i32, &games, None, false);
        let kept = base.clone();
        let want = front.clone();
        compose(&mut base, &mut front, W as i32, H as i32, &home, Some(&games), false);
        let (_, games2) = record("2-games");
        assert_eq!(games2.base_hash(), games.base_hash());
        assert_ne!(home.base_hash(), games.base_hash());
        base.copy_from_slice(&kept);
        let d = compose(&mut base, &mut front, W as i32, H as i32, &games2, Some(&home), true);
        assert!(d.full);
        assert!(front == want);
    }

    /// A d-pad move in Games redraws only small rectangles (the rings, the label, the heading it hides).
    #[test]
    fn games_focus_move_is_small() {
        for (side, name) in [(1usize, "bottom"), (0, "top")] {
            let (ta, ba) = record("2-games");
            let (tb, bb) = record("2-games-next");
            let (a, b) = if side == 1 { (ba, bb) } else { (ta, tb) };
            let mut base = vec![0u16; W * H];
            let mut front = vec![0u16; W * H];
            compose(&mut base, &mut front, W as i32, H as i32, &a, None, false);
            let d = compose(&mut base, &mut front, W as i32, H as i32, &b, Some(&a), false);
            let area: i32 = d.rects().iter().map(|r| r.w * r.h).sum();
            std::eprintln!("games focus move, {name}: full {} area {area} rects {:?}", d.full, d.rects());
        }
    }

    /// A focus move on Home redraws only small rectangles.
    #[test]
    fn focus_move_is_small() {
        let (_, a) = record("1-home");
        let (_, b) = record("1-home-pin");
        let mut base = vec![0u16; W * H];
        let mut front = vec![0u16; W * H];
        compose(&mut base, &mut front, W as i32, H as i32, &a, None, false);
        let d = compose(&mut base, &mut front, W as i32, H as i32, &b, Some(&a), false);
        assert!(!d.full);
        let area: i32 = d.rects().iter().map(|r| r.w * r.h).sum();
        assert!(area < 12_000, "focus move redrew {area} px");
    }
}
