//! A software canvas for the glass look: XRGB8888 pixels like a Juno frame, drawn in design units (dp, a 360 x 800 phone)
//! scaled by `k` pixels per dp, so the same screen renders at 1080 x 2400 for review and at any size on Juno.
//!
//! Every shape is a signed distance field: a pixel's coverage comes from its distance to the edge, which gives anti-aliased
//! rounded rectangles, rings with round caps and soft shadows from one routine each. Glass is a pre-blurred copy of the
//! backdrop seen through a rounded shape and tinted ([`Canvas::glass`]); the blur runs once per backdrop, not per frame.

use ab_glyph_rasterizer::{Rasterizer, point};

/// A colour with straight (not premultiplied) alpha.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    /// Opaque, from `0xRRGGBB`.
    pub const fn hex(v: u32) -> Color {
        Color {
            r: (v >> 16) as u8,
            g: (v >> 8) as u8,
            b: v as u8,
            a: 255,
        }
    }

    /// From `0xRRGGBB` and an alpha 0..255.
    pub const fn hexa(v: u32, a: u8) -> Color {
        Color {
            r: (v >> 16) as u8,
            g: (v >> 8) as u8,
            b: v as u8,
            a,
        }
    }

    /// The same colour with its alpha multiplied by `f` (0..1).
    pub fn alpha(self, f: f32) -> Color {
        Color {
            a: (self.a as f32 * f.clamp(0.0, 1.0)).round() as u8,
            ..self
        }
    }

    /// `self` to `o`, `t` of the way, alpha included.
    pub fn mix(self, o: Color, t: f32) -> Color {
        let t = t.clamp(0.0, 1.0);
        let m = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
        Color {
            r: m(self.r, o.r),
            g: m(self.g, o.g),
            b: m(self.b, o.b),
            a: m(self.a, o.a),
        }
    }

    pub const fn xrgb(self) -> u32 {
        ((self.r as u32) << 16) | ((self.g as u32) << 8) | self.b as u32
    }
}

/// A rectangle in dp.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct R {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl R {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> R {
        R { x, y, w, h }
    }
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
    pub fn cx(&self) -> f32 {
        self.x + self.w / 2.0
    }
    pub fn cy(&self) -> f32 {
        self.y + self.h / 2.0
    }
    pub fn inset(&self, d: f32) -> R {
        R::new(self.x + d, self.y + d, self.w - 2.0 * d, self.h - 2.0 * d)
    }
    pub fn inset_xy(&self, dx: f32, dy: f32) -> R {
        R::new(
            self.x + dx,
            self.y + dy,
            self.w - 2.0 * dx,
            self.h - 2.0 * dy,
        )
    }
    pub fn offset(&self, dx: f32, dy: f32) -> R {
        R::new(self.x + dx, self.y + dy, self.w, self.h)
    }
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && y >= self.y && x < self.right() && y < self.bottom()
    }
    /// Scaled about its centre.
    pub fn scale_about_center(&self, s: f32) -> R {
        let (w, h) = (self.w * s, self.h * s);
        R::new(self.cx() - w / 2.0, self.cy() - h / 2.0, w, h)
    }
    /// The top `h` of it, and the rest below a `gap`.
    pub fn take_top(&self, h: f32, gap: f32) -> (R, R) {
        (
            R::new(self.x, self.y, self.w, h),
            R::new(
                self.x,
                self.y + h + gap,
                self.w,
                (self.h - h - gap).max(0.0),
            ),
        )
    }
    pub fn take_left(&self, w: f32, gap: f32) -> (R, R) {
        (
            R::new(self.x, self.y, w, self.h),
            R::new(
                self.x + w + gap,
                self.y,
                (self.w - w - gap).max(0.0),
                self.h,
            ),
        )
    }
    pub fn take_right(&self, w: f32, gap: f32) -> (R, R) {
        (
            R::new(self.right() - w, self.y, w, self.h),
            R::new(self.x, self.y, (self.w - w - gap).max(0.0), self.h),
        )
    }
}

/// Coverage of a pixel whose centre is `d` pixels outside an edge (negative inside).
#[inline]
fn edge(d: f32) -> f32 {
    (0.5 - d).clamp(0.0, 1.0)
}

