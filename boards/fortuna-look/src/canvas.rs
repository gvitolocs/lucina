//! A software canvas over a 16-bit DS frame (BGR555, bit 15 set) in RAM. Colours are 8-bit with alpha ([`Rgba`]):
//! every write blends in 8-bit space and goes back to 5 bits through the 4x4 ordered dither the Glass concept uses on
//! Fortuna (lucina-glass `layouts/fortuna.rs`), so gradients and translucent materials band evenly instead of
//! posterising. Shapes: rectangles, rounded panels with a hairline rim and a top highlight (anti-aliased corners from
//! `corner`), circles and progress rings, 4-bit icons and text. The kernel draws into a RAM frame with this and copies it
//! to VRAM in the vertical blank (fortuna/docs/PROGRAMMING.md, section 3).
//!
//! With an [`Audit`] attached, every text run records its contrast against the pixels it actually lands on, so a test
//! can hold every screen to 4.5:1 (3:1 for large or bold text) after dithering.

use crate::compose::{List, Op};
use crate::corner;
use crate::icons::Icon;
use crate::text::Font;

/// An 8-bit colour with alpha.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    pub const fn new(r: u8, g: u8, b: u8, a: u8) -> Rgba {
        Rgba { r, g, b, a }
    }
    /// Opaque, from `0xRRGGBB`.
    pub const fn hex(v: u32) -> Rgba {
        Rgba { r: (v >> 16) as u8, g: (v >> 8) as u8, b: v as u8, a: 255 }
    }
    /// The same colour at alpha `a`.
    pub const fn alpha(self, a: u8) -> Rgba {
        Rgba { a, ..self }
    }
    /// A DS colour, widened (`(c << 3) | (c >> 2)` per channel).
    pub const fn from555(c: u16) -> Rgba {
        Rgba { r: widen(c), g: widen(c >> 5), b: widen(c >> 10), a: 255 }
    }
    /// `self` over the opaque `under`.
    pub const fn over(self, under: Rgba) -> Rgba {
        let a = self.a as u32;
        Rgba { r: mix8(self.r, under.r, a), g: mix8(self.g, under.g, a), b: mix8(self.b, under.b, a), a: 255 }
    }
}

const fn mix8(f: u8, b: u8, a: u32) -> u8 {
    ((f as u32 * a + b as u32 * (255 - a) + 127) / 255) as u8
}

const fn q5(v: u8, t: i32) -> u16 {
    let v = v as i32 + t;
    let v = if v < 0 { 0 } else if v > 255 { 255 } else { v };
    (v as u16) >> 3
}

const fn widen(v: u16) -> u8 {
    let v = (v & 31) as u8;
    (v << 3) | (v >> 2)
}

