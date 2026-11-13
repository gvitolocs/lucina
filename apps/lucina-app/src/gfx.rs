//! Drawing: an XRGB8888 frame in memory, rectangles with anti-aliased corners, and text (Lato with a
//! DejaVu fallback, outlines from `skrifa`, coverage from `ab_glyph_rasterizer`) cached per
//! (physical face, size, character).
//!
//! Everything is drawn into a [`Frame`] first; the output (`fb.rs`) only copies the changed
//! rectangles to the screen, so the framebuffer, which is write-combined, is never read.

use crate::dirty::mix;
use std::collections::HashMap;

use ab_glyph_rasterizer::{Point, Rasterizer, point};
use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::{FontRef, GlyphId, MetadataProvider};

/// A colour as `0x00RRGGBB`, the layout of an XRGB8888 scanout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb(pub u32);

impl Rgb {
    pub const fn hex(v: u32) -> Self {
        Rgb(v & 0x00ff_ffff)
    }

    /// `self` over `under` with coverage `a` (0 = only `under`, 255 = only `self`).
    pub fn over(self, under: Rgb, a: u8) -> Rgb {
        match a {
            0 => under,
            255 => self,
            _ => {
                let a = a as u32;
                let mix = |shift: u32| {
                    let s = (self.0 >> shift) & 0xff;
                    let d = (under.0 >> shift) & 0xff;
                    ((s * a + d * (255 - a) + 127) / 255) << shift
                };
                Rgb(mix(16) | mix(8) | mix(0))
            }
        }
    }
}

/// A rectangle in pixels; empty when `w` or `h` is not positive. Signed so that layouts may run
/// off the frame and be clipped.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Rect { x, y, w, h }
    }
    pub fn is_empty(&self) -> bool {
        self.w <= 0 || self.h <= 0
    }
    pub fn right(&self) -> i32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && y >= self.y && x < self.right() && y < self.bottom()
    }
    pub fn intersect(&self, o: &Rect) -> Rect {
        let x = self.x.max(o.x);
        let y = self.y.max(o.y);
        let r = self.right().min(o.right());
        let b = self.bottom().min(o.bottom());
        Rect::new(x, y, (r - x).max(0), (b - y).max(0))
    }
    /// The smallest rectangle holding both (an empty one adds nothing).
    pub fn union(&self, o: &Rect) -> Rect {
        if self.is_empty() {
            return *o;
        }
        if o.is_empty() {
            return *self;
        }
        let x = self.x.min(o.x);
        let y = self.y.min(o.y);
        Rect::new(
            x,
            y,
            self.right().max(o.right()) - x,
            self.bottom().max(o.bottom()) - y,
        )
    }
    /// Shrunk by `d` on every side.
    pub fn inset(&self, d: i32) -> Rect {
        Rect::new(self.x + d, self.y + d, self.w - 2 * d, self.h - 2 * d)
    }
}

/// Where a frame's pixels live: ordinary memory, or a GPU buffer, which is just as fast to paint and which the GPU reads with no copy.
pub enum Pixels {
    Heap(Vec<u32>),
    Gpu(crate::gpu::Buffer),
}

impl std::ops::Deref for Pixels {
    type Target = [u32];
    fn deref(&self) -> &[u32] {
        match self {
            Pixels::Heap(px) => px,
            Pixels::Gpu(buffer) => buffer.as_slice(),
        }
    }
}

impl std::ops::DerefMut for Pixels {
    fn deref_mut(&mut self) -> &mut [u32] {
        match self {
            Pixels::Heap(px) => px,
            Pixels::Gpu(buffer) => buffer.pixels(),
        }
    }
}

/// A frame of `w` x `h` XRGB8888 pixels, rows packed (stride = width).
pub struct Frame {
    pub w: u32,
    pub h: u32,
    pub px: Pixels,
    /// Drawing stays inside this rectangle.
    clip: Rect,
    /// While set, drawing is written down here instead of done: see [`Recording`].
    rec: Option<Box<Recording>>,
}

impl Frame {
    pub fn new(w: u32, h: u32) -> Frame {
        Frame {
            w,
            h,
            px: Pixels::Heap(vec![0; w as usize * h as usize]),
            clip: Rect::new(0, 0, w as i32, h as i32),
            rec: None,
        }
    }

    /// From here on drawing is written down instead of done, until [`take_recording`](Self::take_recording). `rec` is
    /// emptied, and keeps its memory.
    pub(crate) fn start_recording(&mut self, mut rec: Box<Recording>) {
        rec.clear();
        self.rec = Some(rec);
    }

    /// Forget what was written down so far, to record again.
    pub(crate) fn restart_recording(&mut self) {
        if let Some(rec) = &mut self.rec {
            rec.clear();
        }
    }

    pub(crate) fn take_recording(&mut self) -> Option<Box<Recording>> {
        self.rec.take()
    }

    /// Do what was written down, but only inside `areas`, which do not overlap. Drawing inside them starts over from
    /// the bottom of the frame, so blending sees what it would have seen.
    pub(crate) fn replay(&mut self, rec: &Recording, text: &mut Text, areas: &[Rect]) {
        debug_assert!(self.rec.is_none());
        for area in areas {
            for op in &rec.ops {
                if op.bounds.intersect(area).is_empty() {
                    continue;
                }
                let clip = op.clip.intersect(area);
                self.set_clip(clip);
                match op.kind {
                    Kind::Fill(r, c) => self.fill_rect(r, c),
                    Kind::Round(r, rad, c) => self.fill_round_rect(r, rad, c),
                    Kind::Disc(x, y, rad, c) => self.fill_disc(x, y, rad, c),
                    Kind::Blend(x, y, c, a) => self.blend(x, y, c, a),
                    Kind::Dim(a) => self.dim_area(clip, a),
                    Kind::Image { x, y, w, ref at } => {
                        self.blit_alpha(x, y, w, &rec.pixels[at.clone()])
                    }
                    Kind::Pixels { x, y, w, ref at } => {
                        self.copy_pixels(x, y, w, &rec.pixels[at.clone()])
                    }
                    Kind::Text {
                        x,
                        y,
                        face,
                        px,
                        color,
                        ref at,
                    } => {
                        text.draw_clipped(self, clip, x, y, face, px, color, &rec.text[at.clone()]);
                    }
                }
            }
        }
        self.set_clip(self.bounds());
    }

    /// Moves the pixels into a GPU buffer of the frame's own size and packed rows. A buffer that does not fit is handed back.
    pub fn move_to_buffer(
        &mut self,
        mut buffer: crate::gpu::Buffer,
    ) -> Result<(), crate::gpu::Buffer> {
        if buffer.width != self.w || buffer.height != self.h || buffer.pitch != self.w * 4 {
            return Err(buffer);
        }
        buffer.pixels().copy_from_slice(&self.px);
        self.px = Pixels::Gpu(buffer);
        Ok(())
    }

