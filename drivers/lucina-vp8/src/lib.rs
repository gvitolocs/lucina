//! A VP8 encoder for desktops, for one slow CPU: it spends time only on macroblocks that changed.
//!
//! A general encoder searches for motion in every frame; a screen is mostly unchanged, and what changes is usually unrelated
//! to what was there. So this one takes a map of the changed macroblocks from its caller. An unchanged one is a skip with no
//! motion, which costs a fraction of a bit and no time. A changed one is predicted from the same place in the last frame, or
//! from its neighbours when that is worse, and its residual is coded.
//!
//! What it leaves out, all of it optional in the format: motion vectors, 4x4 intra modes, the loop filter, segments, and
//! updating probabilities. Any VP8 decoder reads the result; `tests/` checks libvpx's reconstruction against ours, bit for bit.
//!
//! Format: RFC 6386. Behaviour reference: libvpx.

mod bool_coder;
mod tables;
mod transform;

use bool_coder::BoolEncoder;
use tables::*;
use transform::{fdct4x4, fwht4x4, idct4x4_add, iwht4x4};

/// Quantizer indices run from 0, nearly lossless, to 127.
pub const MAX_Q: u8 = 127;

const ZIGZAG: [usize; 16] = [0, 1, 4, 8, 5, 2, 3, 6, 9, 12, 13, 10, 7, 11, 14, 15];
const BANDS: [usize; 17] = [0, 1, 2, 3, 6, 4, 5, 6, 6, 6, 6, 6, 6, 6, 6, 7, 0];

/// Extra-bit probabilities of the larger token categories, with the smallest value of each.
const CAT1: (&[u8], i32) = (&[159], 5);
const CAT2: (&[u8], i32) = (&[165, 145], 7);
const CAT3: (&[u8], i32) = (&[173, 148, 140], 11);
const CAT4: (&[u8], i32) = (&[176, 155, 140, 135], 19);
const CAT5: (&[u8], i32) = (&[180, 157, 141, 134, 130], 35);
const CAT6: (&[u8], i32) = (&[254, 254, 243, 230, 196, 177, 153, 140, 133, 130, 129], 67);

/// The largest coefficient value the tokens can carry.
const MAX_COEF: i32 = 2048;

/// A picture in 4:2:0, the caller's.
pub struct Yuv<'a> {
    pub y: &'a [u8],
    pub u: &'a [u8],
    pub v: &'a [u8],
    pub y_stride: usize,
    pub uv_stride: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameInfo {
    pub key: bool,
    /// Macroblocks with coefficients.
    pub coded: usize,
    /// Macroblocks looked at, coded or not.
    pub examined: usize,
    pub first_partition: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Dc,
    V,
    H,
    /// From the same place in the last frame.
    Last,
}

#[derive(Clone, Copy)]
struct MbInfo {
    mode: Mode,
    skip: bool,
}

struct Plane {
    w: usize,
    h: usize,
    px: Vec<u8>,
}

impl Plane {
    fn new(w: usize, h: usize) -> Plane {
        Plane { w, h, px: vec![0; w * h] }
    }
}

/// Quantizer steps for one frame.
#[derive(Clone, Copy)]
struct Quant {
    y_dc: i32,
    y_ac: i32,
    y2_dc: i32,
    y2_ac: i32,
    uv_dc: i32,
    uv_ac: i32,
}

impl Quant {
    fn new(q: u8) -> Quant {
        let i = q.min(MAX_Q) as usize;
        Quant {
            y_dc: DC_Q[i],
            y_ac: AC_Q[i],
            y2_dc: DC_Q[i] * 2,
            y2_ac: (AC_Q[i] * 155 / 100).max(8),
            uv_dc: DC_Q[i].min(132),
            uv_ac: AC_Q[i],
        }
    }
}

#[inline]
fn quantize(c: i32, q: i32) -> i32 {
    // A dead zone a little under a half step: tiny differences cost bits and look like nothing on a flat background.
    let a = c.abs();
    let v = ((a + q * 3 / 8) / q).min(MAX_COEF);
    if c < 0 { -v } else { v }
}

/// Which blocks of the neighbours had coefficients, per plane: the contexts of the first coefficient.
#[derive(Clone, Copy, Default)]
struct Above {
    y: [u8; 4],
    u: [u8; 2],
    v: [u8; 2],
    y2: u8,
}