/// The pixels of row `fy` (a pixel centre) whose centres lie at least `m` pixels inside the rounded box centred at (`cx`, `cy`)
/// with half-sizes (`hw`, `hh`) and corner `rad`: an inclusive range of pixel columns, possibly empty (`a > b`). There
/// [`sd_rrect`] is at most `-m`, so the per-pixel coverage is known without computing it: 1 for a fill (`m` 0.5), nothing for a
/// stroke's inside. The bound is worked out from the same formula, a little short at each end so that a pixel on the boundary
/// goes the exact way.
#[inline]
fn inner_run(fy: f32, cx: f32, cy: f32, hw: f32, hh: f32, rad: f32, m: f32) -> (i32, i32) {
    const EPS: f32 = 1e-3;
    let rm = rad - m;
    let qy = (fy - cy).abs() - (hh - rad);
    if qy > rm {
        return (1, 0);
    }
    let qx_max = if qy <= 0.0 {
        rm
    } else {
        // A corner row: `qy > 0` here only when `rm > 0`.
        (rm * rm - qy * qy).max(0.0).sqrt()
    };
    let half = hw - rad + qx_max - EPS;
    if half < 0.0 {
        return (1, 0);
    }
    (
        (cx - half - 0.5).ceil() as i32,
        (cx + half - 0.5).floor() as i32,
    )
}

/// Signed distance from `(px, py)` to a rounded box centred at `(cx, cy)` with half-sizes `(hw, hh)` and corner `rad`.
#[inline]
fn sd_rrect(px: f32, py: f32, cx: f32, cy: f32, hw: f32, hh: f32, rad: f32) -> f32 {
    let qx = (px - cx).abs() - (hw - rad);
    let qy = (py - cy).abs() - (hh - rad);
    let ox = qx.max(0.0);
    let oy = qy.max(0.0);
    (ox * ox + oy * oy).sqrt() + qx.max(qy).min(0.0) - rad
}

/// An XRGB8888 canvas drawn in dp.
#[derive(Clone)]
pub struct Canvas {
    pub w: u32,
    pub h: u32,
    pub px: Vec<u32>,
    /// Pixels per dp.
    pub k: f32,
    /// Multiplies every coverage: fades whole groups (a screen leaving, a card being swiped away).
    pub opacity: f32,
    /// Added to every dp coordinate.
    pub origin: (f32, f32),
    /// Drawing stays inside this pixel rectangle: x0, y0, x1, y1.
    clip: (i32, i32, i32, i32),
    clips: Vec<SavedClip>,
    /// A rounded clip, in pixels: content inside a card follows its corners.
    round: Option<RoundClip>,
}

/// A clip pushed by [`Canvas::push_clip`], to restore.
type SavedClip = ((i32, i32, i32, i32), Option<RoundClip>);

#[derive(Clone, Copy, Debug)]
struct RoundClip {
    cx: f32,
    cy: f32,
    hw: f32,
    hh: f32,
    rad: f32,
}

impl Canvas {
    pub fn new(w: u32, h: u32, k: f32) -> Canvas {
        Canvas {
            w,
            h,
            px: vec![0; w as usize * h as usize],
            k,
            opacity: 1.0,
            origin: (0.0, 0.0),
            clip: (0, 0, w as i32, h as i32),
            clips: Vec::new(),
            round: None,
        }
    }

    /// The canvas in dp.
    pub fn bounds(&self) -> R {
        R::new(0.0, 0.0, self.w as f32 / self.k, self.h as f32 / self.k)
    }

    pub fn clear(&mut self, c: Color) {
        self.px.fill(c.xrgb());
    }

    /// A dp length in pixels.
    pub fn s(&self, v: f32) -> f32 {
        v * self.k
    }

    fn to_px(&self, x: f32, y: f32) -> (f32, f32) {
        ((x + self.origin.0) * self.k, (y + self.origin.1) * self.k)
    }

    fn rect_px(&self, r: R) -> (f32, f32, f32, f32) {
        let (x, y) = self.to_px(r.x, r.y);
        (x, y, r.w * self.k, r.h * self.k)
    }

