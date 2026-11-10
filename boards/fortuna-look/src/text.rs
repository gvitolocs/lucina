//! Text: DejaVu faces (labels, body) and Lato Light (display) rasterised on the host (tools/fortuna-look-font -> font_gen.rs) at the token type ramp scaled to
//! the DSi. Glyphs are 4-bit coverage, blended over whatever is already in the frame.

pub use crate::font_gen::{BOLD_11, BOLD_15, BOLD_23, BOLD_9, LIGHT_15, LIGHT_19, LIGHT_27, LIGHT_40, SANS_11, SANS_9};

/// One glyph: its box relative to the pen (left from the pen x, top from the baseline, y down) and its advance.
#[derive(Clone, Copy, Debug)]
pub struct Glyph {
    pub ch: char,
    pub left: i8,
    pub top: i8,
    pub w: u8,
    pub h: u8,
    pub adv: u8,
    /// Index of the first pixel in `Font::data`, in pixels (two a byte, low nibble first).
    pub off: u32,
}

/// A face at one pixel size.
pub struct Font {
    pub px: u8,
    /// A bold face (the contrast floor drops to 3:1 for bold text at 15 px and up).
    pub bold: bool,
    pub ascent: u8,
    pub descent: u8,
    /// Sorted by `ch`.
    pub glyphs: &'static [Glyph],
    pub data: &'static [u8],
}

impl Font {
    /// The glyph for `c`, or `?` when the face has none.
    pub fn glyph(&self, c: char) -> &Glyph {
        match self.glyphs.binary_search_by(|g| g.ch.cmp(&c)) {
            Ok(i) => &self.glyphs[i],
            Err(_) => match self.glyphs.binary_search_by(|g| g.ch.cmp(&'?')) {
                Ok(i) => &self.glyphs[i],
                Err(_) => &self.glyphs[0],
            },
        }
    }

    /// Coverage 0..=15 of pixel `i` of the glyph data.
    pub fn cov(&self, i: u32) -> u8 {
        let b = self.data[(i / 2) as usize];
        if i & 1 == 0 { b & 15 } else { b >> 4 }
    }

    /// Width of `s` in pixels (the sum of the advances).
    pub fn width(&self, s: &str) -> i32 {
        s.chars().map(|c| self.glyph(c).adv as i32).sum()
    }

    /// Line height: ascent + descent.
    pub fn line(&self) -> i32 {
        self.ascent as i32 + self.descent as i32
    }
}