pub struct Encoder {
    w: usize,
    h: usize,
    mb_w: usize,
    mb_h: usize,
    /// The decoder's view of the last frame.
    y: Plane,
    u: Plane,
    v: Plane,
    /// The quantizer each macroblock was last coded with: lower is better. A refresh goes over the ones left coarse.
    mb_q: Vec<u8>,
    info: Vec<MbInfo>,
    above: Vec<Above>,
    have_key: bool,
    tokens: BoolEncoder,
    header: BoolEncoder,
}

impl Encoder {
    pub fn new(w: usize, h: usize) -> Encoder {
        assert!(w > 0 && h > 0 && w < 16384 && h < 16384, "VP8 sizes are 14 bits");
        let (mb_w, mb_h) = (w.div_ceil(16), h.div_ceil(16));
        Encoder {
            w,
            h,
            mb_w,
            mb_h,
            y: Plane::new(mb_w * 16, mb_h * 16),
            u: Plane::new(mb_w * 8, mb_h * 8),
            v: Plane::new(mb_w * 8, mb_h * 8),
            mb_q: vec![u8::MAX; mb_w * mb_h],
            info: vec![MbInfo { mode: Mode::Dc, skip: true }; mb_w * mb_h],
            above: vec![Above::default(); mb_w],
            have_key: false,
            tokens: BoolEncoder::new(),
            header: BoolEncoder::new(),
        }
    }

    pub fn macroblocks(&self) -> (usize, usize) {
        (self.mb_w, self.mb_h)
    }

    /// Macroblocks that a frame at quantizer `q` would improve.
    pub fn coarser_than(&self, q: u8) -> usize {
        self.mb_q.iter().filter(|&&m| m > q).count()
    }

    /// Encodes one frame into `out`, replacing what it held. `changed` has one flag per macroblock. A key frame comes when asked
    /// for and for the first frame, and codes everything. `refine` macroblocks at most of those left coarser than `q` are coded
    /// again, so that a picture that stops changing sharpens.
    pub fn encode(&mut self, pic: &Yuv, changed: &[bool], q: u8, want_key: bool, refine: usize, out: &mut Vec<u8>) -> FrameInfo {
        assert_eq!(changed.len(), self.mb_w * self.mb_h);
        let key = want_key || !self.have_key;
        self.have_key = true;
        let qt = Quant::new(q);
        self.tokens.reset();
        self.above.iter_mut().for_each(|a| *a = Above::default());
        let mut info = FrameInfo { key, ..FrameInfo::default() };
        let mut refine_left = refine;
        for my in 0..self.mb_h {
            let mut left = Above::default();
            for mx in 0..self.mb_w {
                let i = my * self.mb_w + mx;
                let again = !key && !changed[i] && refine_left > 0 && self.mb_q[i] > q;
                let work = key || changed[i] || again;
                if !work {
                    self.info[i] = MbInfo { mode: Mode::Last, skip: true };
                    // A skip clears every context, Y2 too as all our macroblocks have one (libvpx: vp8_reset_mb_tokens_context).
                    clear_contexts(&mut self.above[mx], &mut left);
                    continue;
                }
                if again {
                    refine_left -= 1;
                }
                info.examined += 1;
                let coded = self.code_macroblock(pic, mx, my, key, q, qt, &mut left);
                info.coded += coded as usize;
                self.mb_q[i] = q;
            }
        }
        self.write_frame(key, q, &mut info, out);
        info
    }

