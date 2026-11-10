//! Frames to pixels: everything here outputs `u32` `0x00RRGGBB`, what `lucina_app::screen::Screen::present_image` takes.

use std::borrow::Cow;

pub use crate::Matrix;

/// YUY2 (Y0 U Y1 V per two pixels) to `0x00RRGGBB`, limited range, integer fixed point with 8 fractional bits, round
/// half up, clamped to 0..=255. Sides that cannot be a frame (`lucina_uvc::rules::yuy2_len`: odd `w`, 0, or too large) or a `src`
/// shorter than `w*h*2` just clear `out`; bytes past `w*h*2` are ignored.
pub fn yuy2_to_xrgb(src: &[u8], w: usize, h: usize, m: Matrix, out: &mut Vec<u32>) {
    out.clear();
    let len = lucina_uvc::rules::yuy2_len(w, h);
    if len == 0 || src.len() < len {
        return;
    }
    let src = &src[..len];
    // The coefficients of E (V-128) and D (U-128) per matrix; C is always 298*(Y-16).
    let (ke, kd_g, ke_g, kd_b) = match m {
        Matrix::Bt601 => (409i32, -100, -208, 516),
        Matrix::Bt709 => (459, -55, -136, 541),
    };
    let clamp = |v: i32| -> u32 { v.clamp(0, 255) as u32 };
    out.reserve(w * h);
    for pair in src.as_chunks::<4>().0 {
        let (u, v) = (i32::from(pair[1]), i32::from(pair[3]));
        // Each pixel of the pair has its own Y but shares U and V.
        for &y in &[pair[0], pair[2]] {
            let c = i32::from(y) - 16;
            let d = u - 128;
            let e = v - 128;
            let r = clamp((298 * c + ke * e + 128) >> 8);
            let g = clamp((298 * c + kd_g * d + ke_g * e + 128) >> 8);
            let b = clamp((298 * c + kd_b * d + 128) >> 8);
            out.push((r << 16) | (g << 8) | b);
        }
    }
}

// The four standard Huffman tables of ITU-T T.81 Annex K.3, which UVC MJPEG streams leave out (no DHT segment).
// Each table is its class/id byte, the 16 code-length counts, and the symbols in order of increasing code.

/// T.81 K.3, table K.3: luminance DC (class 0, id 0).
const STD_LUMA_DC: ([u8; 16], [u8; 12]) = (
    [0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0],
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
);
/// T.81 K.3, table K.4: chrominance DC (class 0, id 1).
const STD_CHROMA_DC: ([u8; 16], [u8; 12]) = (
    [0, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0],
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
);
/// T.81 K.3, table K.5: luminance AC (class 1, id 0).
const STD_LUMA_AC: ([u8; 16], [u8; 162]) = (
    [0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 0x7d],
    [
        0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61,
        0x07, 0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xa1, 0x08, 0x23, 0x42, 0xb1, 0xc1, 0x15, 0x52,
        0xd1, 0xf0, 0x24, 0x33, 0x62, 0x72, 0x82, 0x09, 0x0a, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x25,
        0x26, 0x27, 0x28, 0x29, 0x2a, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44, 0x45,
        0x46, 0x47, 0x48, 0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63, 0x64,
        0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x83,
        0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99,
        0x9a, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6,
        0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3,
        0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda, 0xe1, 0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8,
        0xe9, 0xea, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa,
    ],
);
/// T.81 K.3, table K.6: chrominance AC (class 1, id 1).
const STD_CHROMA_AC: ([u8; 16], [u8; 162]) = (
    [0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 0x77],
    [
        0x00, 0x01, 0x02, 0x03, 0x11, 0x04, 0x05, 0x21, 0x31, 0x06, 0x12, 0x41, 0x51, 0x07, 0x61,
        0x71, 0x13, 0x22, 0x32, 0x81, 0x08, 0x14, 0x42, 0x91, 0xa1, 0xb1, 0xc1, 0x09, 0x23, 0x33,
        0x52, 0xf0, 0x15, 0x62, 0x72, 0xd1, 0x0a, 0x16, 0x24, 0x34, 0xe1, 0x25, 0xf1, 0x17, 0x18,
        0x19, 0x1a, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44,
        0x45, 0x46, 0x47, 0x48, 0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63,
        0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a,
        0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97,
        0x98, 0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4,
        0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca,
        0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda, 0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7,
        0xe8, 0xe9, 0xea, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa,
    ],
);

