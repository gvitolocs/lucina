//! An 8-bit gray canvas over the EPDC update buffer, with the PW3's portrait rotation and a 5x7 font.

pub const WHITE: u8 = 0xf0; // the EPDC reads the high nibble (epdc_from_rgb_clear_lower_nibble)
pub const BLACK: u8 = 0x00;

pub struct Canvas<'a> {
    pub buf: &'a mut [u8],
    /// Native (scan) size.
    pub w: usize,
    pub h: usize,
    /// Logical portrait: native (x, y) = (ly, h - 1 - lx), lucina-display's `R270`, upright on the PW3.
    pub portrait: bool,
}

impl Canvas<'_> {
    pub fn logical_size(&self) -> (usize, usize) {
        if self.portrait { (self.h, self.w) } else { (self.w, self.h) }
    }

    pub fn fill(&mut self, v: u8) {
        self.buf.fill(v);
    }

    pub fn set(&mut self, x: usize, y: usize, v: u8) {
        let (nx, ny) = if self.portrait { (y, self.h.wrapping_sub(1).wrapping_sub(x)) } else { (x, y) };
        if nx < self.w && ny < self.h {
            self.buf[ny * self.w + nx] = v;
        }
    }

    pub fn rect(&mut self, x: usize, y: usize, w: usize, h: usize, v: u8) {
        for yy in y..y + h {
            for xx in x..x + w {
                self.set(xx, yy, v);
            }
        }
    }

    /// Draw `text` with its top-left at (x, y), each font pixel `scale` x `scale`. Returns the width used.
    pub fn text(&mut self, x: usize, y: usize, scale: usize, text: &str, v: u8) -> usize {
        let mut cx = x;
        for c in text.chars() {
            let g = glyph(c);
            for (row, bits) in g.iter().enumerate() {
                for col in 0..5 {
                    if bits & (0x10 >> col) != 0 {
                        self.rect(cx + col * scale, y + row * scale, scale, scale, v);
                    }
                }
            }
            cx += 6 * scale;
        }
        cx.saturating_sub(x + scale)
    }

    pub fn text_width(text: &str, scale: usize) -> usize {
        (6 * text.chars().count() * scale).saturating_sub(scale)
    }

    /// Centred horizontally in logical coordinates.
    pub fn text_centred(&mut self, y: usize, scale: usize, text: &str, v: u8) {
        let (lw, _) = self.logical_size();
        let x = lw.saturating_sub(Self::text_width(text, scale)) / 2;
        self.text(x, y, scale, text, v);
    }

    /// Native bounding box (x, y, w, h) of a logical rectangle.
    pub fn native_rect(&self, x: usize, y: usize, w: usize, h: usize) -> (usize, usize, usize, usize) {
        if self.portrait { (y, self.h - (x + w), h, w) } else { (x, y, w, h) }
    }
}

/// FNV-1a over the buffer, for the QEMU golden check.
pub fn checksum(buf: &[u8]) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for &b in buf {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

/// 5x7 glyphs, one byte per row, bit 4 = leftmost column. Only what the Phase 0 screens print.
fn glyph(c: char) -> [u8; 7] {
    const fn g(rows: [&str; 7]) -> [u8; 7] {
        let mut out = [0u8; 7];
        let mut r = 0;
        while r < 7 {
            let b = rows[r].as_bytes();
            let mut v = 0u8;
            let mut i = 0;
            while i < 5 {
                v = (v << 1) | (b[i] == b'#') as u8;
                i += 1;
            }
            out[r] = v;
            r += 1;
        }
        out
    }
    match c {
        'A' => g([".###.", "#...#", "#...#", "#####", "#...#", "#...#", "#...#"]),
        'L' => g(["#....", "#....", "#....", "#....", "#....", "#....", "#####"]),
        'M' => g(["#...#", "##.##", "#.#.#", "#.#.#", "#...#", "#...#", "#...#"]),
        'P' => g(["####.", "#...#", "#...#", "####.", "#....", "#....", "#...."]),
        'R' => g(["####.", "#...#", "#...#", "####.", "#.#..", "#..#.", "#...#"]),
        'W' => g(["#...#", "#...#", "#...#", "#.#.#", "#.#.#", "##.##", "#...#"]),
        'a' => g([".....", ".....", ".###.", "....#", ".####", "#...#", ".####"]),
        'c' => g([".....", ".....", ".###.", "#....", "#....", "#...#", ".###."]),
        'e' => g([".....", ".....", ".###.", "#...#", "#####", "#....", ".###."]),
        'h' => g(["#....", "#....", "#.##.", "##..#", "#...#", "#...#", "#...#"]),
        'i' => g(["..#..", ".....", ".##..", "..#..", "..#..", "..#..", ".###."]),
        'n' => g([".....", ".....", "#.##.", "##..#", "#...#", "#...#", "#...#"]),
        'r' => g([".....", ".....", "#.##.", "##..#", "#....", "#....", "#...."]),
        's' => g([".....", ".....", ".###.", "#....", ".###.", "....#", "####."]),
        't' => g([".#...", ".#...", "###..", ".#...", ".#...", ".#..#", "..##."]),
        'u' => g([".....", ".....", "#...#", "#...#", "#...#", "#..##", ".##.#"]),
        'v' => g([".....", ".....", "#...#", "#...#", "#...#", ".#.#.", "..#.."]),
        '0' => g([".###.", "#...#", "#..##", "#.#.#", "##..#", "#...#", ".###."]),
        '1' => g(["..#..", ".##..", "..#..", "..#..", "..#..", "..#..", ".###."]),
        '2' => g([".###.", "#...#", "....#", "...#.", "..#..", ".#...", "#####"]),
        '3' => g(["#####", "...#.", "..#..", "...#.", "....#", "#...#", ".###."]),
        '4' => g(["...#.", "..##.", ".#.#.", "#..#.", "#####", "...#.", "...#."]),
        '5' => g(["#####", "#....", "####.", "....#", "....#", "#...#", ".###."]),
        '6' => g(["..##.", ".#...", "#....", "####.", "#...#", "#...#", ".###."]),
        '7' => g(["#####", "....#", "...#.", "..#..", ".#...", ".#...", ".#..."]),
        '8' => g([".###.", "#...#", "#...#", ".###.", "#...#", "#...#", ".###."]),
        '9' => g([".###.", "#...#", "#...#", ".####", "....#", "...#.", ".##.."]),
        '-' => g([".....", ".....", ".....", "#####", ".....", ".....", "....."]),
        '=' => g([".....", ".....", "#####", ".....", "#####", ".....", "....."]),
        _ => [0; 7],
    }
}

/// Decimal digits of `n` into `out`, returning the used tail.
pub fn fmt_u32(mut n: u32, out: &mut [u8; 10]) -> &str {
    let mut i = out.len();
    loop {
        i -= 1;
        out[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    core::str::from_utf8(&out[i..]).unwrap_or("?")
}
