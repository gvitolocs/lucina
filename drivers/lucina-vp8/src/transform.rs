//! VP8's transforms. The inverse ones must match the decoder bit for bit (RFC 6386 section 14); the forward ones are libvpx's,
//! the closest match to them.

const COSPI8_SQRT2_MINUS1: i32 = 20091;
const SINPI8_SQRT2: i32 = 35468;

/// The forward DCT of a 4x4 residual, as `vp8_short_fdct4x4_c`.
pub fn fdct4x4(src: &[i16; 16]) -> [i32; 16] {
    let mut t = [0i32; 16];
    for i in 0..4 {
        let (s0, s1, s2, s3) = (src[i * 4] as i32, src[i * 4 + 1] as i32, src[i * 4 + 2] as i32, src[i * 4 + 3] as i32);
        let a1 = (s0 + s3) * 8;
        let b1 = (s1 + s2) * 8;
        let c1 = (s1 - s2) * 8;
        let d1 = (s0 - s3) * 8;
        t[i * 4] = a1 + b1;
        t[i * 4 + 2] = a1 - b1;
        t[i * 4 + 1] = (c1 * 2217 + d1 * 5352 + 14500) >> 12;
        t[i * 4 + 3] = (d1 * 2217 - c1 * 5352 + 7500) >> 12;
    }
    let mut out = [0i32; 16];
    for i in 0..4 {
        let a1 = t[i] + t[12 + i];
        let b1 = t[4 + i] + t[8 + i];
        let c1 = t[4 + i] - t[8 + i];
        let d1 = t[i] - t[12 + i];
        out[i] = (a1 + b1 + 7) >> 4;
        out[8 + i] = (a1 - b1 + 7) >> 4;
        out[4 + i] = ((c1 * 2217 + d1 * 5352 + 12000) >> 16) + (d1 != 0) as i32;
        out[12 + i] = (d1 * 2217 - c1 * 5352 + 51000) >> 16;
    }
    out
}

/// The forward Walsh-Hadamard transform of the 16 luma DCs, as `vp8_short_walsh4x4_c`.
pub fn fwht4x4(input: &[i32; 16]) -> [i32; 16] {
    let mut t = [0i32; 16];
    for i in 0..4 {
        let (s0, s1, s2, s3) = (input[i * 4], input[i * 4 + 1], input[i * 4 + 2], input[i * 4 + 3]);
        let a1 = (s0 + s2) * 4;
        let d1 = (s1 + s3) * 4;
        let c1 = (s1 - s3) * 4;
        let b1 = (s0 - s2) * 4;
        t[i * 4] = a1 + d1 + (a1 != 0) as i32;
        t[i * 4 + 1] = b1 + c1;
        t[i * 4 + 2] = b1 - c1;
        t[i * 4 + 3] = a1 - d1;
    }
    let mut out = [0i32; 16];
    for i in 0..4 {
        let a1 = t[i] + t[8 + i];
        let d1 = t[4 + i] + t[12 + i];
        let c1 = t[4 + i] - t[12 + i];
        let b1 = t[i] - t[8 + i];
        let fix = |v: i32| (v + (v < 0) as i32 + 3) >> 3;
        out[i] = fix(a1 + d1);
        out[4 + i] = fix(b1 + c1);
        out[8 + i] = fix(b1 - c1);
        out[12 + i] = fix(a1 - d1);
    }
    out
}

/// The inverse Walsh-Hadamard transform, as `vp8_short_inv_walsh4x4_c`.
pub fn iwht4x4(input: &[i32; 16]) -> [i32; 16] {
    let mut t = [0i32; 16];
    for i in 0..4 {
        let a1 = input[i] + input[12 + i];
        let b1 = input[4 + i] + input[8 + i];
        let c1 = input[4 + i] - input[8 + i];
        let d1 = input[i] - input[12 + i];
        t[i] = a1 + b1;
        t[4 + i] = c1 + d1;
        t[8 + i] = a1 - b1;
        t[12 + i] = d1 - c1;
    }
    let mut out = [0i32; 16];
    for i in 0..4 {
        let a1 = t[i * 4] + t[i * 4 + 3];
        let b1 = t[i * 4 + 1] + t[i * 4 + 2];
        let c1 = t[i * 4 + 1] - t[i * 4 + 2];
        let d1 = t[i * 4] - t[i * 4 + 3];
        out[i * 4] = (a1 + b1 + 3) >> 3;
        out[i * 4 + 1] = (c1 + d1 + 3) >> 3;
        out[i * 4 + 2] = (a1 - b1 + 3) >> 3;
        out[i * 4 + 3] = (d1 - c1 + 3) >> 3;
    }
    out
}

