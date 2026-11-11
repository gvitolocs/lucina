//! From compressed frames to pixels: the decoder, what changed since the last frame, and the fit to the panel.
//!
//! One CPU, so nothing here does work for pixels that stayed the same. A desktop mostly stays the same.

use std::mem::MaybeUninit;
use std::ptr::NonNull;

use rav1d::include::dav1d::data::Dav1dData;
use rav1d::include::dav1d::dav1d::{Dav1dContext, Dav1dSettings};
use rav1d::include::dav1d::headers::DAV1D_PIXEL_LAYOUT_I420;
use rav1d::include::dav1d::picture::Dav1dPicture;
use rav1d::src::lib::{
    dav1d_close, dav1d_data_create, dav1d_default_settings, dav1d_get_picture, dav1d_open, dav1d_picture_unref, dav1d_send_data,
};

/// One decoded picture in 4:2:0, borrowed from the decoder until the next call.
pub struct Yuv<'a> {
    pub w: usize,
    pub h: usize,
    pub y: &'a [u8],
    pub u: &'a [u8],
    pub v: &'a [u8],
    pub y_stride: usize,
    pub uv_stride: usize,
}

/// rav1d, the Rust port of dav1d, with its NEON routines.
pub struct Av1 {
    ctx: Option<Dav1dContext>,
    pic: Dav1dPicture,
    has_pic: bool,
}

// SAFETY: the context and picture are used from one thread at a time; they hold no thread-bound state with one decoder thread.
unsafe impl Send for Av1 {}

impl Av1 {
    pub fn new() -> Result<Av1, String> {
        // SAFETY: as the dav1d API asks: settings initialised by the library, a context out parameter.
        unsafe {
            let mut s = MaybeUninit::<Dav1dSettings>::uninit();
            dav1d_default_settings(NonNull::new(s.as_mut_ptr()).ok_or("settings")?);
            let mut s = s.assume_init();
            // One CPU: threads would only add switches. No delay: each frame shows as it comes.
            s.n_threads = 1;
            s.max_frame_delay = 1;
            s.apply_grain = 0;
            let mut ctx = None;
            let r = dav1d_open(NonNull::new(&mut ctx), NonNull::new(&mut s));
            if r.0 != 0 {
                return Err(format!("the AV1 decoder did not start: {}", r.0));
            }
            Ok(Av1 { ctx, pic: std::mem::zeroed(), has_pic: false })
        }
    }

    /// Decodes one frame as the host sent it. `Ok(None)`: the decoder wants more before it shows anything.
    pub fn decode(&mut self, data: &[u8]) -> Result<Option<Yuv<'_>>, String> {
        // SAFETY: the buffer dav1d_data_create returns holds `data.len()` bytes; the picture stays valid until the unref of the
        // next call, which the borrow of `self` enforces.
        unsafe {
            if self.has_pic {
                dav1d_picture_unref(NonNull::new(&mut self.pic));
                self.has_pic = false;
            }
            let mut d: Dav1dData = std::mem::zeroed();
            let p = dav1d_data_create(NonNull::new(&mut d), data.len());
            if p.is_null() {
                return Err("no memory for a frame".into());
            }
            std::ptr::copy_nonoverlapping(data.as_ptr(), p, data.len());
            let r = dav1d_send_data(self.ctx, NonNull::new(&mut d));
            if r.0 != 0 && r.0 != -libc::EAGAIN {
                return Err(format!("the frame was refused: {}", r.0));
            }
            let r = dav1d_get_picture(self.ctx, NonNull::new(&mut self.pic));
            if r.0 == -libc::EAGAIN {
                return Ok(None);
            }
            if r.0 != 0 {
                return Err(format!("no picture from the frame: {}", r.0));
            }
            self.has_pic = true;
            let p = &self.pic.p;
            if p.bpc != 8 || p.layout != DAV1D_PIXEL_LAYOUT_I420 {
                return Err(format!("a picture of {} bits, layout {}", p.bpc, p.layout));
            }
            let (w, h) = (p.w as usize, p.h as usize);
            let (ys, uvs) = (self.pic.stride[0] as usize, self.pic.stride[1] as usize);
            let plane = |i: usize, n: usize| -> Result<&[u8], String> {
                let d = self.pic.data[i].ok_or("a picture without a plane")?;
                Ok(std::slice::from_raw_parts(d.as_ptr().cast::<u8>(), n))
            };
            let chroma_rows = h.div_ceil(2);
            Ok(Some(Yuv {
                w,
                h,
                y: plane(0, ys * h)?,
                u: plane(1, uvs * chroma_rows)?,
                v: plane(2, uvs * chroma_rows)?,
                y_stride: ys,
                uv_stride: uvs,
            }))
        }
    }
}