/// 4x4 Bayer thresholds, 0..15 (the same matrix as lucina-glass).
const BAYER: [[u8; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];

/// An 8-bit colour to a DS pixel at (x, y): the Bayer threshold (-4..+3) added, then the top five bits.
pub const fn to555(c: Rgba, x: i32, y: i32) -> u16 {
    let t = BAYER[(y & 3) as usize][(x & 3) as usize] as i32 / 2 - 4;
    0x8000 | q5(c.r, t) | (q5(c.g, t) << 5) | (q5(c.b, t) << 10)
}

/// A square root for the ring's anti-aliasing (core has none for f32): bit-trick seed, three Newton steps.
fn sqrt(v: f32) -> f32 {
    if v <= 0.0 {
        return 0.0;
    }
    let mut x = f32::from_bits((v.to_bits() >> 1) + 0x1fbd_1df5);
    for _ in 0..3 {
        x = 0.5 * (x + v / x);
    }
    x
}

/// The angle of (dx, dy) as a fraction of a turn, clockwise from twelve o'clock (screen y down), 0..1. A polynomial
/// atan good to about 0.004 rad, plenty for a 1 px ring edge.
fn turn(dx: f32, dy: f32) -> f32 {
    const PI: f32 = core::f32::consts::PI;
    let (x, y) = (-dy, dx); // angle from up, clockwise
    let (ax, ay) = (if x < 0.0 { -x } else { x }, if y < 0.0 { -y } else { y });
    if ax == 0.0 && ay == 0.0 {
        return 0.0;
    }
    let atan = |z: f32| {
        let az = if z < 0.0 { -z } else { z };
        z * (PI / 4.0) + 0.273 * z * (1.0 - az)
    };
    let mut a = if ax >= ay { atan(ay / ax) } else { PI / 2.0 - atan(ax / ay) };
    if x < 0.0 {
        a = PI - a;
    }
    if y < 0.0 {
        a = 2.0 * PI - a;
    }
    a / (2.0 * PI)
}

/// What the contrast audit keeps per text run.
#[derive(Clone, Copy, Debug, Default)]
pub struct Rec {
    /// Contrast ratio x 100 (450 = 4.5:1), text against the mean of the pixels under its ink.
    pub ratio: u32,
    pub px: u8,
    pub bold: bool,
    pub text: [u8; 32],
    pub len: u8,
    /// Where the run's ink can be: x, y, w, h.
    pub rect: (i32, i32, i32, i32),
}

impl Rec {
    pub fn text(&self) -> &str {
        core::str::from_utf8(&self.text[..self.len as usize]).unwrap_or("?")
    }
    /// The floor this run is held to: 3:1 for large (19 px and up) or bold 15 px and up, else 4.5:1.
    pub fn floor(&self) -> u32 {
        if self.px >= 19 || (self.bold && self.px >= 15) {
            300
        } else {
            450
        }
    }
}

/// The contrast of every text run drawn while attached (the first 256).
pub struct Audit {
    pub recs: [Rec; 256],
    pub n: usize,
    /// Wrapped lines that ended inside a word (the layout check: wraps go at spaces only).
    pub splits: u32,
    /// Name labels drawn (x, y, w, h): no other text may lie under one.
    pub labels: [(i32, i32, i32, i32); 4],
    pub n_labels: usize,
    /// Controls drawn (buttons, fields, segments, focus rings): none may be on the top screen, which takes no touch.
    pub controls: u32,
}

impl Default for Audit {
    fn default() -> Self {
        Audit { recs: [Rec::default(); 256], n: 0, splits: 0, labels: [(0, 0, 0, 0); 4], n_labels: 0, controls: 0 }
    }
}

/// WCAG relative luminance of an 8-bit colour, x 10000 (sRGB curve through a 33-point table, linear in between).
fn luminance(c: Rgba) -> u32 {
    // the sRGB decode of i*8 (i = 0..=32, the last clamped to 255), x 10000
    const LIN: [u32; 33] = [
        0, 24, 52, 91, 144, 212, 296, 395, 513, 648, 802, 976, 1170, 1384, 1620, 1878, 2159, 2462, 2789, 3140, 3515,
        3916, 4342, 4793, 5271, 5776, 6308, 6867, 7454, 8070, 8714, 9387, 10000,
    ];
    let lin = |v: u8| {
        let i = v as u32 / 8;
        let f = v as u32 % 8;
        let (a, b) = (LIN[i as usize], LIN[(i + 1) as usize]);
        // the last step runs 248..255, seven wide
        let span = if i == 31 { 7 } else { 8 };
        a + (b - a) * f / span
    };
    (2126 * lin(c.r) + 7152 * lin(c.g) + 722 * lin(c.b)) / 10000
}

/// WCAG contrast ratio x 100.
pub fn contrast(a: Rgba, b: Rgba) -> u32 {
    let (la, lb) = (luminance(a), luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 500) * 100 / (lo + 500)
}

pub struct Canvas<'a> {
    pub px: &'a mut [u16],
    pub w: i32,
    pub h: i32,
    pub stride: i32,
    pub audit: Option<&'a mut Audit>,
    /// Drawing stays inside this rectangle (x0, y0, x1, y1): a partial redraw sets it to the part that changed.
    pub clip: (i32, i32, i32, i32),
    /// With a list attached the canvas records what is drawn instead of drawing it (the compositor, `compose`).
    pub rec: Option<&'a mut List>,
    /// Shapes drawn while this is above 0 are overlays (`begin_overlay`).
    overlay: u32,
}

impl<'a> Canvas<'a> {
    pub fn new(px: &'a mut [u16], w: i32, h: i32, stride: i32) -> Self {
        assert!(px.len() >= (stride * (h - 1) + w) as usize);
        Canvas { px, w, h, stride, audit: None, clip: (0, 0, w, h), rec: None, overlay: 0 }
    }

    fn x0(&self) -> i32 {
        self.clip.0.max(0)
    }
    fn y0(&self) -> i32 {
        self.clip.1.max(0)
    }
    fn x1(&self) -> i32 {
        self.clip.2.min(self.w)
    }
    fn y1(&self) -> i32 {
        self.clip.3.min(self.h)
    }

    /// Shapes from here to `end_overlay` belong to the overlay (redrawn on their own when they change): a name label,
    /// a caret, a pressed key. Text, icons, rings, art and the focus ring always do.
    pub fn begin_overlay(&mut self) {
        self.overlay += 1;
    }
    pub fn end_overlay(&mut self) {
        self.overlay = self.overlay.saturating_sub(1);
    }

    /// Everything drawn so far becomes part of the base (a dialog over the page).
    pub fn flatten(&mut self) {
        if let Some(l) = self.rec.as_deref_mut() {
            l.push(Op::Flatten, false);
        }
    }

    /// Records a shape when recording; true if it did.
    fn rec_shape(&mut self, op: Op) -> bool {
        let overlay = self.overlay > 0;
        match self.rec.as_deref_mut() {
            Some(l) => {
                l.push(op, overlay);
                true
            }
            None => false,
        }
    }

    /// Records an overlay op when recording; true if it did.
    fn rec_overlay(&mut self, op: Op) -> bool {
        match self.rec.as_deref_mut() {
            Some(l) => {
                l.push(op, true);
                true
            }
            None => false,
        }
    }

    /// A DS banner icon (32x32, 4bpp OBJ tile order, palette entry 0 transparent) at (x, y), `scale` times.
    pub fn art(&mut self, tiles: &[u8; 512], palette: &[u16; 16], x: i32, y: i32, scale: i32) {
        if let Some(l) = self.rec.as_deref_mut() {
            l.push_art(tiles, palette, x, y, scale);
            return;
        }
        // only the icon pixels inside the clip (a partial redraw clips to a few small rectangles)
        let sc = scale.max(1);
        let (px0, px1) = (((self.x0() - x) / sc).max(0), ((self.x1() - x + sc - 1) / sc).min(32));
        let (py0, py1) = (((self.y0() - y) / sc).max(0), ((self.y1() - y + sc - 1) / sc).min(32));
        for py in py0..py1 {
            for px in px0..px1 {
                let tile = (py / 8) * 4 + px / 8;
                let i = (tile * 64 + (py % 8) * 8 + px % 8) as usize;
                let b = tiles[i / 2];
                let n = if i.is_multiple_of(2) { b & 15 } else { b >> 4 };
                if n == 0 {
                    continue;
                }
                let c = Rgba::from555(palette[n as usize]);
                // opaque: one dithered value a pixel, no blend (fill would work out four per call)
                for dy in 0..sc {
                    for dx in 0..sc {
                        self.paint(x + px * sc + dx, y + py * sc + dy, c, 16);
                    }
                }
            }
        }
    }