    /// Picks the mode, computes and writes the residual, and reconstructs. Returns whether anything was written.
    #[allow(clippy::too_many_arguments)]
    fn code_macroblock(&mut self, pic: &Yuv, mx: usize, my: usize, key: bool, _q: u8, qt: Quant, left: &mut Above) -> bool {
        let mode = self.pick_mode(pic, mx, my, key);
        let mut pred_y = [0u8; 256];
        let mut pred_u = [0u8; 64];
        let mut pred_v = [0u8; 64];
        self.predict(mode, mx, my, &mut pred_y, &mut pred_u, &mut pred_v);

        // The residual, transformed and quantized, in the order of the bitstream.
        let mut coefs = [[0i32; 16]; 25];
        let mut dc = [0i32; 16];
        let mut src = [0i16; 16];
        for by in 0..4 {
            for bx in 0..4 {
                let n = by * 4 + bx;
                for r in 0..4 {
                    let sy = (my * 16 + by * 4 + r).min(self.h.saturating_sub(1).max(my * 16));
                    let row = &pic.y[self.src_row(sy, pic.y_stride, pic.y.len())..];
                    for c in 0..4 {
                        let x = mx * 16 + bx * 4 + c;
                        let s = row.get(x.min(self.w - 1)).copied().unwrap_or(0) as i16;
                        src[r * 4 + c] = s - pred_y[(by * 4 + r) * 16 + bx * 4 + c] as i16;
                    }
                }
                let t = fdct4x4(&src);
                dc[n] = t[0];
                let mut any = false;
                for k in 1..16 {
                    let v = quantize(t[ZIGZAG[k]], qt.y_ac);
                    coefs[n][k] = v;
                    any |= v != 0;
                }
                let _ = any;
            }
        }
        let wdc = fwht4x4(&dc);
        coefs[24][0] = quantize(wdc[0], qt.y2_dc);
        for k in 1..16 {
            coefs[24][k] = quantize(wdc[ZIGZAG[k]], qt.y2_ac);
        }
        for (plane, data, stride, pred, base) in [(0, pic.u, pic.uv_stride, &pred_u, 16), (1, pic.v, pic.uv_stride, &pred_v, 20)] {
            let _ = plane;
            let (cw, ch) = (self.w.div_ceil(2), self.h.div_ceil(2));
            for by in 0..2 {
                for bx in 0..2 {
                    for r in 0..4 {
                        let sy = (my * 8 + by * 4 + r).min(ch - 1);
                        for c in 0..4 {
                            let sx = (mx * 8 + bx * 4 + c).min(cw - 1);
                            src[r * 4 + c] = data[sy * stride + sx] as i16 - pred[(by * 4 + r) * 8 + bx * 4 + c] as i16;
                        }
                    }
                    let t = fdct4x4(&src);
                    let n = base + by * 2 + bx;
                    coefs[n][0] = quantize(t[0], qt.uv_dc);
                    for k in 1..16 {
                        coefs[n][k] = quantize(t[ZIGZAG[k]], qt.uv_ac);
                    }
                }
            }
        }

        let any = coefs.iter().any(|b| b.iter().any(|&v| v != 0));
        self.info[my * self.mb_w + mx] = MbInfo { mode, skip: !any };
        let (y, u, v) = (&mut self.y, &mut self.u, &mut self.v);
        write_back(y, mx * 16, my * 16, 16, &pred_y, 16);
        write_back(u, mx * 8, my * 8, 8, &pred_u, 8);
        write_back(v, mx * 8, my * 8, 8, &pred_v, 8);
        if !any {
            clear_contexts(&mut self.above[mx], left);
            return false;
        }

        self.write_tokens(&coefs, mx, left);
        self.reconstruct(&coefs, mx, my, qt);
        true
    }

    /// Which picture row holds source row `y`: a coded picture is whole macroblocks, the caller's may be shorter.
    fn src_row(&self, y: usize, stride: usize, len: usize) -> usize {
        (y.min(self.h - 1) * stride).min(len.saturating_sub(1))
    }