impl Drop for Av1 {
    fn drop(&mut self) {
        // SAFETY: both were made by dav1d and are not used after this.
        unsafe {
            if self.has_pic {
                dav1d_picture_unref(NonNull::new(&mut self.pic));
            }
            dav1d_close(NonNull::new(&mut self.ctx));
        }
    }
}

/// BT.601 limited range in one pixel, the matrix RustDesk's libyuv conversion on the host uses.
#[inline]
fn yuv_pixel(y: u8, u: u8, v: u8) -> u32 {
    let yy = (y as i32 - 16) * 298;
    let (uu, vv) = (u as i32 - 128, v as i32 - 128);
    let r = ((yy + 409 * vv + 128) >> 8).clamp(0, 255) as u32;
    let g = ((yy - 100 * uu - 208 * vv + 128) >> 8).clamp(0, 255) as u32;
    let b = ((yy + 516 * uu + 128) >> 8).clamp(0, 255) as u32;
    0xff00_0000 | r << 16 | g << 8 | b
}

/// One row of luma and its chroma, half as wide, into XRGB.
pub fn row_to_xrgb(y: &[u8], u: &[u8], v: &[u8], out: &mut [u32]) {
    let w = y.len().min(out.len());
    let mut x = 0;
    #[cfg(target_arch = "aarch64")]
    if w >= 16 && u.len() >= w / 2 && v.len() >= w / 2 {
        // SAFETY: NEON is part of the AArch64 baseline; the loop reads and writes within the bounds checked here.
        unsafe { x = row_neon(y, u, v, out, w) };
    }
    while x < w {
        out[x] = yuv_pixel(y[x], u[x / 2], v[x / 2]);
        x += 1;
    }
}

/// 16 pixels at a time in 16-bit fixed point with 6 fraction bits: the widest sum still fits, with saturation at the top.
#[cfg(target_arch = "aarch64")]
unsafe fn row_neon(y: &[u8], u: &[u8], v: &[u8], out: &mut [u32], w: usize) -> usize {
    use std::arch::aarch64::*;
    let mut x = 0;
    unsafe {
        while x + 16 <= w {
            let yv = vld1q_u8(y.as_ptr().add(x));
            let us = vsubq_s16(vreinterpretq_s16_u16(vmovl_u8(vld1_u8(u.as_ptr().add(x / 2)))), vdupq_n_s16(128));
            let vs = vsubq_s16(vreinterpretq_s16_u16(vmovl_u8(vld1_u8(v.as_ptr().add(x / 2)))), vdupq_n_s16(128));
            // 298/256 of luma is 74.5 in Q6, so *149 then >>1, unsigned as 255*149 passes the sign bit. Then 16 off, in the same scale,
            // and signed: values under 16 are not on a real desktop, but a damaged stream has them.
            let off = vdupq_n_s16(1192);
            let yl = vsubq_s16(vreinterpretq_s16_u16(vshrq_n_u16::<1>(vmull_n_u8_149(vget_low_u8(yv)))), off);
            let yh = vsubq_s16(vreinterpretq_s16_u16(vshrq_n_u16::<1>(vmull_n_u8_149(vget_high_u8(yv)))), off);
            let rd = vmulq_n_s16(vs, 102);
            let gd = vaddq_s16(vmulq_n_s16(us, 25), vmulq_n_s16(vs, 52));
            let bd = vmulq_n_s16(us, 129);
            let (rl, rh) = (vzip1q_s16(rd, rd), vzip2q_s16(rd, rd));
            let (gl, gh) = (vzip1q_s16(gd, gd), vzip2q_s16(gd, gd));
            let (bl, bh) = (vzip1q_s16(bd, bd), vzip2q_s16(bd, bd));
            let r = vcombine_u8(vqrshrun_n_s16::<6>(vqaddq_s16(yl, rl)), vqrshrun_n_s16::<6>(vqaddq_s16(yh, rh)));
            let g = vcombine_u8(vqrshrun_n_s16::<6>(vqsubq_s16(yl, gl)), vqrshrun_n_s16::<6>(vqsubq_s16(yh, gh)));
            let b = vcombine_u8(vqrshrun_n_s16::<6>(vqaddq_s16(yl, bl)), vqrshrun_n_s16::<6>(vqaddq_s16(yh, bh)));
            vst4q_u8(out.as_mut_ptr().add(x).cast(), uint8x16x4_t(b, g, r, vdupq_n_u8(255)));
            x += 16;
        }
    }
    x
}

