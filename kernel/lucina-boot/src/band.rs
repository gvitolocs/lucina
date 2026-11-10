//! A status band LucinaBoot paints on the boot framebuffer, readable from a photo of the phone.
//!
//! On surya there is no console LucinaBoot can use without a driver: the debug UART needs the phone opened. ABL
//! leaves the panel scanning out the splash framebuffer, so LucinaBoot writes plain pixels there (decision D-009,
//! "no display driver"). The band is a row of [`CELLS`] coloured cells across the top of the screen:
//!
//! ```text
//! cell 0      white            LucinaBoot runs (anything else: it never got this far)
//! cell 1      EL colour        EL1 green, EL2 blue, EL3 magenta
//! cells 2-6   stage reached    green when reached, grey when not: device tree read, payload found, memory
//!                              planned, kernel copied, jumping to the kernel
//! cell 7      verdict          yellow while running, red after an error
//! lower half  error code       on error, cells 0-7 show the code's bits, most significant first: white = 1
//! ```
//!
//! The layout lives here so that the host-side check (`lucina-boot check-band`) reads exactly what LucinaBoot
//! writes.

/// Cells across the band.
pub const CELLS: u32 = 8;
/// Stage cells (2 to 6).
pub const STAGES: u8 = 5;

/// What LucinaBoot has done so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Status {
    /// CurrentEL (1 on surya: QHEE owns EL2).
    pub el: u8,
    /// Stages reached, 0 to [`STAGES`].
    pub stages: u8,
    /// Error code, if LucinaBoot stopped.
    pub error: Option<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    White,
    Black,
    Grey,
    Green,
    Blue,
    Magenta,
    Yellow,
    Red,
}

impl Color {
    pub const ALL: [Color; 8] = [
        Color::White,
        Color::Black,
        Color::Grey,
        Color::Green,
        Color::Blue,
        Color::Magenta,
        Color::Yellow,
        Color::Red,
    ];

    /// 0xRRGGBB.
    pub fn rgb(self) -> u32 {
        match self {
            Color::White => 0xff_ffff,
            Color::Black => 0x00_0000,
            Color::Grey => 0x50_5050,
            Color::Green => 0x00_e000,
            Color::Blue => 0x20_40ff,
            Color::Magenta => 0xe0_00e0,
            Color::Yellow => 0xff_d000,
            Color::Red => 0xff_0000,
        }
    }

    pub fn for_el(el: u8) -> Color {
        match el {
            1 => Color::Green,
            2 => Color::Blue,
            3 => Color::Magenta,
            _ => Color::Red,
        }
    }

    /// The colour nearest to `rgb` (squared distance per channel).
    pub fn nearest(rgb: u32) -> Color {
        let d = |a: u32, b: u32| {
            (0..3)
                .map(|s| {
                    let (x, y) = ((a >> (8 * s)) & 0xff, (b >> (8 * s)) & 0xff);
                    (x as i32 - y as i32).pow(2) as u32
                })
                .sum::<u32>()
        };
        Color::ALL
            .into_iter()
            .min_by_key(|c| d(c.rgb(), rgb))
            .unwrap_or(Color::Black)
    }
}

/// Pixel formats of a DT `simple-framebuffer` that LucinaBoot writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// a8r8g8b8 or x8r8g8b8: 32-bit little-endian words 0xAARRGGBB.
    Xrgb8888,
    /// r5g6b5: 16-bit little-endian.
    Rgb565,
}

impl Format {
    pub fn parse(s: &str) -> Option<Format> {
        match s {
            "a8r8g8b8" | "x8r8g8b8" => Some(Format::Xrgb8888),
            "r5g6b5" => Some(Format::Rgb565),
            _ => None,
        }
    }

    pub fn bytes_per_pixel(self) -> u32 {
        match self {
            Format::Xrgb8888 => 4,
            Format::Rgb565 => 2,
        }
    }