    /// Clip further to `r` (dp), with rounded corners `rad`.
    pub fn push_clip(&mut self, r: R, rad: f32) {
        self.clips.push((self.clip, self.round));
        let (x, y, w, h) = self.rect_px(r);
        let c = (
            self.clip.0.max(x.floor() as i32),
            self.clip.1.max(y.floor() as i32),
            self.clip.2.min((x + w).ceil() as i32),
            self.clip.3.min((y + h).ceil() as i32),
        );
        self.clip = c;
        if rad > 0.0 {
            self.round = Some(RoundClip {
                cx: x + w / 2.0,
                cy: y + h / 2.0,
                hw: w / 2.0,
                hh: h / 2.0,
                rad: rad * self.k,
            });
        }
    }

    pub fn pop_clip(&mut self) {
        if let Some((c, r)) = self.clips.pop() {
            self.clip = c;
            self.round = r;
        }
    }

    /// The clip in pixels: `(x0, y0, x1, y1)`.
    pub fn clip_px(&self) -> (i32, i32, i32, i32) {
        self.clip
    }

    /// The pixel box `(x0, y0, x1, y1)` of a dp-sized area, grown by `pad` pixels and clipped.
    fn span(&self, x: f32, y: f32, w: f32, h: f32, pad: f32) -> Option<(i32, i32, i32, i32)> {
        let x0 = ((x - pad).floor() as i32).max(self.clip.0);
        let y0 = ((y - pad).floor() as i32).max(self.clip.1);
        let x1 = ((x + w + pad).ceil() as i32).min(self.clip.2);
        let y1 = ((y + h + pad).ceil() as i32).min(self.clip.3);
        (x0 < x1 && y0 < y1).then_some((x0, y0, x1, y1))
    }

    /// Blend `c` at pixel (`x`, `y`) with coverage `cov` (0..1), times the canvas opacity and the rounded clip.
    #[inline]
    pub fn blend_px(&mut self, x: i32, y: i32, c: Color, cov: f32) {
        if x < self.clip.0 || y < self.clip.1 || x >= self.clip.2 || y >= self.clip.3 {
            return;
        }
        let mut a = cov * self.opacity * (c.a as f32 / 255.0);
        if let Some(rc) = self.round {
            a *= edge(sd_rrect(
                x as f32 + 0.5,
                y as f32 + 0.5,
                rc.cx,
                rc.cy,
                rc.hw,
                rc.hh,
                rc.rad,
            ));
        }
        if a <= 0.0 {
            return;
        }
        let i = y as usize * self.w as usize + x as usize;
        if a >= 0.999 {
            self.px[i] = c.xrgb();
            return;
        }
        let d = self.px[i];
        let m = |s: u8, shift: u32| {
            let dv = ((d >> shift) & 0xff) as f32;
            ((s as f32 - dv) * a + dv).round() as u32
        };
        self.px[i] = (m(c.r, 16) << 16) | (m(c.g, 8) << 8) | m(c.b, 0);
    }

    /// Fill pixels `a..=b` of row `y` (already inside the clip) with `c` at full coverage: the same bytes as [`Canvas::blend_px`]
    /// with coverage 1 for each, without its per-pixel float work. An opaque colour is written; a translucent one goes through a
    /// table of the float formula's results for each byte value, built once for the run.
    fn fill_run(&mut self, y: i32, a: i32, b: i32, c: Color) {
        if a > b {
            return;
        }
        if self.round.is_some() {
            for x in a..=b {
                self.blend_px(x, y, c, 1.0);
            }
            return;
        }
        let alpha = 1.0 * self.opacity * (c.a as f32 / 255.0);
        if alpha <= 0.0 {
            return;
        }
        let row = y as usize * self.w as usize;
        let span = &mut self.px[row + a as usize..=row + b as usize];
        if alpha >= 0.999 {
            span.fill(c.xrgb());
            return;
        }
        let lut = |s: u8| -> [u8; 256] {
            let mut t = [0u8; 256];
            for (d, v) in t.iter_mut().enumerate() {
                let dv = d as f32;
                *v = ((s as f32 - dv) * alpha + dv).round() as u8;
            }
            t
        };
        let (lr, lg, lb) = (lut(c.r), lut(c.g), lut(c.b));
        for p in span {
            let d = *p;
            *p = ((lr[((d >> 16) & 0xff) as usize] as u32) << 16)
                | ((lg[((d >> 8) & 0xff) as usize] as u32) << 8)
                | lb[(d & 0xff) as usize] as u32;
        }
    }

