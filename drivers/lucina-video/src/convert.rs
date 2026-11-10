//! A decoded picture to screen pixels: YUV 4:2:0 to `0xAARRGGBB`, scaled to fit, keeping the shape.
//!
//! This runs for every frame, on a CPU with no time to spare, so it is built in stages that each do little: a row is converted once at the
//! source's size with NEON, then stretched. The maths is one integer formula in both the NEON and plain versions, 6 bits after the point,
//! so that a test on the laptop can prove them the same (`lucina-video-bench selftest`).

/// One decoded picture, in whatever memory the decoder keeps it.
#[derive(Clone, Copy)]
pub struct Planes<'a> {
    pub w: usize,
    pub h: usize,
    pub y: &'a [u8],
    pub u: &'a [u8],
    pub v: &'a [u8],
    pub y_stride: usize,
    pub uv_stride: usize,
}

/// Which colours the numbers mean. Video this size is HD unless it says otherwise: standard definition uses the older matrix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Matrix {
    y_off: i16,
    cy: i16,
    rv: i16,
    gu: i16,
    gv: i16,
    bu: i16,
}

impl Matrix {
    pub const BT709: Matrix = Matrix { y_off: 16, cy: 74, rv: 115, gu: 14, gv: 34, bu: 135 };
    pub const BT601: Matrix = Matrix { y_off: 16, cy: 74, rv: 102, gu: 25, gv: 52, bu: 129 };

    pub fn for_size(_w: usize, h: usize) -> Matrix {
        if h > 576 { Matrix::BT709 } else { Matrix::BT601 }
    }
}

#[inline]
fn clamp8(v: i32) -> u32 {
    v.clamp(0, 255) as u32
}

/// One row, plain. `y` has at least `out.len()` samples, `u` and `v` half of that, rounded up.
pub fn row_plain(m: &Matrix, y: &[u8], u: &[u8], v: &[u8], out: &mut [u32]) {
    for (x, p) in out.iter_mut().enumerate() {
        let l = (y[x] as i32 - m.y_off as i32) * m.cy as i32;
        let (cu, cv) = (u[x / 2] as i32 - 128, v[x / 2] as i32 - 128);
        let r = clamp8((l + cv * m.rv as i32) >> 6);
        let g = clamp8((l - cu * m.gu as i32 - cv * m.gv as i32) >> 6);
        let b = clamp8((l + cu * m.bu as i32) >> 6);
        *p = 0xff00_0000 | r << 16 | g << 8 | b;
    }
}

