//! Pictures: JPEG, PNG and WebP decoded in Rust into ARGB pixels, scaled down with a box filter.

use std::io::Cursor;

/// Decoding stops above this many pixels instead of asking for the memory (64 MP is 256 MiB). A phone's largest photos are 48 MP.
pub const MAX_PIXELS: u64 = 64_000_000;

/// An image of `w` x `h` pixels as `0xAARRGGBB`, rows packed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    pub w: u32,
    pub h: u32,
    pub px: Vec<u32>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ImageError {
    /// Not JPEG, PNG or WebP.
    UnknownFormat,
    /// Larger than [`MAX_PIXELS`].
    TooLarge(u32, u32),
    /// The file is damaged or uses something the decoder does not support.
    Decode(String),
}

impl std::fmt::Display for ImageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImageError::UnknownFormat => write!(f, "not a JPEG, PNG or WebP file"),
            ImageError::TooLarge(w, h) => write!(f, "image too large: {w} x {h}"),
            ImageError::Decode(e) => write!(f, "cannot decode the image: {e}"),
        }
    }
}

impl std::error::Error for ImageError {}

fn check_size(w: u32, h: u32) -> Result<(), ImageError> {
    if w == 0 || h == 0 {
        return Err(ImageError::Decode("empty image".into()));
    }
    if w as u64 * h as u64 > MAX_PIXELS {
        return Err(ImageError::TooLarge(w, h));
    }
    Ok(())
}

fn err(e: impl std::fmt::Display) -> ImageError {
    ImageError::Decode(e.to_string())
}

fn from_rgb(w: u32, h: u32, b: &[u8]) -> Image {
    let px = b
        .chunks_exact(3)
        .map(|p| 0xff00_0000 | (p[0] as u32) << 16 | (p[1] as u32) << 8 | p[2] as u32)
        .collect();
    Image { w, h, px }
}

fn from_rgba(w: u32, h: u32, b: &[u8]) -> Image {
    let px = b
        .chunks_exact(4)
        .map(|p| (p[3] as u32) << 24 | (p[0] as u32) << 16 | (p[1] as u32) << 8 | p[2] as u32)
        .collect();
    Image { w, h, px }
}

fn decode_jpeg(bytes: &[u8]) -> Result<Image, ImageError> {
    use zune_jpeg::JpegDecoder;
    use zune_jpeg::zune_core::bytestream::ZCursor;
    use zune_jpeg::zune_core::colorspace::ColorSpace;
    use zune_jpeg::zune_core::options::DecoderOptions;
    let opts = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGB);
    let mut d = JpegDecoder::new_with_options(ZCursor::new(bytes), opts);
    d.decode_headers().map_err(err)?;
    let (w, h) = d.dimensions().ok_or_else(|| err("no dimensions"))?;
    check_size(w as u32, h as u32)?;
    let rgb = d.decode().map_err(err)?;
    Ok(from_rgb(w as u32, h as u32, &rgb))
}

fn decode_png(bytes: &[u8]) -> Result<Image, ImageError> {
    let mut d = png::Decoder::new(Cursor::new(bytes));
    d.set_transformations(png::Transformations::normalize_to_color8());
    let mut r = d.read_info().map_err(err)?;
    let (w, h) = (r.info().width, r.info().height);
    check_size(w, h)?;
    let size = r
        .output_buffer_size()
        .ok_or_else(|| err("no output size"))?;
    let mut buf = vec![0u8; size];
    let info = r.next_frame(&mut buf).map_err(err)?;
    let b = &buf[..info.buffer_size()];
    Ok(match info.color_type {
        png::ColorType::Rgb => from_rgb(w, h, b),
        png::ColorType::Rgba => from_rgba(w, h, b),
        png::ColorType::Grayscale => {
            let px = b
                .iter()
                .map(|&v| 0xff00_0000 | (v as u32) * 0x01_0101)
                .collect();
            Image { w, h, px }
        }
        png::ColorType::GrayscaleAlpha => {
            let px = b
                .chunks_exact(2)
                .map(|p| (p[1] as u32) << 24 | (p[0] as u32) * 0x01_0101)
                .collect();
            Image { w, h, px }
        }
        png::ColorType::Indexed => return Err(err("indexed output")),
    })
}