    /// Blend an already-computed colour value (`0x00RRGGBB`) at a pixel, with coverage.
    #[inline]
    fn blend_raw(&mut self, x: i32, y: i32, v: u32, cov: f32) {
        let c = Color {
            r: (v >> 16) as u8,
            g: (v >> 8) as u8,
            b: v as u8,
            a: 255,
        };
        self.blend_px(x, y, c, cov);
    }

    /// The colour at a pixel.
    pub fn get(&self, x: i32, y: i32) -> u32 {
        let x = x.clamp(0, self.w as i32 - 1) as usize;
        let y = y.clamp(0, self.h as i32 - 1) as usize;
        self.px[y * self.w as usize + x]
    }

    /// A filled rectangle with corner radius `rad` (dp).
    pub fn fill_rrect(&mut self, r: R, rad: f32, c: Color) {
        self.rrect_with(r, rad, 0.0, |_, _| c, 0.0);
    }

    /// A rounded rectangle whose edge fades over `soft` dp: shadows and glows.
    pub fn fill_rrect_soft(&mut self, r: R, rad: f32, soft: f32, c: Color) {
        self.rrect_with(r, rad, soft, |_, _| c, 0.0);
    }

    /// A rounded rectangle filled top to bottom from `top` to `bottom`.
    pub fn gradient_rrect(&mut self, r: R, rad: f32, top: Color, bottom: Color) {
        let (_, y, _, h) = self.rect_px(r);
        self.rrect_with(
            r,
            rad,
            0.0,
            |_, py| top.mix(bottom, (py - y) / h.max(1.0)),
            0.0,
        );
    }

    /// A ring along the edge of a rounded rectangle, `width` dp wide, inside it.
    pub fn stroke_rrect(&mut self, r: R, rad: f32, width: f32, c: Color) {
        self.rrect_with(r, rad, 0.0, |_, _| c, width);
    }

    /// The general rounded-rectangle routine: colour per pixel, an optional soft edge, an optional stroke width (0 = filled).
    fn rrect_with(
        &mut self,
        r: R,
        rad: f32,
        soft: f32,
        color: impl Fn(f32, f32) -> Color,
        stroke: f32,
    ) {
        let (x, y, w, h) = self.rect_px(r);
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        let rad = (rad * self.k).min(w / 2.0).min(h / 2.0).max(0.0);
        let soft = soft * self.k;
        let stroke = stroke * self.k;
        let Some((x0, y0, x1, y1)) = self.span(x, y, w, h, soft + 1.0) else {
            return;
        };
        let (cx, cy, hw, hh) = (x + w / 2.0, y + h / 2.0, w / 2.0, h / 2.0);
        // Where the coverage is known without the distance: a fill's (and a soft edge's) inside is 1; a stroke's inside is 0.
        let margin = if soft > 0.0 {
            soft
        } else if stroke > 0.0 {
            stroke + 0.5
        } else {
            0.5
        };
        for py in y0..y1 {
            let fy = py as f32 + 0.5;
            let (ia, ib) = inner_run(fy, cx, cy, hw, hh, rad, margin);
            let (ia, ib) = (ia.max(x0), ib.min(x1 - 1));
            if ia <= ib && stroke <= 0.0 {
                // The colour may depend on the row only (fills, gradients): one value for the run.
                let c = color(ia as f32 + 0.5, fy);
                self.fill_run(py, ia, ib, c);
            }
            for pxx in x0..x1 {
                if pxx >= ia && pxx <= ib {
                    continue;
                }
                let fx = pxx as f32 + 0.5;
                let d = sd_rrect(fx, fy, cx, cy, hw, hh, rad);
                let cov = if soft > 0.0 {
                    // A smooth falloff centred on the edge, roughly a Gaussian's.
                    let t = ((d + soft) / (2.0 * soft)).clamp(0.0, 1.0);
                    1.0 - t * t * (3.0 - 2.0 * t)
                } else if stroke > 0.0 {
                    edge(d) * edge(-(d + stroke))
                } else {
                    edge(d)
                };
                if cov > 0.0 {
                    self.blend_px(pxx, py, color(fx, fy), cov);
                }
            }
        }
    }

    /// A disc of radius `rad` dp.
    pub fn fill_circle(&mut self, cx: f32, cy: f32, rad: f32, c: Color) {
        self.fill_rrect(R::new(cx - rad, cy - rad, rad * 2.0, rad * 2.0), rad, c);
    }