    /// The GPU buffer the pixels are in, if they are in one.
    pub fn buffer(&self) -> Option<&crate::gpu::Buffer> {
        match &self.px {
            Pixels::Heap(_) => None,
            Pixels::Gpu(buffer) => Some(buffer),
        }
    }

    pub fn bounds(&self) -> Rect {
        Rect::new(0, 0, self.w as i32, self.h as i32)
    }

    /// Keep drawing inside `r` (and the frame) until the next call.
    pub fn set_clip(&mut self, r: Rect) {
        self.clip = r.intersect(&self.bounds());
    }

    pub(crate) fn clip(&self) -> Rect {
        self.clip
    }

    pub fn get(&self, x: i32, y: i32) -> Option<Rgb> {
        self.bounds()
            .contains(x, y)
            .then(|| Rgb(self.px[y as usize * self.w as usize + x as usize]))
    }

    /// Blend `c` with coverage `a` onto one pixel; outside the frame is ignored.
    pub fn blend(&mut self, x: i32, y: i32, c: Rgb, a: u8) {
        if a == 0 || !self.clip.contains(x, y) {
            return;
        }
        let clip = self.clip;
        if let Some(rec) = &mut self.rec {
            rec.push(Kind::Blend(x, y, c, a), Rect::new(x, y, 1, 1), clip);
            return;
        }
        let p = &mut self.px[y as usize * self.w as usize + x as usize];
        *p = c.over(Rgb(*p), a).0;
    }

    pub fn fill_rect(&mut self, r: Rect, c: Rgb) {
        let r = r.intersect(&self.clip);
        if r.is_empty() {
            return;
        }
        let clip = self.clip;
        if let Some(rec) = &mut self.rec {
            rec.push(Kind::Fill(r, c), r, clip);
            return;
        }
        let w = self.w as usize;
        let _p = crate::prof::region(crate::prof::FILL);
        for y in r.y..r.bottom() {
            let row = y as usize * w;
            self.px[row + r.x as usize..row + r.right() as usize].fill(c.0);
        }
    }

    /// A rectangle with anti-aliased corners of radius `rad`.
    pub fn fill_round_rect(&mut self, r: Rect, rad: i32, c: Rgb) {
        let rad = rad.min(r.w / 2).min(r.h / 2).max(0);
        if rad == 0 {
            return self.fill_rect(r, c);
        }
        let clip = self.clip;
        if let Some(rec) = &mut self.rec {
            let bounds = r.intersect(&clip);
            if !bounds.is_empty() {
                rec.push(Kind::Round(r, rad, c), bounds, clip);
            }
            return;
        }
        self.fill_rect(Rect::new(r.x, r.y + rad, r.w, r.h - 2 * rad), c);
        self.fill_rect(Rect::new(r.x + rad, r.y, r.w - 2 * rad, rad), c);
        self.fill_rect(
            Rect::new(r.x + rad, r.bottom() - rad, r.w - 2 * rad, rad),
            c,
        );
        let rf = rad as f32;
        let _s = crate::prof::region(crate::prof::SHAPE);
        for dy in 0..rad {
            for dx in 0..rad {
                // Distance from the pixel's centre to the centre of the top-left corner's arc.
                let fx = rf - (dx as f32 + 0.5);
                let fy = rf - (dy as f32 + 0.5);
                let a = edge_coverage((fx * fx + fy * fy).sqrt(), rf);
                if a == 0 {
                    continue;
                }
                self.blend(r.x + dx, r.y + dy, c, a);
                self.blend(r.right() - 1 - dx, r.y + dy, c, a);
                self.blend(r.x + dx, r.bottom() - 1 - dy, c, a);
                self.blend(r.right() - 1 - dx, r.bottom() - 1 - dy, c, a);
            }
        }
    }

    /// A rounded rectangle filled with `fill` inside a `width`-pixel border of `border`.
    pub fn bordered_round_rect(&mut self, r: Rect, rad: i32, width: i32, border: Rgb, fill: Rgb) {
        self.fill_round_rect(r, rad, border);
        self.fill_round_rect(r.inset(width), (rad - width).max(0), fill);
    }

    /// An anti-aliased disc of radius `rad` centred on (`cx`, `cy`).
    pub fn fill_disc(&mut self, cx: f32, cy: f32, rad: f32, c: Rgb) {
        let (x0, x1) = (
            (cx - rad - 1.0).floor() as i32,
            (cx + rad + 1.0).ceil() as i32,
        );
        let (y0, y1) = (
            (cy - rad - 1.0).floor() as i32,
            (cy + rad + 1.0).ceil() as i32,
        );
        let clip = self.clip;
        if let Some(rec) = &mut self.rec {
            let bounds = Rect::new(x0, y0, x1 - x0 + 1, y1 - y0 + 1).intersect(&clip);
            if !bounds.is_empty() {
                rec.push(Kind::Disc(cx, cy, rad, c), bounds, clip);
            }
            return;
        }
        let _s = crate::prof::region(crate::prof::SHAPE);
        for y in y0..=y1 {
            for x in x0..=x1 {
                let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
                self.blend(x, y, c, edge_coverage((dx * dx + dy * dy).sqrt(), rad));
            }
        }
    }

    /// Darken the whole frame: `a` of 255 black over it, behind a dialog.
    pub fn dim(&mut self, a: u8) {
        let all = self.bounds();
        if let Some(rec) = &mut self.rec {
            rec.push(Kind::Dim(a), all, all);
            return;
        }
        self.dim_area(all, a);
    }

    fn dim_area(&mut self, r: Rect, a: u8) {
        let k = 255 - a as u32;
        let w = self.w as usize;
        for y in r.y..r.bottom() {
            let row = y as usize * w;
            for p in &mut self.px[row + r.x as usize..row + r.right() as usize] {
                let rb = (((*p & 0x00ff_00ff) * k) >> 8) & 0x00ff_00ff;
                let g = (((*p & 0x0000_ff00) * k) >> 8) & 0x0000_ff00;
                *p = rb | g;
            }
        }
    }

    /// A picture with its top-left corner at (`x`, `y`), one to one, using its alpha.
    pub fn draw_image(&mut self, x: i32, y: i32, img: &crate::image::Image) {
        let clip = self.clip;
        if let Some(rec) = &mut self.rec {
            rec.push_pixels(x, y, img.w as usize, &img.px, clip, true);
            return;
        }
        self.blit_alpha(x, y, img.w as usize, &img.px);
    }