    /// Notes a name label's rectangle for the audit (the host test: no other text under a label).
    pub fn note_label(&mut self, x: i32, y: i32, w: i32, h: i32) {
        if let Some(a) = self.audit.as_deref_mut() {
            if a.n_labels < a.labels.len() {
                a.labels[a.n_labels] = (x, y, w, h);
                a.n_labels += 1;
            }
        }
    }

    /// Notes that a control is drawn (the host test: the top screen holds content and status only).
    pub fn note_control(&mut self) {
        if let Some(a) = self.audit.as_deref_mut() {
            a.controls += 1;
        }
    }

    /// Clip drawing to the rectangle (x, y, w, h); `unclip` restores the whole frame.
    pub fn clip_to(&mut self, x: i32, y: i32, w: i32, h: i32) {
        self.clip = (x, y, x + w, y + h);
    }
    pub fn unclip(&mut self) {
        self.clip = (0, 0, self.w, self.h);
    }

    #[inline]
    fn idx(&self, x: i32, y: i32) -> Option<usize> {
        let (x0, y0, x1, y1) = self.clip;
        if x >= x0.max(0) && y >= y0.max(0) && x < x1.min(self.w) && y < y1.min(self.h) {
            Some((y * self.stride + x) as usize)
        } else {
            None
        }
    }

    pub fn get(&self, x: i32, y: i32) -> u16 {
        self.idx(x, y).map(|i| self.px[i]).unwrap_or(0x8000)
    }

    /// `c` over the pixel at (x, y) with coverage `cov`/16, dithered back to 5 bits.
    #[inline]
    pub fn paint(&mut self, x: i32, y: i32, c: Rgba, cov: u32) {
        if cov == 0 || c.a == 0 {
            return;
        }
        if let Some(i) = self.idx(x, y) {
            let a = c.a as u32 * cov.min(16) / 16;
            let out = if a >= 255 { c } else { c.alpha(a as u8).over(Rgba::from555(self.px[i])) };
            self.px[i] = to555(out, x, y);
        }
    }

    /// `c` at full coverage over pixels `x0..x1` of row `y` (already inside the clip). An opaque colour repeats every
    /// four pixels (the dither); a translucent one goes through a four-lane memo, since what lies under a span (the
    /// backdrop, a flat panel) repeats every four pixels too. This is what keeps a full redraw quick on the DSi.
    fn span(&mut self, y: i32, x0: i32, x1: i32, c: Rgba) {
        if x0 >= x1 || c.a == 0 {
            return;
        }
        let row = (y * self.stride) as usize;
        if c.a == 255 {
            let v = [to555(c, 0, y), to555(c, 1, y), to555(c, 2, y), to555(c, 3, y)];
            for x in x0..x1 {
                self.px[row + x as usize] = v[(x & 3) as usize];
            }
            return;
        }
        let mut memo_in = [u32::MAX; 4];
        let mut memo_out = [0u16; 4];
        for x in x0..x1 {
            let i = row + x as usize;
            let lane = (x & 3) as usize;
            let p = self.px[i];
            if memo_in[lane] != p as u32 {
                memo_in[lane] = p as u32;
                memo_out[lane] = to555(c.over(Rgba::from555(p)), x, y);
            }
            self.px[i] = memo_out[lane];
        }
    }

    /// A filled rectangle (blended if `c` is translucent).
    pub fn fill(&mut self, x: i32, y: i32, w: i32, h: i32, c: Rgba) {
        if self.rec_shape(Op::Fill(x, y, w, h, c)) {
            return;
        }
        let (x0, x1) = (x.max(self.x0()), (x + w).min(self.x1()));
        for yy in y.max(self.y0())..(y + h).min(self.y1()) {
            self.span(yy, x0, x1, c);
        }
    }

    pub fn clear(&mut self, c: Rgba) {
        if self.rec_shape(Op::Clear(c)) {
            return;
        }
        let (w, h) = (self.w, self.h);
        self.fill(0, 0, w, h, c.alpha(255));
    }

    /// A vertical gradient from `top` to `bottom` (opaque), dithered.
    pub fn gradient(&mut self, x: i32, y: i32, w: i32, h: i32, top: Rgba, bottom: Rgba) {
        if self.rec_shape(Op::Gradient(x, y, w, h, top, bottom)) {
            return;
        }
        let (x0, x1) = (x.max(self.x0()), (x + w).min(self.x1()));
        for yy in y.max(self.y0())..(y + h).min(self.y1()) {
            let t = ((yy - y) * 256 / h.max(1)) as u32;
            let m = |a: u8, b: u8| ((a as u32 * (256 - t) + b as u32 * t) / 256) as u8;
            let c = Rgba::new(m(top.r, bottom.r), m(top.g, bottom.g), m(top.b, bottom.b), 255);
            self.span(yy, x0, x1, c);
        }
    }