/// One row, as fast as this machine can.
#[cfg(target_arch = "aarch64")]
pub fn row(m: &Matrix, y: &[u8], u: &[u8], v: &[u8], out: &mut [u32]) {
    use std::arch::aarch64::*;
    let w = out.len();
    let wide = w & !15;
    assert!(y.len() >= w && u.len() >= w.div_ceil(2) && v.len() >= w.div_ceil(2));
    // SAFETY: NEON is part of AArch64. Every load and store below is inside the slices checked above: `x + 16 <= wide <= w`, and the chroma
    // reads `x / 2 + 8 <= w / 2`.
    unsafe {
        let (yoff, cy) = (vdup_n_u8(m.y_off as u8), vdupq_n_s16(m.cy));
        let (rv, gu, gv, bu) = (vdupq_n_s16(m.rv), vdupq_n_s16(m.gu), vdupq_n_s16(m.gv), vdupq_n_s16(m.bu));
        let c128 = vdup_n_u8(128);
        let alpha = vdupq_n_u8(255);
        let mut x = 0;
        while x < wide {
            let yy = vld1q_u8(y.as_ptr().add(x));
            let uu = vld1_u8(u.as_ptr().add(x / 2));
            let vv = vld1_u8(v.as_ptr().add(x / 2));
            let cu = vreinterpretq_s16_u16(vsubl_u8(uu, c128));
            let cv = vreinterpretq_s16_u16(vsubl_u8(vv, c128));
            let dr = vmulq_s16(cv, rv);
            let dg = vaddq_s16(vmulq_s16(cu, gu), vmulq_s16(cv, gv));
            let db = vmulq_s16(cu, bu);
            // One chroma sample for two pixels. The difference is in 16 bits, and the sums below stay in them for video-range input
            // and saturate for anything else, which is what the plain version's clamp gives for those.
            let (r0, r1) = (vzip1q_s16(dr, dr), vzip2q_s16(dr, dr));
            let (g0, g1) = (vzip1q_s16(dg, dg), vzip2q_s16(dg, dg));
            let (b0, b1) = (vzip1q_s16(db, db), vzip2q_s16(db, db));
            // Below the black level the plain version goes negative and clamps: so does a signed difference here.
            let l0 = vmulq_s16(vreinterpretq_s16_u16(vsubl_u8(vget_low_u8(yy), yoff)), cy);
            let l1 = vmulq_s16(vreinterpretq_s16_u16(vsubl_high_u8(yy, vcombine_u8(yoff, yoff))), cy);
            let r = vcombine_u8(vqshrun_n_s16::<6>(vqaddq_s16(l0, r0)), vqshrun_n_s16::<6>(vqaddq_s16(l1, r1)));
            let g = vcombine_u8(vqshrun_n_s16::<6>(vqsubq_s16(l0, g0)), vqshrun_n_s16::<6>(vqsubq_s16(l1, g1)));
            let b = vcombine_u8(vqshrun_n_s16::<6>(vqaddq_s16(l0, b0)), vqshrun_n_s16::<6>(vqaddq_s16(l1, b1)));
            vst4q_u8(out.as_mut_ptr().add(x).cast::<u8>(), uint8x16x4_t(b, g, r, alpha));
            x += 16;
        }
    }
    row_plain(m, &y[wide..], &u[wide / 2..], &v[wide / 2..], &mut out[wide..]);
}

#[cfg(not(target_arch = "aarch64"))]
pub fn row(m: &Matrix, y: &[u8], u: &[u8], v: &[u8], out: &mut [u32]) {
    row_plain(m, y, u, v, out);
}

/// How a picture is stretched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quality {
    /// Nearest neighbour: costs almost nothing, and shows blocks.
    Fast,
    /// Bilinear both ways.
    Smooth,
}

/// Where a picture lands on the screen, and its pixels.
pub struct Scaler {
    src: (usize, usize),
    pub dst: (usize, usize),
    /// Top left corner on the screen.
    pub at: (usize, usize),
    pub quality: Quality,
    matrix: Matrix,
    /// The source column, and 8 bits of the way to the next, for each screen column.
    columns: Vec<(u32, u32)>,
    /// `dst.0 * dst.1` pixels.
    pub out: Vec<u32>,
    row_a: Vec<u32>,
    row_b: Vec<u32>,
    mixed: Vec<u32>,
    /// Which source rows `row_a` and `row_b` hold now.
    have: (usize, usize),
}

/// Where a picture of `src` pixels goes on a screen of `screen` pixels, keeping its shape: its size there and its top left corner. The
/// GPU path places pictures with this too, so both look the same.
pub fn fit(src: (usize, usize), screen: (usize, usize)) -> ((usize, usize), (usize, usize)) {
    let k = (screen.0 as f64 / src.0.max(1) as f64).min(screen.1 as f64 / src.1.max(1) as f64);
    let dw = ((src.0 as f64 * k).round() as usize).clamp(1, screen.0);
    let dh = ((src.1 as f64 * k).round() as usize).clamp(1, screen.1);
    ((dw, dh), ((screen.0 - dw) / 2, (screen.1 - dh) / 2))
}

impl Scaler {
    /// A picture of `src` pixels, fitted to a screen of `screen` pixels.
    pub fn new(src: (usize, usize), screen: (usize, usize), quality: Quality) -> Scaler {
        let ((dw, dh), at) = fit(src, screen);
        let columns = (0..dw)
            .map(|x| {
                let s = ((x as f64 + 0.5) * src.0 as f64 / dw as f64 - 0.5).max(0.0);
                let i = (s as usize).min(src.0 - 1);
                (i as u32, ((s.fract() * 256.0) as u32).min(255))
            })
            .collect();
        Scaler {
            src,
            dst: (dw, dh),
            at,
            quality,
            matrix: Matrix::for_size(src.0, src.1),
            columns,
            out: vec![0xff00_0000; dw * dh],
            row_a: vec![0; src.0],
            row_b: vec![0; src.0],
            mixed: vec![0; src.0],
            have: (usize::MAX, usize::MAX),
        }
    }

