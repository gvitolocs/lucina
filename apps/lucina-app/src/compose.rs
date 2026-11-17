//! Composition: a screen made of whole surfaces placed, scaled, dimmed and blended, instead of pixels the CPU writes every frame.
//!
//! A program paints surfaces (a page, a backdrop, an app's window) when they change, and then each frame only says where they go:
//! a list of [`Op`]s, drawn in order, so the list's order is the z-order. A [`Compositor`] shows that list. Two backends exist and a
//! third is planned:
//! - [`CpuCompositor`]: the CPU writes the ops into the [`Frame`], which the caller then presents as usual. Always there.
//! - [`GpuCompositor`]: the surfaces are GPU buffers and the ops become one batch of the kernel's `quad`/`blit`/`fill` on
//!   `/dev/lucina-gpu`. The CPU touches no pixel per frame.
//! - wgpu (`docs/research/juno-gpu-3d.md`, stage 3): the same trait over the Rust wgpu backend once it draws textured quads. Nothing
//!   above this trait changes when it arrives.
//!
//! Surfaces live until they are removed, whether or not an op shows them: an app's window stays valid while it is off screen or its
//! app is frozen (the window manager, `docs/design/juno-vertical-integration.md`). Transforms are scale and translate, which is what
//! the GPU's quad does for free: `from` and `to` of a [`Op::Layer`] may differ in size. Opacity is the tint's alpha with `blend`.
//! A layer may be cut to a rectangle ([`Op::LayerClip`]), which is how the window manager keeps a window inside its rounded or
//! moving frame. Both backends cut the same destination and the same proportion of the source, so they show the same picture
//! where the clip falls on source pixels, and a pixel apart at the cut where it does not; the GPU's commands hold whole pixels.
//!
//! Measured on the laptop, the shell's page slide: `docs/research/juno-perf.md`.

use std::io;

use crate::gfx::{Frame, Rect};
use crate::gpu::{self, Batch, Gpu};

/// No tint: a layer's pixels as they are.
pub const NO_TINT: u32 = 0xffff_ffff;

/// A surface of a compositor, from [`Compositor::add_surface`].
pub type SurfaceId = usize;

/// One step of a composed frame, drawn in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    /// A solid rectangle, 0xRRGGBB.
    Fill { to: Rect, colour: u32 },
    /// `from` of surface `surface` drawn over `to`, scaled when the sizes differ (bilinear with `filter`, else nearest), each
    /// channel multiplied by `tint`'s (0xAARRGGBB, 255 = 1), blended over what is there by alpha (the pixels' times the tint's)
    /// if `blend`.
    Layer {
        surface: SurfaceId,
        from: Rect,
        to: Rect,
        tint: u32,
        blend: bool,
        filter: bool,
    },
    /// [`Op::Layer`] with the picture cut to `clip`: only `to` intersected with `clip` — and with the screen and the surface,
    /// which every op already is — is drawn, and the same proportion of `from` with it, so what is left shows the very pixels
    /// the unclipped layer shows at those screen pixels. A cut that leaves nothing draws nothing. The window manager's ask
    /// (`docs/design/juno-window-manager.md` 1 and 5a): a window clipped to its rounded or moving frame.
    LayerClip {
        surface: SurfaceId,
        from: Rect,
        to: Rect,
        clip: Rect,
        tint: u32,
        blend: bool,
        filter: bool,
    },
}

impl Op {
    /// A surface's `from` at `to` with no tint, blending or scaling: the cheapest op.
    pub fn copy(surface: SurfaceId, from: Rect, to: Rect) -> Op {
        Op::Layer {
            surface,
            from,
            to,
            tint: NO_TINT,
            blend: false,
            filter: false,
        }
    }

    fn target(&self) -> Rect {
        match *self {
            Op::Fill { to, .. } | Op::Layer { to, .. } => to,
            Op::LayerClip { to, clip, .. } => to.intersect(&clip),
        }
    }
}

/// Where a composed frame ended up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shown {
    /// On the screen already: nothing to present. The frame's pixels are not what the screen shows any more.
    Screen,
    /// In the frame: present this rectangle as usual.
    Frame(Rect),
}

/// Shows frames made of surfaces. See the module's text.
pub trait Compositor {
    /// What this backend is called in traces.
    fn name(&self) -> &'static str;
    /// A new surface `w` x `h`, all zero, alive until [`Compositor::remove_surface`].
    fn add_surface(&mut self, w: u32, h: u32) -> io::Result<SurfaceId>;
    /// Ends a surface; ops that still name it draw nothing.
    fn remove_surface(&mut self, id: SurfaceId);
    /// A live surface's width and height.
    fn surface_size(&self, id: SurfaceId) -> Option<(u32, u32)>;
    /// Surface `id`'s pixels, 0xAARRGGBB, rows its width apart, to paint. Empty for a surface that does not exist.
    fn surface_mut(&mut self, id: SurfaceId) -> &mut [u32];
    /// `r` of surface `id` was painted since it was last shown.
    fn changed(&mut self, id: SurfaceId, r: Rect);
    /// Show `ops`. With `vblank`, the first pixels wait for the panel's scan to pass (where the backend can).
    fn show(&mut self, ops: &[Op], vblank: bool, frame: &mut Frame) -> io::Result<Shown>;
    /// The GPU's `clock` / `freq` status lines, for traces; `None` where there is no GPU to ask.
    fn status_line(&mut self) -> Option<String> {
        None
    }
}

/// One channel times a tint channel, both 0..=255, as the GPU's unorm multiply rounds it.
#[inline]
fn mul(c: u32, t: u32) -> u32 {
    (c * t + 127) / 255
}

#[inline]
fn tinted(p: u32, tint: u32) -> u32 {
    if tint == NO_TINT {
        return p;
    }
    let ch = |s: u32| mul((p >> s) & 0xff, (tint >> s) & 0xff) << s;
    ch(24) | ch(16) | ch(8) | ch(0)
}

