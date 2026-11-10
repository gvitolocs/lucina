//! Type for the glass kit: Lato Thin for the huge display words, Lato Light, Regular and Semibold for the small sans, and DejaVu
//! Sans Mono for the uppercase mono blocks. All embedded (subsets of Latin-1 and a few symbols, `fonts/`), so a frame renders
//! the same on buildhost and on Juno. Glyphs are rasterised like lucina-app's (skrifa outlines, ab_glyph_rasterizer coverage)
//! at the canvas's pixel size, at quarter-pixel horizontal positions so thin type keeps even spacing, and cached.

use std::collections::HashMap;

use ab_glyph_rasterizer::{Point, Rasterizer, point};
use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::{FontRef, GlyphId, MetadataProvider};

use crate::canvas::{Canvas, Color};

/// The faces of the design (docs/design/phone-concept.md, "Type").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Font {
    /// Lato Thin: the display words ("Build", "Notification", the huge numerals).
    Thin,
    /// Lato Light: subtitles and quotes.
    Light,
    /// Lato Regular: small text.
    Regular,
    /// Lato Semibold: pills and labels.
    Semibold,
    /// DejaVu Sans Mono: codes and timers (rules.md, rule 5).
    Mono,
}

const FONTS: [&[u8]; 5] = [
    include_bytes!("../fonts/Lato-Thin.ttf"),
    include_bytes!("../fonts/Lato-Light.ttf"),
    include_bytes!("../fonts/Lato-Regular.ttf"),
    include_bytes!("../fonts/Lato-Semibold.ttf"),
    include_bytes!("../fonts/DejaVuSansMono.ttf"),
];

struct Glyph {
    left: i32,
    top: i32,
    w: usize,
    h: usize,
    advance: f32,
    alpha: Vec<u8>,
}

#[derive(Default)]
struct Pen {
    lines: Vec<(Point, Point)>,
    quads: Vec<(Point, Point, Point)>,
    cubics: Vec<(Point, Point, Point, Point)>,
    start: Option<Point>,
    at: Point,
    min: (f32, f32),
    max: (f32, f32),
    any: bool,
}

impl Pen {
    fn p(&mut self, x: f32, y: f32) -> Point {
        let p = point(x, -y);
        if !self.any {
            self.min = (p.x, p.y);
            self.max = (p.x, p.y);
            self.any = true;
        }
        self.min = (self.min.0.min(p.x), self.min.1.min(p.y));
        self.max = (self.max.0.max(p.x), self.max.1.max(p.y));
        p
    }
}

impl OutlinePen for Pen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.close();
        let p = self.p(x, y);
        self.start = Some(p);
        self.at = p;
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let p = self.p(x, y);
        self.lines.push((self.at, p));
        self.at = p;
    }
    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        let (c, p) = (self.p(cx, cy), self.p(x, y));
        self.quads.push((self.at, c, p));
        self.at = p;
    }
    fn curve_to(&mut self, c0x: f32, c0y: f32, c1x: f32, c1y: f32, x: f32, y: f32) {
        let (c0, c1, p) = (self.p(c0x, c0y), self.p(c1x, c1y), self.p(x, y));
        self.cubics.push((self.at, c0, c1, p));
        self.at = p;
    }
    fn close(&mut self) {
        if let Some(s) = self.start.take()
            && (s.x != self.at.x || s.y != self.at.y)
        {
            self.lines.push((self.at, s));
            self.at = s;
        }
    }
}

/// The fonts and their glyph cache.
pub struct Type {
    fonts: Vec<FontRef<'static>>,
    /// (font, pixel size in quarter pixels, character, quarter-pixel x phase).
    cache: HashMap<(Font, u32, char, u8), Glyph>,
}

impl Default for Type {
    fn default() -> Self {
        Self::new()
    }
}

impl Type {
    pub fn new() -> Type {
        Type { fonts: FONTS.iter().map(|b| FontRef::new(b).expect("embedded font")).collect(), cache: HashMap::new() }
    }