    /// The picture size this was made for.
    pub fn source(&self) -> (usize, usize) {
        self.src
    }

    fn load(&mut self, p: &Planes, sy: usize, into_b: bool) {
        let cy = sy / 2;
        let y = &p.y[sy * p.y_stride..][..p.w];
        let cw = p.w.div_ceil(2);
        let u = &p.u[cy * p.uv_stride..][..cw];
        let v = &p.v[cy * p.uv_stride..][..cw];
        let out = if into_b { &mut self.row_b } else { &mut self.row_a };
        row(&self.matrix, y, u, v, &mut out[..p.w]);
    }

    /// Turn `p` into [`Scaler::out`]. A picture of another size than this was made for is the caller's to catch: it makes a new one.
    pub fn render(&mut self, p: &Planes) {
        assert_eq!((p.w, p.h), self.src, "a Scaler is made for one size");
        self.have = (usize::MAX, usize::MAX);
        let (dw, dh) = self.dst;
        let same_shape = dw == p.w && dh == p.h;
        for dy in 0..dh {
            let start = dy * dw;
            if same_shape {
                self.load(p, dy, false);
                self.out[start..start + dw].copy_from_slice(&self.row_a[..dw]);
                continue;
            }
            match self.quality {
                Quality::Fast => {
                    let sy = (dy * p.h / dh).min(p.h - 1);
                    if self.have.0 != sy {
                        self.load(p, sy, false);
                        self.have.0 = sy;
                        let (out, a) = (&mut self.out[start..start + dw], &self.row_a);
                        for (d, c) in out.iter_mut().zip(&self.columns) {
                            *d = a[c.0 as usize];
                        }
                    } else {
                        self.out.copy_within(start - dw..start, start);
                    }
                }
                Quality::Smooth => {
                    let s = ((dy as f64 + 0.5) * p.h as f64 / dh as f64 - 0.5).max(0.0);
                    let sy = (s as usize).min(p.h - 1);
                    let next = (sy + 1).min(p.h - 1);
                    let t = ((s.fract() * 256.0) as u32).min(255);
                    // Rows move down the picture: the last one read is often the one needed first.
                    if self.have.0 != sy {
                        if self.have.1 == sy {
                            std::mem::swap(&mut self.row_a, &mut self.row_b);
                            self.have.0 = sy;
                            self.have.1 = usize::MAX;
                        } else {
                            self.load(p, sy, false);
                            self.have.0 = sy;
                        }
                    }
                    if self.have.1 != next {
                        self.load(p, next, true);
                        self.have.1 = next;
                    }
                    if t == 0 {
                        stretch(&self.row_a[..p.w], &self.columns, &mut self.out[start..start + dw]);
                    } else {
                        mix_rows(&self.row_a[..p.w], &self.row_b[..p.w], t, &mut self.mixed[..p.w]);
                        stretch(&self.mixed[..p.w], &self.columns, &mut self.out[start..start + dw]);
                    }
                }
            }
        }
    }
}

#[inline]
fn mix(p: u32, q: u32, t: u32) -> u32 {
    let rb = (((p & 0xff00ff) * (256 - t) + (q & 0xff00ff) * t) >> 8) & 0xff00ff;
    let g = (((p & 0xff00) * (256 - t) + (q & 0xff00) * t) >> 8) & 0xff00;
    0xff00_0000 | rb | g
}