#[cfg(target_arch = "aarch64")]
#[inline(always)]
unsafe fn vmull_n_u8_149(v: std::arch::aarch64::uint8x8_t) -> std::arch::aarch64::uint16x8_t {
    use std::arch::aarch64::*;
    unsafe { vmull_u8(v, vdup_n_u8(149)) }
}

/// The picture as pixels, and which rows differ from what was last taken with [`Canvas::take_dirty`].
#[derive(Default)]
pub struct Canvas {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u32>,
    dirty: Vec<bool>,
    any_dirty: bool,
    last_y: Vec<u8>,
    last_u: Vec<u8>,
    last_v: Vec<u8>,
}

impl Canvas {
    /// Takes a decoded picture: only rows that changed are converted. Returns how many were.
    pub fn update(&mut self, p: &Yuv) -> usize {
        if (p.w, p.h) != (self.w, self.h) {
            self.w = p.w;
            self.h = p.h;
            self.px = vec![0xff00_0000; p.w * p.h];
            self.dirty = vec![true; p.h];
            self.any_dirty = true;
            self.last_y = vec![0; p.w * p.h];
            let cw = p.w.div_ceil(2);
            let ch = p.h.div_ceil(2);
            self.last_u = vec![0; cw * ch];
            self.last_v = vec![0; cw * ch];
            // Nothing compares equal on the first picture.
            return self.convert_all(p);
        }
        let cw = p.w.div_ceil(2);
        let mut changed = 0;
        // A chroma row belongs to two luma rows, so rows go in pairs.
        let mut row = 0;
        while row < p.h {
            let cr = row / 2;
            let (us, vs) = (&p.u[cr * p.uv_stride..][..cw], &p.v[cr * p.uv_stride..][..cw]);
            let (lu, lv) = (&mut self.last_u[cr * cw..][..cw], &mut self.last_v[cr * cw..][..cw]);
            let chroma_changed = us != &lu[..] || vs != &lv[..];
            if chroma_changed {
                lu.copy_from_slice(us);
                lv.copy_from_slice(vs);
            }
            for r in row..(row + 2).min(p.h) {
                let ys = &p.y[r * p.y_stride..][..p.w];
                let ly = &mut self.last_y[r * p.w..][..p.w];
                if chroma_changed || ys != &ly[..] {
                    ly.copy_from_slice(ys);
                    row_to_xrgb(ys, us, vs, &mut self.px[r * p.w..][..p.w]);
                    self.dirty[r] = true;
                    self.any_dirty = true;
                    changed += 1;
                }
            }
            row += 2;
        }
        changed
    }