    /// Rows of `stride` pixels with their alpha, at (`x`, `y`), inside the clip.
    fn blit_alpha(&mut self, x: i32, y: i32, stride: usize, px: &[u32]) {
        if stride == 0 {
            return;
        }
        let rows = (px.len() / stride) as i32;
        let r = Rect::new(x, y, stride as i32, rows).intersect(&self.clip);
        if r.is_empty() {
            return;
        }
        let w = self.w as usize;
        for py in r.y..r.bottom() {
            let src = (py - y) as usize * stride + (r.x - x) as usize;
            let dst = py as usize * w + r.x as usize;
            let n = r.w as usize;
            for (d, &s) in self.px[dst..dst + n].iter_mut().zip(&px[src..src + n]) {
                let a = (s >> 24) as u8;
                *d = Rgb(s & 0x00ff_ffff).over(Rgb(*d), a).0;
            }
        }
    }

    /// Copy opaque pixels to (`x`, `y`), inside the clip: rows of `stride` pixels, as many as `px` holds.
    /// A whole screen of them, every frame, is what a browser's page is, so this does no blending.
    pub fn copy_pixels(&mut self, x: i32, y: i32, stride: usize, px: &[u32]) {
        if stride == 0 {
            return;
        }
        let clip = self.clip;
        if let Some(rec) = &mut self.rec {
            rec.push_pixels(x, y, stride, px, clip, false);
            return;
        }
        let rows = (px.len() / stride) as i32;
        let r = Rect::new(x, y, stride as i32, rows).intersect(&self.clip);
        if r.is_empty() {
            return;
        }
        let w = self.w as usize;
        let n = r.w as usize;
        for py in r.y..r.bottom() {
            let src = (py - y) as usize * stride + (r.x - x) as usize;
            let dst = py as usize * w + r.x as usize;
            self.px[dst..dst + n].copy_from_slice(&px[src..src + n]);
        }
    }

    /// Rows `r` of the frame, for copying to an output.
    pub fn rows(&self, r: Rect) -> impl Iterator<Item = (i32, &[u32])> {
        let r = r.intersect(&self.bounds());
        let w = self.w as usize;
        (r.y..r.bottom()).map(move |y| {
            let row = y as usize * w;
            (y, &self.px[row + r.x as usize..row + r.right() as usize])
        })
    }

    /// The frame as a binary PPM (P6), the format of QEMU's `screendump`.
    pub fn to_ppm(&self) -> Vec<u8> {
        let mut out = format!("P6\n{} {}\n255\n", self.w, self.h).into_bytes();
        out.reserve(self.px.len() * 3);
        for &p in self.px.iter() {
            out.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, p as u8]);
        }
        out
    }
}

/// A paint pass written down instead of drawn. An app redraws everything for every event, which is what makes it simple to
/// write, and on one CPU with no GPU, filling 8 MB and then hashing it to find out what changed took 11 ms when nothing had.
/// So [`crate::dirty::Tiles::diff_ops`] compares what was asked for, tile by tile, and only the tiles where that differs
/// are drawn. Nothing changes for an app.
#[derive(Default)]
pub(crate) struct Recording {
    pub(crate) ops: Vec<Op>,
    text: String,
    pixels: Vec<u32>,
}

pub(crate) struct Op {
    /// What it can touch. For a tile it does not reach, it might as well not exist.
    pub(crate) bounds: Rect,
    /// Same hash, same pixels, given the same pixels under it.
    pub(crate) hash: u64,
    clip: Rect,
    kind: Kind,
}

enum Kind {
    Fill(Rect, Rgb),
    Round(Rect, i32, Rgb),
    Disc(f32, f32, f32, Rgb),
    Blend(i32, i32, Rgb, u8),
    Dim(u8),
    Image {
        x: i32,
        y: i32,
        w: usize,
        at: std::ops::Range<usize>,
    },
    Pixels {
        x: i32,
        y: i32,
        w: usize,
        at: std::ops::Range<usize>,
    },
    Text {
        x: i32,
        y: i32,
        face: Face,
        px: u16,
        color: Rgb,
        at: std::ops::Range<usize>,
    },
}

fn hash_rect(h: u64, r: Rect) -> u64 {
    let h = mix(h, (r.x as u32 as u64) << 32 | r.y as u32 as u64);
    mix(h, (r.w as u32 as u64) << 32 | r.h as u32 as u64)
}

/// Which of the [`prof`](crate::prof) counters counts an op of this kind.
#[inline(always)]
fn op_counter(kind: &Kind) -> usize {
    match kind {
        Kind::Fill(..) => crate::prof::OPS_FILL,
        Kind::Round(..) | Kind::Disc(..) => crate::prof::OPS_SHAPE,
        Kind::Blend(..) => crate::prof::OPS_BLEND,
        Kind::Image { .. } | Kind::Pixels { .. } | Kind::Dim(..) => crate::prof::OPS_PIX,
        Kind::Text { .. } => crate::prof::OPS_TEXT,
    }
}

impl Kind {
    fn hash(&self, rec: &Recording) -> u64 {
        let two = |a: i32, b: i32| (a as u32 as u64) << 32 | b as u32 as u64;
        match *self {
            Kind::Fill(r, c) => mix(hash_rect(1, r), c.0 as u64),
            Kind::Round(r, rad, c) => mix(hash_rect(2, r), two(rad, c.0 as i32)),
            Kind::Disc(x, y, rad, c) => {
                let h = mix(3, two(x.to_bits() as i32, y.to_bits() as i32));
                mix(h, two(rad.to_bits() as i32, c.0 as i32))
            }
            Kind::Blend(x, y, c, a) => mix(mix(4, two(x, y)), two(a as i32, c.0 as i32)),
            Kind::Dim(a) => mix(5, a as u64),
            Kind::Image { x, y, w, ref at } => {
                hash_px(mix(mix(6, two(x, y)), w as u64), &rec.pixels[at.clone()])
            }
            Kind::Pixels { x, y, w, ref at } => {
                hash_px(mix(mix(7, two(x, y)), w as u64), &rec.pixels[at.clone()])
            }
            Kind::Text {
                x,
                y,
                face,
                px,
                color,
                ref at,
            } => {
                let h = mix(
                    mix(8, two(x, y)),
                    two(face as i32 | (px as i32) << 8, color.0 as i32),
                );
                rec.text.as_bytes()[at.clone()].chunks(8).fold(h, |h, b| {
                    mix(
                        h,
                        b.iter().fold(0u64, |v, &c| v << 8 | c as u64) ^ (b.len() as u64) << 56,
                    )
                })
            }
        }
    }
}

fn hash_px(h: u64, px: &[u32]) -> u64 {
    let (pairs, last) = px.as_chunks::<2>();
    let h = pairs
        .iter()
        .fold(h, |h, p| mix(h, (p[0] as u64) << 32 | p[1] as u64));
    last.first().map_or(h, |p| mix(h, *p as u64))
}

impl Recording {
    fn clear(&mut self) {
        self.ops.clear();
        self.text.clear();
        self.pixels.clear();
    }