    /// Coverage (0..=16) of pixel (px, py) by the rounded rectangle (x, y, w, h, r).
    fn cov_rr(x: i32, y: i32, w: i32, h: i32, r: i32, px: i32, py: i32) -> u32 {
        if w <= 0 || h <= 0 || px < x || py < y || px >= x + w || py >= y + h {
            return 0;
        }
        let r = r.min(w / 2).min(h / 2).min(corner::MAX_RADIUS as i32).max(0);
        let dx = (px - x).min(x + w - 1 - px);
        let dy = (py - y).min(y + h - 1 - py);
        if dx < r && dy < r {
            corner::at(r as u16, dx as u16, dy as u16) as u32
        } else {
            corner::FULL as u32
        }
    }

    fn radius(w: i32, h: i32, r: i32) -> i32 {
        r.min(w / 2).min(h / 2).min(corner::MAX_RADIUS as i32).max(0)
    }

    /// A rounded rectangle of radius `r` (clamped to half the shorter side), corners anti-aliased.
    pub fn round_rect(&mut self, x: i32, y: i32, w: i32, h: i32, r: i32, c: Rgba) {
        if self.rec_shape(Op::RoundRect(x, y, w, h, r, c)) {
            return;
        }
        if w <= 0 || h <= 0 {
            return;
        }
        let r = Self::radius(w, h, r);
        let (cx0, cx1) = (self.x0(), self.x1());
        for yy in y.max(self.y0())..(y + h).min(self.y1()) {
            let dy = (yy - y).min(y + h - 1 - yy);
            if dy >= r {
                self.span(yy, x.max(cx0), (x + w).min(cx1), c);
                continue;
            }
            // a corner row: anti-aliased ends, a full middle
            for dx in 0..r {
                let a = corner::at(r as u16, dx as u16, dy as u16) as u32;
                self.paint(x + dx, yy, c, a);
                self.paint(x + w - 1 - dx, yy, c, a);
            }
            self.span(yy, (x + r).max(cx0), (x + w - r).min(cx1), c);
        }
    }

    /// A capsule: radius half the height.
    pub fn pill(&mut self, x: i32, y: i32, w: i32, h: i32, c: Rgba) {
        self.round_rect(x, y, w, h, h / 2, c);
    }

    /// A filled circle of diameter `d` with its top-left at (x, y).
    pub fn dot(&mut self, x: i32, y: i32, d: i32, c: Rgba) {
        self.round_rect(x, y, d, d, d / 2, c);
    }

    /// One pixel of a panel's edge: the fill inside, the rim between the outer and inner shapes, the highlight.
    #[allow(clippy::too_many_arguments)]
    fn panel_px(&mut self, x: i32, y: i32, w: i32, h: i32, r: i32, xx: i32, yy: i32, fill: Rgba, rim: Rgba, hi: Rgba) {
        let co = Self::cov_rr(x, y, w, h, r, xx, yy);
        if co == 0 {
            return;
        }
        let ci = Self::cov_rr(x + 1, y + 1, w - 2, h - 2, (r - 1).max(0), xx, yy);
        self.paint(xx, yy, fill, ci);
        self.paint(xx, yy, rim, co.saturating_sub(ci));
        if yy == y + 1 {
            self.paint(xx, yy, hi, ci);
        }
    }

    /// A panel in the Glass manner: `fill` inside, a 1 px `rim` round the edge and a `hi` highlight along the inner top
    /// row, in one pass so translucent layers do not double up. The edges go pixel by pixel; the inside in spans.
    #[allow(clippy::too_many_arguments)]
    pub fn panel(&mut self, x: i32, y: i32, w: i32, h: i32, r: i32, fill: Rgba, rim: Rgba, hi: Rgba) {
        if self.rec_shape(Op::Panel(x, y, w, h, r, fill, rim, hi)) {
            return;
        }
        if w <= 2 || h <= 2 {
            self.round_rect(x, y, w, h, r, rim);
            return;
        }
        let r = Self::radius(w, h, r);
        let (cx0, cx1) = (self.x0(), self.x1());
        for yy in y.max(self.y0())..(y + h).min(self.y1()) {
            let dy = (yy - y).min(y + h - 1 - yy);
            if dy == 0 {
                for xx in x.max(cx0)..(x + w).min(cx1) {
                    self.panel_px(x, y, w, h, r, xx, yy, fill, rim, hi);
                }
                continue;
            }
            // the columns done pixel by pixel at each end: the corner, or just the rim
            let edge = if dy < r { r.max(1) } else { 1 };
            for xx in x.max(cx0)..(x + edge).min(cx1) {
                self.panel_px(x, y, w, h, r, xx, yy, fill, rim, hi);
            }
            for xx in (x + w - edge).max(cx0).max(x + edge)..(x + w).min(cx1) {
                self.panel_px(x, y, w, h, r, xx, yy, fill, rim, hi);
            }
            let (m0, m1) = ((x + edge).max(cx0), (x + w - edge).min(cx1));
            self.span(yy, m0, m1, fill);
            if yy == y + 1 {
                self.span(yy, m0, m1, hi);
            }
        }
    }

