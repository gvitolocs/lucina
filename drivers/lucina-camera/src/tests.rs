use std::borrow::Cow;
use std::io::ErrorKind;

use crate::convert::{self, Matrix};
use crate::photo;
use crate::{Header, Info, StreamRequest};

/// The `/proc/lucina-camera` of the laptop's Chicony camera while it streams.
const SAMPLE: &str = "camera: Chicony Electronics Co.,Ltd. 720p HD Camera (04f2:b72f) at dwc3@a600000/dev3\n\
uvc: 1.0, clock 15000000 Hz, control interface 0, streaming interface 1, endpoint 0x81\n\
format 1 MJPG: 1280x720@30.00/15.00 640x480@30.00 (colour 1/1/4)\n\
format 2 YUY2: 640x480@30.00/15.00/10.00 1280x720@10.00 320x240@30.00-5.00 (colour 1/1/4)\n\
alternate settings (bytes per microframe): 1:128 2:1024 3:3072\n\
state: streaming MJPG 1280x720 at 30.00 fps (alternate setting 3, 3072 bytes x 32 packets)\n\
frames: 120 (0 with errors), published 120; packets 3840, lost 0, invalid 0, out of sync 0\n\
power: TLMM 13 high\n\
capturing: yes, pid 412 (lucina-app-came)\n";

#[test]
fn parse_info_reads_the_sample() {
    let info = crate::parse_info(SAMPLE);
    assert_eq!(
        info.name.as_deref(),
        Some("Chicony Electronics Co.,Ltd. 720p HD Camera (04f2:b72f) at dwc3@a600000/dev3")
    );
    let offers: Vec<(&[u8; 4], u16, u16, Vec<f32>)> = info
        .offers
        .iter()
        .map(|o| (&o.fourcc, o.width, o.height, o.fps.clone()))
        .collect();
    assert_eq!(
        offers,
        vec![
            (b"MJPG", 1280, 720, vec![30.0, 15.0]),
            (b"MJPG", 640, 480, vec![30.0]),
            (b"YUY2", 640, 480, vec![30.0, 15.0, 10.0]),
            (b"YUY2", 1280, 720, vec![10.0]),
            // A rate written a-b is a continuous range; both ends are rates.
            (b"YUY2", 320, 240, vec![30.0, 5.0]),
        ]
    );
    // Colour 1/1/4: matrix 4 is UVC's default, BT.601.
    assert_eq!(
        info.matrix_by_fourcc,
        vec![(*b"MJPG", Matrix::Bt601), (*b"YUY2", Matrix::Bt601)]
    );
    assert_eq!(
        info.state,
        "streaming MJPG 1280x720 at 30.00 fps (alternate setting 3, 3072 bytes x 32 packets)"
    );
    assert_eq!(info.capturing, Some((412, String::from("lucina-app-came"))));
}

#[test]
fn parse_info_with_no_camera() {
    let info = crate::parse_info("camera: none\nstate: idle\ncapturing: no\n");
    assert_eq!(info.name, None);
    assert!(info.offers.is_empty());
    assert_eq!(info.state, "idle");
    assert_eq!(info.capturing, None);
}

#[test]
fn parse_info_never_panics_on_garbage() {
    for text in [
        "",
        "garbage",
        "\0\u{ff}\u{fe} binary \x00",
        "camera:",
        "camera: none extra",
        "format",
        "format x",
        "format 1",
        "format 1 MJPG",
        "format 1 MJPG:",
        "format 1 MJPG: nonsense",
        "format 1 MJPG: 1280x720@ (colour 1/1/4)",
        "format 1 MJPG: 1280x720@abc (colour x/y/z)",
        "format 1 MJPG: 99999x99999@30.00 (colour 1/1/4)",
        "capturing: yes",
        "capturing: yes, pid",
        "capturing: yes, pid abc (name)",
        "capturing: yes, pid 5",
        "state:",
        ":",
        "::",
        "format 1 MJPG: 640x480@30.00-5.00-1.00 (colour 1/1/1)",
    ] {
        let _ = crate::parse_info(text);
    }
    // A BT.709 matrix (1) is recognised.
    let info = crate::parse_info("format 1 YUY2: 640x480@30.00 (colour 1/1/1)\n");
    assert_eq!(info.matrix_by_fourcc, vec![(*b"YUY2", Matrix::Bt709)]);
}