/// The inverse DCT added to the prediction already in `dst`, as `vp8_short_idct4x4llm_c`.
pub fn idct4x4_add(input: &[i32; 16], dst: &mut [u8], stride: usize) {
    let mut t = [0i32; 16];
    for i in 0..4 {
        let a1 = input[i] + input[8 + i];
        let b1 = input[i] - input[8 + i];
        let t1 = (input[4 + i] * SINPI8_SQRT2) >> 16;
        let t2 = input[12 + i] + ((input[12 + i] * COSPI8_SQRT2_MINUS1) >> 16);
        let c1 = t1 - t2;
        let t1 = input[4 + i] + ((input[4 + i] * COSPI8_SQRT2_MINUS1) >> 16);
        let t2 = (input[12 + i] * SINPI8_SQRT2) >> 16;
        let d1 = t1 + t2;
        t[i] = a1 + d1;
        t[12 + i] = a1 - d1;
        t[4 + i] = b1 + c1;
        t[8 + i] = b1 - c1;
    }
    for r in 0..4 {
        let (i0, i1, i2, i3) = (t[r * 4], t[r * 4 + 1], t[r * 4 + 2], t[r * 4 + 3]);
        let a1 = i0 + i2;
        let b1 = i0 - i2;
        let t1 = (i1 * SINPI8_SQRT2) >> 16;
        let t2 = i3 + ((i3 * COSPI8_SQRT2_MINUS1) >> 16);
        let c1 = t1 - t2;
        let t1 = i1 + ((i1 * COSPI8_SQRT2_MINUS1) >> 16);
        let t2 = (i3 * SINPI8_SQRT2) >> 16;
        let d1 = t1 + t2;
        let row = &mut dst[r * stride..][..4];
        for (d, v) in row.iter_mut().zip([a1 + d1, b1 + c1, b1 - c1, a1 - d1]) {
            *d = (*d as i32 + ((v + 4) >> 3)).clamp(0, 255) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flat_block_is_only_dc_and_comes_back() {
        let src = [40i16; 16];
        let f = fdct4x4(&src);
        assert_eq!(f[0], 40 * 8);
        // The forward transform's rounding leaves crumbs, which any quantizer removes.
        assert!(f[1..].iter().all(|&v| v.abs() <= 1), "{f:?}");
        let mut dc = [0i32; 16];
        dc[0] = f[0];
        let mut dst = [10u8; 16];
        idct4x4_add(&dc, &mut dst, 4);
        assert!(dst.iter().all(|&v| v == 50), "{dst:?}");
    }

    #[test]
    fn the_round_trip_stays_close() {
        let src: [i16; 16] = [10, -20, 30, -40, 50, -60, 70, -80, 5, 15, 25, 35, -5, -15, -25, -35];
        let f = fdct4x4(&src);
        let mut dst = [128u8; 16];
        idct4x4_add(&f, &mut dst, 4);
        for i in 0..16 {
            assert!((dst[i] as i32 - 128 - src[i] as i32).abs() <= 2, "{i}: {} against {}", dst[i] as i32 - 128, src[i]);
        }
    }

    #[test]
    fn walsh_round_trip_stays_close() {
        let v: [i32; 16] = [100, -50, 25, 12, 8, -3, 44, 90, -120, 60, 30, -30, 7, 7, -7, 1];
        let back = iwht4x4(&fwht4x4(&v));
        for i in 0..16 {
            assert!((back[i] - v[i]).abs() <= 2, "{i}: {} against {}", back[i], v[i]);
        }
    }

    #[test]
    fn dc_only_walsh_is_what_the_decoder_short_cut_gives() {
        let mut v = [0i32; 16];
        v[0] = 83;
        let out = iwht4x4(&v);
        assert!(out.iter().all(|&o| o == (83 + 3) >> 3));
    }
}