    fn push(&mut self, kind: Kind, bounds: Rect, clip: Rect) {
        let _r = crate::prof::region(crate::prof::RECORD);
        crate::prof::count(op_counter(&kind), 1);
        crate::prof::count(crate::prof::OPS, 1);
        // Where it is clipped is part of what it draws.
        let hash = hash_rect(hash_rect(kind.hash(self), clip), bounds);
        self.ops.push(Op {
            bounds,
            hash,
            clip,
            kind,
        });
    }

    /// Only what can show is kept: a list of pictures costs what is on the screen, whatever their size.
    fn push_pixels(&mut self, x: i32, y: i32, stride: usize, px: &[u32], clip: Rect, alpha: bool) {
        if stride == 0 {
            return;
        }
        let rows = (px.len() / stride) as i32;
        let r = Rect::new(x, y, stride as i32, rows).intersect(&clip);
        if r.is_empty() {
            return;
        }
        let start = self.pixels.len();
        for py in r.y..r.bottom() {
            let src = (py - y) as usize * stride + (r.x - x) as usize;
            self.pixels.extend_from_slice(&px[src..src + r.w as usize]);
        }
        let at = start..self.pixels.len();
        let (x, y, w) = (r.x, r.y, r.w as usize);
        let kind = if alpha {
            Kind::Image { x, y, w, at }
        } else {
            Kind::Pixels { x, y, w, at }
        };
        self.push(kind, r, clip);
    }
}

/// The pointer: X outline, . fill.
const ARROW: [&str; 17] = [
    "X",
    "XX",
    "X.X",
    "X..X",
    "X...X",
    "X....X",
    "X.....X",
    "X......X",
    "X.......X",
    "X........X",
    "X.....XXXXX",
    "X..X..X",
    "X.X X..X",
    "XX  X..X",
    "X    X..X",
    "     X..X",
    "      XX",
];

/// The pointer's size in pixels at scale `n`.
pub fn pointer_size(n: i32) -> (i32, i32) {
    (11 * n, ARROW.len() as i32 * n)
}

/// The pointer drawn over the frame's pixels at (`x`, `y`), as an image of `pointer_size(n)`. Off the
/// frame `off` shows through.
pub fn pointer_image(fr: &Frame, x: i32, y: i32, n: i32, off: Rgb) -> Vec<u32> {
    let (w, h) = pointer_size(n);
    let mut img = vec![0u32; (w * h) as usize];
    for py in 0..h {
        for px in 0..w {
            let under = fr.get(x + px, y + py).unwrap_or(off);
            let row = ARROW[(py / n) as usize].as_bytes();
            let c = match row.get((px / n) as usize) {
                Some(b'X') => Rgb::hex(0x000000),
                Some(b'.') => Rgb::hex(0xffffff),
                _ => under,
            };
            img[(py * w + px) as usize] = c.0;
        }
    }
    img
}

/// Coverage (0..=255) of a pixel whose centre is `d` from a circle's centre, for radius `rad`:
/// a one-pixel ramp across the edge.
fn edge_coverage(d: f32, rad: f32) -> u8 {
    let t = rad - d + 0.5;
    if t <= 0.0 {
        0
    } else if t >= 1.0 {
        255
    } else {
        (t * 255.0 + 0.5) as u8
    }
}

/// The faces an app asks for, named as they have always been. The two sans are Lato, the glass kit's
/// UI face; DejaVu stands behind them for the glyphs Lato's subsets do not carry, and the mono stays
/// DejaVu so that a cell of the terminal and a column of figures keep lining up.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Face {
    /// Lato Regular.
    Sans,
    /// Lato Semibold.
    Bold,
    /// DejaVu Sans Mono.
    Mono,
}

impl Face {
    /// The glass kit's name for this face: what `tokens::ty` sizes are drawn in.
    pub fn glass(self) -> lucina_glass::text::Font {
        match self {
            Face::Sans => lucina_glass::text::Font::Regular,
            Face::Bold => lucina_glass::text::Font::Semibold,
            Face::Mono => lucina_glass::text::Font::Mono,
        }
    }

    /// The file the face's own glyphs come from, before any fallback: what [`Face::glass`] names.
    pub fn file(self) -> &'static str {
        self.slot().file()
    }

    /// The physical faces tried for a glyph of this face, first hit wins.
    fn chain(self) -> [Slot; 3] {
        match self {
            Face::Sans => [Slot::Lato, Slot::Sans, Slot::SansBold],
            Face::Bold => [Slot::LatoSemibold, Slot::SansBold, Slot::Sans],
            Face::Mono => [Slot::Mono, Slot::Sans, Slot::SansBold],
        }
    }

    /// The face line metrics come from: the chain's first, never a fallback, so a line's height
    /// does not depend on which scripts the string happens to hold.
    fn slot(self) -> Slot {
        self.chain()[0]
    }
}

/// A file that is really loaded, one per `FontRef` of [`Text::fonts`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Slot {
    Lato,
    LatoSemibold,
    Sans,
    SansBold,
    Mono,
}

impl Slot {
    const fn file(self) -> &'static str {
        match self {
            Slot::Lato => "Lato-Regular.ttf",
            Slot::LatoSemibold => "Lato-Semibold.ttf",
            Slot::Sans => "DejaVuSans.ttf",
            Slot::SansBold => "DejaVuSans-Bold.ttf",
            Slot::Mono => "DejaVuSansMono.ttf",
        }
    }
}

/// In the order of [`Slot`]: `fonts[slot as usize]` is that face.
const ALL_SLOTS: [Slot; 5] = [
    Slot::Lato,
    Slot::LatoSemibold,
    Slot::Sans,
    Slot::SansBold,
    Slot::Mono,
];

/// Where the Lato faces are on the device, beside [`FONT_DIR`]: the two the toolkit uses, installed by
/// `scripts/gui-initramfs.sh` from the glass kit's subsets. Mapped like DejaVu, for the same reason.
pub const LATO_DIR: &str = "/usr/share/fonts/lato";

/// Where the DejaVu faces are on the device: one copy in the initramfs for all programs, mapped
/// read-only, rather than 1.8 MB inside each of twenty binaries.
pub const FONT_DIR: &str = "/usr/share/fonts/dejavu";

/// A face, mapped from a file for the rest of the program's life.
pub(crate) fn load_font(dir: &std::path::Path, name: &str) -> Option<&'static [u8]> {
    use std::os::fd::AsRawFd;
    let file = std::fs::File::open(dir.join(name)).ok()?;
    let len = file.metadata().ok()?.len() as usize;
    // SAFETY: a private read-only mapping of a whole file, never unmapped, so the slice stays valid.
    // The fonts are not changed while running.
    let map = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            len,
            libc::PROT_READ,
            libc::MAP_PRIVATE,
            file.as_raw_fd(),
            0,
        )
    };
    if map != libc::MAP_FAILED {
        // SAFETY: as above: `len` readable bytes that live forever.
        return Some(unsafe { std::slice::from_raw_parts(map.cast::<u8>(), len) });
    }
    Some(Box::leak(
        std::fs::read(dir.join(name)).ok()?.into_boxed_slice(),
    ))
}