    fn convert_all(&mut self, p: &Yuv) -> usize {
        let cw = p.w.div_ceil(2);
        for r in 0..p.h {
            let cr = r / 2;
            let (us, vs) = (&p.u[cr * p.uv_stride..][..cw], &p.v[cr * p.uv_stride..][..cw]);
            let ys = &p.y[r * p.y_stride..][..p.w];
            self.last_y[r * p.w..][..p.w].copy_from_slice(ys);
            self.last_u[cr * cw..][..cw].copy_from_slice(us);
            self.last_v[cr * cw..][..cw].copy_from_slice(vs);
            row_to_xrgb(ys, us, vs, &mut self.px[r * p.w..][..p.w]);
        }
        p.h
    }

    /// The runs of changed rows, `(first, count)`, marking them clean.
    pub fn take_dirty(&mut self) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        if !self.any_dirty {
            return out;
        }
        self.any_dirty = false;
        let mut r = 0;
        while r < self.dirty.len() {
            if self.dirty[r] {
                let start = r;
                while r < self.dirty.len() && self.dirty[r] {
                    self.dirty[r] = false;
                    r += 1;
                }
                out.push((start, r - start));
            } else {
                r += 1;
            }
        }
        out
    }

    /// Everything counts as changed: the screen was taken over, or shown afresh.
    pub fn mark_all(&mut self) {
        self.dirty.iter_mut().for_each(|d| *d = true);
        self.any_dirty = !self.dirty.is_empty();
    }
}

/// Where a picture of the host's size goes on the panel: 1:1 and centred when it fits, scaled down to fit when it does not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fit {
    pub src_w: usize,
    pub src_h: usize,
    /// The rectangle on the panel.
    pub x: i32,
    pub y: i32,
    pub w: usize,
    pub h: usize,
}

impl Fit {
    pub fn new(src_w: usize, src_h: usize, panel_w: usize, panel_h: usize) -> Fit {
        let (mut w, mut h) = (src_w, src_h);
        if src_w > panel_w || src_h > panel_h {
            if src_w * panel_h > src_h * panel_w {
                w = panel_w;
                h = (src_h * panel_w / src_w).max(1);
            } else {
                h = panel_h;
                w = (src_w * panel_h / src_h).max(1);
            }
        }
        Fit { src_w, src_h, x: ((panel_w - w) / 2) as i32, y: ((panel_h - h) / 2) as i32, w, h }
    }

    pub fn is_one_to_one(&self) -> bool {
        self.w == self.src_w && self.h == self.src_h
    }

    /// A point on the panel as a point on the host, kept inside the picture.
    pub fn to_host(&self, x: i32, y: i32) -> (i32, i32) {
        let px = (x - self.x).clamp(0, self.w as i32 - 1) as i64;
        let py = (y - self.y).clamp(0, self.h as i32 - 1) as i64;
        ((px * self.src_w as i64 / self.w as i64) as i32, (py * self.src_h as i64 / self.h as i64) as i32)
    }
}

/// Scales down with two taps each way: readable for the moderate ratios of a laptop panel, and cheap enough for changed rows only.
pub struct Scaler {
    fit: Fit,
    /// For each column on the panel: the source column and the weight of the next one, 0..=256.
    cols: Vec<(u32, u32)>,
    /// The scaled picture, what the pointer is restored from.
    pub px: Vec<u32>,
}

impl Scaler {
    pub fn new(fit: Fit) -> Scaler {
        let cols = (0..fit.w)
            .map(|x| {
                let s = (x as u64 * 256 * fit.src_w as u64 + 128) / fit.w as u64;
                ((s >> 8) as u32, (s & 255) as u32)
            })
            .collect();
        Scaler { fit, cols, px: vec![0xff00_0000; fit.w * fit.h] }
    }

    fn src_row(&self, y: usize) -> (usize, u32) {
        let s = (y as u64 * 256 * self.fit.src_h as u64 + 128) / self.fit.h as u64;
        ((s >> 8) as usize, (s & 255) as u32)
    }