fn decode_webp(bytes: &[u8]) -> Result<Image, ImageError> {
    let mut d = image_webp::WebPDecoder::new(Cursor::new(bytes)).map_err(err)?;
    let (w, h) = d.dimensions();
    check_size(w, h)?;
    let alpha = d.has_alpha();
    let mut buf = vec![
        0u8;
        d.output_buffer_size()
            .ok_or_else(|| err("no output size"))?
    ];
    d.read_image(&mut buf).map_err(err)?;
    Ok(if alpha {
        from_rgba(w, h, &buf)
    } else {
        from_rgb(w, h, &buf)
    })
}

impl Image {
    /// Decode a JPEG, PNG or WebP file; which one it is comes from its first bytes.
    pub fn decode(bytes: &[u8]) -> Result<Image, ImageError> {
        if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
            decode_jpeg(bytes)
        } else if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
            decode_png(bytes)
        } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
            decode_webp(bytes)
        } else {
            Err(ImageError::UnknownFormat)
        }
    }

    /// Read and decode a file.
    pub fn open(path: &str) -> Result<Image, ImageError> {
        let bytes = std::fs::read(path).map_err(err)?;
        Image::decode(&bytes)
    }

    /// This image fitted inside `max_w` x `max_h`, keeping its shape, never larger than it is. A box
    /// filter: every source pixel counts, so thumbnails of photographs do not shimmer. Do this once
    /// when the picture arrives, and keep the result: drawing it is then a copy.
    pub fn fit_within(&self, max_w: u32, max_h: u32) -> Image {
        let k = (max_w as f64 / self.w as f64)
            .min(max_h as f64 / self.h as f64)
            .min(1.0);
        let w = ((self.w as f64 * k).round() as u32).max(1);
        let h = ((self.h as f64 * k).round() as u32).max(1);
        if (w, h) == (self.w, self.h) {
            return self.clone();
        }
        // Which source columns make each column, once: this runs on millions of pixels on a slow CPU, so no division per pixel.
        let columns: Vec<(usize, usize, u64)> = (0..w as u64)
            .map(|x| {
                let x0 = x * self.w as u64 / w as u64;
                let x1 = ((x + 1) * self.w as u64 / w as u64).max(x0 + 1);
                (x0 as usize, x1 as usize, x1 - x0)
            })
            .collect();
        let mut px = Vec::with_capacity(w as usize * h as usize);
        for y in 0..h {
            let y0 = y as u64 * self.h as u64 / h as u64;
            let y1 = ((y as u64 + 1) * self.h as u64 / h as u64).max(y0 + 1);
            let rows = y1 - y0;
            for &(x0, x1, cols) in &columns {
                // Two channels in each sum, with room between them for any number of pixels.
                let (mut ar, mut gb) = (0u64, 0u64);
                for sy in y0..y1 {
                    let row = sy as usize * self.w as usize;
                    for &p in &self.px[row + x0..row + x1] {
                        let p = p as u64;
                        ar += (p >> 24) | ((p & 0xff_0000) << 16);
                        gb += ((p >> 8) & 0xff) | ((p & 0xff) << 32);
                    }
                }
                let n = cols * rows;
                let (a, r, g, b) = (ar & 0xffff_ffff, ar >> 32, gb & 0xffff_ffff, gb >> 32);
                px.push(if n == 1 {
                    (a << 24 | r << 16 | g << 8 | b) as u32
                } else if n <= 4096 {
                    // Each sum is at most 255 n, so multiplying by the rounded-up inverse gives exactly what the division would.
                    let inv = ((1u64 << 32) + n - 1) / n;
                    let d = |v: u64| (v * inv) >> 32;
                    (d(a) << 24 | d(r) << 16 | d(g) << 8 | d(b)) as u32
                } else {
                    ((a / n) << 24 | (r / n) << 16 | (g / n) << 8 | (b / n)) as u32
                });
            }
        }
        Image { w, h, px }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_bytes(w: u32, h: u32, color: png::ColorType, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut e = png::Encoder::new(&mut out, w, h);
        e.set_color(color);
        e.set_depth(png::BitDepth::Eight);
        e.write_header().unwrap().write_image_data(data).unwrap();
        out
    }

    #[test]
    fn png_with_alpha() {
        let f = png_bytes(2, 1, png::ColorType::Rgba, &[1, 2, 3, 4, 5, 6, 7, 8]);
        let i = Image::decode(&f).unwrap();
        assert_eq!((i.w, i.h), (2, 1));
        assert_eq!(i.px, vec![0x04010203, 0x08050607]);
    }

    #[test]
    fn png_grey_and_rgb() {
        let f = png_bytes(1, 1, png::ColorType::Grayscale, &[9]);
        assert_eq!(Image::decode(&f).unwrap().px, vec![0xff090909]);
        let f = png_bytes(1, 1, png::ColorType::Rgb, &[1, 2, 3]);
        assert_eq!(Image::decode(&f).unwrap().px, vec![0xff010203]);
    }

    #[test]
    fn webp_lossless() {
        let mut out = Vec::new();
        image_webp::WebPEncoder::new(&mut out)
            .encode(&[10, 20, 30, 40, 50, 60], 2, 1, image_webp::ColorType::Rgb8)
            .unwrap();
        let i = Image::decode(&out).unwrap();
        assert_eq!(i.px, vec![0xff0a141e, 0xff28323c]);
    }

    #[test]
    fn jpeg_fixture() {
        // A 16 x 16 solid red JPEG (ffmpeg, colour lavfi), so colours may be off by a little.
        let i = Image::decode(include_bytes!("../tests/red16.jpg")).unwrap();
        assert_eq!((i.w, i.h), (16, 16));
        let p = i.px[8 * 16 + 8];
        assert_eq!(p >> 24, 0xff);
        assert!(
            (p >> 16) & 0xff > 240 && (p >> 8) & 0xff < 16 && p & 0xff < 16,
            "{p:x}"
        );
    }

    /// The shortcut for the division against the division, at every sum it can meet.
    #[test]
    fn dividing_by_the_inverse_is_exact() {
        for n in (2..=4096u64).step_by(7).chain([3, 4, 6, 8, 9, 4095, 4096]) {
            let inv = ((1u64 << 32) + n - 1) / n;
            for v in (0..=255 * n).step_by((n as usize / 3).max(1)).chain([255 * n, n - 1, n]) {
                assert_eq!((v * inv) >> 32, v / n, "{v} / {n}");
            }
        }
    }

    #[test]
    fn fitting_averages_every_channel_at_uneven_sizes() {
        // 3 to 2 columns: 1 pixel, then 2.
        let i = Image { w: 3, h: 1, px: vec![0x80102030, 0x00000000, 0xff204060] };
        let t = i.fit_within(2, 1);
        assert_eq!(t.px, vec![0x80102030, (0xff / 2) << 24 | 0x10 << 16 | 0x20 << 8 | 0x30]);
    }

    #[test]
    fn rejects_what_it_cannot_read() {
        assert_eq!(Image::decode(b"hello"), Err(ImageError::UnknownFormat));
        // The right magic with nothing behind it must be an error, not a panic.
        assert!(matches!(
            Image::decode(&[0x89, b'P', b'N', b'G']),
            Err(ImageError::Decode(_))
        ));
        assert!(Image::decode(&[0xff, 0xd8, 0xff, 0xe0, 0, 16]).is_err());
        assert!(Image::decode(b"RIFF\x04\0\0\0WEBPxxxx").is_err());
        assert_eq!(
            check_size(100_000, 100_000),
            Err(ImageError::TooLarge(100_000, 100_000))
        );
    }

    #[test]
    fn fit_keeps_the_shape_and_averages() {
        let i = Image {
            w: 4,
            h: 2,
            px: vec![
                0xff000000, 0xff000000, 0xffffffff, 0xffffffff, 0xff000000, 0xff000000, 0xffffffff,
                0xffffffff,
            ],
        };
        let t = i.fit_within(2, 2);
        assert_eq!((t.w, t.h), (2, 1));
        assert_eq!(t.px, vec![0xff000000, 0xffffffff]);
        // Never larger.
        assert_eq!(i.fit_within(100, 100), i);
        let row = i.fit_within(1, 1);
        assert_eq!(row.px, vec![0xff7f7f7f]);
    }
}