/// `(a * (256 - t) + b * t) / 256` for each channel, for t from 1 to 255.
fn mix_rows(a: &[u32], b: &[u32], t: u32, out: &mut [u32]) {
    #[cfg(target_arch = "aarch64")]
    {
        use std::arch::aarch64::*;
        let wide = out.len() & !3;
        assert!(a.len() >= out.len() && b.len() >= out.len());
        // SAFETY: NEON is part of AArch64; loads and stores stay under `wide`, within the three slices.
        unsafe {
            // t is 1 to 255 here, so `256 - t` fits a byte.
            let (ta, tb) = (vdup_n_u8((256 - t) as u8), vdup_n_u8(t as u8));
            let mut x = 0;
            while x < wide {
                let (pa, pb) = (vld1q_u8(a.as_ptr().add(x).cast()), vld1q_u8(b.as_ptr().add(x).cast()));
                let lo = vaddq_u16(vmull_u8(vget_low_u8(pa), ta), vmull_u8(vget_low_u8(pb), tb));
                let hi = vaddq_u16(vmull_high_u8(pa, vcombine_u8(ta, ta)), vmull_high_u8(pb, vcombine_u8(tb, tb)));
                let r = vcombine_u8(vshrn_n_u16::<8>(lo), vshrn_n_u16::<8>(hi));
                vst1q_u8(out.as_mut_ptr().add(x).cast(), r);
                x += 4;
            }
        }
        for x in wide..out.len() {
            out[x] = mix(a[x], b[x], t);
        }
    }
    #[cfg(not(target_arch = "aarch64"))]
    for x in 0..out.len() {
        out[x] = mix(a[x], b[x], t);
    }
}