#[test]
fn choose_preview_picks_mjpg_1280x720_at_30() {
    assert_eq!(crate::interval_for_fps(30.0), 333_333);
    assert_eq!(crate::interval_for_fps(15.0), 666_667);
    let info = crate::parse_info(SAMPLE);
    let r = crate::choose_preview(&info).unwrap();
    assert_eq!(
        r,
        StreamRequest {
            fourcc: *b"MJPG",
            width: 1280,
            height: 720,
            interval: 333_333
        }
    );
}

#[test]
fn choose_preview_falls_back_to_yuy2_640x480() {
    let info = crate::parse_info("format 2 YUY2: 640x480@15.00 1280x720@10.00 (colour 1/1/4)\n");
    let r = crate::choose_preview(&info).unwrap();
    assert_eq!(
        r,
        StreamRequest {
            fourcc: *b"YUY2",
            width: 640,
            height: 480,
            interval: 666_667
        }
    );
    // No offers at all: no preview.
    assert_eq!(crate::choose_preview(&Info::default()), None);
}

/// One YUY2 pair (two pixels) as one pixel value, for the colour checks.
fn yuy2_pixel(y: u8, u: u8, v: u8, m: Matrix) -> u32 {
    let mut out = Vec::new();
    convert::yuy2_to_xrgb(&[y, u, y, v], 2, 1, m, &mut out);
    out[0]
}

fn assert_near(px: u32, want: (u8, u8, u8)) {
    for (shift, w) in [(16u32, want.0), (8, want.1), (0, want.2)] {
        let got = (px >> shift) & 0xFF;
        assert!(
            (i32::from(got as u8) - i32::from(w)).abs() <= 2,
            "pixel {px:#08x}, channel at shift {shift}: got {got}, want {w}"
        );
    }
}

#[test]
fn yuy2_black_white_and_colours() {
    let mut out = Vec::new();
    // Limited range: Y=16 is black, Y=235 is white, U=V=128 is no chroma.
    convert::yuy2_to_xrgb(&[16, 128, 16, 128], 2, 1, Matrix::Bt601, &mut out);
    assert_eq!(out, vec![0x000000, 0x000000]);
    convert::yuy2_to_xrgb(&[235, 128, 235, 128], 2, 1, Matrix::Bt601, &mut out);
    assert_eq!(out, vec![0xFFFFFF, 0xFFFFFF]);
    // BT.601 primaries.
    assert_near(yuy2_pixel(81, 90, 240, Matrix::Bt601), (255, 0, 0));
    assert_near(yuy2_pixel(145, 54, 34, Matrix::Bt601), (0, 255, 0));
    assert_near(yuy2_pixel(41, 240, 110, Matrix::Bt601), (0, 0, 255));
}

#[test]
fn yuy2_each_pixel_of_a_pair_has_its_own_y() {
    // 4x2: four pairs, eight pixels with different Y but shared U=V=128.
    let ys = [16u8, 235, 100, 200, 16, 235, 100, 200];
    let mut src = Vec::new();
    for pair in ys.as_chunks::<2>().0 {
        src.extend_from_slice(&[pair[0], 128, pair[1], 128]);
    }
    let mut out = Vec::new();
    convert::yuy2_to_xrgb(&src, 4, 2, Matrix::Bt601, &mut out);
    // Gray level of a Y with no chroma: clamp((298*(Y-16)+128)>>8).
    let gray = |y: u8| {
        let v = (298 * (i32::from(y) - 16) + 128) >> 8;
        let v = v.clamp(0, 255) as u32;
        (v << 16) | (v << 8) | v
    };
    let want: Vec<u32> = ys.iter().map(|&y| gray(y)).collect();
    assert_eq!(out, want);
    assert_eq!(out.len(), 8);
    // An odd width, or a src shorter than w*h*2, just clears out.
    convert::yuy2_to_xrgb(&[0u8; 16], 3, 2, Matrix::Bt601, &mut out);
    assert!(out.is_empty());
    convert::yuy2_to_xrgb(&[0u8; 15], 4, 2, Matrix::Bt601, &mut out);
    assert!(out.is_empty());
}