/// One DHT segment (marker 0xFFC4) holding the four standard tables: classes/ids 0x00, 0x01, 0x10, 0x11.
fn standard_dht_segment() -> Vec<u8> {
    let tables: [(u8, &[u8], &[u8]); 4] = [
        (0x00, &STD_LUMA_DC.0, &STD_LUMA_DC.1),
        (0x01, &STD_CHROMA_DC.0, &STD_CHROMA_DC.1),
        (0x10, &STD_LUMA_AC.0, &STD_LUMA_AC.1),
        (0x11, &STD_CHROMA_AC.0, &STD_CHROMA_AC.1),
    ];
    let mut seg = vec![0xFF, 0xC4];
    // The length counts itself and every table's id byte, 16 counts and symbols.
    let len = 2 + tables
        .iter()
        .map(|(_, bits, vals)| 1 + bits.len() + vals.len())
        .sum::<usize>();
    seg.extend_from_slice(&(len as u16).to_be_bytes());
    for (id, bits, vals) in tables {
        seg.push(id);
        seg.extend_from_slice(bits);
        seg.extend_from_slice(vals);
    }
    seg
}

/// UVC MJPEG frames often carry no DHT segment, which a plain JPEG decoder refuses. If `jpeg` has no DHT (0xFFC4)
/// before its first SOS (0xFFDA), this returns a copy with the four standard tables of ITU-T T.81 Annex K.3 inserted
/// right before the SOS; otherwise (or if it is not a JPEG at all) it returns `jpeg` borrowed. Markers are walked by
/// their lengths from the SOI.
pub fn ensure_huffman_tables(jpeg: &[u8]) -> Cow<'_, [u8]> {
    if jpeg.len() < 2 || jpeg[0] != 0xFF || jpeg[1] != 0xD8 {
        return Cow::Borrowed(jpeg);
    }
    let mut pos = 2;
    while pos < jpeg.len() {
        if jpeg[pos] != 0xFF {
            return Cow::Borrowed(jpeg); // not a marker where one should be; leave the stream alone
        }
        let mut m = pos + 1;
        while m < jpeg.len() && jpeg[m] == 0xFF {
            m += 1; // fill bytes before the marker
        }
        if m >= jpeg.len() {
            return Cow::Borrowed(jpeg);
        }
        match jpeg[m] {
            0x00 => return Cow::Borrowed(jpeg), // a stuffed byte, not a marker
            0x01 | 0xD0..=0xD8 => pos = m + 1,  // standalone markers, no length
            0xD9 => return Cow::Borrowed(jpeg), // EOI before any SOS: nothing to fix
            0xC4 => return Cow::Borrowed(jpeg), // already has a DHT
            0xDA => {
                // First SOS and no DHT before it: insert the standard tables right before this marker.
                let dht = standard_dht_segment();
                let mut out = Vec::with_capacity(jpeg.len() + dht.len());
                out.extend_from_slice(&jpeg[..pos]);
                out.extend_from_slice(&dht);
                out.extend_from_slice(&jpeg[pos..]);
                return Cow::Owned(out);
            }
            _ => {
                // Any other marker starts a segment whose two length bytes count the whole segment.
                if m + 3 > jpeg.len() {
                    return Cow::Borrowed(jpeg);
                }
                let len = u16::from_be_bytes([jpeg[m + 1], jpeg[m + 2]]) as usize;
                if len < 2 {
                    return Cow::Borrowed(jpeg);
                }
                pos = m + 1 + len;
            }
        }
    }
    Cow::Borrowed(jpeg)
}

/// Decodes a JPEG (putting the standard Huffman tables in first if it has none) to `0x00RRGGBB` in `out`.
/// Returns the (width, height).
pub fn jpeg_to_xrgb(jpeg: &[u8], out: &mut Vec<u32>) -> Result<(usize, usize), String> {
    use zune_jpeg::JpegDecoder;
    use zune_jpeg::zune_core::bytestream::ZCursor;
    use zune_jpeg::zune_core::colorspace::ColorSpace;
    use zune_jpeg::zune_core::options::DecoderOptions;

    let jpeg = ensure_huffman_tables(jpeg);
    let opts = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGB);
    let mut d = JpegDecoder::new_with_options(ZCursor::new(&jpeg[..]), opts);
    d.decode_headers().map_err(|e| e.to_string())?;
    let (w, h) = d
        .dimensions()
        .ok_or_else(|| String::from("the JPEG has no dimensions"))?;
    let rgb = d.decode().map_err(|e| e.to_string())?;
    out.clear();
    out.reserve(w * h);
    for p in rgb.as_chunks::<3>().0 {
        out.push((u32::from(p[0]) << 16) | (u32::from(p[1]) << 8) | u32::from(p[2]));
    }
    Ok((w, h))
}

