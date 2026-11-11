//! The screen as a source of frames. Reading the framebuffer is the slow part, as it is write-combined memory, so the scan reads
//! a few rows of each band of 16 and reads a whole band only when one of them differs. Which rows it reads moves on each round, so a
//! change as small as a text cursor shows within a few rounds.

use lucina_app::fb::Fb;

/// The screen, or a test's stand-in.
pub trait Source: Send {
    fn size(&self) -> (usize, usize);
    /// One row of XRGB pixels. `out` is the row's width.
    fn read_row(&mut self, y: usize, out: &mut [u32]);
    /// A stand-in: input goes to the log, not to the machine.
    fn is_test(&self) -> bool {
        false
    }
}

pub struct FbSource {
    fb: Fb,
}

// SAFETY: `Fb` holds a mapping that this one thread uses; the raw pointer is what stops the compiler from seeing that.
unsafe impl Send for FbSource {}

impl FbSource {
    pub fn open(path: &str) -> Result<FbSource, String> {
        let fb = Fb::open(path).map_err(|e| format!("{path}: {e}"))?;
        if !fb.layout.is_xrgb8888() {
            return Err(format!("the screen is {}, and only XRGB8888 is captured", fb.layout.describe()));
        }
        Ok(FbSource { fb })
    }
}

impl Source for FbSource {
    fn size(&self) -> (usize, usize) {
        (self.fb.width as usize, self.fb.height as usize)
    }

    fn read_row(&mut self, y: usize, out: &mut [u32]) {
        self.fb.read_row(y as u32, out);
    }
}

/// The browser host (lucina-desk-web) reads the screen through its own trait; the same reader serves it, so that a fix to the
/// reads of /dev/fb0 reaches both hosts.
impl lucina_desk_web::Screen for FbSource {
    fn size(&self) -> (usize, usize) {
        Source::size(self)
    }

    fn read_row(&mut self, y: usize, out: &mut [u32]) {
        Source::read_row(self, y, out);
    }
}

/// What it costs to find out nothing changed: this many rows of each band, out of 16. On the laptop reading the whole screen takes
/// 73 ms, so one row a band is about 5 ms.
const SAMPLES: usize = 1;

pub struct Capture {
    w: usize,
    h: usize,
    mb_w: usize,
    mb_h: usize,
    /// The screen as last read.
    px: Vec<u32>,
    row: Vec<u32>,
    changed: Vec<bool>,
    to_convert: Vec<usize>,
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
    round: usize,
}

impl Capture {
    pub fn new(w: usize, h: usize) -> Capture {
        let (mb_w, mb_h) = (w.div_ceil(16), h.div_ceil(16));
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        Capture {
            w,
            h,
            mb_w,
            mb_h,
            px: vec![0; w * h],
            row: vec![0; w],
            changed: vec![false; mb_w * mb_h],
            to_convert: Vec::new(),
            y: vec![16; w * h],
            u: vec![128; cw * ch],
            v: vec![128; cw * ch],
            round: 0,
        }
    }

    /// Looks at the screen and marks what changed; `all` reads everything and marks everything, for a key frame. Returns how many
    /// macroblocks changed.
    pub fn scan(&mut self, source: &mut dyn Source, all: bool) -> usize {
        self.changed.iter_mut().for_each(|c| *c = false);
        self.to_convert.clear();
        self.round = self.round.wrapping_add(1);
        for band in 0..self.mb_h {
            let top = band * 16;
            let rows = (self.h - top).min(16);
            let mut differs = all;
            if !differs {
                for s in 0..SAMPLES {
                    // Spread over the band, and moved on one row each round.
                    let r = (self.round + s * 16 / SAMPLES + band) % rows;
                    source.read_row(top + r, &mut self.row);
                    if self.row != self.px[(top + r) * self.w..][..self.w] {
                        differs = true;
                        break;
                    }
                }
            }
            if !differs {
                continue;
            }
            for r in 0..rows {
                let at = (top + r) * self.w;
                source.read_row(top + r, &mut self.row);
                let line = &mut self.px[at..][..self.w];
                if all {
                    line.copy_from_slice(&self.row);
                    continue;
                }
                if *line == *self.row {
                    continue;
                }
                for (col, (now, was)) in self.row.chunks(16).zip(line.chunks_mut(16)).enumerate() {
                    if now != was {
                        self.changed[band * self.mb_w + col] = true;
                        was.copy_from_slice(now);
                    }
                }
            }
        }
        if all {
            self.changed.iter_mut().for_each(|c| *c = true);
        }
        for (i, c) in self.changed.iter().enumerate() {
            if *c {
                self.to_convert.push(i);
            }
        }
        self.to_convert.len()
    }