    /// A `width` px outline just outside the rounded rectangle (x, y, w, h, r), `gap` px away: the focus ring.
    #[allow(clippy::too_many_arguments)]
    pub fn outline(&mut self, x: i32, y: i32, w: i32, h: i32, r: i32, gap: i32, width: i32, c: Rgba) {
        if self.rec_overlay(Op::Outline(x, y, w, h, r, gap, width, c)) {
            return;
        }
        let o = gap + width;
        let (ox, oy, ow, oh, or) = (x - o, y - o, w + 2 * o, h + 2 * o, r + o);
        let (ix, iy, iw, ih, ir) = (x - gap, y - gap, w + 2 * gap, h + 2 * gap, r + gap);
        for yy in oy.max(self.y0())..(oy + oh).min(self.y1()) {
            for xx in ox.max(self.x0())..(ox + ow).min(self.x1()) {
                let co = Self::cov_rr(ox, oy, ow, oh, or, xx, yy);
                let ci = Self::cov_rr(ix, iy, iw, ih, ir, xx, yy);
                self.paint(xx, yy, c, co.saturating_sub(ci));
            }
        }
    }

    /// A progress ring centred on (cx, cy) (pixel centres at +0.5): outer radius `r`, `width` px thick, `track` all
    /// round, then `c` over `len`/`den` of the turn starting `start`/`den` clockwise from twelve o'clock, round caps.
    #[allow(clippy::too_many_arguments)]
    pub fn ring(&mut self, cx: f32, cy: f32, r: f32, width: f32, start: u32, len: u32, den: u32, c: Rgba, track: Rgba) {
        if self.rec_overlay(Op::Ring(cx, cy, r, width, start, len, den, c, track)) {
            return;
        }
        // A generated mask (ring_gen.rs) when there is one for this ring and the centre is on a whole pixel: integer
        // lookups only, as the ARM9 has no FPU. The float path below stays for any other ring.
        if let Some(m) = crate::ring_gen::find(r, width).filter(|_| cx == (cx as i32) as f32 && cy == (cy as i32) as f32) {
            if den == 0 {
                return;
            }
            let (st, ln) = (((start % den) as u64 * 256 / den as u64) as u32, (len.min(den) as u64 * 256 / den as u64) as u32);
            let full = len >= den;
            // the caps' centres in half pixels from the ring's centre, once
            let mid = r - width / 2.0;
            let cap2 = |t: f32| {
                let (s, c) = sincos(t * 2.0 * core::f32::consts::PI);
                ((2.0 * mid * s) as i32, (-2.0 * mid * c) as i32)
            };
            let caps = if ln > 0 && !full {
                let f = len.min(den) as f32 / den as f32;
                let s0 = (start % den) as f32 / den as f32;
                Some([cap2(s0), cap2(s0 + f)])
            } else {
                None
            };
            let rw2 = (width * width) as i32;
            let (x0, y0) = (cx as i32 + m.off as i32, cy as i32 + m.off as i32);
            let n = m.size as i32;
            for j in 0..n {
                for i in 0..n {
                    let k = (j * n + i) as usize;
                    let cov = m.cov[k] as u32;
                    if cov == 0 {
                        continue;
                    }
                    let rel = (m.ang[k] as u32 + 256 - st) & 255;
                    let mut on = full || (ln > 0 && rel < ln);
                    if let (false, Some(cs)) = (on, caps) {
                        let (dx2, dy2) = (2 * (i + m.off as i32) + 1, 2 * (j + m.off as i32) + 1);
                        on = cs.iter().any(|&(ex, ey)| (dx2 - ex) * (dx2 - ex) + (dy2 - ey) * (dy2 - ey) <= rw2);
                    }
                    self.paint(x0 + i, y0 + j, if on { c } else { track }, cov);
                }
            }
            return;
        }
        let frac = if den == 0 { 0.0 } else { (len.min(den) as f32) / den as f32 };
        let st = if den == 0 { 0.0 } else { (start % den.max(1)) as f32 / den as f32 };
        let (r0, r1) = (r - width, r);
        let mid = (r0 + r1) / 2.0;
        // the caps' centres, once: the start and the end of the lit arc, on the ring's middle line
        let at = |t: f32| {
            let (s, c) = sincos(t * 2.0 * core::f32::consts::PI);
            (mid * s, -mid * c)
        };
        let caps = [at(st), at(st + frac)];
        let x0 = (cx - r - 1.0) as i32;
        let y0 = (cy - r - 1.0) as i32;
        for yy in y0..(cy + r + 2.0) as i32 {
            for xx in x0..(cx + r + 2.0) as i32 {
                let (dx, dy) = (xx as f32 + 0.5 - cx, yy as f32 + 0.5 - cy);
                let d = sqrt(dx * dx + dy * dy);
                let cov = (0.5 + r1 - d).clamp(0.0, 1.0).min((0.5 + d - r0).clamp(0.0, 1.0));
                let mut t = turn(dx, dy) - st;
                if t < 0.0 {
                    t += 1.0;
                }
                let mut on = frac > 0.0 && t < frac;
                // round caps at both ends
                if frac > 0.0 && frac < 1.0 {
                    let cap = |(ex, ey): (f32, f32)| {
                        let (qx, qy) = (dx - ex, dy - ey);
                        qx * qx + qy * qy <= (width / 2.0) * (width / 2.0)
                    };
                    on = on || cap(caps[0]) || cap(caps[1]);
                }
                let a = (cov * 16.0 + 0.5) as u32;
                if a > 0 {
                    self.paint(xx, yy, if on { c } else { track }, a);
                }
            }
        }
    }