/// A picture in I420: full-resolution luma, then Cb and Cr at half width and half height (rounded up), each plane tightly packed
/// (line length = width for `y`, `width.div_ceil(2)` for `u` and `v`). Full range (JPEG/JFIF), BT.601 matrix.
#[derive(Debug, Default, Clone)]
pub struct I420 {
    pub width: usize,
    pub height: usize,
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
    /// zune-jpeg's interleaved YCbCr output, kept between calls so that no frame allocates.
    scratch: Vec<u8>,
}

/// Decodes a JPEG (UVC MJPEG: standard Huffman tables put in when missing) into `out` without any colour conversion:
/// zune-jpeg with output colour space YCbCr, then luma copied and each 2x2 block of chroma averaged with rounding
/// ((a + b + c + d + 2) >> 2). At an odd right or bottom edge the missing samples repeat the last column/row.
/// Returns (width, height). Errors (as String) if the JPEG does not decode or is not 3-component YCbCr.
pub fn jpeg_to_i420(jpeg: &[u8], out: &mut I420) -> Result<(usize, usize), String> {
    use zune_jpeg::JpegDecoder;
    use zune_jpeg::zune_core::bytestream::ZCursor;
    use zune_jpeg::zune_core::colorspace::ColorSpace;
    use zune_jpeg::zune_core::options::DecoderOptions;

    let jpeg = ensure_huffman_tables(jpeg);
    let opts = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::YCbCr);
    let mut d = JpegDecoder::new_with_options(ZCursor::new(&jpeg[..]), opts);
    d.decode_headers().map_err(|e| e.to_string())?;
    let (w, h) = d
        .dimensions()
        .ok_or_else(|| String::from("the JPEG has no dimensions"))?;
    let info = d
        .info()
        .ok_or_else(|| String::from("the JPEG has no info"))?;
    if d.input_colorspace() != Some(ColorSpace::YCbCr) || info.components != 3 {
        return Err(String::from("the JPEG is not 3-component YCbCr"));
    }
    // The plane sizes from the Verus-checked rule: no product overflows, and a size no frame can have is refused here.
    let planes = lucina_uvc::rules::i420_size(w, h);
    if !planes.ok {
        return Err(format!("a {w}x{h} JPEG is no picture this preview shows"));
    }
    let size = d
        .output_buffer_size()
        .ok_or_else(|| String::from("the JPEG output size overflows"))?;
    // decode_into writes through the existing allocation, so a steady-size stream allocates nothing per frame.
    if out.scratch.len() != size {
        out.scratch.resize(size, 0);
    }
    d.decode_into(&mut out.scratch[..size])
        .map_err(|e| e.to_string())?;

    out.width = w;
    out.height = h;
    let cw = w.div_ceil(2);
    out.y.resize(planes.y, 0);
    out.u.resize(planes.c, 0);
    out.v.resize(planes.c, 0);

    // zune's YCbCr output is interleaved Y, Cb, Cr at full width and height (it upsamples the chroma to that), so the
    // luma plane is every third byte and each output chroma sample is the 2x2 average of the source chroma.
    let stride = w * 3;
    for (yrow, src) in out.y.chunks_mut(w).zip(out.scratch.chunks_exact(stride)) {
        for (d, p) in yrow.iter_mut().zip(src.as_chunks::<3>().0) {
            *d = p[0];
        }
    }
    let full_cols = w / 2;
    for (cy, (urow, vrow)) in out.u.chunks_mut(cw).zip(out.v.chunks_mut(cw)).enumerate() {
        let sy = 2 * cy;
        let top = &out.scratch[sy * stride..][..stride];
        // An odd height has no second row: repeating the last row is the same as using it twice.
        let bottom = if sy + 1 < h {
            &out.scratch[(sy + 1) * stride..][..stride]
        } else {
            top
        };
        // Whole 2x2 blocks, two source pixels (six bytes) at a time; no branches or divisions in this loop.
        for (((t, b), uo), vo) in top
            .as_chunks::<6>()
            .0
            .iter()
            .zip(bottom.as_chunks::<6>().0.iter())
            .zip(urow.iter_mut())
            .zip(vrow.iter_mut())
        {
            *uo = ((u16::from(t[1]) + u16::from(t[4]) + u16::from(b[1]) + u16::from(b[4]) + 2) >> 2)
                as u8;
            *vo = ((u16::from(t[2]) + u16::from(t[5]) + u16::from(b[2]) + u16::from(b[5]) + 2) >> 2)
                as u8;
        }
        // An odd width leaves a last chroma column whose second sample repeats the last one.
        if w & 1 == 1 {
            let last = (w - 1) * 3;
            urow[full_cols] =
                ((u16::from(top[last + 1]) * 2 + u16::from(bottom[last + 1]) * 2 + 2) >> 2) as u8;
            vrow[full_cols] =
                ((u16::from(top[last + 2]) * 2 + u16::from(bottom[last + 2]) * 2 + 2) >> 2) as u8;
        }
    }
    Ok((w, h))
}