/// The directories to look for fonts in: `LUCINA_FONT_DIR`, the device's, and off the device (tests,
/// `--render`) the source tree's — Lato's files live in the glass kit.
fn font_dirs() -> Vec<std::path::PathBuf> {
    let mut dirs: Vec<std::path::PathBuf> = std::env::var_os("LUCINA_FONT_DIR")
        .map(Into::into)
        .into_iter()
        .collect();
    dirs.push(LATO_DIR.into());
    dirs.push(FONT_DIR.into());
    #[cfg(not(target_env = "musl"))]
    dirs.extend([
        concat!(env!("CARGO_MANIFEST_DIR"), "/fonts").into(),
        concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/lucina-glass/fonts").into(),
    ]);
    dirs
}

struct Glyph {
    /// Offset of the bitmap's top-left corner from the pen position on the baseline (y down).
    left: i32,
    top: i32,
    w: usize,
    h: usize,
    advance: f32,
    alpha: Vec<u8>,
}

/// One outline segment in pixels, y down, from the pen position on the baseline.
#[derive(Clone, Copy)]
enum Segment {
    Line(Point, Point),
    Quad(Point, Point, Point),
    Cubic(Point, Point, Point, Point),
}

/// Collects a glyph's outline from skrifa (font units already scaled to pixels, y up) as segments
/// with y down, closing every contour, and keeps their bounds.
#[derive(Default)]
struct Pen {
    segments: Vec<Segment>,
    start: Option<Point>,
    at: Point,
    min: (f32, f32),
    max: (f32, f32),
}

impl Pen {
    fn p(&mut self, x: f32, y: f32) -> Point {
        let p = point(x, -y);
        if self.segments.is_empty() && self.start.is_none() {
            self.min = (p.x, p.y);
            self.max = (p.x, p.y);
        }
        self.min = (self.min.0.min(p.x), self.min.1.min(p.y));
        self.max = (self.max.0.max(p.x), self.max.1.max(p.y));
        p
    }
}

impl OutlinePen for Pen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.close();
        let p = self.p(x, y);
        self.start = Some(p);
        self.at = p;
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let p = self.p(x, y);
        self.segments.push(Segment::Line(self.at, p));
        self.at = p;
    }
    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        let (c, p) = (self.p(cx0, cy0), self.p(x, y));
        self.segments.push(Segment::Quad(self.at, c, p));
        self.at = p;
    }
    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        let (c0, c1, p) = (self.p(cx0, cy0), self.p(cx1, cy1), self.p(x, y));
        self.segments.push(Segment::Cubic(self.at, c0, c1, p));
        self.at = p;
    }
    fn close(&mut self) {
        if let Some(s) = self.start.take()
            && (s.x != self.at.x || s.y != self.at.y)
        {
            self.segments.push(Segment::Line(self.at, s));
            self.at = s;
        }
    }
}

/// Lato Regular and Semibold, DejaVu Sans, Sans Bold and Sans Mono, with a glyph cache. A [`Face`]
/// is a chain of these: the glyph is looked for in each until one has it, so a Cyrillic word in a
/// Lato label comes out of DejaVu and the Latin beside it stays Lato. Sizes are em sizes in pixels
/// (what a CSS or GRUB font size means).
pub struct Text {
    fonts: [FontRef<'static>; ALL_SLOTS.len()],
    /// The glyph of a character in a face at a size, fallback slot resolved included.
    cache: HashMap<(Face, u16, char), Glyph>,
}

impl Default for Text {
    fn default() -> Self {
        Self::new()
    }
}

impl Text {
    /// # Panics
    /// When the fonts are not installed (scripts/gui-initramfs.sh puts them in the image).
    pub fn new() -> Text {
        let dirs = font_dirs();
        let load = |name: &str| {
            let bytes = dirs
                .iter()
                .find_map(|d| load_font(d, name))
                .unwrap_or_else(|| panic!("font {name} not found in {dirs:?}"));
            FontRef::new(bytes).unwrap_or_else(|e| panic!("font {name}: {e}"))
        };
        Text {
            fonts: ALL_SLOTS.map(|s| load(s.file())),
            cache: HashMap::new(),
        }
    }

    fn font(&self, f: Face) -> &FontRef<'static> {
        &self.fonts[f.slot() as usize]
    }

    /// Whether a glyph for `c` will be found: in the face itself or in the one behind it.
    pub fn has(&self, f: Face, c: char) -> bool {
        let fonts = &self.fonts;
        !c.is_control() && f.chain().into_iter().any(|s| Self::slot_has(fonts, s, c))
    }