    fn pick_mode(&self, pic: &Yuv, mx: usize, my: usize, key: bool) -> Mode {
        let (x0, y0) = (mx * 16, my * 16);
        let sad_against = |pred: &dyn Fn(usize, usize) -> u8| -> u32 {
            let mut sad = 0u32;
            for r in 0..16 {
                let row = self.src_row(y0 + r, pic.y_stride, pic.y.len());
                for c in 0..16 {
                    let s = pic.y.get(row + (x0 + c).min(self.w - 1)).copied().unwrap_or(0);
                    sad += (s as i32 - pred(r, c) as i32).unsigned_abs();
                }
            }
            sad
        };
        let stride = self.y.w;
        let ref_px = &self.y.px;
        let mut best = (u32::MAX, Mode::Dc);
        if !key {
            let sad = sad_against(&|r, c| ref_px[(y0 + r) * stride + x0 + c]);
            // Close enough to what was there: no reason to look further.
            if sad < 256 * 4 {
                return Mode::Last;
            }
            best = (sad, Mode::Last);
        }
        let mut above = [127u8; 16];
        let mut left = [129u8; 16];
        if my > 0 {
            above.copy_from_slice(&ref_px[(y0 - 1) * stride + x0..][..16]);
        }
        if mx > 0 {
            for (r, l) in left.iter_mut().enumerate() {
                *l = ref_px[(y0 + r) * stride + x0 - 1];
            }
        }
        let dc = dc_value(if my > 0 { Some(&above[..]) } else { None }, if mx > 0 { Some(&left[..]) } else { None }, 16);
        let mut try_mode = |m: Mode, sad: u32| {
            if sad < best.0 {
                best = (sad, m);
            }
        };
        try_mode(Mode::Dc, sad_against(&|_, _| dc));
        try_mode(Mode::V, sad_against(&|_, c| above[c]));
        try_mode(Mode::H, sad_against(&|r, _| left[r]));
        best.1
    }

    fn predict(&self, mode: Mode, mx: usize, my: usize, y: &mut [u8; 256], u: &mut [u8; 64], v: &mut [u8; 64]) {
        predict_plane(&self.y, mode, mx * 16, my * 16, 16, y);
        // Chroma follows luma's kind of prediction.
        predict_plane(&self.u, mode, mx * 8, my * 8, 8, u);
        predict_plane(&self.v, mode, mx * 8, my * 8, 8, v);
    }

    fn write_tokens(&mut self, coefs: &[[i32; 16]; 25], mx: usize, left: &mut Above) {
        let a = &mut self.above[mx];
        let t = &mut self.tokens;
        // Y2.
        let ctx = a.y2 + left.y2;
        let nz = write_block(t, &coefs[24], 1, ctx as usize, 0);
        a.y2 = nz;
        left.y2 = nz;
        for by in 0..4 {
            for bx in 0..4 {
                let ctx = a.y[bx] + left.y[by];
                let nz = write_block(t, &coefs[by * 4 + bx], 0, ctx as usize, 1);
                a.y[bx] = nz;
                left.y[by] = nz;
            }
        }
        for by in 0..2 {
            for bx in 0..2 {
                let ctx = a.u[bx] + left.u[by];
                let nz = write_block(t, &coefs[16 + by * 2 + bx], 2, ctx as usize, 0);
                a.u[bx] = nz;
                left.u[by] = nz;
            }
        }
        for by in 0..2 {
            for bx in 0..2 {
                let ctx = a.v[bx] + left.v[by];
                let nz = write_block(t, &coefs[20 + by * 2 + bx], 2, ctx as usize, 0);
                a.v[bx] = nz;
                left.v[by] = nz;
            }
        }
    }

    /// What the decoder will have: prediction plus the inverse transforms of what was written.
    fn reconstruct(&mut self, coefs: &[[i32; 16]; 25], mx: usize, my: usize, qt: Quant) {
        let mut y2 = [0i32; 16];
        y2[0] = coefs[24][0] * qt.y2_dc;
        for k in 1..16 {
            y2[ZIGZAG[k]] = coefs[24][k] * qt.y2_ac;
        }
        let dc = iwht4x4(&y2);
        for by in 0..4 {
            for bx in 0..4 {
                let n = by * 4 + bx;
                let mut b = [0i32; 16];
                b[0] = dc[n];
                for k in 1..16 {
                    b[ZIGZAG[k]] = coefs[n][k] * qt.y_ac;
                }
                let at = (my * 16 + by * 4) * self.y.w + mx * 16 + bx * 4;
                idct4x4_add(&b, &mut self.y.px[at..], self.y.w);
            }
        }
        for (plane, base) in [(&mut self.u, 16), (&mut self.v, 20)] {
            for by in 0..2 {
                for bx in 0..2 {
                    let n = base + by * 2 + bx;
                    let mut b = [0i32; 16];
                    b[0] = coefs[n][0] * qt.uv_dc;
                    for k in 1..16 {
                        b[ZIGZAG[k]] = coefs[n][k] * qt.uv_ac;
                    }
                    let at = (my * 8 + by * 4) * plane.w + mx * 8 + bx * 4;
                    let stride = plane.w;
                    idct4x4_add(&b, &mut plane.px[at..], stride);
                }
            }
        }
    }