/// Encodes a 64x48 colour gradient with the `image` crate's JPEG encoder (quality 90); returns (jpeg, rgb bytes).
fn encode_gradient() -> (Vec<u8>, Vec<u8>) {
    use image::ImageEncoder;
    let (w, h) = (64u32, 48u32);
    let mut rgb = Vec::with_capacity((w * h * 3) as usize);
    for y in 0..h {
        for x in 0..w {
            rgb.extend_from_slice(&[(x * 4) as u8, (y * 5) as u8, ((x + y) * 2) as u8]);
        }
    }
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 90)
        .write_image(&rgb, w, h, image::ExtendedColorType::Rgb8)
        .unwrap();
    (jpeg, rgb)
}

/// Walks the markers and copies everything but the DHT (0xFFC4) segments, stopping at the first SOS.
fn strip_dht(jpeg: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&jpeg[..2]); // SOI
    let mut pos = 2;
    while pos + 4 <= jpeg.len() {
        assert_eq!(jpeg[pos], 0xFF, "expected a marker at {pos}");
        let marker = jpeg[pos + 1];
        if marker == 0xDA {
            out.extend_from_slice(&jpeg[pos..]);
            return out;
        }
        let len = u16::from_be_bytes([jpeg[pos + 2], jpeg[pos + 3]]) as usize;
        if marker != 0xC4 {
            out.extend_from_slice(&jpeg[pos..pos + 2 + len]);
        }
        pos += 2 + len;
    }
    out
}

#[test]
fn jpeg_decodes_and_gets_standard_tables_put_in() {
    let (jpeg, rgb) = encode_gradient();
    // The encoded JPEG decodes to the gradient within a mean absolute error of 6 per channel.
    let mut px = Vec::new();
    let (w, h) = convert::jpeg_to_xrgb(&jpeg, &mut px).unwrap();
    assert_eq!((w, h), (64, 48));
    assert_eq!(px.len(), 64 * 48);
    let mut sum = [0u64; 3];
    for (i, p) in px.iter().enumerate() {
        for c in 0..3 {
            let got = (p >> (16 - 8 * c)) & 0xFF;
            sum[c] += (i64::from(got as u8) - i64::from(rgb[i * 3 + c])).unsigned_abs();
        }
    }
    for (c, s) in sum.iter().enumerate() {
        let mae = *s as f64 / (64.0 * 48.0);
        assert!(mae <= 6.0, "channel {c} mean absolute error {mae}");
    }
    // A JPEG with every DHT stripped: zune-jpeg alone cannot read it, and ensure_huffman_tables puts tables in.
    let stripped = strip_dht(&jpeg);
    assert!(stripped.len() < jpeg.len());
    let owned = matches!(convert::ensure_huffman_tables(&stripped), Cow::Owned(_));
    let zune_alone_fails = {
        use zune_jpeg::JpegDecoder;
        use zune_jpeg::zune_core::bytestream::ZCursor;
        let mut d = JpegDecoder::new(ZCursor::new(&stripped[..]));
        match d.decode_headers() {
            Ok(()) => d.decode().is_err(),
            Err(_) => true,
        }
    };
    assert!(
        zune_alone_fails || owned,
        "neither zune-jpeg alone nor ensure_huffman_tables handles the stripped JPEG"
    );
    assert!(
        owned,
        "ensure_huffman_tables should insert the standard tables into a DHT-less JPEG"
    );
    // With the tables put back, the pixels are exactly those of the original.
    let mut a = Vec::new();
    let mut b = Vec::new();
    convert::jpeg_to_xrgb(&jpeg, &mut a).unwrap();
    convert::jpeg_to_xrgb(&stripped, &mut b).unwrap();
    assert_eq!(a, b);
    // A stream that is not a JPEG is returned borrowed, and a JPEG that already has a DHT too.
    assert!(matches!(
        convert::ensure_huffman_tables(b"not a jpeg"),
        Cow::Borrowed(_)
    ));
    assert!(matches!(
        convert::ensure_huffman_tables(&jpeg),
        Cow::Borrowed(_)
    ));
}