#[inline]
fn over(src: u32, dst: u32) -> u32 {
    let a = src >> 24;
    let ch = |s: u32| {
        let (c, d) = ((src >> s) & 0xff, (dst >> s) & 0xff);
        ((c * a + d * (255 - a) + 127) / 255) << s
    };
    0xff00_0000 | ch(16) | ch(8) | ch(0)
}

/// A surface for [`compose_cpu`]: its pixels and width.
#[derive(Clone, Copy)]
pub struct View<'a> {
    pub px: &'a [u32],
    pub w: u32,
}

/// Bilinear sample of `s` at (`x`, `y`) in pixel centres, clamped to the surface.
fn sample(s: &View, x: f32, y: f32) -> u32 {
    let (w, h) = (s.w as i32, (s.px.len() / s.w.max(1) as usize) as i32);
    let (fx, fy) = ((x - 0.5).max(0.0), (y - 0.5).max(0.0));
    let (x0, y0) = ((fx as i32).min(w - 1), (fy as i32).min(h - 1));
    let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
    let (ax, ay) = (fx - x0 as f32, fy - y0 as f32);
    let p = |x: i32, y: i32| s.px[(y * w + x) as usize];
    let ch = |sh: u32| {
        let c = |v: u32| ((v >> sh) & 0xff) as f32;
        let top = c(p(x0, y0)) * (1.0 - ax) + c(p(x1, y0)) * ax;
        let bot = c(p(x0, y1)) * (1.0 - ax) + c(p(x1, y1)) * ax;
        ((top * (1.0 - ay) + bot * ay).round() as u32).min(255) << sh
    };
    ch(24) | ch(16) | ch(8) | ch(0)
}

/// What a layer op says to draw, both layer ops in one shape so [`layer`] draws either: `clip` is the rectangle the picture is
/// cut to, and an [`Op::Layer`] is a layer with `clip` = `to`.
struct Draw {
    from: Rect,
    to: Rect,
    clip: Rect,
    tint: u32,
    blend: bool,
    filter: bool,
}

/// `from` of `src` drawn into `out` (`w` pixels a row) at `to`, cut to `d.clip` and to `screen`: what [`Op::Layer`] and
/// [`Op::LayerClip`] have in common. A cut that leaves nothing draws nothing.
fn layer(out: &mut [u32], w: usize, screen: Rect, src: &View, d: &Draw) {
    let (from, to, clip) = (d.from, d.to, d.clip);
    let (tint, blend, filter) = (d.tint, d.blend, d.filter);
    let sw = src.w as i32;
    let sh = (src.px.len() / src.w.max(1) as usize) as i32;
    if from.is_empty() || to.is_empty() || sw == 0 {
        return;
    }
    let put = |o: &mut u32, p: u32| {
        let p = tinted(p, tint);
        *o = if blend { over(p, *o) } else { p };
    };
    if from.w == to.w && from.h == to.h {
        // Same size: clip `to` to the screen and to the clip, `from` to the surface, keeping them in step.
        let (dx, dy) = (to.x - from.x, to.y - from.y);
        let src_r = from.intersect(&Rect::new(0, 0, sw, sh));
        let dst_r = Rect::new(src_r.x + dx, src_r.y + dy, src_r.w, src_r.h)
            .intersect(&screen)
            .intersect(&clip);
        if dst_r.is_empty() {
            return;
        }
        for y in dst_r.y..dst_r.y + dst_r.h {
            let o = &mut out[y as usize * w + dst_r.x as usize..][..dst_r.w as usize];
            let s = &src.px[((y - dy) * sw + dst_r.x - dx) as usize..][..dst_r.w as usize];
            if tint == NO_TINT && !blend {
                o.copy_from_slice(s);
            } else {
                o.iter_mut().zip(s).for_each(|(o, s)| put(o, *s));
            }
        }
    } else {
        // Scaled: each target pixel's centre mapped back into `from`, drawn over the cut part only. The mapping still runs
        // from the uncut `to` and `from`, which is what makes a cut show the pixels the whole draw would have shown there.
        let dst_r = to.intersect(&screen).intersect(&clip);
        let (kx, ky) = (from.w as f32 / to.w as f32, from.h as f32 / to.h as f32);
        for y in dst_r.y..dst_r.y + dst_r.h {
            let sy = from.y as f32 + (y - to.y) as f32 * ky + ky / 2.0;
            for x in dst_r.x..dst_r.x + dst_r.w {
                let sx = from.x as f32 + (x - to.x) as f32 * kx + kx / 2.0;
                let p = if filter {
                    sample(src, sx, sy)
                } else {
                    let (ix, iy) = (sx as i32, sy as i32);
                    if ix < 0 || iy < 0 || ix >= sw || iy >= sh {
                        continue;
                    }
                    src.px[(iy * sw + ix) as usize]
                };
                put(&mut out[y as usize * w + x as usize], p);
            }
        }
    }
}

/// The ops drawn into `out` (`w` pixels a row) from `surfaces`, clipped to `out` and to each surface. The CPU backend, and the
/// reference the GPU's picture is compared with.
pub fn compose_cpu(out: &mut [u32], w: usize, surfaces: &[Option<View>], ops: &[Op]) {
    let h = out.len() / w.max(1);
    let screen = Rect::new(0, 0, w as i32, h as i32);
    for op in ops {
        match *op {
            Op::Fill { to, colour } => {
                let r = to.intersect(&screen);
                for y in r.y..r.y + r.h {
                    let row = y as usize * w;
                    out[row + r.x as usize..row + (r.x + r.w) as usize].fill(colour & 0xff_ffff);
                }
            }
            Op::Layer {
                surface,
                from,
                to,
                tint,
                blend,
                filter,
            } => {
                let Some(Some(src)) = surfaces.get(surface) else {
                    continue;
                };
                layer(
                    out,
                    w,
                    screen,
                    src,
                    &Draw {
                        from,
                        to,
                        clip: to,
                        tint,
                        blend,
                        filter,
                    },
                );
            }
            Op::LayerClip {
                surface,
                from,
                to,
                clip,
                tint,
                blend,
                filter,
            } => {
                let Some(Some(src)) = surfaces.get(surface) else {
                    continue;
                };
                layer(
                    out,
                    w,
                    screen,
                    src,
                    &Draw {
                        from,
                        to,
                        clip,
                        tint,
                        blend,
                        filter,
                    },
                );
            }
        }
    }
}