fn stretch(row: &[u32], columns: &[(u32, u32)], out: &mut [u32]) {
    let last = row.len() - 1;
    for (d, &(i, t)) in out.iter_mut().zip(columns) {
        let i = i as usize;
        *d = if t == 0 { row[i] } else { mix(row[i], row[(i + 1).min(last)], t) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn planes<'a>(w: usize, h: usize, y: &'a [u8], u: &'a [u8], v: &'a [u8]) -> Planes<'a> {
        Planes { w, h, y, u, v, y_stride: w, uv_stride: w.div_ceil(2) }
    }

    /// Black, white, and the primaries as the matrix's own inverse would give them.
    #[test]
    fn known_colours() {
        let m = Matrix::BT709;
        let one = |y, u, v| {
            let mut o = [0u32; 1];
            row_plain(&m, &[y], &[u], &[v], &mut o);
            o[0]
        };
        assert_eq!(one(16, 128, 128), 0xff000000);
        let white = one(235, 128, 128);
        assert!(white & 0xff >= 250, "{white:x}");
        // BT.709 red: Y 63, Cb 102, Cr 240.
        let red = one(63, 102, 240);
        assert!((red >> 16 & 0xff) > 245 && (red >> 8 & 0xff) < 12 && (red & 0xff) < 12, "{red:x}");
        // Below black and above white clamp instead of wrapping.
        assert_eq!(one(0, 128, 128), 0xff000000);
        assert_eq!(one(255, 128, 128) & 0xff, 255);
    }

    #[test]
    fn neon_and_plain_are_the_same_on_every_length_and_extreme() {
        let mut seed = 0x1234_5678u32;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed as u8
        };
        for w in [1usize, 2, 15, 16, 17, 31, 32, 33, 640] {
            let mut y: Vec<u8> = (0..w).map(|_| next()).collect();
            let mut u: Vec<u8> = (0..w.div_ceil(2)).map(|_| next()).collect();
            let mut v: Vec<u8> = (0..w.div_ceil(2)).map(|_| next()).collect();
            // The corners of the range, where saturation shows.
            if w >= 16 {
                y[0] = 0;
                y[1] = 255;
                u[0] = 0;
                v[0] = 255;
                y[2] = 255;
                y[3] = 255;
                u[1] = 255;
                v[1] = 255;
            }
            for m in [Matrix::BT709, Matrix::BT601] {
                let (mut a, mut b) = (vec![0u32; w], vec![0u32; w]);
                row(&m, &y, &u, &v, &mut a);
                row_plain(&m, &y, &u, &v, &mut b);
                assert_eq!(a, b, "width {w}");
            }
        }
    }

    #[test]
    fn fits_the_screen_and_keeps_the_shape() {
        let s = Scaler::new((1920, 800), (1920, 1080), Quality::Fast);
        assert_eq!((s.dst, s.at), ((1920, 800), (0, 140)));
        let s = Scaler::new((854, 480), (1920, 1080), Quality::Fast);
        assert_eq!((s.dst, s.at), ((1920, 1079), (0, 0)));
        let s = Scaler::new((640, 480), (1920, 1080), Quality::Fast);
        assert_eq!((s.dst, s.at), ((1440, 1080), (240, 0)));
        // A picture larger than the screen comes down.
        let s = Scaler::new((3840, 2160), (1920, 1080), Quality::Smooth);
        assert_eq!(s.dst, (1920, 1080));
    }

    /// Left half black, right half white.
    fn half(w: usize, h: usize) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let y = (0..h).flat_map(|_| (0..w).map(move |x| if x < w / 2 { 16 } else { 235 })).collect();
        let c = vec![128u8; w.div_ceil(2) * h.div_ceil(2)];
        (y, c.clone(), c)
    }

    #[test]
    fn each_way_of_stretching_puts_things_in_the_same_place() {
        let (y, u, v) = half(64, 36);
        let p = planes(64, 36, &y, &u, &v);
        for q in [Quality::Fast, Quality::Smooth] {
            let mut s = Scaler::new((64, 36), (640, 360), q);
            s.render(&p);
            assert_eq!(s.dst, (640, 360));
            let (w, h) = s.dst;
            for row in [0, h / 2, h - 1] {
                assert_eq!(s.out[row * w + 10] & 0xff, 0, "{q:?}");
                assert!(s.out[row * w + w - 10] & 0xff >= 250, "{q:?}");
            }
        }
    }

    #[test]
    fn smooth_makes_a_ramp_and_fast_does_not() {
        let (y, u, v) = half(8, 8);
        let p = planes(8, 8, &y, &u, &v);
        let mut s = Scaler::new((8, 8), (80, 80), Quality::Smooth);
        s.render(&p);
        let mid: Vec<u32> = s.out[40 * 80..41 * 80].iter().map(|p| p & 0xff).collect();
        assert!(mid.iter().any(|v| *v > 40 && *v < 200));
        let mut s = Scaler::new((8, 8), (80, 80), Quality::Fast);
        s.render(&p);
        assert!(s.out.iter().all(|p| p & 0xff == 0 || p & 0xff >= 250));
    }

    #[test]
    fn a_picture_at_its_own_size_is_only_converted() {
        let (y, u, v) = half(32, 2);
        let p = planes(32, 2, &y, &u, &v);
        let mut s = Scaler::new((32, 2), (32, 2), Quality::Smooth);
        s.render(&p);
        let mut direct = vec![0u32; 32];
        row(&Matrix::for_size(32, 2), &y[..32], &u[..16], &v[..16], &mut direct);
        assert_eq!(&s.out[..32], &direct[..]);
    }

    #[test]
    fn strides_wider_than_the_picture_are_followed() {
        // Two rows of 4 pixels in a stride of 6, each row with its own level.
        let y = [16, 16, 16, 16, 9, 9, 235, 235, 235, 235, 9, 9];
        let c = [128u8, 128, 7, 128, 128, 7];
        let p = Planes { w: 4, h: 2, y: &y, u: &c, v: &c, y_stride: 6, uv_stride: 3 };
        let mut s = Scaler::new((4, 2), (4, 2), Quality::Fast);
        s.render(&p);
        assert_eq!(s.out[0] & 0xff, 0);
        assert!(s.out[4] & 0xff >= 250);
    }

    #[test]
    fn mixing_keeps_opaque_pixels_opaque() {
        let (a, b) = ([0xff00_0000u32; 5], [0xffff_ffffu32; 5]);
        let mut o = [0u32; 5];
        mix_rows(&a, &b, 128, &mut o);
        for p in o {
            assert!((p >> 24) >= 0xfe, "{p:x}");
            assert!((p & 0xff).abs_diff(128) <= 1, "{p:x}");
        }
    }
}
