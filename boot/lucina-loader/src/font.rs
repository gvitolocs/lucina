//! The theme's fonts, rasterized at build time (`build.rs`): DejaVu Sans for printable ASCII, 8-bit coverage.

use lucina_boot::menu::Face;

/// One glyph: its coverage map is `w * h` bytes at `at`, row by row from the top.
pub struct Glyph {
    pub w: u16,
    pub h: u16,
    /// From the pen to the map's left edge.
    pub left: i16,
    /// From the baseline up to the map's top row.
    pub top: i16,
    /// Pen advance in 1/64 pixel.
    pub advance: u32,
    pub at: u32,
}

pub struct Font {
    /// From the baseline up to the top of `l` (the line [`lucina_boot::menu::Text`]'s `top` names).
    pub ascent: i32,
    /// `' '..='~'`.
    pub glyphs: &'static [Glyph],
    pub coverage: &'static [u8],
}

include!(concat!(env!("OUT_DIR"), "/fonts.rs"));

impl Font {
    /// The glyph for `c`; anything outside printable ASCII is drawn as `?`.
    pub fn glyph(&self, c: char) -> &Glyph {
        let i = match c {
            ' '..='~' => c as usize - ' ' as usize,
            _ => '?' as usize - ' ' as usize,
        };
        &self.glyphs[i]
    }

    pub fn map(&self, g: &Glyph) -> &[u8] {
        let start = g.at as usize;
        self.coverage
            .get(start..start + usize::from(g.w) * usize::from(g.h))
            .unwrap_or(&[])
    }
}

pub fn face(f: Face) -> &'static Font {
    match f {
        Face::Bold48 => &BOLD48,
        Face::Regular20 => &REGULAR20,
        Face::Bold20 => &BOLD20,
        Face::Regular16 => &REGULAR16,
    }
}