    fn slot_has(fonts: &[FontRef<'static>], s: Slot, c: char) -> bool {
        fonts[s as usize]
            .charmap()
            .map(c)
            .is_some_and(|g| g != GlyphId::NOTDEF)
    }

    /// Which face `c` is really drawn in. Control characters keep the primary face: they have no
    /// glyph anywhere and must not pick up another font's `.notdef` box.
    fn slot_for(fonts: &[FontRef<'static>], f: Face, c: char) -> Slot {
        if c.is_control() {
            return f.slot();
        }
        f.chain()
            .into_iter()
            .find(|s| Self::slot_has(fonts, *s, c))
            .unwrap_or(f.slot())
    }

    /// Height above the baseline of tall letters. Of the face's own font, never of a fallback: a
    /// line's height is the same whatever scripts its string happens to hold.
    pub fn ascent(&self, f: Face, px: u16) -> i32 {
        let m = self
            .font(f)
            .metrics(Size::new(px as f32), LocationRef::default());
        m.ascent.round() as i32
    }

    /// Depth below the baseline (positive).
    pub fn descent(&self, f: Face, px: u16) -> i32 {
        let m = self
            .font(f)
            .metrics(Size::new(px as f32), LocationRef::default());
        (-m.descent).round() as i32
    }

    /// What is kept is the whole answer, the fallback with it: a frame that draws known glyphs pays
    /// one cache lookup each and walks no charmap.
    fn glyph(&mut self, f: Face, px: u16, c: char) -> &Glyph {
        let fonts = &self.fonts;
        self.cache.entry((f, px, c)).or_insert_with(|| {
            crate::prof::count(crate::prof::MISS, 1);
            let _r = crate::prof::region(crate::prof::RASTER);
            let slot = Self::slot_for(fonts, f, c);
            Self::rasterise(fonts, slot, px, c)
        })
    }

    fn rasterise(fonts: &[FontRef<'static>], slot: Slot, px: u16, c: char) -> Glyph {
        let font = &fonts[slot as usize];
        let size = Size::new(px as f32);
        let id = font.charmap().map(c).unwrap_or(GlyphId::NOTDEF);
        // A character no face in the chain has (CJK, an emoji in mono) is left out rather than drawn
        // as a box.
        if id == GlyphId::NOTDEF && !c.is_control() {
            return Glyph {
                left: 0,
                top: 0,
                w: 0,
                h: 0,
                advance: 0.0,
                alpha: Vec::new(),
            };
        }
        let advance = font
            .glyph_metrics(size, LocationRef::default())
            .advance_width(id)
            .unwrap_or(0.0);
        let mut pen = Pen::default();
        let drawn = font.outline_glyphs().get(id).map(|o| {
            o.draw(
                DrawSettings::unhinted(size, LocationRef::default()),
                &mut pen,
            )
        });
        pen.close();
        if !matches!(drawn, Some(Ok(_))) || pen.segments.is_empty() {
            // A space (or an outline that cannot be read): nothing to draw.
            return Glyph {
                left: 0,
                top: 0,
                w: 0,
                h: 0,
                advance,
                alpha: Vec::new(),
            };
        }
        let (left, top) = (pen.min.0.floor(), pen.min.1.floor());
        let w = (pen.max.0 - left).ceil() as usize + 1;
        let h = (pen.max.1 - top).ceil() as usize + 1;
        let o = |p: Point| point(p.x - left, p.y - top);
        let mut r = Rasterizer::new(w, h);
        for s in &pen.segments {
            match *s {
                Segment::Line(a, b) => r.draw_line(o(a), o(b)),
                Segment::Quad(a, b, c) => r.draw_quad(o(a), o(b), o(c)),
                Segment::Cubic(a, b, c, d) => r.draw_cubic(o(a), o(b), o(c), o(d)),
            }
        }
        let mut alpha = vec![0u8; w * h];
        r.for_each_pixel(|i, cov| alpha[i] = (cov.clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
        Glyph {
            left: left as i32,
            top: top as i32,
            w,
            h,
            advance,
            alpha,
        }
    }

    /// Horizontal advance of one character.
    pub fn advance(&mut self, f: Face, px: u16, c: char) -> f32 {
        self.glyph(f, px, c).advance
    }

    /// Width of `s` in pixels.
    pub fn measure(&mut self, f: Face, px: u16, s: &str) -> i32 {
        let mut pen = 0.0;
        for c in s.chars() {
            pen += self.glyph(f, px, c).advance;
        }
        pen.round() as i32
    }

    /// Draw `s` with its baseline at `y` from `x`, clipped to `clip`; returns the width.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_clipped(
        &mut self,
        fr: &mut Frame,
        clip: Rect,
        x: i32,
        y: i32,
        f: Face,
        px: u16,
        color: Rgb,
        s: &str,
    ) -> i32 {
        let clip = clip.intersect(&fr.clip);
        crate::prof::count(crate::prof::STRINGS, 1);
        crate::prof::count(crate::prof::CHARS, s.chars().count() as u64);
        if fr.rec.is_some() {
            let _w = crate::prof::region(crate::prof::WALK_REC);
            // Where its glyphs really land: a tile they reach has to know about them.
            let mut pen = x as f32;
            let mut bounds: Option<(i32, i32, i32, i32)> = None;
            for c in s.chars() {
                let g = self.glyph(f, px, c);
                if g.w > 0 && g.h > 0 {
                    let (gx, gy) = (pen.round() as i32 + g.left, y + g.top);
                    let (r, b) = (gx + g.w as i32, gy + g.h as i32);
                    bounds = Some(bounds.map_or((gx, gy, r, b), |o| {
                        (o.0.min(gx), o.1.min(gy), o.2.max(r), o.3.max(b))
                    }));
                }
                pen += g.advance;
            }
            if let (Some(rec), Some((l, t, r, b))) = (&mut fr.rec, bounds) {
                let bounds = Rect::new(l, t, r - l, b - t).intersect(&clip);
                if !bounds.is_empty() {
                    let start = rec.text.len();
                    rec.text.push_str(s);
                    let at = start..rec.text.len();
                    rec.push(
                        Kind::Text {
                            x,
                            y,
                            face: f,
                            px,
                            color,
                            at,
                        },
                        bounds,
                        clip,
                    );
                }
            }
            return (pen - x as f32).round() as i32;
        }
        let stride = fr.w as usize;
        let _w = crate::prof::region(crate::prof::WALK_DRAW);
        let mut pen = x as f32;
        for c in s.chars() {
            let g = self.glyph(f, px, c);
            let gx = pen.round() as i32 + g.left;
            let gy = y + g.top;
            let first = (clip.x - gx).max(0) as usize;
            let last = ((clip.right() - gx).max(0) as usize).min(g.w);
            for row in 0..g.h {
                let py = gy + row as i32;
                if py < clip.y || py >= clip.bottom() || first >= last {
                    continue;
                }
                let dst = py as usize * stride + (gx + first as i32) as usize;
                let cover = &g.alpha[row * g.w + first..row * g.w + last];
                for (d, &a) in fr.px[dst..dst + cover.len()].iter_mut().zip(cover) {
                    if a != 0 {
                        *d = color.over(Rgb(*d), a).0;
                    }
                }
            }
            pen += g.advance;
        }
        (pen - x as f32).round() as i32
    }

    /// Draw `s` with its baseline at `y` from `x`; returns the width.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        fr: &mut Frame,
        x: i32,
        y: i32,
        f: Face,
        px: u16,
        color: Rgb,
        s: &str,
    ) -> i32 {
        let all = fr.bounds();
        self.draw_clipped(fr, all, x, y, f, px, color, s)
    }

    /// The byte offset in `s` closest to `x` pixels from its start: where a click lands.
    pub fn offset_at(&mut self, f: Face, px: u16, s: &str, x: i32) -> usize {
        let mut pen = 0.0;
        for (i, c) in s.char_indices() {
            if c == '\n' {
                return i;
            }
            let a = self.advance(f, px, c);
            if (x as f32) < pen + a / 2.0 {
                return i;
            }
            pen += a;
        }
        s.len()
    }

    /// `s` broken into lines at most `max` pixels wide, as byte ranges: at spaces when it can, inside
    /// a word that is wider than the line. Every `\\n` ends a line (and is left out of it); spaces
    /// stay at the end of their line, so the ranges cover all the text. Always at least one line.
    pub fn wrap(&mut self, f: Face, px: u16, s: &str, max: i32) -> Vec<std::ops::Range<usize>> {
        let max = max.max(1) as f32;
        let mut lines = Vec::new();
        let mut start = 0;
        let mut pen = 0.0;
        // The last place to break, and the width up to it.
        let mut brk: Option<(usize, f32)> = None;
        for (i, c) in s.char_indices() {
            if c == '\n' {
                lines.push(start..i);
                start = i + 1;
                pen = 0.0;
                brk = None;
                continue;
            }
            let a = self.advance(f, px, c);
            if pen + a > max && !c.is_whitespace() && i > start {
                match brk {
                    Some((at, w)) if at > start => {
                        lines.push(start..at);
                        start = at;
                        pen -= w;
                    }
                    _ => {
                        lines.push(start..i);
                        start = i;
                        pen = 0.0;
                    }
                }
                brk = None;
            }
            pen += a;
            if c.is_whitespace() {
                brk = Some((i + c.len_utf8(), pen));
            }
        }
        lines.push(start..s.len());
        lines
    }

    /// `s` cut to `max` pixels, with an ellipsis when it was cut.
    pub fn fit(&mut self, f: Face, px: u16, s: &str, max: i32) -> String {
        if self.measure(f, px, s) <= max {
            return s.to_string();
        }
        let ell = self.advance(f, px, '…');
        let mut pen = 0.0;
        let mut out = String::new();
        for c in s.chars() {
            let a = self.advance(f, px, c);
            if pen + a + ell > max as f32 {
                break;
            }
            pen += a;
            out.push(c);
        }
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn copy_pixels_stays_inside_the_clip() {
        let mut f = super::Frame::new(4, 4);
        f.set_clip(super::Rect::new(1, 1, 2, 2));
        f.copy_pixels(0, 0, 4, &[7; 16]);
        assert_eq!(f.px.iter().filter(|&&p| p == 7).count(), 4);
        assert_eq!(f.get(1, 1).map(|c| c.0), Some(7));
        assert_eq!(f.get(0, 0).map(|c| c.0), Some(0));
        assert_eq!(f.get(3, 3).map(|c| c.0), Some(0));
    }

    use super::*;

    #[test]
    fn blending_ends_and_middle() {
        let (w, k) = (Rgb::hex(0xffffff), Rgb::hex(0));
        assert_eq!(w.over(k, 0), k);
        assert_eq!(w.over(k, 255), w);
        assert_eq!(w.over(k, 128), Rgb::hex(0x808080));
    }

    #[test]
    fn rects_clip_and_join() {
        let a = Rect::new(0, 0, 10, 10);
        assert_eq!(
            a.intersect(&Rect::new(5, -5, 10, 10)),
            Rect::new(5, 0, 5, 5)
        );
        assert!(a.intersect(&Rect::new(20, 20, 1, 1)).is_empty());
        assert_eq!(a.union(&Rect::new(20, 5, 1, 1)), Rect::new(0, 0, 21, 10));
        assert_eq!(Rect::default().union(&a), a);
    }

    #[test]
    fn fill_clips_to_the_frame() {
        let mut f = Frame::new(5, 4);
        f.fill_rect(Rect::new(-2, 2, 4, 9), Rgb::hex(0x123456));
        assert_eq!(f.get(0, 1), Some(Rgb(0)));
        assert_eq!(f.get(1, 3), Some(Rgb::hex(0x123456)));
        assert_eq!(f.get(2, 3), Some(Rgb(0)));
    }

    #[test]
    fn round_corners_are_cut() {
        let mut f = Frame::new(40, 40);
        f.fill_round_rect(Rect::new(0, 0, 40, 40), 10, Rgb::hex(0xffffff));
        assert_eq!(f.get(0, 0), Some(Rgb(0)));
        assert_eq!(f.get(20, 0), Some(Rgb::hex(0xffffff)));
        assert_eq!(f.get(20, 20), Some(Rgb::hex(0xffffff)));
    }

    #[test]
    fn text_draws_inside_its_box() {
        let mut t = Text::new();
        let mut f = Frame::new(300, 80);
        let w = t.draw(
            &mut f,
            10,
            60,
            Face::Bold,
            48,
            Rgb::hex(0xffffff),
            "LucinaOS",
        );
        assert!(w > 150 && w < 300, "{w}");
        assert_eq!(w, t.measure(Face::Bold, 48, "LucinaOS"));
        assert!(f.px.iter().filter(|&&p| p != 0).count() > 1000);
        // Nothing below the baseline for these letters.
        assert!(f.px[62 * 300..].iter().all(|&p| p == 0));
    }

    #[test]
    fn clipping_keeps_text_inside() {
        let mut t = Text::new();
        let mut f = Frame::new(100, 40);
        t.draw_clipped(
            &mut f,
            Rect::new(0, 0, 30, 40),
            0,
            30,
            Face::Sans,
            24,
            Rgb::hex(0xffffff),
            "WWWWWW",
        );
        for y in 0..40 {
            for x in 30..100 {
                assert_eq!(f.get(x, y), Some(Rgb(0)));
            }
        }
    }

    #[test]
    fn mono_cells_are_equal() {
        let mut t = Text::new();
        let a = t.advance(Face::Mono, 16, 'i');
        assert_eq!(a, t.advance(Face::Mono, 16, 'W'));
        assert_eq!(a, t.advance(Face::Mono, 16, '─'));
    }

    #[test]
    fn has_the_shell_symbols() {
        let t = Text::new();
        for c in ['⚙', '✦', '…', '·', '—', '●'] {
            assert!(t.has(Face::Sans, c), "{c}");
        }
        for c in ['─', '│', '█', '▒'] {
            assert!(t.has(Face::Mono, c), "{c}");
        }
    }

    /// Which file a glyph really comes from: Lato for the Latin the subset carries, DejaVu for
    /// everything else, the mono file first for the mono face.
    #[test]
    fn lato_is_the_ui_face_with_dejavu_behind_it() {
        let t = Text::new();
        let slot = |f: Face, c: char| Text::slot_for(&t.fonts, f, c);
        // The array behind `fonts[slot as usize]`, so a reordered enum shows up here.
        assert_eq!(ALL_SLOTS.map(|s| s as usize), [0, 1, 2, 3, 4]);
        assert_eq!(Face::Sans.glass(), lucina_glass::text::Font::Regular);
        assert_eq!(Face::Bold.glass(), lucina_glass::text::Font::Semibold);
        assert_eq!(Face::Mono.glass(), lucina_glass::text::Font::Mono);
        for c in ['a', 'É', 'é', 'æ', '—', '•', '…', '·', '↑'] {
            assert_eq!(slot(Face::Sans, c), Slot::Lato, "{c} is in Lato");
        }
        assert_eq!(slot(Face::Bold, 'a'), Slot::LatoSemibold);
        // Per glyph, not per string: the Latin of a title stays Lato beside its Cyrillic.
        assert_eq!(slot(Face::Sans, 'Я'), Slot::Sans);
        assert_eq!(slot(Face::Sans, 'β'), Slot::Sans);
        assert_eq!(slot(Face::Sans, '★'), Slot::Sans);
        assert_eq!(slot(Face::Bold, '★'), Slot::SansBold);
        assert_eq!(slot(Face::Mono, '─'), Slot::Mono);
        assert_eq!(slot(Face::Mono, 'Я'), Slot::Mono);
        assert_eq!(slot(Face::Mono, '😀'), Slot::Sans);
        // A control character stays in the face asked for: it has no glyph in any of them.
        assert_eq!(slot(Face::Sans, '\n'), Slot::Lato);
        // What no face in the chain has is still left out, and still has no width.
        assert!(!t.has(Face::Sans, '一'));
        assert_eq!(slot(Face::Sans, '一'), Slot::Lato);
    }

    /// Latin, Cyrillic and a star in one string: every character has a width, the mixed word draws,
    /// and `fit` and `wrap` still work across the switch of face.
    #[test]
    fn mixed_scripts_measure_and_draw() {
        let mut t = Text::new();
        let s = "Idéer ★ Лекция — 2026";
        let mut sum = 0.0;
        for c in s.chars() {
            let a = t.advance(Face::Sans, 21, c);
            assert!(a > 0.0, "{c} has no width");
            sum += a;
        }
        assert_eq!(t.measure(Face::Sans, 21, s), sum.round() as i32);
        assert!(t.measure(Face::Sans, 21, "Лекция") > 40);
        // Every non-space character of the mixed string puts pixels somewhere.
        for c in s.chars().filter(|c| !c.is_whitespace()) {
            let mut f = Frame::new(60, 40);
            let one = c.to_string();
            t.draw(&mut f, 4, 28, Face::Sans, 21, Rgb::hex(0xffffff), &one);
            assert!(f.px.iter().any(|&p| p != 0), "{c} drew nothing");
        }
        // The same in a heading and in the mono face of the terminal.
        let mut f = Frame::new(400, 40);
        let w = t.draw(&mut f, 2, 28, Face::Bold, 21, Rgb::hex(0xffffff), s);
        assert!(w > 0 && f.px.iter().any(|&p| p != 0));
        let mut f = Frame::new(400, 40);
        t.draw(&mut f, 2, 28, Face::Mono, 21, Rgb::hex(0xffffff), s);
        // Cut and wrapped: the ellipsis itself is a glyph of the face.
        let cut = t.fit(Face::Sans, 21, s, 120);
        let cut_w = t.measure(Face::Sans, 21, &cut);
        assert!(cut.ends_with('…') && cut_w <= 120, "{cut}");
        let lines = t.wrap(Face::Sans, 21, s, 90);
        assert!(lines.len() >= 2 && !lines.is_empty());
    }

    #[test]
    fn wrap_breaks_at_spaces_and_inside_long_words() {
        let mut t = Text::new();
        let s = "one two three four";
        let w = t.measure(Face::Sans, 16, "one two ");
        let lines = t.wrap(Face::Sans, 16, s, w);
        let got: Vec<&str> = lines.iter().map(|r| &s[r.clone()]).collect();
        assert_eq!(got, ["one two ", "three ", "four"]);
        // The lines cover the text.
        assert_eq!(lines.iter().map(|r| r.len()).sum::<usize>(), s.len());
        let long = "aaaaaaaaaaaaaaaaaaaa";
        let lines = t.wrap(Face::Sans, 16, long, 50);
        assert!(lines.len() > 1);
        for r in &lines {
            assert!(t.measure(Face::Sans, 16, &long[r.clone()]) <= 50);
        }
        let s = "a\n\nb\n";
        let lines = t.wrap(Face::Sans, 16, s, 500);
        let got: Vec<&str> = lines.iter().map(|r| &s[r.clone()]).collect();
        assert_eq!(got, ["a", "", "b", ""]);
        assert_eq!(t.wrap(Face::Sans, 16, "", 10), vec![0..0]);
        // A width no character fits in still ends.
        assert_eq!(t.wrap(Face::Sans, 16, "abc", 1).len(), 3);
    }

    #[test]
    fn offset_at_finds_the_nearest_gap() {
        let mut t = Text::new();
        let w = t.measure(Face::Mono, 20, "a");
        assert_eq!(t.offset_at(Face::Mono, 20, "abc", -5), 0);
        assert_eq!(t.offset_at(Face::Mono, 20, "abc", w + 1), 1);
        assert_eq!(t.offset_at(Face::Mono, 20, "abc", w * 9), 3);
        assert_eq!(t.offset_at(Face::Mono, 20, "åb", w + 1), 2);
    }

    #[test]
    fn clip_applies_to_everything() {
        let mut f = Frame::new(20, 20);
        f.set_clip(Rect::new(5, 5, 5, 5));
        f.fill_rect(Rect::new(0, 0, 20, 20), Rgb::hex(0xffffff));
        assert_eq!(f.get(4, 5), Some(Rgb(0)));
        assert_eq!(f.get(5, 5), Some(Rgb::hex(0xffffff)));
        assert_eq!(f.get(10, 9), Some(Rgb(0)));
        let img = crate::image::Image {
            w: 2,
            h: 2,
            px: vec![0xff112233, 0x00445566, 0x80ffffff, 0xff778899],
        };
        let mut f = Frame::new(4, 4);
        f.draw_image(-1, 3, &img);
        // Only the top right pixel of the picture is in the frame, and it is transparent.
        assert!(f.px.iter().all(|&p| p == 0));
        f.draw_image(1, 1, &img);
        assert_eq!(f.get(1, 1), Some(Rgb::hex(0x112233)));
        assert_eq!(f.get(2, 1), Some(Rgb(0)));
        assert_eq!(f.get(1, 2), Some(Rgb::hex(0x808080)));
        assert_eq!(f.get(2, 2), Some(Rgb::hex(0x778899)));
    }

    #[test]
    fn fit_cuts_with_an_ellipsis() {
        let mut t = Text::new();
        let s = "a rather long line of text for a narrow box";
        let cut = t.fit(Face::Sans, 16, s, 120);
        assert!(cut.ends_with('…'));
        assert!(t.measure(Face::Sans, 16, &cut) <= 120);
        assert_eq!(t.fit(Face::Sans, 16, "short", 120), "short");
    }

    #[test]
    fn pointer_has_outline_and_fill() {
        let fr = Frame::new(2, 2);
        let off = Rgb::hex(0x070b18);
        let img = pointer_image(&fr, 0, 0, 2, off);
        let (w, _) = pointer_size(2);
        assert_eq!(img[0], 0); // outline (black) at the tip
        assert_eq!(img[(6 * w + 2) as usize], 0xffffff);
        assert_eq!(img[(w - 1) as usize], off.0); // off the frame
    }
}