    /// The pixel value for 0xRRGGBB (opaque).
    pub fn encode(self, rgb: u32) -> u32 {
        match self {
            Format::Xrgb8888 => 0xff00_0000 | rgb,
            Format::Rgb565 => {
                let (r, g, b) = ((rgb >> 16) & 0xff, (rgb >> 8) & 0xff, rgb & 0xff);
                ((r >> 3) << 11) | ((g >> 2) << 5) | (b >> 3)
            }
        }
    }

    /// 0xRRGGBB back from a pixel value (low bits of 565 channels are zero).
    pub fn decode(self, px: u32) -> u32 {
        match self {
            Format::Xrgb8888 => px & 0xff_ffff,
            Format::Rgb565 => {
                let (r, g, b) = ((px >> 11) & 0x1f, (px >> 5) & 0x3f, px & 0x1f);
                ((r << 3) << 16) | ((g << 2) << 8) | (b << 3)
            }
        }
    }
}

/// A framebuffer's geometry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
    pub width: u32,
    pub height: u32,
    /// Bytes per line.
    pub stride: u32,
    pub format: Format,
}

impl Geometry {
    /// `None` when the stride cannot hold a line or the band would be smaller than one pixel per cell.
    pub fn new(width: u32, height: u32, stride: u32, format: Format) -> Option<Geometry> {
        let line = width.checked_mul(format.bytes_per_pixel())?;
        (width >= CELLS * 4 && height >= 32 && stride >= line).then_some(Geometry {
            width,
            height,
            stride,
            format,
        })
    }

    /// Bytes the framebuffer spans.
    pub fn size(&self) -> u64 {
        u64::from(self.stride) * u64::from(self.height)
    }
}

/// A rectangle in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub fn center(&self) -> (u32, u32) {
        (self.x + self.w / 2, self.y + self.h / 2)
    }
}

/// The band: below the top 1/16 of the screen (a phone's status-bar area), 1/8 of the height.
pub fn band(g: &Geometry) -> Rect {
    Rect {
        x: 0,
        y: g.height / 16,
        w: g.width,
        h: g.height / 8,
    }
}

/// Cell `i` (0 to CELLS-1), upper half of the band when `lower` is false.
pub fn cell(g: &Geometry, i: u32, lower: bool) -> Rect {
    let b = band(g);
    let pitch = g.width / CELLS;
    let gap = (pitch / 10).max(1);
    let half = b.h / 2;
    Rect {
        x: i * pitch + gap,
        y: b.y + if lower { half } else { 0 } + gap.min(half / 4),
        w: pitch - 2 * gap,
        h: half - 2 * gap.min(half / 4),
    }
}

/// Every rectangle to paint for `s`, in painting order: the whole band black first, then each cell.
pub fn paint_list(g: &Geometry, s: &Status) -> ([(Rect, Color); 1 + 2 * CELLS as usize], usize) {
    let mut out = [(band(g), Color::Black); 1 + 2 * CELLS as usize];
    let mut n = 1;
    let mut put = |r: Rect, c: Color| {
        out[n] = (r, c);
        n += 1;
    };
    put(cell(g, 0, false), Color::White);
    put(cell(g, 1, false), Color::for_el(s.el));
    for k in 0..STAGES {
        let c = if k < s.stages {
            Color::Green
        } else {
            Color::Grey
        };
        put(cell(g, 2 + u32::from(k), false), c);
    }
    put(
        cell(g, 7, false),
        if s.error.is_some() {
            Color::Red
        } else {
            Color::Yellow
        },
    );
    if let Some(code) = s.error {
        for i in 0..CELLS {
            let bit = (code >> (CELLS - 1 - i)) & 1;
            put(
                cell(g, i, true),
                if bit == 1 { Color::White } else { Color::Black },
            );
        }
    }
    (out, n)
}