/// The smallest rectangle that holds every op's target.
fn covered(ops: &[Op]) -> Rect {
    ops.iter()
        .map(Op::target)
        .filter(|r| !r.is_empty())
        .fold(
            Rect::default(),
            |a, r| if a.is_empty() { r } else { a.union(&r) },
        )
}

/// The CPU backend: surfaces in memory, ops drawn into the frame.
#[derive(Default)]
pub struct CpuCompositor {
    surfaces: Vec<Option<(Vec<u32>, u32)>>,
}

impl CpuCompositor {
    /// With `surfaces` surfaces of `w` x `h` made already, ids 0 on.
    pub fn new(w: u32, h: u32, surfaces: usize) -> CpuCompositor {
        let mut c = CpuCompositor::default();
        for _ in 0..surfaces {
            let _ = c.add_surface(w, h);
        }
        c
    }
}

/// The first free slot of `v`, or a new one.
fn slot<T>(v: &mut Vec<Option<T>>, t: T) -> SurfaceId {
    if let Some(i) = v.iter().position(Option::is_none) {
        v[i] = Some(t);
        i
    } else {
        v.push(Some(t));
        v.len() - 1
    }
}

impl Compositor for CpuCompositor {
    fn name(&self) -> &'static str {
        "cpu"
    }
    fn add_surface(&mut self, w: u32, h: u32) -> io::Result<SurfaceId> {
        Ok(slot(
            &mut self.surfaces,
            (vec![0; w as usize * h as usize], w),
        ))
    }
    fn remove_surface(&mut self, id: SurfaceId) {
        if let Some(s) = self.surfaces.get_mut(id) {
            *s = None;
        }
    }
    fn surface_size(&self, id: SurfaceId) -> Option<(u32, u32)> {
        let (px, w) = self.surfaces.get(id)?.as_ref()?;
        Some((*w, (px.len() / (*w).max(1) as usize) as u32))
    }
    fn surface_mut(&mut self, id: SurfaceId) -> &mut [u32] {
        match self.surfaces.get_mut(id) {
            Some(Some((px, _))) => px,
            _ => &mut [],
        }
    }
    fn changed(&mut self, _id: SurfaceId, _r: Rect) {}
    fn show(&mut self, ops: &[Op], _vblank: bool, frame: &mut Frame) -> io::Result<Shown> {
        let views: Vec<Option<View>> = self
            .surfaces
            .iter()
            .map(|s| s.as_ref().map(|(px, w)| View { px, w: *w }))
            .collect();
        let w = frame.w as usize;
        compose_cpu(&mut frame.px[..], w, &views, ops);
        Ok(Shown::Frame(covered(ops).intersect(&frame.bounds())))
    }
}

/// A GPU surface and the rows to hand to the GPU before it next reads them: (first, end).
struct GpuSurface {
    buf: gpu::Buffer,
    dirty: Option<(u32, u32)>,
}

/// One axis of [`cut`]: `at..at + len` of a destination span of `span` onto a source span of `size`, in whole pixels — the
/// start and the length. An edge rounds towards the middle of the source, so the result never reaches outside `0..size` and
/// never crosses the cut: the GPU's commands refuse a rectangle that leaves its surface (`lucina-adreno`'s `holds`) and a
/// pixel outside the clip must stay unwritten. A destination thinner than one source pixel keeps the one it falls in.
#[inline]
fn cut_axis(size: i32, span: i32, at: i32, len: i32) -> (i32, i32) {
    let k = size as f32 / span as f32;
    let whole = |v: f32| v.clamp(0.0, size as f32) as i32;
    let (a, b) = (
        whole((at as f32 * k).ceil()),
        whole(((at + len) as f32 * k).floor()),
    );
    if b > a {
        (a, b - a)
    } else if b == size {
        (size - 1, 1)
    } else {
        (a, 1)
    }
}

/// What [`Op::LayerClip`] costs the GPU: the `from` and `to` of the one `blit` or `quad` that draws it, `to` cut to `clip` and
/// `from` cut to match. Whole pixels only, which is the most the commands hold: where the clip falls between two source
/// pixels the GPU's cut starts at the pixel after the CPU's would, inside a pixel of it. `None` when the cut leaves nothing.
fn cut(from: Rect, to: Rect, clip: Rect) -> Option<(Rect, Rect)> {
    let drawn = to.intersect(&clip);
    if drawn.is_empty() || from.is_empty() {
        return None;
    }
    let (x, w) = cut_axis(from.w, to.w, drawn.x - to.x, drawn.w);
    let (y, h) = cut_axis(from.h, to.h, drawn.y - to.y, drawn.h);
    Some((Rect::new(from.x + x, from.y + y, w, h), drawn))
}

/// The kernel GPU backend: surfaces are GPU buffers, ops one batch of quads, blits and fills.
pub struct GpuCompositor {
    gpu: Gpu,
    surfaces: Vec<Option<GpuSurface>>,
}

impl GpuCompositor {
    /// With `surfaces` surfaces of `w` x `h` made already, ids 0 on. `None` where the kernel offers no GPU or no buffers this size.
    pub fn open(w: u32, h: u32, surfaces: usize) -> Option<GpuCompositor> {
        let mut c = GpuCompositor {
            gpu: Gpu::open()?,
            surfaces: Vec::new(),
        };
        for _ in 0..surfaces {
            c.add_surface(w, h).ok()?;
        }
        Some(c)
    }