    fn write_frame(&mut self, key: bool, q: u8, info: &mut FrameInfo, out: &mut Vec<u8>) {
        let n = self.info.len();
        let skips = self.info.iter().filter(|m| m.skip).count();
        // The probability that a macroblock has coefficients, as the frame really has it.
        let prob_skip = (((n - skips) * 255) / n).clamp(1, 255) as u8;
        let intra = self.info.iter().filter(|m| m.mode != Mode::Last).count();
        let prob_intra = ((intra * 255) / n).clamp(1, 255) as u8;

        let h = &mut self.header;
        h.reset();
        if key {
            h.put_bit(false); // colour space
            h.put_bit(false); // clamping needed
        }
        h.put_bit(false); // no segments
        h.put_bit(false); // normal filter, unused
        h.put_literal(0, 6); // loop filter level 0: off
        h.put_literal(0, 3); // sharpness
        h.put_bit(false); // no loop filter deltas
        h.put_literal(0, 2); // one token partition
        h.put_literal(q.min(MAX_Q) as u32, 7);
        for _ in 0..5 {
            h.put_bit(false); // no quantizer deltas
        }
        if key {
            h.put_bit(true); // keep the probabilities
        } else {
            h.put_bit(false); // don't refresh golden
            h.put_bit(false); // don't refresh altref
            h.put_literal(0, 2); // no copy to golden
            h.put_literal(0, 2); // no copy to altref
            h.put_bit(false); // sign bias golden
            h.put_bit(false); // sign bias altref
            h.put_bit(true); // keep the probabilities
            h.put_bit(true); // refresh last
        }
        for p in COEF_UPDATE_PROBS {
            h.put(p, false);
        }
        h.put_bit(true); // skips are signalled
        h.put_literal(prob_skip as u32, 8);
        if !key {
            h.put_literal(prob_intra as u32, 8);
            h.put_literal(255, 8); // prob_last: always the last frame
            h.put_literal(128, 8); // prob_golden, unused
            h.put_bit(false); // intra 16x16 probabilities stay
            h.put_bit(false); // intra chroma probabilities stay
            for p in MV_UPDATE_PROBS {
                h.put(p, false);
            }
        }
        // The modes.
        for my in 0..self.mb_h {
            for mx in 0..self.mb_w {
                let m = self.info[my * self.mb_w + mx];
                h.put(prob_skip, m.skip);
                if key {
                    write_kf_mode(h, m.mode);
                } else {
                    match m.mode {
                        Mode::Last => {
                            h.put(prob_intra, true);
                            h.put(255, false); // the last frame
                            // Every neighbour has no motion: only the count of those that are inter matters, weighted 2, 2, 1.
                            let inter = |i: isize, j: isize| -> u8 {
                                let (x, y) = (mx as isize + i, my as isize + j);
                                if x < 0 || y < 0 || x >= self.mb_w as isize {
                                    return 0;
                                }
                                (self.info[y as usize * self.mb_w + x as usize].mode == Mode::Last) as u8
                            };
                            let cnt = inter(0, -1) * 2 + inter(-1, 0) * 2 + inter(-1, -1);
                            h.put(MODE_CONTEXTS[cnt as usize * 4], false); // ZEROMV
                        }
                        other => {
                            h.put(prob_intra, false);
                            write_inter_frame_intra_mode(h, other);
                        }
                    }
                }
            }
        }
        h.finish();
        self.tokens.finish();
        let first = h.bytes().len();
        info.first_partition = first;

        out.clear();
        out.reserve(first + self.tokens.bytes().len() + 10);
        let tag = (!key) as u32 | 0 << 1 | 1 << 4 | (first as u32) << 5;
        out.extend_from_slice(&tag.to_le_bytes()[..3]);
        if key {
            out.extend_from_slice(&[0x9d, 0x01, 0x2a]);
            out.extend_from_slice(&(self.w as u16).to_le_bytes());
            out.extend_from_slice(&(self.h as u16).to_le_bytes());
        }
        out.extend_from_slice(h.bytes());
        out.extend_from_slice(self.tokens.bytes());
    }