/// Encodes a `w`x`h` smooth colour gradient with the `image` crate's JPEG encoder (quality 90).
fn encode_smooth(w: u32, h: u32) -> Vec<u8> {
    use image::ImageEncoder;
    let mut rgb = Vec::with_capacity((w * h * 3) as usize);
    for y in 0..h {
        for x in 0..w {
            // Gentle ramps, so that the 2x2 chroma averaging barely changes an already smooth picture.
            rgb.extend_from_slice(&[
                (x * 64 / w) as u8,
                (y * 64 / h) as u8,
                ((x + y) * 64 / (w + h)) as u8,
            ]);
        }
    }
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 90)
        .write_image(&rgb, w, h, image::ExtendedColorType::Rgb8)
        .unwrap();
    jpeg
}

/// The RGB of an I420 pixel with full-range BT.601, its chroma sampled at (x/2, y/2).
fn i420_rgb(img: &convert::I420, x: usize, y: usize) -> (f64, f64, f64) {
    let yy = f64::from(img.y[y * img.width + x]);
    let ci = (y / 2) * img.width.div_ceil(2) + x / 2;
    let cb = f64::from(img.u[ci]) - 128.0;
    let cr = f64::from(img.v[ci]) - 128.0;
    (
        (yy + 1.402 * cr).clamp(0.0, 255.0),
        (yy - 0.344_136 * cb - 0.714_136 * cr).clamp(0.0, 255.0),
        (yy + 1.772 * cb).clamp(0.0, 255.0),
    )
}

/// Asserts that every I420 pixel converted to RGB is close to the xrgb decode of the same JPEG: per-channel mean
/// absolute difference below 3 and maximum below 24, which allows for the different chroma resampling.
fn assert_i420_matches_xrgb(img: &convert::I420, jpeg: &[u8]) {
    let mut px = Vec::new();
    let (w, h) = convert::jpeg_to_xrgb(jpeg, &mut px).unwrap();
    assert_eq!((w, h), (img.width, img.height));
    let mut sum = [0f64; 3];
    let mut max = [0f64; 3];
    for y in 0..h {
        for x in 0..w {
            let got = i420_rgb(img, x, y);
            let p = px[y * w + x];
            for c in 0..3 {
                let want = f64::from(((p >> (16 - 8 * c)) & 0xFF) as u8);
                let got = [got.0, got.1, got.2][c];
                let diff = (got - want).abs();
                sum[c] += diff;
                max[c] = max[c].max(diff);
            }
        }
    }
    for c in 0..3 {
        let mean = sum[c] / (w * h) as f64;
        assert!(
            mean < 3.0,
            "channel {c} mean absolute difference {mean} is not below 3"
        );
        assert!(
            max[c] < 24.0,
            "channel {c} maximum difference {} is not below 24",
            max[c]
        );
    }
}