    /// The batches for `ops`, split where the kernel's command limit falls.
    fn batches(&mut self, ops: &[Op], vblank: bool) -> Vec<Batch> {
        let mut out = vec![Batch::new()];
        if vblank {
            let r = covered(ops);
            out[0].vblank((r.y + r.h).max(0) as u32);
        }
        for s in self.surfaces.iter_mut().flatten() {
            if let Some((y, end)) = s.dirty.take() {
                if out
                    .last()
                    .is_some_and(|b| b.len() >= gpu::BATCH_COMMANDS - 1)
                {
                    out.push(Batch::new());
                }
                out.last_mut()
                    .expect("one batch at least")
                    .clean(&s.buf, y, end - y);
            }
        }
        for op in ops {
            if out
                .last()
                .is_some_and(|b| b.len() >= gpu::BATCH_COMMANDS - 1)
            {
                out.push(Batch::new());
            }
            let b = out.last_mut().expect("one batch at least");
            match *op {
                Op::Fill { to, colour } if !to.is_empty() => {
                    b.fill(to, colour);
                }
                Op::Layer {
                    surface,
                    from,
                    to,
                    tint,
                    blend,
                    filter,
                } if !to.is_empty() && !from.is_empty() => {
                    let Some(Some(s)) = self.surfaces.get(surface) else {
                        continue;
                    };
                    if tint == NO_TINT && !blend {
                        b.blit(&s.buf, from, to, filter);
                    } else {
                        b.quad(&s.buf, from, to, tint, blend, filter);
                    }
                }
                Op::LayerClip {
                    surface,
                    from,
                    to,
                    clip,
                    tint,
                    blend,
                    filter,
                } => {
                    let Some(Some(s)) = self.surfaces.get(surface) else {
                        continue;
                    };
                    let Some((from, to)) = cut(from, to, clip) else {
                        continue;
                    };
                    if tint == NO_TINT && !blend {
                        b.blit(&s.buf, from, to, filter);
                    } else {
                        b.quad(&s.buf, from, to, tint, blend, filter);
                    }
                }
                _ => {}
            }
        }
        out
    }
}

impl Compositor for GpuCompositor {
    fn name(&self) -> &'static str {
        "gpu"
    }
    fn status_line(&mut self) -> Option<String> {
        let s = self.gpu.status().ok()?;
        let l: Vec<&str> = s
            .lines()
            .filter(|l| l.contains("clock") || l.contains("freq"))
            .collect();
        Some(l.join("; "))
    }
    fn add_surface(&mut self, w: u32, h: u32) -> io::Result<SurfaceId> {
        let buf = self.gpu.buffer(w, h)?;
        if buf.pitch != w * 4 {
            let _ = self.gpu.free(buf);
            return Err(io::Error::other(
                "the GPU's rows are not the surface's width",
            ));
        }
        Ok(slot(
            &mut self.surfaces,
            GpuSurface {
                buf,
                dirty: Some((0, h)),
            },
        ))
    }
    fn remove_surface(&mut self, id: SurfaceId) {
        if let Some(s) = self.surfaces.get_mut(id).and_then(Option::take) {
            let _ = self.gpu.free(s.buf);
        }
    }
    fn surface_size(&self, id: SurfaceId) -> Option<(u32, u32)> {
        let s = self.surfaces.get(id)?.as_ref()?;
        Some((s.buf.width, s.buf.height))
    }
    fn surface_mut(&mut self, id: SurfaceId) -> &mut [u32] {
        match self.surfaces.get_mut(id) {
            Some(Some(s)) => s.buf.pixels(),
            _ => &mut [],
        }
    }
    fn changed(&mut self, id: SurfaceId, r: Rect) {
        let Some(Some(s)) = self.surfaces.get_mut(id) else {
            return;
        };
        let r = r.intersect(&s.buf.bounds());
        if r.is_empty() {
            return;
        }
        let (y, end) = (r.y as u32, (r.y + r.h) as u32);
        s.dirty = Some(match s.dirty {
            Some((a, b)) => (a.min(y), b.max(end)),
            None => (y, end),
        });
    }
    fn show(&mut self, ops: &[Op], vblank: bool, _frame: &mut Frame) -> io::Result<Shown> {
        for b in self.batches(ops, vblank) {
            if !b.is_empty() {
                self.gpu.run(&b)?;
            }
        }
        Ok(Shown::Screen)
    }
}