    /// A circle's outline, `width` dp, centred on radius `rad`.
    pub fn stroke_circle(&mut self, cx: f32, cy: f32, rad: f32, width: f32, c: Color) {
        self.arc(cx, cy, rad, width, 0.0, std::f32::consts::TAU, c);
    }

    /// An arc of the circle of radius `rad` around (`cx`, `cy`), `width` dp wide with round caps, from `start` radians
    /// (0 at twelve o'clock, clockwise) through `sweep` radians. A full sweep is a closed ring.
    #[allow(clippy::too_many_arguments)]
    pub fn arc(
        &mut self,
        cx: f32,
        cy: f32,
        rad: f32,
        width: f32,
        start: f32,
        sweep: f32,
        c: Color,
    ) {
        use std::f32::consts::TAU;
        if sweep <= 0.0 {
            return;
        }
        let (cx, cy) = self.to_px(cx, cy);
        let (rad, hw) = (rad * self.k, width * self.k / 2.0);
        let full = sweep >= TAU - 1e-4;
        let ends = [start, start + sweep].map(|a| (cx + rad * a.sin(), cy - rad * a.cos()));
        let Some((x0, y0, x1, y1)) = self.span(cx - rad, cy - rad, 2.0 * rad, 2.0 * rad, hw + 1.0)
        else {
            return;
        };
        for py in y0..y1 {
            for pxx in x0..x1 {
                let (fx, fy) = (pxx as f32 + 0.5, py as f32 + 0.5);
                let (dx, dy) = (fx - cx, fy - cy);
                let dist = (dx * dx + dy * dy).sqrt();
                // The pixel's angle clockwise from twelve o'clock, relative to the start.
                let mut a = dx.atan2(-dy) - start;
                a = a.rem_euclid(TAU);
                let d = if full || a <= sweep {
                    (dist - rad).abs() - hw
                } else {
                    let mut best = f32::MAX;
                    for (ex, ey) in ends {
                        best = best.min(((fx - ex).powi(2) + (fy - ey).powi(2)).sqrt() - hw);
                    }
                    best
                };
                let cov = edge(d);
                if cov > 0.0 {
                    self.blend_px(pxx, py, c, cov);
                }
            }
        }
    }