    fn font(&self, f: Font) -> &FontRef<'static> {
        &self.fonts[f as usize]
    }

    fn glyph(&mut self, f: Font, px_q: u32, c: char, phase: u8) -> &Glyph {
        let font = self.fonts[f as usize].clone();
        self.cache.entry((f, px_q, c, phase)).or_insert_with(|| {
            let size = Size::new(px_q as f32 / 4.0);
            let id = font.charmap().map(c).unwrap_or(GlyphId::NOTDEF);
            let advance = font.glyph_metrics(size, LocationRef::default()).advance_width(id).unwrap_or(0.0);
            if id == GlyphId::NOTDEF {
                return Glyph { left: 0, top: 0, w: 0, h: 0, advance, alpha: Vec::new() };
            }
            let mut pen = Pen::default();
            let ok = font
                .outline_glyphs()
                .get(id)
                .map(|o| o.draw(DrawSettings::unhinted(size, LocationRef::default()), &mut pen).is_ok())
                .unwrap_or(false);
            pen.close();
            if !ok || !pen.any || (pen.lines.is_empty() && pen.quads.is_empty() && pen.cubics.is_empty()) {
                return Glyph { left: 0, top: 0, w: 0, h: 0, advance, alpha: Vec::new() };
            }
            let shift = phase as f32 / 4.0;
            let (left, top) = ((pen.min.0 + shift).floor(), pen.min.1.floor());
            let w = (pen.max.0 + shift - left).ceil() as usize + 1;
            let h = (pen.max.1 - top).ceil() as usize + 1;
            let o = |p: Point| point(p.x + shift - left, p.y - top);
            let mut r = Rasterizer::new(w, h);
            for &(a, b) in &pen.lines {
                r.draw_line(o(a), o(b));
            }
            for &(a, b, c) in &pen.quads {
                r.draw_quad(o(a), o(b), o(c));
            }
            for &(a, b, c, d) in &pen.cubics {
                r.draw_cubic(o(a), o(b), o(c), o(d));
            }
            let mut alpha = vec![0u8; w * h];
            r.for_each_pixel(|i, v| alpha[i] = (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
            Glyph { left: left as i32, top: top as i32, w, h, advance, alpha }
        })
    }

    fn px_q(cv: &Canvas, size: f32) -> u32 {
        (size * cv.k * 4.0).round().max(4.0) as u32
    }

    /// Width of `s` in dp at `size` dp with `tracking` dp added after each character but the last.
    pub fn measure(&mut self, cv: &Canvas, f: Font, size: f32, tracking: f32, s: &str) -> f32 {
        let q = Self::px_q(cv, size);
        let mut w = 0.0;
        let n = s.chars().count();
        for (i, c) in s.chars().enumerate() {
            w += self.glyph(f, q, c, 0).advance;
            if i + 1 < n {
                w += tracking * cv.k;
            }
        }
        w / cv.k
    }

    /// Height of capitals above the baseline, in dp.
    pub fn cap_height(&self, f: Font, size: f32) -> f32 {
        let m = self.font(f).metrics(Size::new(size), LocationRef::default());
        m.cap_height.unwrap_or(m.ascent * 0.7)
    }

    /// Draw `s` with its baseline at `y` from `x` (dp); returns the width in dp.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(&mut self, cv: &mut Canvas, f: Font, size: f32, tracking: f32, x: f32, y: f32, color: Color, s: &str) -> f32 {
        let q = Self::px_q(cv, size);
        let mut pen = (x + cv.origin.0) * cv.k;
        let base = ((y + cv.origin.1) * cv.k).round() as i32;
        let n = s.chars().count();
        for (i, c) in s.chars().enumerate() {
            let whole = pen.floor();
            let phase = ((pen - whole) * 4.0).round() as u8 % 4;
            let carry = if ((pen - whole) * 4.0).round() as u8 == 4 { 1 } else { 0 };
            let g = self.glyph(f, q, c, phase);
            let (gx, gy) = (whole as i32 + carry + g.left, base + g.top);
            let (gw, gh, adv) = (g.w, g.h, g.advance);
            // Copy out what blending needs: the cache borrow ends here.
            let alpha: Vec<u8> = g.alpha.clone();
            for row in 0..gh {
                for col in 0..gw {
                    let a = alpha[row * gw + col];
                    if a != 0 {
                        cv.blend_px(gx + col as i32, gy + row as i32, color, a as f32 / 255.0);
                    }
                }
            }
            pen += adv;
            if i + 1 < n {
                pen += tracking * cv.k;
            }
        }
        pen / cv.k - cv.origin.0 - x
    }

    /// Draw `s` centred on `cx` (dp).
    #[allow(clippy::too_many_arguments)]
    pub fn draw_centered(&mut self, cv: &mut Canvas, f: Font, size: f32, tracking: f32, cx: f32, y: f32, color: Color, s: &str) {
        let w = self.measure(cv, f, size, tracking, s);
        self.draw(cv, f, size, tracking, cx - w / 2.0, y, color, s);
    }

    /// Draw `s` ending at `right` (dp).
    #[allow(clippy::too_many_arguments)]
    pub fn draw_right(&mut self, cv: &mut Canvas, f: Font, size: f32, tracking: f32, right: f32, y: f32, color: Color, s: &str) {
        let w = self.measure(cv, f, size, tracking, s);
        self.draw(cv, f, size, tracking, right - w, y, color, s);
    }

    /// Words of `s` wrapped to `width` dp: the lines.
    pub fn wrap(&mut self, cv: &Canvas, f: Font, size: f32, width: f32, s: &str) -> Vec<String> {
        let mut lines = Vec::new();
        let mut line = String::new();
        for word in s.split_whitespace() {
            let try_line = if line.is_empty() { word.to_string() } else { format!("{line} {word}") };
            if self.measure(cv, f, size, 0.0, &try_line) > width && !line.is_empty() {
                lines.push(std::mem::take(&mut line));
                line = word.to_string();
            } else {
                line = try_line;
            }
        }
        if !line.is_empty() {
            lines.push(line);
        }
        lines
    }

    /// Wrapped text from the top line's baseline at `y`, `leading` dp apart; returns the baseline after the last line.
    #[allow(clippy::too_many_arguments)]
    pub fn paragraph(&mut self, cv: &mut Canvas, f: Font, size: f32, leading: f32, x: f32, y: f32, width: f32, color: Color, s: &str) -> f32 {
        let lines = self.wrap(cv, f, size, width, s);
        let mut by = y;
        for l in &lines {
            self.draw(cv, f, size, 0.0, x, by, color, l);
            by += leading;
        }
        by - leading
    }
}