/// The largest `sw`x`sh` with the source's aspect ratio that fits inside the `bw`x`bh` box, centred: (x, y, w, h).
/// Never zero-sized unless the box (or the source) is.
pub fn fit(sw: usize, sh: usize, bw: usize, bh: usize) -> (usize, usize, usize, usize) {
    if sw == 0 || sh == 0 || bw == 0 || bh == 0 {
        return (0, 0, 0, 0);
    }
    // w/h = sw/sh with w <= bw and h <= bh, in integers so there is no rounding drift.
    let w = (bw as u128).min(sw as u128 * bh as u128 / sh as u128) as usize;
    let h = (w as u128 * sh as u128 / sw as u128) as usize;
    ((bw - w) / 2, (bh - h) / 2, w, h)
}

/// Bilinear scale of `sw`x`sh` `0x00RRGGBB` pixels to `dw`x`dh`, with the edge pixels clamped. A zero `dw`, `dh`,
/// `sw` or `sh`, or a `src` shorter than `sw*sh`, gives an empty `out`; the same size is a copy.
///
/// Destination pixel (x, y) samples the source at fx = x*sw/dw, fy = y*sh/dh in 16.16 fixed point (no half-pixel
/// offset), exactly like the reference implementation: the source columns and 8-bit x weights are computed once per
/// call into tables of length `dw` (one division per column, not per pixel), the source rows and 8-bit y weight once
/// per row, and each axis is then lerped on packed u32 channels (see [`lerp_px`]). Every output channel stays
/// within 2 of the reference's.
pub fn scale_xrgb(src: &[u32], sw: usize, sh: usize, dw: usize, dh: usize, out: &mut Vec<u32>) {
    out.clear();
    if dw == 0 || dh == 0 || sw == 0 || sh == 0 || src.len() < sw * sh {
        return;
    }
    if (dw, dh) == (sw, sh) {
        out.extend_from_slice(&src[..sw * sh]);
        return;
    }
    // Source columns and 8-bit x weight of every destination column: one division per column, not per pixel.
    // The weight is the fraction rounded to 8 bits, so it ranges 0..=256 (256 = all of the next pixel).
    let mut x0 = vec![0usize; dw];
    let mut x1 = vec![0usize; dw];
    let mut wx = vec![0u32; dw];
    for x in 0..dw {
        let fx = ((x as u128 * sw as u128) << 16) / dw as u128;
        let x0x = (fx >> 16) as usize;
        x0[x] = x0x;
        x1[x] = (x0x + 1).min(sw - 1);
        wx[x] = (((fx & 0xFFFF) + 128) >> 8) as u32;
    }
    // The two horizontally lerped source rows of the destination row being written.
    let mut h0 = vec![0u32; dw];
    let mut h1 = vec![0u32; dw];
    out.resize(dw * dh, 0);
    for y in 0..dh {
        // Source rows and 8-bit y weight of this destination row: one division per row.
        let fy = ((y as u128 * sh as u128) << 16) / dh as u128;
        let y0 = (fy >> 16) as usize;
        let y1 = (y0 + 1).min(sh - 1);
        let wy = (((fy & 0xFFFF) + 128) >> 8) as u32;
        let row0 = &src[y0 * sw..][..sw];
        let row1 = &src[y1 * sw..][..sw];
        // Horizontal lerp of both source rows.
        for ((o0, o1), ((&xa, &xb), &t)) in
            h0.iter_mut().zip(&mut h1).zip(x0.iter().zip(&x1).zip(&wx))
        {
            *o0 = lerp_px(row0[xa], row0[xb], t);
            *o1 = lerp_px(row1[xa], row1[xb], t);
        }
        // Vertical lerp into the destination row; when the weight makes it the identity, just copy.
        let dst = &mut out[y * dw..][..dw];
        if wy == 0 || y0 == y1 {
            dst.copy_from_slice(&h0);
        } else if wy == 256 {
            dst.copy_from_slice(&h1);
        } else {
            for (d, (&a, &b)) in dst.iter_mut().zip(h0.iter().zip(&h1)) {
                *d = lerp_px(a, b, wy);
            }
        }
    }
}