#[test]
fn jpeg_to_i420_matches_the_rgb_decode() {
    let (jpeg, _rgb) = encode_gradient();
    let mut img = convert::I420::default();
    let (w, h) = convert::jpeg_to_i420(&jpeg, &mut img).unwrap();
    assert_eq!((w, h), (64, 48));
    assert_eq!((img.width, img.height), (64, 48));
    assert_eq!(img.y.len(), 64 * 48);
    assert_eq!(img.u.len(), 32 * 24);
    assert_eq!(img.v.len(), 32 * 24);
    assert_i420_matches_xrgb(&img, &jpeg);
}

#[test]
fn jpeg_to_i420_odd_size_and_no_dht() {
    let jpeg = encode_smooth(33, 17);
    let stripped = strip_dht(&jpeg);
    assert!(stripped.len() < jpeg.len());
    let mut img = convert::I420::default();
    let (w, h) = convert::jpeg_to_i420(&stripped, &mut img).unwrap();
    assert_eq!((w, h), (33, 17));
    assert_eq!(img.y.len(), 33 * 17);
    assert_eq!(img.u.len(), 17 * 9);
    assert_eq!(img.v.len(), 17 * 9);
    assert_i420_matches_xrgb(&img, &stripped);
}

#[test]
fn jpeg_to_i420_reuses_its_buffers() {
    let (jpeg, _rgb) = encode_gradient();
    let mut img = convert::I420::default();
    convert::jpeg_to_i420(&jpeg, &mut img).unwrap();
    let first = img.clone();
    let ptrs = [img.y.as_ptr(), img.u.as_ptr(), img.v.as_ptr()];
    let caps = [img.y.capacity(), img.u.capacity(), img.v.capacity()];
    convert::jpeg_to_i420(&jpeg, &mut img).unwrap();
    assert_eq!([img.y.as_ptr(), img.u.as_ptr(), img.v.as_ptr()], ptrs);
    assert_eq!([img.y.capacity(), img.u.capacity(), img.v.capacity()], caps);
    assert_eq!(img.y, first.y);
    assert_eq!(img.u, first.u);
    assert_eq!(img.v, first.v);
}

#[test]
fn jpeg_to_i420_rejects_garbage() {
    let mut img = convert::I420::default();
    assert!(convert::jpeg_to_i420(&[0xFF, 0xD8, 1, 2, 3], &mut img).is_err());
    assert!(convert::jpeg_to_i420(&[], &mut img).is_err());
}

#[test]
fn fit_centres_the_largest_similar_size() {
    assert_eq!(convert::fit(1280, 720, 1920, 1080), (0, 0, 1920, 1080));
    assert_eq!(convert::fit(640, 480, 1920, 1080), (240, 0, 1440, 1080));
    assert_eq!(convert::fit(1920, 1080, 1280, 720), (0, 0, 1280, 720));
    assert_eq!(convert::fit(100, 100, 0, 50), (0, 0, 0, 0));
}

#[test]
fn scale_xrgb_bilinear() {
    // 2x2 black/white checker to 4x4: the corners keep their source pixels.
    let checker = vec![0x000000, 0xFFFFFF, 0xFFFFFF, 0x000000];
    let mut out = Vec::new();
    convert::scale_xrgb(&checker, 2, 2, 4, 4, &mut out);
    assert_eq!(out.len(), 16);
    assert_eq!(out[0], 0x000000);
    assert_eq!(out[3], 0xFFFFFF);
    assert_eq!(out[12], 0xFFFFFF);
    assert_eq!(out[15], 0x000000);
    // The middle is the average of all four (127.5 rounds down in the fixed-point sum).
    assert_eq!(out[5], 0x7F7F7F);
    // Same size is a copy.
    convert::scale_xrgb(&checker, 2, 2, 2, 2, &mut out);
    assert_eq!(out, checker);
    // Zero width gives an empty out.
    convert::scale_xrgb(&checker, 2, 2, 0, 4, &mut out);
    assert!(out.is_empty());
    convert::scale_xrgb(&checker, 2, 2, 4, 0, &mut out);
    assert!(out.is_empty());
}