    /// The decoder's view of the last frame, for tests and for anyone who wants to show what was sent.
    pub fn reconstruction(&self) -> (&[u8], &[u8], &[u8]) {
        (&self.y.px, &self.u.px, &self.v.px)
    }

    pub fn strides(&self) -> (usize, usize) {
        (self.y.w, self.u.w)
    }

    /// Plane height in whole macroblocks, as [`Self::reconstruction`] holds it.
    pub fn coded_height(&self) -> usize {
        self.y.h
    }
}

fn clear_contexts(a: &mut Above, l: &mut Above) {
    a.y = [0; 4];
    a.u = [0; 2];
    a.v = [0; 2];
    a.y2 = 0;
    *l = Above::default();
}

fn write_back(p: &mut Plane, x: usize, y: usize, n: usize, pred: &[u8], stride: usize) {
    for r in 0..n {
        p.px[(y + r) * p.w + x..][..n].copy_from_slice(&pred[r * stride..][..n]);
    }
}

fn dc_value(above: Option<&[u8]>, left: Option<&[u8]>, n: usize) -> u8 {
    let shift = n.trailing_zeros() + above.is_some() as u32 + left.is_some() as u32 - 1;
    if above.is_none() && left.is_none() {
        return 128;
    }
    let sum: u32 = above.iter().chain(left.iter()).flat_map(|s| s.iter()).map(|&v| v as u32).sum();
    ((sum + (1 << (shift - 1))) >> shift) as u8
}

fn predict_plane(p: &Plane, mode: Mode, x0: usize, y0: usize, n: usize, out: &mut [u8]) {
    let stride = p.w;
    if mode == Mode::Last {
        for r in 0..n {
            out[r * n..][..n].copy_from_slice(&p.px[(y0 + r) * stride + x0..][..n]);
        }
        return;
    }
    let mut above = [127u8; 16];
    let mut left = [129u8; 16];
    if y0 > 0 {
        above[..n].copy_from_slice(&p.px[(y0 - 1) * stride + x0..][..n]);
    }
    if x0 > 0 {
        for (r, l) in left[..n].iter_mut().enumerate() {
            *l = p.px[(y0 + r) * stride + x0 - 1];
        }
    }
    match mode {
        Mode::Dc => {
            let dc = dc_value(if y0 > 0 { Some(&above[..n]) } else { None }, if x0 > 0 { Some(&left[..n]) } else { None }, n);
            out[..n * n].fill(dc);
        }
        Mode::V => {
            for r in 0..n {
                out[r * n..][..n].copy_from_slice(&above[..n]);
            }
        }
        Mode::H => {
            for r in 0..n {
                out[r * n..][..n].fill(left[r]);
            }
        }
        Mode::Last => unreachable!(),
    }
}

fn write_kf_mode(h: &mut BoolEncoder, m: Mode) {
    // Not B_PRED.
    h.put(KF_YMODE_PROB[0], true);
    let (uv, a, b) = match m {
        Mode::Dc | Mode::Last => (0, false, false),
        Mode::V => (1, false, true),
        Mode::H => (2, true, false),
    };
    h.put(KF_YMODE_PROB[1], a);
    h.put(KF_YMODE_PROB[2 + a as usize], b);
    write_uv_mode(h, uv, &KF_UV_MODE_PROB);
}

fn write_inter_frame_intra_mode(h: &mut BoolEncoder, m: Mode) {
    // The tree is DC first, then V and H on one side, TM and B_PRED on the other.
    match m {
        Mode::Dc | Mode::Last => {
            h.put(YMODE_PROB[0], false);
            write_uv_mode(h, 0, &UV_MODE_PROB);
        }
        Mode::V => {
            h.put(YMODE_PROB[0], true);
            h.put(YMODE_PROB[1], false);
            h.put(YMODE_PROB[2], false);
            write_uv_mode(h, 1, &UV_MODE_PROB);
        }
        Mode::H => {
            h.put(YMODE_PROB[0], true);
            h.put(YMODE_PROB[1], false);
            h.put(YMODE_PROB[2], true);
            write_uv_mode(h, 2, &UV_MODE_PROB);
        }
    }
}