    /// A 4-bit icon with its top-left at (x, y).
    pub fn icon(&mut self, ic: &'static Icon, x: i32, y: i32, c: Rgba) {
        if self.rec_overlay(Op::Icon(ic, x, y, c)) {
            return;
        }
        let s = ic.size as i32;
        if x + s <= self.x0() || x >= self.x1() || y + s <= self.y0() || y >= self.y1() {
            return;
        }
        for row in 0..s {
            for col in 0..s {
                let cov = ic.cov((row * s + col) as u32) as u32;
                self.paint(x + col, y + row, c, cov * 16 / 15);
            }
        }
    }

    /// `s` with its baseline at `y`, starting at `x`; returns the width drawn.
    pub fn text(&mut self, f: &'static Font, x: i32, y: i32, s: &str, c: Rgba) -> i32 {
        if let Some(l) = self.rec.as_deref_mut() {
            l.push_text(f, x, y, s, c);
            return f.width(s);
        }
        if self.audit.is_some() {
            self.audit_text(f, x, y, s, c);
        }
        let mut pen = x;
        let (cx0, cy0, cx1, cy1) = (self.x0(), self.y0(), self.x1(), self.y1());
        for ch in s.chars() {
            let g = f.glyph(ch);
            let (gx, gy) = (pen + g.left as i32, y + g.top as i32);
            // a glyph outside the clip costs nothing
            if gx + (g.w as i32) > cx0 && gx < cx1 && gy + (g.h as i32) > cy0 && gy < cy1 {
                for row in (cy0 - gy).max(0)..(g.h as i32).min(cy1 - gy) {
                    for col in (cx0 - gx).max(0)..(g.w as i32).min(cx1 - gx) {
                        let a = f.cov(g.off + (row * g.w as i32 + col) as u32) as u32;
                        self.paint(gx + col, gy + row, c, a * 16 / 15);
                    }
                }
            }
            pen += g.adv as i32;
        }
        pen - x
    }

    /// Records the run's contrast: its colour over, and against, the mean of the pixels its ink will cover.
    fn audit_text(&mut self, f: &Font, x: i32, y: i32, s: &str, c: Rgba) {
        let (mut n, mut sr, mut sg, mut sb) = (0u32, 0u32, 0u32, 0u32);
        let mut pen = x;
        for ch in s.chars() {
            let g = f.glyph(ch);
            let (gx, gy) = (pen + g.left as i32, y + g.top as i32);
            for row in 0..g.h as i32 {
                for col in 0..g.w as i32 {
                    if f.cov(g.off + (row * g.w as i32 + col) as u32) >= 8 {
                        if let Some(i) = self.idx(gx + col, gy + row) {
                            let p = Rgba::from555(self.px[i]);
                            n += 1;
                            sr += p.r as u32;
                            sg += p.g as u32;
                            sb += p.b as u32;
                        }
                    }
                }
            }
            pen += g.adv as i32;
        }
        if n == 0 {
            return;
        }
        let bg = Rgba::new((sr / n) as u8, (sg / n) as u8, (sb / n) as u8, 255);
        let rect = (x, y - f.px as i32, f.width(s), f.px as i32 + f.descent as i32);
        let mut rec = Rec { ratio: contrast(c.over(bg), bg), px: f.px, bold: f.bold, rect, ..Rec::default() };
        let b = s.as_bytes();
        let mut k = b.len().min(rec.text.len());
        while !s.is_char_boundary(k) {
            k -= 1;
        }
        rec.text[..k].copy_from_slice(&b[..k]);
        rec.len = k as u8;
        if let Some(a) = self.audit.as_deref_mut() {
            if a.n < a.recs.len() {
                a.recs[a.n] = rec;
                a.n += 1;
            }
        }
    }

    /// `s` centred on `cx`.
    pub fn text_centre(&mut self, f: &'static Font, cx: i32, y: i32, s: &str, c: Rgba) -> i32 {
        let w = f.width(s);
        self.text(f, cx - w / 2, y, s, c)
    }

    /// `s` ending at `right`.
    pub fn text_right(&mut self, f: &'static Font, right: i32, y: i32, s: &str, c: Rgba) -> i32 {
        let w = f.width(s);
        self.text(f, right - w, y, s, c)
    }

    /// `s` ending at `right`, cut with an ellipsis to fit `max` pixels; returns the width drawn.
    pub fn text_right_fit(&mut self, f: &'static Font, right: i32, y: i32, s: &str, max: i32, c: Rgba) -> i32 {
        let end = fit(f, s, max);
        let w = if end == s.len() { f.width(s) } else { f.width(&s[..end]) + f.width("…") };
        self.text_fit(f, right - w, y, s, max, c)
    }

    /// `s` cut with an ellipsis to fit `max` pixels.
    pub fn text_fit(&mut self, f: &'static Font, x: i32, y: i32, s: &str, max: i32, c: Rgba) -> i32 {
        let end = fit(f, s, max);
        if end == s.len() {
            return self.text(f, x, y, s, c);
        }
        let n = self.text(f, x, y, &s[..end], c);
        n + self.text(f, x + n, y, "…", c)
    }