/// The best backend here: the GPU's when the kernel offers it (and `LUCINA_GPU_COMPOSE` is not `0`), else the CPU's. Either has
/// `surfaces` surfaces of `w` x `h` made already, ids 0 on.
pub fn best(w: u32, h: u32, surfaces: usize) -> Box<dyn Compositor> {
    let wanted = std::env::var_os("LUCINA_GPU_COMPOSE").is_none_or(|v| v != "0");
    if wanted && let Some(g) = GpuCompositor::open(w, h, surfaces) {
        return Box::new(g);
    }
    Box::new(CpuCompositor::new(w, h, surfaces))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The screen the clipped tests draw into: 8 x 8.
    const W: usize = 8;
    /// The surface every clipped test draws from: 6 x 6, and every pixel says which pixel it is.
    const SW: u32 = 6;
    const SENTINEL: u32 = 0x1122_3344;

    fn v(px: &[u32], w: u32) -> Option<View<'_>> {
        Some(View { px, w })
    }

    fn source() -> Vec<u32> {
        (0..36).map(|i| 0xff00_0000 | i).collect()
    }

    /// A screen of nothing but the sentinel.
    fn screen() -> Vec<u32> {
        vec![SENTINEL; W * W]
    }

    fn at(out: &[u32], x: i32, y: i32) -> u32 {
        out[(y as usize) * W + x as usize]
    }

    fn set(out: &mut [u32], x: i32, y: i32, p: u32) {
        out[(y as usize) * W + x as usize] = p;
    }

    /// A layer op of the whole 6 x 6 surface at `to`.
    fn layer_of(from: Rect, to: Rect, clip: Rect, tint: u32, blend: bool, filter: bool) -> Op {
        Op::LayerClip {
            surface: 0,
            from,
            to,
            clip,
            tint,
            blend,
            filter,
        }
    }

    /// The plain layer drawn, and every pixel the clip leaves out put back to the sentinel: what a clip is asked to be.
    fn unclipped_masked(
        src: &[u32],
        from: Rect,
        to: Rect,
        clip: Rect,
        tint: u32,
        blend: bool,
        filter: bool,
    ) -> Vec<u32> {
        let mut out = screen();
        compose_cpu(
            &mut out,
            W,
            &[v(src, SW)],
            &[Op::Layer {
                surface: 0,
                from,
                to,
                tint,
                blend,
                filter,
            }],
        );
        for y in 0..W as i32 {
            for x in 0..W as i32 {
                if !clip.contains(x, y) {
                    set(&mut out, x, y, SENTINEL);
                }
            }
        }
        out
    }

    /// The clipped op drawn whole.
    fn drawn(src: &[u32], op: Op) -> Vec<u32> {
        let mut out = screen();
        compose_cpu(&mut out, W, &[v(src, SW)], &[op]);
        out
    }

    #[test]
    fn layers_fills_and_tints_land_where_the_ops_say() {
        let w = 4;
        let a: Vec<u32> = (0..16).map(|i| 0xff00_0000 | i).collect();
        let b = vec![0x80ff_ffffu32; 16];
        let mut out = vec![0u32; 16];
        let ops = [
            Op::copy(0, Rect::new(1, 0, 2, 4), Rect::new(0, 0, 2, 4)),
            Op::Fill {
                to: Rect::new(2, 0, 1, 4),
                colour: 0x12_3456,
            },
            Op::Layer {
                surface: 0,
                from: Rect::new(0, 0, 1, 4),
                to: Rect::new(3, 0, 1, 4),
                tint: 0xff80_8080,
                blend: false,
                filter: false,
            },
        ];
        compose_cpu(&mut out, w, &[v(&a, 4), v(&b, 4)], &ops);
        assert_eq!(out[0], 0xff00_0001);
        assert_eq!(out[1], 0xff00_0002);
        assert_eq!(out[2], 0x12_3456);
        // Blue 4 (row 1, column 0) times 128/255 rounds to 2.
        assert_eq!(out[7] & 0xff, 2);
        // Half-transparent white over black is mid grey.
        let mut out = vec![0xff00_0000u32; 16];
        compose_cpu(
            &mut out,
            w,
            &[v(&a, 4), v(&b, 4)],
            &[Op::Layer {
                surface: 1,
                from: Rect::new(0, 0, 4, 4),
                to: Rect::new(0, 0, 4, 4),
                tint: NO_TINT,
                blend: true,
                filter: false,
            }],
        );
        assert_eq!(out[5], 0xff80_8080);
    }

    #[test]
    fn ops_outside_the_screen_or_the_surface_are_clipped() {
        let w = 4;
        let a = vec![0xff11_1111u32; 16];
        let mut out = vec![0u32; 16];
        compose_cpu(
            &mut out,
            w,
            &[v(&a, 4), None],
            &[
                Op::copy(0, Rect::new(-2, -2, 8, 8), Rect::new(1, 1, 8, 8)),
                Op::Fill {
                    to: Rect::new(-5, -3, 50, 4),
                    colour: 0x22,
                },
                Op::copy(1, Rect::new(0, 0, 4, 4), Rect::new(0, 0, 4, 4)),
                Op::copy(9, Rect::new(0, 0, 4, 4), Rect::new(0, 0, 4, 4)),
            ],
        );
        // Only surface pixel (0, 0) is on both: it lands at (3, 3). The fill keeps to row 0; surfaces 1 (removed) and 9 do not exist.
        assert_eq!(out[15], 0xff11_1111);
        assert!(out[4..15].iter().all(|p| *p == 0));
        assert!(out[..4].iter().all(|p| *p == 0x22));
    }

    #[test]
    fn a_layer_scales_to_its_target_and_a_half_opacity_tint_blends() {
        // A 2x2 surface drawn into 4x4: nearest gives 2x2 blocks.
        let s = [0xff00_0001u32, 0xff00_0002, 0xff00_0003, 0xff00_0004];
        let mut out = vec![0u32; 16];
        compose_cpu(
            &mut out,
            4,
            &[v(&s, 2)],
            &[Op::copy(0, Rect::new(0, 0, 2, 2), Rect::new(0, 0, 4, 4))],
        );
        assert_eq!(
            &out[..4],
            &[0xff00_0001, 0xff00_0001, 0xff00_0002, 0xff00_0002]
        );
        assert_eq!(out[12], 0xff00_0003);
        // A 4x4 white surface shrunk to 2x2 at half opacity over black, bilinear: mid grey, the rest untouched.
        let white = vec![0xffff_ffffu32; 16];
        let mut out = vec![0xff00_0000u32; 16];
        compose_cpu(
            &mut out,
            4,
            &[v(&white, 4)],
            &[Op::Layer {
                surface: 0,
                from: Rect::new(0, 0, 4, 4),
                to: Rect::new(1, 1, 2, 2),
                tint: 0x80ff_ffff,
                blend: true,
                filter: true,
            }],
        );
        assert_eq!(out[5], 0xff80_8080);
        assert_eq!(out[0], 0xff00_0000);
        assert_eq!(out[15], 0xff00_0000);
    }

    #[test]
    fn surfaces_come_and_go_and_reuse_free_ids() {
        let mut c = CpuCompositor::new(4, 4, 2);
        assert_eq!(c.surface_size(1), Some((4, 4)));
        let small = c.add_surface(2, 3).unwrap();
        assert_eq!((small, c.surface_size(small)), (2, Some((2, 3))));
        c.remove_surface(0);
        assert_eq!(c.surface_size(0), None);
        assert!(c.surface_mut(0).is_empty());
        assert_eq!(c.add_surface(1, 1).unwrap(), 0);
        assert_eq!(c.surface_mut(small).len(), 6);
    }

    /// A clip that holds `to` is no clip at all: every pixel of the plain layer, and nothing else.
    #[test]
    fn a_clip_that_holds_the_layer_draws_exactly_what_the_layer_draws() {
        let s = source();
        let holds = [
            Rect::new(-40, -40, 100, 100),
            Rect::new(0, 0, W as i32, W as i32),
        ];
        let layers = [
            (Rect::new(0, 0, 6, 6), Rect::new(-1, 1, 6, 6)),
            (Rect::new(1, 1, 4, 4), Rect::new(0, 2, 4, 4)),
            (Rect::new(0, 0, 6, 6), Rect::new(0, 0, 3, 3)),
            (Rect::new(0, 0, 3, 3), Rect::new(0, 0, 7, 7)),
        ];
        for (from, to) in layers {
            for clip in holds {
                let plain = drawn(
                    &s,
                    Op::Layer {
                        surface: 0,
                        from,
                        to,
                        tint: NO_TINT,
                        blend: false,
                        filter: false,
                    },
                );
                let clipped = drawn(&s, layer_of(from, to, clip, NO_TINT, false, false));
                assert_eq!(plain, clipped, "{from:?} at {to:?} cut to {clip:?}");
            }
        }
    }

    /// The left half cut away is the right half drawn: the source pixels it shows are the ones at those screen pixels.
    #[test]
    fn a_clip_cutting_the_left_half_leaves_the_right_half() {
        let s = source();
        let out = drawn(
            &s,
            layer_of(
                Rect::new(0, 0, 6, 6),
                Rect::new(0, 0, 6, 6),
                Rect::new(3, 0, 3, 6),
                NO_TINT,
                false,
                false,
            ),
        );
        for y in 0..6i32 {
            for x in 0..3i32 {
                assert_eq!(at(&out, x, y), SENTINEL, "({x}, {y}) is cut away");
            }
            for x in 3..6i32 {
                assert_eq!(
                    at(&out, x, y),
                    0xff00_0000 | (y * 6 + x) as u32,
                    "({x}, {y})"
                );
            }
            for x in 6..8i32 {
                assert_eq!(at(&out, x, y), SENTINEL, "({x}, {y}) was never the layer's");
            }
        }
        assert_eq!(at(&out, 3, 7), SENTINEL, "row 7 is below the layer");
    }

    /// A clip that cuts both the top and the bottom keeps the rows between them.
    #[test]
    fn a_clip_cutting_the_top_and_the_bottom_keeps_the_rows_between() {
        let s = source();
        let out = drawn(
            &s,
            layer_of(
                Rect::new(0, 0, 6, 6),
                Rect::new(0, 0, 6, 6),
                Rect::new(0, 1, 6, 4),
                NO_TINT,
                false,
                false,
            ),
        );
        for y in 0..8i32 {
            let kept = (1..5).contains(&y);
            for x in 0..8i32 {
                let want = if kept && x < 6 {
                    0xff00_0000 | (y * 6 + x) as u32
                } else {
                    SENTINEL
                };
                assert_eq!(at(&out, x, y), want, "({x}, {y})");
            }
        }
    }

    /// A scaled layer cut shows the source pixels the unclipped scaled draw shows at those screen pixels — both a shrink and
    /// a zoom, each compared with the unclipped draw with the cut pixels put back.
    #[test]
    fn a_cut_scaled_layer_shows_the_source_pixels_the_unclipped_draw_shows_there() {
        let s = source();
        for (from, to, clip) in [
            // 6 x 6 shrunk to 3 x 3, the left column of the picture cut away.
            (
                Rect::new(0, 0, 6, 6),
                Rect::new(0, 0, 3, 3),
                Rect::new(1, 0, 2, 3),
            ),
            // 3 x 3 zoomed to 6 x 6, cut to its middle.
            (
                Rect::new(0, 0, 3, 3),
                Rect::new(0, 0, 6, 6),
                Rect::new(2, 2, 4, 4),
            ),
            // Same zoom, a clip that falls between source pixels on both axes.
            (
                Rect::new(0, 0, 5, 5),
                Rect::new(1, 1, 6, 6),
                Rect::new(2, 3, 4, 3),
            ),
        ] {
            let out = drawn(&s, layer_of(from, to, clip, NO_TINT, false, false));
            assert_eq!(
                out,
                unclipped_masked(&s, from, to, clip, NO_TINT, false, false),
                "{from:?} at {to:?} cut to {clip:?}"
            );
        }
        // The shrink's first kept column is the surface's fourth and its first kept row the second (a draw samples pixel
        // centres, and a 2x shrink starts a whole source pixel in), which is what the uncut draw shows at those pixels.
        let out = drawn(
            &s,
            layer_of(
                Rect::new(0, 0, 6, 6),
                Rect::new(0, 0, 3, 3),
                Rect::new(1, 0, 2, 3),
                NO_TINT,
                false,
                false,
            ),
        );
        assert_eq!(at(&out, 1, 0), 0xff00_0009);
        assert_eq!(at(&out, 2, 0), 0xff00_000b);
        assert_eq!(at(&out, 1, 2), 0xff00_0021);
        assert_eq!(at(&out, 0, 0), SENTINEL);
    }

    /// A clip that leaves nothing — no width, no height, or none of `to` in it — draws nothing at all.
    #[test]
    fn an_empty_clip_draws_nothing() {
        let s = source();
        for clip in [
            Rect::new(1, 1, 0, 4),
            Rect::new(1, 1, 4, 0),
            Rect::new(1, 1, -3, 9),
            Rect::new(9, 9, 4, 4),
        ] {
            for (from, to) in [
                (Rect::new(0, 0, 6, 6), Rect::new(0, 0, 6, 6)),
                (Rect::new(0, 0, 6, 6), Rect::new(0, 0, 3, 3)),
            ] {
                for (tint, blend, filter) in [
                    (NO_TINT, false, false),
                    (0x80ff_8080, true, false),
                    (NO_TINT, false, true),
                ] {
                    let out = drawn(&s, layer_of(from, to, clip, tint, blend, filter));
                    assert_eq!(out, screen(), "{to:?} cut to {clip:?} drew something");
                    assert_eq!(cut(from, to, clip), None, "{clip:?} leaves nothing");
                }
            }
        }
    }

    /// The clip cuts together with the screen: what falls outside either is not drawn, and a clip the screen does not hold
    /// draws nothing.
    #[test]
    fn a_clip_outside_the_screen_draws_nothing_and_an_edge_of_the_screen_is_cut_too() {
        let s = source();
        // A clip the 8 x 8 screen holds none of: nothing is drawn.
        let out = drawn(
            &s,
            layer_of(
                Rect::new(0, 0, 6, 6),
                Rect::new(-6, -6, 6, 6),
                Rect::new(6, 6, 5, 5),
                NO_TINT,
                false,
                false,
            ),
        );
        assert_eq!(out, screen());
        // The layer hangs off the top left, the clip reaches off the bottom right: only their overlap on screen is drawn.
        let out = drawn(
            &s,
            layer_of(
                Rect::new(0, 0, 6, 6),
                Rect::new(-2, -2, 6, 6),
                Rect::new(2, 2, 8, 8),
                NO_TINT,
                false,
                false,
            ),
        );
        assert_eq!(at(&out, 2, 2), 0xff00_0000 | (4 * 6 + 4));
        assert_eq!(at(&out, 3, 3), 0xff00_0000 | (5 * 6 + 5));
        assert_eq!(at(&out, 1, 2), SENTINEL, "left of the clip");
        assert_eq!(at(&out, 4, 4), SENTINEL, "below the layer");
        assert_eq!(at(&out, 0, 0), SENTINEL, "off the layer altogether");
    }

    /// A tint and a blend ride along with the clip: the kept pixels are the blended ones, the cut ones untouched.
    #[test]
    fn a_clip_keeps_the_tint_and_the_blend() {
        let s = source();
        for (tint, blend) in [
            (0x80ff_8080, true),
            (0xff80_4020, false),
            (0x01ff_ffff, true),
        ] {
            for (from, to, clip) in [
                (
                    Rect::new(0, 0, 6, 6),
                    Rect::new(0, 0, 6, 6),
                    Rect::new(0, 0, 3, 6),
                ),
                (
                    Rect::new(0, 0, 6, 6),
                    Rect::new(1, 1, 4, 4),
                    Rect::new(2, 1, 3, 3),
                ),
            ] {
                let out = drawn(&s, layer_of(from, to, clip, tint, blend, false));
                assert_eq!(
                    out,
                    unclipped_masked(&s, from, to, clip, tint, blend, false),
                    "{tint:08x} blend {blend} at {to:?} cut to {clip:?}"
                );
                let kept: Vec<u32> = (0..8)
                    .flat_map(|y| (0..8).map(move |x| (x, y)))
                    .filter(|(x, y)| clip.contains(*x, *y) && to.contains(*x, *y))
                    .map(|(x, y)| at(&out, x, y))
                    .collect();
                assert!(
                    kept.iter().all(|p| *p != SENTINEL),
                    "a kept pixel of tint {tint:08x} still holds the sentinel"
                );
            }
        }
    }

    /// A filtered (bilinear) layer cut to a clip shows the filtered pixels of the whole draw, not the pixels next to the cut.
    #[test]
    fn a_clip_of_a_filtered_layer_shows_the_filtered_pixels_and_not_their_neighbours() {
        let s = source();
        for (from, to, clip) in [
            (
                Rect::new(0, 0, 4, 4),
                Rect::new(0, 0, 8, 8),
                Rect::new(0, 4, 8, 4),
            ),
            (
                Rect::new(1, 1, 5, 5),
                Rect::new(0, 0, 4, 4),
                Rect::new(1, 0, 3, 4),
            ),
            (
                Rect::new(0, 0, 6, 6),
                Rect::new(2, 2, 6, 6),
                Rect::new(4, 4, 4, 4),
            ),
        ] {
            let out = drawn(&s, layer_of(from, to, clip, NO_TINT, false, true));
            assert_eq!(
                out,
                unclipped_masked(&s, from, to, clip, NO_TINT, false, true),
                "filtered {from:?} at {to:?} cut to {clip:?}"
            );
        }
    }

    /// Not one pixel outside the cut changes, with a sentinel round every side of the screen.
    #[test]
    fn no_pixel_outside_the_clip_changes() {
        let s = source();
        // A background that says which pixel it is, none of them a source pixel.
        let background = |x: i32, y: i32| 0xff00_0000 | 0x8000 | (y * 8 + x) as u32;
        let cases = [
            (
                Rect::new(0, 0, 6, 6),
                Rect::new(1, 1, 5, 5),
                Rect::new(2, 2, 3, 3),
            ),
            (
                Rect::new(0, 0, 6, 6),
                Rect::new(-1, 4, 6, 6),
                Rect::new(1, 3, 6, 6),
            ),
            (
                Rect::new(2, 0, 4, 6),
                Rect::new(0, 0, 8, 8),
                Rect::new(3, 5, 2, 4),
            ),
        ];
        for (from, to, clip) in cases {
            let mut out = (0..8)
                .flat_map(|y| (0..8).map(move |x| background(x, y)))
                .collect::<Vec<u32>>();
            compose_cpu(
                &mut out,
                W,
                &[v(&s, SW)],
                &[layer_of(from, to, clip, NO_TINT, false, false)],
            );
            let area = to.intersect(&clip).intersect(&Rect::new(0, 0, 8, 8));
            for y in 0..8 {
                for x in 0..8 {
                    if area.contains(x, y) {
                        assert_ne!(
                            at(&out, x, y),
                            background(x, y),
                            "({x}, {y}) is inside the clip and was not drawn on"
                        );
                    } else {
                        assert_eq!(at(&out, x, y), background(x, y), "({x}, {y}) changed");
                    }
                }
            }
        }
    }

    /// The screen, the surface and the clip cut the same layer at once, each keeping the other two.
    #[test]
    fn a_clip_the_surface_and_the_screen_cut_the_same_layer() {
        let s = source();
        // `from` reaches two rows past the 6 x 6 surface, `to` two columns past the screen's left edge, the clip one row above.
        let out = drawn(
            &s,
            layer_of(
                Rect::new(0, 4, 6, 4),
                Rect::new(-2, 1, 6, 4),
                Rect::new(1, 1, 6, 4),
                NO_TINT,
                false,
                false,
            ),
        );
        // What is left is screen rows 1 and 2, columns 1 to 3: the surface's rows 4 and 5, columns 3 to 5.
        for y in 0..8i32 {
            for x in 0..8i32 {
                let want = if (1..4).contains(&x) && (1..3).contains(&y) {
                    0xff00_0000 | ((y + 3) * 6 + (x + 2)) as u32
                } else {
                    SENTINEL
                };
                assert_eq!(at(&out, x, y), want, "({x}, {y})");
            }
        }
    }

    /// The damage a clipped layer marks is the cut, not the whole layer.
    #[test]
    fn a_clipped_layer_marks_only_the_cut_as_dirty() {
        let (to, clip) = (Rect::new(2, 1, 6, 4), Rect::new(4, 0, 8, 3));
        let clipped = layer_of(Rect::new(0, 0, 6, 4), to, clip, NO_TINT, false, false);
        assert_eq!(covered(&[clipped]), to.intersect(&clip));
        assert_eq!(
            covered(&[
                clipped,
                Op::Fill {
                    to: Rect::new(0, 0, 1, 1),
                    colour: 0
                }
            ]),
            Rect::new(0, 0, 1, 1).union(&to.intersect(&clip))
        );
        // A clip that leaves nothing marks nothing, the way an empty layer does.
        assert!(
            covered(&[layer_of(
                Rect::new(0, 0, 6, 4),
                to,
                Rect::new(9, 9, 2, 2),
                NO_TINT,
                false,
                false,
            )])
            .is_empty()
        );
    }

    /// The GPU gets whole-pixel rectangles out of [`cut`] and draws them as one `blit` or `quad`; replayed on the CPU that is a
    /// plain layer of those rectangles. Where the clip falls on whole source pixels — every 1:1 layer, whatever the clip, and
    /// a scale that lands on pixels — the two backends then agree pixel for pixel.
    #[test]
    fn the_gpu_cut_of_a_clipped_layer_draws_what_the_cpu_clip_draws() {
        let s = source();
        let cases = [
            // 1:1: the window manager's case, a window moved behind a fixed clip.
            (
                Rect::new(0, 0, 6, 6),
                Rect::new(1, 1, 6, 6),
                Rect::new(3, 2, 4, 5),
            ),
            (
                Rect::new(1, 1, 4, 4),
                Rect::new(0, 0, 4, 4),
                Rect::new(0, 0, 2, 3),
            ),
            // 6 x 6 shrunk to 3 x 3, and 3 x 3 zoomed to 6 x 6, cut on source pixels.
            (
                Rect::new(0, 0, 6, 6),
                Rect::new(0, 0, 3, 3),
                Rect::new(1, 0, 2, 3),
            ),
            (
                Rect::new(0, 0, 3, 3),
                Rect::new(0, 0, 6, 6),
                Rect::new(2, 2, 4, 4),
            ),
            // Scaled on one axis only, from a surface that starts mid-buffer.
            (
                Rect::new(2, 0, 4, 6),
                Rect::new(0, 0, 2, 6),
                Rect::new(1, 1, 2, 4),
            ),
            // Hanging off the screen on two sides, cut by a clip that reaches past both.
            (
                Rect::new(0, 0, 6, 6),
                Rect::new(-2, 4, 6, 6),
                Rect::new(1, 3, 8, 8),
            ),
        ];
        for (from, to, clip) in cases {
            for (tint, blend, filter) in [
                (NO_TINT, false, false),
                (0x80c0_c0c0, true, false),
                (0xff80_4020, false, true),
            ] {
                let Some((cf, ct)) = cut(from, to, clip) else {
                    panic!("{clip:?} leaves nothing of {to:?}");
                };
                assert_eq!(ct, to.intersect(&clip), "the GPU draws the cut area");
                assert!(
                    cf.x >= from.x
                        && cf.y >= from.y
                        && cf.right() <= from.right()
                        && cf.bottom() <= from.bottom()
                        && !cf.is_empty(),
                    "{cf:?} is not a whole part of {from:?}"
                );
                let cpu = drawn(&s, layer_of(from, to, clip, tint, blend, filter));
                let gpu = drawn(
                    &s,
                    Op::Layer {
                        surface: 0,
                        from: cf,
                        to: ct,
                        tint,
                        blend,
                        filter,
                    },
                );
                assert_eq!(
                    cpu, gpu,
                    "{from:?} at {to:?} cut to {clip:?} as {cf:?} at {ct:?} ({tint:08x} {blend} {filter})"
                );
            }
        }
    }

    /// A clip that falls between source pixels cannot be whole pixels. The GPU's cut then stays inside the op's `from`, keeps
    /// at least one pixel, and still draws exactly the area the clip leaves — never a pixel outside the clip.
    #[test]
    fn a_cut_between_source_pixels_stays_inside_the_layer_and_holds_the_clip() {
        for (from, to, clip) in [
            (
                Rect::new(0, 0, 3, 3),
                Rect::new(0, 0, 7, 7),
                Rect::new(2, 0, 3, 7),
            ),
            (
                Rect::new(0, 0, 7, 7),
                Rect::new(0, 0, 3, 3),
                Rect::new(1, 1, 1, 2),
            ),
            (
                Rect::new(0, 0, 5, 7),
                Rect::new(0, 0, 3, 4),
                Rect::new(0, 2, 2, 2),
            ),
            (
                Rect::new(1, 2, 6, 5),
                Rect::new(-3, -1, 9, 7),
                Rect::new(-2, 1, 5, 4),
            ),
        ] {
            let Some((cf, ct)) = cut(from, to, clip) else {
                panic!("{clip:?} leaves nothing of {to:?}");
            };
            assert_eq!(ct, to.intersect(&clip));
            assert!(
                cf.x >= from.x
                    && cf.y >= from.y
                    && cf.right() <= from.right()
                    && cf.bottom() <= from.bottom()
                    && cf.w >= 1
                    && cf.h >= 1,
                "{from:?} at {to:?} cut to {clip:?} gave {cf:?}"
            );
            // The cut is never more than a pixel away from the proportional one.
            for (got, size, span, at) in [
                (cf.x - from.x, from.w, to.w, ct.x - to.x),
                (cf.y - from.y, from.h, to.h, ct.y - to.y),
            ] {
                let want = at as f32 * (size as f32 / span as f32);
                assert!(
                    (got as f32 - want).abs() < 1.0,
                    "{got} against the proportional {want} of {from:?} at {to:?} cut to {clip:?}"
                );
            }
        }
    }
}