/// A small deterministic xorshift PRNG, so the tests need no rand crate.
struct XorShift(u64);

impl XorShift {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    /// A pseudo-random byte.
    fn byte(&mut self) -> u8 {
        (self.next() >> 24) as u8
    }
}

/// A pseudo-random `w`x`h` image of `0x00RRGGBB` pixels.
fn random_image(w: usize, h: usize, seed: u64) -> Vec<u32> {
    let mut rng = XorShift(seed | 1);
    (0..w * h)
        .map(|_| {
            let (r, g, b) = (rng.byte(), rng.byte(), rng.byte());
            (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b)
        })
        .collect()
}

/// A `w`x`h` colour gradient with a little pseudo-random noise on each channel.
fn gradient_image(w: usize, h: usize, seed: u64) -> Vec<u32> {
    let mut rng = XorShift(seed | 1);
    let mut img = Vec::with_capacity(w * h);
    for y in 0..h {
        for x in 0..w {
            let n = rng.byte();
            let r = (x * 255 / w) as u8 ^ (n & 0x0F);
            let g = (y * 255 / h) as u8 ^ (n >> 4);
            let b = ((x + y) * 255 / (w + h)) as u8 ^ (n & 0x0F);
            img.push((u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b));
        }
    }
    img
}

/// Scales `src` with both `scale_xrgb` and the reference and checks they agree: every output channel within 2 of
/// the reference's, the four corners within 1, and the top byte of every output pixel 0.
fn assert_scale_matches_reference(src: &[u32], sw: usize, sh: usize, dw: usize, dh: usize) {
    let mut got = Vec::new();
    let mut want = Vec::new();
    convert::scale_xrgb(src, sw, sh, dw, dh, &mut got);
    convert::scale_xrgb_reference(src, sw, sh, dw, dh, &mut want);
    assert_eq!(
        got.len(),
        dw * dh,
        "wrong out length for {sw}x{sh} -> {dw}x{dh}"
    );
    assert_eq!(want.len(), dw * dh);
    let corners = [(0usize, 0usize), (dw - 1, 0), (0, dh - 1), (dw - 1, dh - 1)];
    for (y, (row_g, row_w)) in got.chunks(dw).zip(want.chunks(dw)).enumerate() {
        for (x, (&g, &w)) in row_g.iter().zip(row_w).enumerate() {
            assert_eq!(
                g >> 24,
                0,
                "top byte of ({x}, {y}) for {sw}x{sh} -> {dw}x{dh} is not 0"
            );
            let tol = if corners.contains(&(x, y)) { 1 } else { 2 };
            for shift in [16u32, 8, 0] {
                let diff = (i32::from(((g >> shift) & 0xFF) as u8)
                    - i32::from(((w >> shift) & 0xFF) as u8))
                .abs();
                assert!(
                    diff <= tol,
                    "channel at shift {shift} of ({x}, {y}) for {sw}x{sh} -> {dw}x{dh}: \
                     got {g:#010x}, reference {w:#010x}, diff {diff} > {tol}"
                );
            }
        }
    }
}

#[test]
fn scale_xrgb_matches_reference() {
    let cases = [
        (2usize, 2usize, 4usize, 4usize),
        (3, 5, 7, 2),
        (17, 9, 40, 31),
        (64, 48, 1440, 1080),
        (1280, 720, 1920, 1080),
        (1920, 1080, 640, 360),
        (1, 1, 3, 3),
    ];
    for (i, &(sw, sh, dw, dh)) in cases.iter().enumerate() {
        let seed = 0x9E37_79B9_7F4A_7C15u64.wrapping_add(i as u64);
        // The 1280x720 preview frame is a gradient plus noise; the others are plain pseudo-random.
        let src = if (sw, sh) == (1280, 720) {
            gradient_image(sw, sh, seed)
        } else {
            random_image(sw, sh, seed)
        };
        assert_scale_matches_reference(&src, sw, sh, dw, dh);
    }
}