    /// The changed macroblocks into 4:2:0.
    pub fn convert(&mut self) {
        let (w, h) = (self.w, self.h);
        let cw = w.div_ceil(2);
        for &i in &self.to_convert {
            let (mx, my) = (i % self.mb_w, i / self.mb_w);
            let (x0, y0) = (mx * 16, my * 16);
            let (x1, y1) = ((x0 + 16).min(w), (y0 + 16).min(h));
            for y in y0..y1 {
                let line = &self.px[y * w..][x0..x1];
                let out = &mut self.y[y * w..][x0..x1];
                for (o, &p) in out.iter_mut().zip(line) {
                    let (r, g, b) = ((p >> 16 & 255) as i32, (p >> 8 & 255) as i32, (p & 255) as i32);
                    *o = (((66 * r + 129 * g + 25 * b + 128) >> 8) + 16) as u8;
                }
            }
            let mut y = y0;
            while y < y1 {
                let mut x = x0;
                while x < x1 {
                    let (mut r, mut g, mut b) = (0i32, 0i32, 0i32);
                    let mut n = 0;
                    for yy in y..(y + 2).min(h) {
                        for xx in x..(x + 2).min(w) {
                            let p = self.px[yy * w + xx];
                            r += (p >> 16 & 255) as i32;
                            g += (p >> 8 & 255) as i32;
                            b += (p & 255) as i32;
                            n += 1;
                        }
                    }
                    let (r, g, b) = (r / n, g / n, b / n);
                    let at = (y / 2) * cw + x / 2;
                    self.u[at] = (((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128) as u8;
                    self.v[at] = (((112 * r - 94 * g - 18 * b + 128) >> 8) + 128) as u8;
                    x += 2;
                }
                y += 2;
            }
        }
    }

    pub fn planes(&self) -> (&[u8], &[u8], &[u8]) {
        (&self.y, &self.u, &self.v)
    }

    /// The screen as last read.
    pub fn screen(&self) -> &[u32] {
        &self.px
    }

    pub fn changed(&self) -> &[bool] {
        &self.changed
    }
}

/// A moving picture, for a test without a screen: a coloured square travels, and a counter of bars grows at the bottom.
pub struct TestPattern {
    pub w: usize,
    pub h: usize,
    pub frame: u32,
    /// Advances each time the first row is read.
    pub moving: bool,
}

impl Source for TestPattern {
    fn is_test(&self) -> bool {
        true
    }

    fn size(&self) -> (usize, usize) {
        (self.w, self.h)
    }

    fn read_row(&mut self, y: usize, out: &mut [u32]) {
        if y == 0 && self.moving {
            self.frame = self.frame.wrapping_add(1);
        }
        let square_x = (self.frame as usize * 6) % self.w.saturating_sub(80).max(1);
        let square_y = 100;
        for (x, o) in out.iter_mut().enumerate() {
            *o = if x >= square_x && x < square_x + 80 && y >= square_y && y < square_y + 80 {
                0xffe0_4020
            } else if y > self.h.saturating_sub(40) && x < (self.frame as usize / 4) % self.w {
                0xff20_a060
            } else {
                0xff00_0000 | ((x * 255 / self.w.max(1)) as u32) << 8 | (y * 255 / self.h.max(1)) as u32
            };
        }
    }
}

impl lucina_desk_web::Screen for TestPattern {
    fn size(&self) -> (usize, usize) {
        Source::size(self)
    }

    fn read_row(&mut self, y: usize, out: &mut [u32]) {
        Source::read_row(self, y, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Flat {
        w: usize,
        h: usize,
        px: Vec<u32>,
        rows_read: usize,
    }

    impl Source for Flat {
        fn size(&self) -> (usize, usize) {
            (self.w, self.h)
        }
        fn read_row(&mut self, y: usize, out: &mut [u32]) {
            self.rows_read += 1;
            out.copy_from_slice(&self.px[y * self.w..][..self.w]);
        }
    }

    fn flat(w: usize, h: usize) -> Flat {
        Flat { w, h, px: vec![0xff10_2030; w * h], rows_read: 0 }
    }

    #[test]
    fn nothing_changed_reads_few_rows() {
        let mut s = flat(64, 64);
        let mut c = Capture::new(64, 64);
        assert_eq!(c.scan(&mut s, true), 16);
        s.rows_read = 0;
        assert_eq!(c.scan(&mut s, false), 0);
        assert_eq!(s.rows_read, 4 * SAMPLES);
    }

    #[test]
    fn a_change_marks_only_its_macroblock_and_is_found_within_a_few_rounds() {
        let mut s = flat(64, 64);
        let mut c = Capture::new(64, 64);
        c.scan(&mut s, true);
        // One pixel, as a text cursor's corner would be.
        s.px[20 * 64 + 40] = 0xffff_ffff;
        let mut found = 0;
        for _ in 0..16 {
            if c.scan(&mut s, false) > 0 {
                found += 1;
                let marked: Vec<usize> = c.changed().iter().enumerate().filter(|(_, v)| **v).map(|(i, _)| i).collect();
                // Column 2, row 1 in macroblocks.
                assert_eq!(marked, vec![4 + 2]);
                break;
            }
        }
        assert_eq!(found, 1, "a one-pixel change must be seen");
    }

    #[test]
    fn conversion_matches_the_limited_range_matrix() {
        let mut s = flat(32, 32);
        s.px.iter_mut().for_each(|p| *p = 0xffff_ffff);
        let mut c = Capture::new(32, 32);
        c.scan(&mut s, true);
        c.convert();
        let (y, u, v) = c.planes();
        assert_eq!(y[0], 235);
        assert_eq!((u[0], v[0]), (128, 128));
        s.px.iter_mut().for_each(|p| *p = 0xff00_0000);
        c.scan(&mut s, true);
        c.convert();
        assert_eq!(c.planes().0[0], 16);
    }

    #[test]
    fn a_size_not_in_whole_macroblocks() {
        let mut s = flat(50, 37);
        let mut c = Capture::new(50, 37);
        assert_eq!(c.scan(&mut s, true), 4 * 3);
        c.convert();
        s.px[36 * 50 + 49] = 0xffff_ffff;
        let mut seen = 0;
        for _ in 0..16 {
            seen = c.scan(&mut s, false);
            if seen > 0 {
                break;
            }
        }
        assert_eq!(seen, 1);
        c.convert();
    }
}