/// Lerp of two `0x00RRGGBB` pixels with the 8-bit weight `t`: `((a*(256-t) + b*t) >> 8) & mask`, red+blue packed
/// with mask `0x00FF00FF` and green with `0x0000FF00`. Both operands are masked first, so each product is at most
/// `0x00FF00FF * 256 = 0xFF00FF00` and their sum at most `max(a, b) * 256`: nothing overflows a u32, and the
/// mask after the shift keeps every channel in its own byte.
#[inline]
fn lerp_px(a: u32, b: u32, t: u32) -> u32 {
    const RB: u32 = 0x00FF00FF;
    const G: u32 = 0x0000FF00;
    let (ar, br) = (a & RB, b & RB);
    let (ag, bg) = (a & G, b & G);
    (((ar * (256 - t) + br * t) >> 8) & RB) | (((ag * (256 - t) + bg * t) >> 8) & G)
}

/// Bilinear scale of `sw`x`sh` `0x00RRGGBB` pixels to `dw`x`dh`, channel by channel in 16.16 fixed point, with the
/// edge pixels clamped. A zero `dw` or `dh` gives an empty `out`; the same size is a copy. The straightforward
/// per-pixel version, kept as the reference the tests compare the fast [`scale_xrgb`] against.
#[cfg(test)]
pub(crate) fn scale_xrgb_reference(
    src: &[u32],
    sw: usize,
    sh: usize,
    dw: usize,
    dh: usize,
    out: &mut Vec<u32>,
) {
    out.clear();
    if dw == 0 || dh == 0 || sw == 0 || sh == 0 || src.len() < sw * sh {
        return;
    }
    if (dw, dh) == (sw, sh) {
        out.extend_from_slice(&src[..sw * sh]);
        return;
    }
    out.reserve(dw * dh);
    for y in 0..dh {
        // Source position of this destination pixel, 16.16 fixed point.
        let fy = ((y as u128 * sh as u128) << 16) / dh as u128;
        let y0 = (fy >> 16) as usize;
        let y1 = (y0 + 1).min(sh - 1);
        let ay = (fy & 0xFFFF) as u64;
        for x in 0..dw {
            let fx = ((x as u128 * sw as u128) << 16) / dw as u128;
            let x0 = (fx >> 16) as usize;
            let x1 = (x0 + 1).min(sw - 1);
            let ax = (fx & 0xFFFF) as u64;
            let (p00, p10, p01, p11) = (
                src[y0 * sw + x0],
                src[y0 * sw + x1],
                src[y1 * sw + x0],
                src[y1 * sw + x1],
            );
            // Bilinear weights; each channel sum fits in 40 bits, so u64 cannot overflow.
            let w00 = (0x1_0000 - ay) * (0x1_0000 - ax);
            let w10 = (0x1_0000 - ay) * ax;
            let w01 = ay * (0x1_0000 - ax);
            let w11 = ay * ax;
            let mut px = 0u32;
            for shift in [16u32, 8, 0] {
                let c = |p: u32| u64::from((p >> shift) & 0xFF);
                let v = (c(p00) * w00 + c(p10) * w10 + c(p01) * w01 + c(p11) * w11) >> 32;
                px |= (v as u32) << shift;
            }
            out.push(px);
        }
    }
}