    /// A line from (`x0`, `y0`) to (`x1`, `y1`) in dp, `width` dp wide, with round caps.
    pub fn line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, width: f32, c: Color) {
        let (ax, ay) = self.to_px(x0, y0);
        let (bx, by) = self.to_px(x1, y1);
        let hw = width * self.k / 2.0;
        let Some((sx0, sy0, sx1, sy1)) = self.span(
            ax.min(bx),
            ay.min(by),
            (ax - bx).abs(),
            (ay - by).abs(),
            hw + 1.0,
        ) else {
            return;
        };
        let (vx, vy) = (bx - ax, by - ay);
        let len2 = (vx * vx + vy * vy).max(1e-6);
        for py in sy0..sy1 {
            for pxx in sx0..sx1 {
                let (fx, fy) = (pxx as f32 + 0.5, py as f32 + 0.5);
                let t = (((fx - ax) * vx + (fy - ay) * vy) / len2).clamp(0.0, 1.0);
                let (qx, qy) = (ax + vx * t - fx, ay + vy * t - fy);
                let cov = edge((qx * qx + qy * qy).sqrt() - hw);
                if cov > 0.0 {
                    self.blend_px(pxx, py, c, cov);
                }
            }
        }
    }

    /// A polyline through `pts` (dp), joints rounded.
    pub fn polyline(&mut self, pts: &[(f32, f32)], width: f32, c: Color) {
        for w in pts.windows(2) {
            self.line(w[0].0, w[0].1, w[1].0, w[1].1, width, c);
        }
    }

    /// A filled polygon through `pts` (dp), anti-aliased (non-zero winding).
    pub fn polygon(&mut self, pts: &[(f32, f32)], c: Color) {
        if pts.len() < 3 {
            return;
        }
        let pp: Vec<(f32, f32)> = pts.iter().map(|&(x, y)| self.to_px(x, y)).collect();
        let minx = pp.iter().map(|p| p.0).fold(f32::MAX, f32::min).floor();
        let miny = pp.iter().map(|p| p.1).fold(f32::MAX, f32::min).floor();
        let maxx = pp.iter().map(|p| p.0).fold(f32::MIN, f32::max).ceil();
        let maxy = pp.iter().map(|p| p.1).fold(f32::MIN, f32::max).ceil();
        let (w, h) = ((maxx - minx) as usize + 1, (maxy - miny) as usize + 1);
        if w > 8192 || h > 8192 {
            return;
        }
        let mut r = Rasterizer::new(w, h);
        for i in 0..pp.len() {
            let (a, b) = (pp[i], pp[(i + 1) % pp.len()]);
            r.draw_line(point(a.0 - minx, a.1 - miny), point(b.0 - minx, b.1 - miny));
        }
        let (ox, oy) = (minx as i32, miny as i32);
        let mut cov = vec![0f32; w * h];
        r.for_each_pixel(|i, v| cov[i] = v.clamp(0.0, 1.0));
        for (i, v) in cov.into_iter().enumerate() {
            if v > 0.0 {
                self.blend_px(ox + (i % w) as i32, oy + (i / w) as i32, c, v);
            }
        }
    }

    /// A radial glow: `inner` at the centre fading to transparent at `rad` dp.
    pub fn glow(&mut self, cx: f32, cy: f32, rad: f32, inner: Color) {
        let (cx, cy) = self.to_px(cx, cy);
        let rad = rad * self.k;
        let Some((x0, y0, x1, y1)) = self.span(cx - rad, cy - rad, 2.0 * rad, 2.0 * rad, 1.0)
        else {
            return;
        };
        for py in y0..y1 {
            for pxx in x0..x1 {
                let (dx, dy) = (pxx as f32 + 0.5 - cx, py as f32 + 0.5 - cy);
                let t = 1.0 - ((dx * dx + dy * dy).sqrt() / rad).min(1.0);
                if t > 0.0 {
                    self.blend_px(pxx, py, inner, t * t);
                }
            }
        }
    }

    /// A vertical gradient over `r` (no rounding), for scrims over photographs.
    pub fn scrim(&mut self, r: R, top: Color, bottom: Color) {
        self.gradient_rrect(r, 0.0, top, bottom);
    }

    /// Glass: inside the rounded rectangle `r`, the pixels of `backdrop` (a blurred copy of what is behind, the same size as
    /// this canvas), tinted with `tint`; then a hairline rim, brighter at the top, as light catching an edge.
    pub fn glass(&mut self, r: R, rad: f32, backdrop: &Canvas, tint: Color, rim: Color) {
        let (x, y, w, h) = self.rect_px(r);
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        let radp = (rad * self.k).min(w / 2.0).min(h / 2.0);
        let Some((x0, y0, x1, y1)) = self.span(x, y, w, h, 1.0) else {
            return;
        };
        let (cx, cy, hw, hh) = (x + w / 2.0, y + h / 2.0, w / 2.0, h / 2.0);
        let ta = tint.a as f32 / 255.0;
        // The tint over each byte value of the backdrop, as the float formula below gives it.
        let lut = |t: u8| -> [u8; 256] {
            let mut o = [0u8; 256];
            for (b, v) in o.iter_mut().enumerate() {
                let bv = b as f32;
                *v = ((t as f32 - bv) * ta + bv).round().clamp(0.0, 255.0) as u8;
            }
            o
        };
        let (lr, lg, lb) = (lut(tint.r), lut(tint.g), lut(tint.b));
        // A blurred backdrop may be kept at half the size (it is smooth): then pixel (x, y) shows its (x/2, y/2).
        let half = backdrop.w * 2 == self.w && backdrop.h * 2 == self.h;
        // A backdrop one pixel wide is a colour per row (a plain ground): pixel (x, y) shows its row's colour.
        let column = backdrop.w == 1 && backdrop.h == self.h;
        // Fully inside, opaque, no rounded clip: the tinted backdrop is the pixel (blend_px writes it as it is at coverage 1).
        let direct = self.round.is_none()
            && self.opacity >= 0.999
            && ((backdrop.w == self.w && backdrop.h == self.h) || half || column);
        let w_px = self.w as usize;
        let bw = backdrop.w as usize;
        for py in y0..y1 {
            let (ia, ib) = if direct {
                inner_run(py as f32 + 0.5, cx, cy, hw, hh, radp, 0.5)
            } else {
                (1, 0)
            };
            let (ia, ib) = (ia.max(x0), ib.min(x1 - 1));
            if ia <= ib {
                let row = py as usize * w_px;
                let brow = if half { (py as usize >> 1) * bw } else { row };
                for i in row + ia as usize..=row + ib as usize {
                    let b = if column {
                        backdrop.px[py as usize]
                    } else if half {
                        backdrop.px[brow + ((i - row) >> 1)]
                    } else {
                        backdrop.px[i]
                    };
                    self.px[i] = ((lr[((b >> 16) & 0xff) as usize] as u32) << 16)
                        | ((lg[((b >> 8) & 0xff) as usize] as u32) << 8)
                        | lb[(b & 0xff) as usize] as u32;
                }
            }
            for pxx in x0..x1 {
                if pxx >= ia && pxx <= ib {
                    continue;
                }
                let d = sd_rrect(pxx as f32 + 0.5, py as f32 + 0.5, cx, cy, hw, hh, radp);
                let cov = edge(d);
                if cov <= 0.0 {
                    continue;
                }
                let b = if column {
                    backdrop.get(0, py)
                } else if half {
                    backdrop.get(pxx >> 1, py >> 1)
                } else {
                    backdrop.get(pxx, py)
                };
                let m = |shift: u32, t: u8| {
                    let bv = ((b >> shift) & 0xff) as f32;
                    ((t as f32 - bv) * ta + bv).round().clamp(0.0, 255.0) as u32
                };
                let v = (m(16, tint.r) << 16) | (m(8, tint.g) << 8) | m(0, tint.b);
                self.blend_raw(pxx, py, v, cov);
            }
        }
        // The rim: a hairline that is brighter along the top edge.
        let bottom_rim = rim.alpha(0.35);
        self.rrect_with(
            r,
            rad,
            0.0,
            |_, py| rim.mix(bottom_rim, ((py - y) / h).clamp(0.0, 1.0)),
            0.7,
        );
    }

    /// Copy `src` (any size) scaled into `dst` (dp), bilinear, with `alpha`. Rounded to `rad`.
    pub fn blit(&mut self, src: &Canvas, dst: R, rad: f32, alpha: f32) {
        self.blit_region(src, (0.0, 0.0, src.w as f32, src.h as f32), dst, rad, alpha);
    }

    /// `src` cropped to cover `dst` without distortion (the CSS `object-fit: cover`), rounded to `rad`.
    pub fn blit_cover(&mut self, src: &Canvas, dst: R, rad: f32, alpha: f32) {
        let (_, _, w, h) = self.rect_px(dst);
        let sa = src.w as f32 / src.h as f32;
        let da = w / h.max(1.0);
        let region = if sa > da {
            let sw = src.h as f32 * da;
            ((src.w as f32 - sw) / 2.0, 0.0, sw, src.h as f32)
        } else {
            let sh = src.w as f32 / da;
            (0.0, (src.h as f32 - sh) / 2.0, src.w as f32, sh)
        };
        self.blit_region(src, region, dst, rad, alpha);
    }

    /// `src`'s pixel region `(sx, sy, sw, sh)` scaled into `dst` (dp), bilinear, rounded to `rad`, with `alpha`.
    pub fn blit_region(
        &mut self,
        src: &Canvas,
        region: (f32, f32, f32, f32),
        dst: R,
        rad: f32,
        alpha: f32,
    ) {
        let (x, y, w, h) = self.rect_px(dst);
        if w <= 0.0 || h <= 0.0 || src.w == 0 || src.h == 0 {
            return;
        }
        let radp = (rad * self.k).min(w / 2.0).min(h / 2.0).max(0.0);
        let Some((x0, y0, x1, y1)) = self.span(x, y, w, h, 1.0) else {
            return;
        };
        let (cx, cy, hw, hh) = (x + w / 2.0, y + h / 2.0, w / 2.0, h / 2.0);
        let (sx, sy, sw, sh) = region;
        let (fx, fy) = (sw / w, sh / h);
        for py in y0..y1 {
            for pxx in x0..x1 {
                let cov = edge(sd_rrect(
                    pxx as f32 + 0.5,
                    py as f32 + 0.5,
                    cx,
                    cy,
                    hw,
                    hh,
                    radp,
                )) * alpha;
                if cov <= 0.0 {
                    continue;
                }
                let u = sx + (pxx as f32 + 0.5 - x) * fx - 0.5;
                let v = sy + (py as f32 + 0.5 - y) * fy - 0.5;
                let c = sample(src, u, v);
                self.blend_raw(pxx, py, c, cov);
            }
        }
    }

    #[allow(clippy::needless_range_loop)]
    /// A copy blurred with radius `r` pixels (three box passes each way: close to a Gaussian of sigma about r/1.7).
    pub fn blurred(&self, r: u32) -> Canvas {
        let mut out = Canvas::new(self.w, self.h, self.k);
        out.px.copy_from_slice(&self.px);
        if r == 0 {
            return out;
        }
        let (w, h) = (self.w as usize, self.h as usize);
        let mut tmp = vec![0u32; w.max(h)];
        for _ in 0..3 {
            for y in 0..h {
                box_line(&mut out.px[y * w..(y + 1) * w], w, r as usize, &mut tmp);
            }
            for x in 0..w {
                let mut col: Vec<u32> = (0..h).map(|y| out.px[y * w + x]).collect();
                box_line(&mut col, h, r as usize, &mut tmp);
                for y in 0..h {
                    out.px[y * w + x] = col[y];
                }
            }
        }
        out
    }

    /// A copy at another size (bilinear), for the gallery's smaller views.
    pub fn resized(&self, w: u32, h: u32) -> Canvas {
        let mut out = Canvas::new(w, h, self.k * w as f32 / self.w as f32);
        let k = out.k;
        out.k = 1.0;
        out.blit(self, R::new(0.0, 0.0, w as f32, h as f32), 0.0, 1.0);
        out.k = k;
        out
    }

    /// The canvas as PNG bytes (RGB, 8 bits).
    pub fn png(&self) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, self.w, self.h);
            enc.set_color(png::ColorType::Rgb);
            enc.set_depth(png::BitDepth::Eight);
            enc.set_compression(png::Compression::Fast);
            let mut wr = enc.write_header().expect("png header");
            let mut rgb = Vec::with_capacity(self.px.len() * 3);
            for &p in &self.px {
                rgb.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, p as u8]);
            }
            wr.write_image_data(&rgb).expect("png data");
        }
        out
    }
}