/// Reads the band back from a framebuffer dump (host-side check): the colour at each cell's centre.
pub fn read_back(g: &Geometry, raw: &[u8]) -> Option<Status> {
    let px = |r: Rect| -> Option<Color> {
        let (x, y) = r.center();
        let bpp = g.format.bytes_per_pixel() as usize;
        let at = y as usize * g.stride as usize + x as usize * bpp;
        let b = raw.get(at..at + bpp)?;
        let v = b
            .iter()
            .rev()
            .fold(0u32, |acc, &x| (acc << 8) | u32::from(x));
        Some(Color::nearest(g.format.decode(v)))
    };
    if px(cell(g, 0, false))? != Color::White {
        return None;
    }
    let el = match px(cell(g, 1, false))? {
        Color::Green => 1,
        Color::Blue => 2,
        Color::Magenta => 3,
        _ => 0,
    };
    let mut stages = 0;
    for k in 0..STAGES {
        if px(cell(g, 2 + u32::from(k), false))? == Color::Green {
            stages = k + 1;
        }
    }
    let error = if px(cell(g, 7, false))? == Color::Red {
        let mut code = 0u8;
        for i in 0..CELLS {
            code = (code << 1) | u8::from(px(cell(g, i, true))? == Color::White);
        }
        Some(code)
    } else {
        None
    };
    Some(Status { el, stages, error })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Paints into a byte buffer the way LucinaBoot paints into the framebuffer.
    fn paint(g: &Geometry, s: &Status, raw: &mut [u8]) {
        let (list, n) = paint_list(g, s);
        let bpp = g.format.bytes_per_pixel() as usize;
        for &(r, c) in &list[..n] {
            let px = g.format.encode(c.rgb()).to_le_bytes();
            for y in r.y..r.y + r.h {
                for x in r.x..r.x + r.w {
                    let at = y as usize * g.stride as usize + x as usize * bpp;
                    raw[at..at + bpp].copy_from_slice(&px[..bpp]);
                }
            }
        }
    }

    #[test]
    fn round_trips_on_surya_and_qemu_geometries() {
        let geometries = [
            Geometry::new(1080, 2400, 4320, Format::Xrgb8888).unwrap(),
            Geometry::new(640, 480, 2560, Format::Xrgb8888).unwrap(),
            Geometry::new(640, 480, 1280, Format::Rgb565).unwrap(),
            Geometry::new(100, 64, 512, Format::Xrgb8888).unwrap(),
        ];
        let statuses = [
            Status {
                el: 1,
                stages: 0,
                error: Some(3),
            },
            Status {
                el: 1,
                stages: 5,
                error: None,
            },
            Status {
                el: 2,
                stages: 2,
                error: Some(0xa5),
            },
            Status {
                el: 3,
                stages: 4,
                error: Some(0),
            },
        ];
        for g in &geometries {
            for s in &statuses {
                let mut raw = vec![0x11u8; g.size() as usize];
                paint(g, s, &mut raw);
                assert_eq!(read_back(g, &raw), Some(*s), "{g:?}");
            }
            // Nothing painted: no band.
            assert_eq!(read_back(g, &vec![0u8; g.size() as usize]), None);
        }
    }

    #[test]
    fn cells_stay_inside_the_band_and_apart() {
        for (w, h) in [(1080, 2400), (640, 480), (32, 32), (4096, 4096)] {
            let g = Geometry::new(w, h, w * 4, Format::Xrgb8888).unwrap();
            let b = band(&g);
            assert!(b.y + b.h <= h);
            for lower in [false, true] {
                for i in 0..CELLS {
                    let c = cell(&g, i, lower);
                    assert!(c.w > 0 && c.h > 0, "{w}x{h} cell {i}");
                    assert!(c.x + c.w <= w && c.y >= b.y && c.y + c.h <= b.y + b.h);
                    if i > 0 {
                        assert!(cell(&g, i - 1, lower).x + cell(&g, i - 1, lower).w < c.x);
                    }
                }
            }
        }
        assert!(Geometry::new(31, 32, 124, Format::Xrgb8888).is_none());
        assert!(Geometry::new(64, 64, 255, Format::Xrgb8888).is_none());
    }

    #[test]
    fn colors_are_distinct_after_565() {
        for a in Color::ALL {
            let back = Format::Rgb565.decode(Format::Rgb565.encode(a.rgb()));
            assert_eq!(Color::nearest(back), a);
        }
        assert_eq!(Format::parse("a8r8g8b8"), Some(Format::Xrgb8888));
        assert_eq!(Format::parse("r8g8b8"), None);
    }
}