    /// `s` wrapped at spaces only into at most `lines` lines of `max` pixels, `step` apart; a word too long for its
    /// line, and the last line, end in "…". Returns the number of lines drawn.
    #[allow(clippy::too_many_arguments)]
    pub fn text_wrap(&mut self, f: &'static Font, x: i32, y: i32, s: &str, max: i32, lines: i32, step: i32, c: Rgba) -> i32 {
        let mut rest = s.trim();
        let mut i = 0;
        while !rest.is_empty() && i < lines {
            let line = if i + 1 == lines { rest } else { wrap_line(f, rest, max) };
            let after = &rest[line.len()..];
            if !after.is_empty() && !after.starts_with(' ') {
                if let Some(a) = self.audit.as_deref_mut() {
                    a.splits += 1;
                }
            }
            self.text_fit(f, x, y + i * step, line, max, c);
            rest = after.trim_start();
            i += 1;
        }
        i
    }
}

/// sin and cos of `a` (radians) by a short polynomial after reducing to -pi..pi; good to about 1e-3.
fn sincos(a: f32) -> (f32, f32) {
    const PI: f32 = core::f32::consts::PI;
    let mut a = a;
    while a > PI {
        a -= 2.0 * PI;
    }
    while a < -PI {
        a += 2.0 * PI;
    }
    let sin = |x: f32| {
        // Bhaskara-style: 16x(pi - x) / (5pi^2 - 4x(pi - x)) on 0..pi, odd
        let ax = if x < 0.0 { -x } else { x };
        let v = 16.0 * ax * (PI - ax) / (5.0 * PI * PI - 4.0 * ax * (PI - ax));
        if x < 0.0 { -v } else { v }
    };
    let c = {
        let mut b = a + PI / 2.0;
        if b > PI {
            b -= 2.0 * PI;
        }
        sin(b)
    };
    (sin(a), c)
}

/// The byte length of the longest prefix of `s` that fits `max` pixels with an ellipsis, or `s.len()` if all fits.
pub fn fit(f: &Font, s: &str, max: i32) -> usize {
    if f.width(s) <= max {
        return s.len();
    }
    let ell = f.width("…");
    let (mut w, mut end) = (0, 0);
    for (i, ch) in s.char_indices() {
        let a = f.glyph(ch).adv as i32;
        if w + a + ell > max {
            break;
        }
        w += a;
        end = i + ch.len_utf8();
    }
    end
}