fn write_uv_mode(h: &mut BoolEncoder, mode: u8, p: &[u8; 3]) {
    match mode {
        0 => h.put(p[0], false),
        1 => {
            h.put(p[0], true);
            h.put(p[1], false);
        }
        _ => {
            h.put(p[0], true);
            h.put(p[1], true);
            h.put(p[2], false);
        }
    }
}

/// Writes one block; returns 1 if it holds any token before the end, the context its neighbours see.
fn write_block(t: &mut BoolEncoder, c: &[i32; 16], kind: usize, ctx: usize, first: usize) -> u8 {
    let mut end = 16;
    while end > first && c[end - 1] == 0 {
        end -= 1;
    }
    let probs = &DEFAULT_COEF_PROBS[kind * 8 * 3 * 11..][..8 * 3 * 11];
    let at = |band: usize, ctx: usize| &probs[(band * 3 + ctx) * 11..][..11];
    let mut p = at(BANDS[first], ctx);
    let mut i = first;
    let mut after_zero = false;
    while i < end {
        if !after_zero {
            t.put(p[0], true); // not the end
        }
        let v = c[i];
        let a = v.unsigned_abs() as i32;
        if a == 0 {
            t.put(p[1], false);
            i += 1;
            p = at(BANDS[i], 0);
            after_zero = true;
            continue;
        }
        t.put(p[1], true);
        if a == 1 {
            t.put(p[2], false);
        } else {
            t.put(p[2], true);
            if a <= 4 {
                t.put(p[3], false);
                if a == 2 {
                    t.put(p[4], false);
                } else {
                    t.put(p[4], true);
                    t.put(p[5], a == 4);
                }
            } else {
                t.put(p[3], true);
                if a <= 10 {
                    t.put(p[6], false);
                    if a <= 6 {
                        t.put(p[7], false);
                        write_extra(t, CAT1, a);
                    } else {
                        t.put(p[7], true);
                        write_extra(t, CAT2, a);
                    }
                } else {
                    t.put(p[6], true);
                    if a <= 34 {
                        t.put(p[8], false);
                        if a <= 18 {
                            t.put(p[9], false);
                            write_extra(t, CAT3, a);
                        } else {
                            t.put(p[9], true);
                            write_extra(t, CAT4, a);
                        }
                    } else {
                        t.put(p[8], true);
                        if a <= 66 {
                            t.put(p[10], false);
                            write_extra(t, CAT5, a);
                        } else {
                            t.put(p[10], true);
                            write_extra(t, CAT6, a);
                        }
                    }
                }
            }
        }
        t.put_bit(v < 0);
        i += 1;
        p = at(BANDS[i], if a == 1 { 1 } else { 2 });
        after_zero = false;
    }
    if i < 16 {
        t.put(p[0], false); // the end
    }
    (end > first) as u8
}

fn write_extra(t: &mut BoolEncoder, cat: (&[u8], i32), a: i32) {
    let (probs, base) = cat;
    let v = (a - base) as u32;
    let n = probs.len();
    for (k, &p) in probs.iter().enumerate() {
        t.put(p, v >> (n - 1 - k) & 1 == 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dc_prediction_at_the_edges() {
        let a = [10u8; 16];
        let l = [20u8; 16];
        assert_eq!(dc_value(None, None, 16), 128);
        assert_eq!(dc_value(Some(&a), None, 16), 10);
        assert_eq!(dc_value(None, Some(&l), 16), 20);
        assert_eq!(dc_value(Some(&a), Some(&l), 16), 15);
        assert_eq!(dc_value(Some(&a[..8]), Some(&l[..8]), 8), 15);
    }

    #[test]
    fn quantizer_steps_follow_the_format() {
        let q = Quant::new(0);
        assert_eq!((q.y_dc, q.y_ac, q.y2_dc, q.y2_ac, q.uv_dc), (4, 4, 8, 8, 4));
        let q = Quant::new(127);
        assert_eq!((q.y_dc, q.y_ac, q.y2_dc, q.y2_ac, q.uv_dc), (157, 284, 314, 440, 132));
    }

    #[test]
    fn quantize_is_symmetric_and_capped() {
        assert_eq!(quantize(100, 10), 10);
        assert_eq!(quantize(-100, 10), -10);
        assert_eq!(quantize(2, 10), 0);
        assert_eq!(quantize(1_000_000, 4), MAX_COEF);
    }
}