    /// The rows on the panel touched by source rows `first..first + count`.
    pub fn rows_for(&self, first: usize, count: usize) -> (usize, usize) {
        let f = &self.fit;
        let top = (first.saturating_sub(1) * f.h / f.src_h).min(f.h);
        let bottom = (((first + count + 1) * f.h).div_ceil(f.src_h)).min(f.h);
        (top, bottom - top)
    }

    /// Computes panel rows `first..first + count` from the source picture.
    pub fn scale(&mut self, src: &[u32], first: usize, count: usize) {
        let (sw, sh, w) = (self.fit.src_w, self.fit.src_h, self.fit.w);
        for y in first..first + count {
            let (sy, wy) = self.src_row(y);
            let a = &src[sy.min(sh - 1) * sw..][..sw];
            let b = &src[(sy + 1).min(sh - 1) * sw..][..sw];
            let out = &mut self.px[y * w..][..w];
            for (o, &(sx, wx)) in out.iter_mut().zip(&self.cols) {
                let (sx, nx) = (sx as usize, (sx as usize + 1).min(sw - 1));
                *o = mix4(a[sx], a[nx], b[sx], b[nx], wx, wy);
            }
        }
    }
}

/// Bilinear on packed XRGB: red and blue together, green apart.
#[inline]
fn mix4(p00: u32, p01: u32, p10: u32, p11: u32, wx: u32, wy: u32) -> u32 {
    #[inline]
    fn lerp(a: u32, b: u32, w: u32) -> u32 {
        let rb = ((a & 0xff00ff) * (256 - w) + (b & 0xff00ff) * w) >> 8;
        let g = ((a & 0x00ff00) * (256 - w) + (b & 0x00ff00) * w) >> 8;
        (rb & 0xff00ff) | (g & 0x00ff00)
    }
    0xff00_0000 | lerp(lerp(p00, p01, wx), lerp(p10, p11, wx), wy)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pic<'a>(w: usize, h: usize, y: &'a [u8], u: &'a [u8], v: &'a [u8]) -> Yuv<'a> {
        Yuv { w, h, y, u, v, y_stride: w, uv_stride: w.div_ceil(2) }
    }

    #[test]
    fn known_colours() {
        // libyuv's limited range: black, white, and the corners a screen shows.
        assert_eq!(yuv_pixel(16, 128, 128), 0xff00_0000);
        assert_eq!(yuv_pixel(235, 128, 128), 0xffff_ffff);
        let near = |p: u32, want: u32| {
            for s in [0, 8, 16] {
                assert!(((p >> s & 255) as i32 - (want >> s & 255) as i32).abs() <= 3, "{p:x} against {want:x}");
            }
        };
        near(yuv_pixel(81, 90, 240), 0xff0000);
        near(yuv_pixel(145, 54, 34), 0x00ff00);
        near(yuv_pixel(41, 240, 110), 0x0000ff);
    }

    /// The fast path against the plain one, over every kind of value: within 2 in each channel, as they round differently.
    #[test]
    fn the_row_routine_agrees_with_the_plain_formula() {
        let n = 64;
        let mut seed = 0x1234_5678u32;
        let mut rnd = move || {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            (seed >> 16) as u8
        };
        for round in 0..200 {
            let y: Vec<u8> = (0..n).map(|_| if round == 0 { 0 } else if round == 1 { 255 } else { rnd() }).collect();
            let u: Vec<u8> = (0..n / 2).map(|_| if round < 2 { 255 } else { rnd() }).collect();
            let v: Vec<u8> = (0..n / 2).map(|_| if round < 2 { 0 } else { rnd() }).collect();
            let mut out = vec![0u32; n];
            row_to_xrgb(&y, &u, &v, &mut out);
            for x in 0..n {
                let want = yuv_pixel(y[x], u[x / 2], v[x / 2]);
                for shift in [0, 8, 16] {
                    let (a, b) = ((out[x] >> shift & 255) as i32, (want >> shift & 255) as i32);
                    assert!((a - b).abs() <= 2, "x {x} shift {shift}: {a} against {b} from {} {} {}", y[x], u[x / 2], v[x / 2]);
                }
            }
        }
    }

    #[test]
    fn only_changed_rows_count() {
        let (w, h) = (8, 6);
        let mut y = vec![50u8; w * h];
        let u = vec![128u8; 4 * 3];
        let mut v = vec![128u8; 4 * 3];
        let mut c = Canvas::default();
        assert_eq!(c.update(&pic(w, h, &y, &u, &v)), h);
        assert_eq!(c.take_dirty(), vec![(0, h)]);
        assert_eq!(c.update(&pic(w, h, &y, &u, &v)), 0);
        assert!(c.take_dirty().is_empty());
        // One luma row.
        y[3 * w + 2] = 200;
        assert_eq!(c.update(&pic(w, h, &y, &u, &v)), 1);
        assert_eq!(c.take_dirty(), vec![(3, 1)]);
        // One chroma row is two luma rows.
        v[4 * 2] = 10;
        assert_eq!(c.update(&pic(w, h, &y, &u, &v)), 2);
        assert_eq!(c.take_dirty(), vec![(4, 2)]);
        assert_eq!(c.px[4 * w], yuv_pixel(50, 128, 10));
        c.mark_all();
        assert_eq!(c.take_dirty(), vec![(0, h)]);
    }

    #[test]
    fn an_odd_size_and_a_stride_wider_than_the_picture() {
        let (w, h) = (5, 3);
        let y = vec![100u8; 8 * h];
        let u = vec![128u8; 4 * 2];
        let v = vec![128u8; 4 * 2];
        let mut c = Canvas::default();
        let p = Yuv { w, h, y: &y, u: &u, v: &v, y_stride: 8, uv_stride: 4 };
        assert_eq!(c.update(&p), h);
        assert_eq!(c.px.len(), w * h);
        assert_eq!(c.update(&p), 0);
    }

    #[test]
    fn fit_and_coordinates() {
        let f = Fit::new(1920, 1080, 1920, 1080);
        assert!(f.is_one_to_one());
        assert_eq!((f.x, f.y), (0, 0));
        assert_eq!(f.to_host(100, 200), (100, 200));
        // A small desktop sits in the middle, and the pointer outside it stays on its edge.
        let f = Fit::new(1280, 720, 1920, 1080);
        assert_eq!((f.x, f.y, f.w, f.h), (320, 180, 1280, 720));
        assert_eq!(f.to_host(320, 180), (0, 0));
        assert_eq!(f.to_host(0, 0), (0, 0));
        assert_eq!(f.to_host(1919, 1079), (1279, 719));
        // A Mac's 2560x1664 goes down by height and leaves bars at the sides.
        let f = Fit::new(2560, 1664, 1920, 1080);
        assert_eq!((f.w, f.h), (1661, 1080));
        assert_eq!(f.x, 129);
        assert_eq!(f.to_host(f.x + f.w as i32 - 1, 1079).0, 2558);
        assert_eq!(f.to_host(960, 540), (1280, 832));
    }

    #[test]
    fn scaling_keeps_flat_colour_and_finds_the_changed_rows() {
        let f = Fit::new(40, 40, 20, 20);
        let mut s = Scaler::new(f);
        let src = vec![0xff12_3456u32; 40 * 40];
        s.scale(&src, 0, 20);
        assert!(s.px.iter().all(|&p| (p & 0xffffff) == 0x123456), "{:x}", s.px[0]);
        // A change in source row 20 of 40 reaches row 10 of 20, with a row of margin for the second tap.
        let (top, n) = s.rows_for(20, 1);
        assert!(top <= 9 && top + n >= 11, "{top} {n}");
        assert_eq!(s.rows_for(39, 1).0 + s.rows_for(39, 1).1, 20);
        let (top, n) = s.rows_for(0, 1);
        assert_eq!(top, 0);
        assert!(n >= 1);
    }
}