/// Bilinear sample of `src` at pixel coordinates (`u`, `v`), clamped at the edges.
fn sample(src: &Canvas, u: f32, v: f32) -> u32 {
    let (x0, y0) = (u.floor(), v.floor());
    let (tx, ty) = (u - x0, v - y0);
    let (x0, y0) = (x0 as i32, y0 as i32);
    let p = [
        src.get(x0, y0),
        src.get(x0 + 1, y0),
        src.get(x0, y0 + 1),
        src.get(x0 + 1, y0 + 1),
    ];
    let ch = |shift: u32| {
        let c = |i: usize| ((p[i] >> shift) & 0xff) as f32;
        let top = c(0) + (c(1) - c(0)) * tx;
        let bot = c(2) + (c(3) - c(2)) * tx;
        ((top + (bot - top) * ty).round().clamp(0.0, 255.0)) as u32
    };
    (ch(16) << 16) | (ch(8) << 8) | ch(0)
}

/// One box-blur pass over `n` pixels, radius `r`, edges extended.
#[allow(clippy::needless_range_loop)]
fn box_line(line: &mut [u32], n: usize, r: usize, tmp: &mut [u32]) {
    let get = |l: &[u32], i: isize| l[i.clamp(0, n as isize - 1) as usize];
    let (mut sr, mut sg, mut sb) = (0u32, 0u32, 0u32);
    let ri = r as isize;
    for i in -ri..=ri {
        let p = get(line, i);
        sr += (p >> 16) & 0xff;
        sg += (p >> 8) & 0xff;
        sb += p & 0xff;
    }
    let d = (2 * r + 1) as u32;
    for i in 0..n {
        tmp[i] = ((sr / d) << 16) | ((sg / d) << 8) | (sb / d);
        let out = get(line, i as isize - ri);
        let inn = get(line, i as isize + ri + 1);
        sr = sr + ((inn >> 16) & 0xff) - ((out >> 16) & 0xff);
        sg = sg + ((inn >> 8) & 0xff) - ((out >> 8) & 0xff);
        sb = sb + (inn & 0xff) - (out & 0xff);
    }
    line[..n].copy_from_slice(&tmp[..n]);
}