#[test]
fn scale_xrgb_rejects_short_input() {
    let src = random_image(4, 3, 7);
    assert_eq!(src.len(), 12);
    let mut out = vec![0x00FF00FFu32; 16];
    // A src shorter than sw*sh gives an empty out, whatever the destination size.
    convert::scale_xrgb(&src[..11], 4, 3, 8, 6, &mut out);
    assert!(out.is_empty());
    convert::scale_xrgb(&src[..11], 4, 3, 4, 3, &mut out);
    assert!(out.is_empty());
    // Exactly sw*sh pixels is fine.
    convert::scale_xrgb(&src, 4, 3, 8, 6, &mut out);
    assert_eq!(out.len(), 8 * 6);
    // Zero source dimensions give an empty out too.
    convert::scale_xrgb(&src, 0, 3, 8, 6, &mut out);
    assert!(out.is_empty());
    convert::scale_xrgb(&src, 4, 0, 8, 6, &mut out);
    assert!(out.is_empty());
}

#[test]
fn save_jpeg_writes_durably_and_avoids_taken_names() {
    assert_eq!(
        photo::photo_name("20261107_143012"),
        "IMG_20261107_143012.jpg"
    );
    let dir = std::env::temp_dir().join(format!("lucina-camera-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    // Not a real JPEG: save_jpeg writes bytes, it does not decode.
    let jpeg = [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x00, 1, 2, 3, 0xFF, 0xD9];
    let p1 = photo::save_jpeg(&dir, "20261107_143012", &jpeg).unwrap();
    assert_eq!(p1, dir.join("IMG_20261107_143012.jpg"));
    let bytes = std::fs::read(&p1).unwrap();
    assert!(bytes.starts_with(&[0xFF, 0xD8]), "a JPEG starts with SOI");
    assert_eq!(&bytes[..], &jpeg[..]);
    // No leftover tmp file.
    assert!(!dir.join(".IMG_20261107_143012.jpg.tmp").exists());
    // The same stamp again gets _2.
    let p2 = photo::save_jpeg(&dir, "20261107_143012", &jpeg).unwrap();
    assert_eq!(p2, dir.join("IMG_20261107_143012_2.jpg"));
    assert!(p2.exists());
    let p3 = photo::save_jpeg(&dir, "20261107_143012", &jpeg).unwrap();
    assert_eq!(p3, dir.join("IMG_20261107_143012_3.jpg"));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn split_frame_checks_the_header_and_the_length() {
    let h = Header {
        flags: 0,
        fourcc: *b"MJPG",
        width: 4,
        height: 2,
        bytes: 5,
        sequence: 9,
        time_us: 123,
    };
    let mut buf = h.encode().to_vec();
    buf.extend_from_slice(&[1, 2, 3, 4, 5]);
    let f = crate::split_frame(&buf, buf.len()).unwrap();
    assert_eq!(f.header, h);
    assert_eq!(f.data, vec![1, 2, 3, 4, 5]);
    // A bad magic is InvalidData.
    let mut bad = buf.clone();
    bad[0] = b'X';
    assert_eq!(
        crate::split_frame(&bad, bad.len()).unwrap_err().kind(),
        ErrorKind::InvalidData
    );
    // A header whose bytes do not match what was read minus HEADER_LEN is InvalidData.
    let mut h2 = h;
    h2.bytes = 6;
    let mut buf2 = h2.encode().to_vec();
    buf2.extend_from_slice(&[1, 2, 3, 4, 5]);
    assert_eq!(
        crate::split_frame(&buf2, buf2.len()).unwrap_err().kind(),
        ErrorKind::InvalidData
    );
    // Fewer bytes than a header is InvalidData too.
    assert_eq!(
        crate::split_frame(&buf, 10).unwrap_err().kind(),
        ErrorKind::InvalidData
    );
}