/// The longest run of whole words of `s` that fits `max` pixels, breaking at spaces only; a first word longer than
/// the line comes back whole (the caller ends it in "…").
pub fn wrap_line<'s>(f: &Font, s: &'s str, max: i32) -> &'s str {
    if f.width(s) <= max {
        return s;
    }
    let mut end = None;
    for (i, ch) in s.char_indices() {
        if ch == ' ' {
            if f.width(&s[..i]) > max {
                break;
            }
            end = Some(i);
        }
    }
    match end {
        Some(e) => &s[..e],
        None => s.split(' ').next().unwrap_or(s),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dither_keeps_flat_colours_flat_and_ends_exact() {
        for (x, y) in [(0, 0), (1, 2), (3, 3)] {
            assert_eq!(to555(Rgba::hex(0x000000), x, y), 0x8000);
            assert_eq!(to555(Rgba::hex(0xffffff), x, y), 0xffff);
        }
        // a colour on a 5-bit step stays that step everywhere
        let c = Rgba::from555(0x8000 | 12 | (20 << 5) | (5 << 10));
        for y in 0..4 {
            for x in 0..4 {
                let v = to555(c, x, y);
                assert!((v & 31).abs_diff(12) <= 1 && ((v >> 5) & 31).abs_diff(20) <= 1, "{x},{y} {v:04x}");
            }
        }
    }

    #[test]
    fn contrast_matches_wcag() {
        assert!(contrast(Rgba::hex(0xffffff), Rgba::hex(0x000000)).abs_diff(2100) <= 5);
        assert_eq!(contrast(Rgba::hex(0x777777), Rgba::hex(0x777777)), 100);
        // #767676 on white is the classic 4.54:1
        assert!(contrast(Rgba::hex(0x767676), Rgba::hex(0xffffff)).abs_diff(454) <= 6);
    }

    #[test]
    fn round_rect_corner_is_background_and_middle_is_fill() {
        let mut px = [0x8000u16; 64 * 32];
        let mut c = Canvas::new(&mut px, 64, 32, 64);
        c.round_rect(0, 0, 64, 32, 12, Rgba::hex(0xffffff));
        assert_eq!(c.get(0, 0), 0x8000);
        assert_eq!(c.get(63, 31), 0x8000);
        assert_eq!(c.get(32, 16), 0xFFFF);
        assert_eq!(c.get(32, 0), 0xFFFF);
        assert_eq!(c.get(0, 16), 0xFFFF);
    }

    #[test]
    fn ring_draws_progress_from_twelve_clockwise() {
        let mut px = [0x8000u16; 40 * 40];
        let mut c = Canvas::new(&mut px, 40, 40, 40);
        c.ring(20.0, 20.0, 16.0, 4.0, 0, 1, 4, Rgba::hex(0xffffff), Rgba::hex(0xff0000));
        // a quarter: the top-right of the ring is lit, the bottom-left is track
        assert_eq!(c.get(27, 6) & 0x7fff, 0x7fff, "top right {:04x}", c.get(27, 6));
        assert_eq!(c.get(6, 27) & 31, 31, "bottom left is red track");
        assert_eq!((c.get(6, 27) >> 5) & 31, 0);
        assert_eq!(c.get(20, 20), 0x8000, "the middle stays clear");
    }

    #[test]
    fn turn_and_sincos_agree() {
        assert!(turn(0.0, -1.0) < 0.01 || turn(0.0, -1.0) > 0.99);
        assert!((turn(1.0, 0.0) - 0.25).abs() < 0.01);
        assert!((turn(0.0, 1.0) - 0.5).abs() < 0.01);
        assert!((turn(-1.0, 0.0) - 0.75).abs() < 0.01);
        let (s, c) = sincos(core::f32::consts::PI / 2.0);
        assert!((s - 1.0).abs() < 0.01 && c.abs() < 0.01);
        assert!((sqrt(2.0) - core::f32::consts::SQRT_2).abs() < 1e-4);
    }

    #[test]
    fn panel_spans_match_pixel_by_pixel() {
        let fill = Rgba::new(28, 28, 30, 168);
        let rim = Rgba::new(235, 235, 245, 36);
        let hi = Rgba::new(255, 255, 255, 20);
        for &(x, y, w, h, r) in &[(3, 2, 50, 30, 10), (0, 0, 64, 40, 12), (5, 5, 20, 8, 4), (-4, 3, 30, 30, 15), (2, 2, 3, 3, 1)] {
            let mut a = [0u16; 64 * 48];
            let mut b = [0u16; 64 * 48];
            {
                let mut ca = Canvas::new(&mut a, 64, 48, 64);
                ca.gradient(0, 0, 64, 48, Rgba::hex(0x0b1630), Rgba::hex(0x020306));
                ca.panel(x, y, w, h, r, fill, rim, hi);
                ca.round_rect(x + 7, y + 3, 17, 9, 4, Rgba::new(127, 143, 224, 200));
            }
            {
                let mut cb = Canvas::new(&mut b, 64, 48, 64);
                cb.gradient(0, 0, 64, 48, Rgba::hex(0x0b1630), Rgba::hex(0x020306));
                let rr = Canvas::radius(w, h, r);
                for yy in 0..48 {
                    for xx in 0..64 {
                        cb.panel_px(x, y, w, h, rr, xx, yy, fill, rim, hi);
                    }
                }
                let r2 = Canvas::radius(17, 9, 4);
                for yy in 0..48 {
                    for xx in 0..64 {
                        let cov = Canvas::cov_rr(x + 7, y + 3, 17, 9, r2, xx, yy);
                        cb.paint(xx, yy, Rgba::new(127, 143, 224, 200), cov);
                    }
                }
            }
            assert!(a == b, "panel {x},{y} {w}x{h} r{r} differs from the per-pixel reference");
        }
    }

    #[test]
    fn wraps_only_at_spaces() {
        let f = &crate::text::SANS_9;
        for (s, max) in [("Example Game", 36), ("Cooking Mama", 36), ("fireda-engine", 40), ("Kirby Mass Attack", 40), ("a b c", 5)] {
            let mut rest = s;
            while !rest.is_empty() {
                let line = wrap_line(f, rest, max);
                let after = &rest[line.len()..];
                assert!(after.is_empty() || after.starts_with(' '), "{s:?} broke inside a word after {line:?}");
                rest = after.trim_start();
            }
        }
        let mut px = [0x8000u16; 64 * 40];
        let mut a = Audit::default();
        {
            let mut c = Canvas::new(&mut px, 64, 40, 64);
            c.audit = Some(&mut a);
            c.text_wrap(f, 0, 10, "Example Dungeon", 36, 2, 10, Rgba::hex(0xffffff));
        }
        assert_eq!(a.splits, 0);
    }

    #[test]
    fn clipping_never_panics() {
        let mut px = [0x8000u16; 16 * 16];
        let mut c = Canvas::new(&mut px, 16, 16, 16);
        c.round_rect(-10, -10, 40, 40, 9, Rgba::hex(0xffffff));
        c.fill(10, 10, 100, 100, Rgba::hex(0xff0000));
        c.text(&crate::text::BOLD_23, -5, 30, "Wg", Rgba::hex(0xffffff));
        c.ring(0.0, 0.0, 30.0, 3.0, 1, 2, 3, Rgba::hex(0xffffff), Rgba::hex(0x333333));
        c.outline(-3, -3, 30, 30, 6, 2, 2, Rgba::hex(0x7f8fe0));
        c.panel(-4, 8, 40, 20, 8, Rgba::new(28, 28, 30, 168), Rgba::new(235, 235, 245, 36), Rgba::new(255, 255, 255, 20));
        assert_eq!(c.get(0, 0), 0xFFFF);
    }

    #[test]
    fn audit_records_contrast() {
        let mut px = [0x8000u16; 64 * 16];
        let mut a = Audit::default();
        {
            let mut c = Canvas::new(&mut px, 64, 16, 64);
            c.audit = Some(&mut a);
            c.text(&crate::text::SANS_11, 2, 12, "Hi", Rgba::hex(0xffffff));
            c.text(&crate::text::SANS_11, 30, 12, "lo", Rgba::hex(0x202020));
        }
        assert_eq!(a.n, 2);
        assert!(a.recs[0].ratio > 2000, "{}", a.recs[0].ratio);
        assert!(a.recs[1].ratio < 150, "{}", a.recs[1].ratio);
        assert_eq!(a.recs[0].text(), "Hi");
    }
}
