//! The LucinaOS theme drawn into a screen-sized buffer of 0x00RRGGBB pixels, which `efi::Display::show` copies to
//! the screen. Positions and colours are `lucina_boot::menu::Layout` and `menu::colour`.

use core::fmt::{self, Write};

use lucina_boot::menu::{EDGE_W, Layout, Menu, Rect, Text, colour, first_visible};

use crate::font::{self, Font};

pub struct Canvas<'a> {
    px: &'a mut [u32],
    w: u32,
    h: u32,
}

impl<'a> Canvas<'a> {
    /// `px` holds at least `w * h` pixels.
    pub fn new(px: &'a mut [u32], w: u32, h: u32) -> Option<Canvas<'a>> {
        (px.len() >= w as usize * h as usize).then_some(Canvas { px, w, h })
    }

    pub fn pixels(&mut self) -> &mut [u32] {
        self.px
    }

    pub fn fill(&mut self, r: Rect, c: u32) {
        let x1 = r.x.saturating_add(r.w).min(self.w);
        let y1 = r.y.saturating_add(r.h).min(self.h);
        for y in r.y.min(y1)..y1 {
            let row = (y * self.w) as usize;
            self.px[row + r.x.min(x1) as usize..row + x1 as usize].fill(c);
        }
    }

    pub fn clear(&mut self, c: u32) {
        self.fill(
            Rect {
                x: 0,
                y: 0,
                w: self.w,
                h: self.h,
            },
            c,
        );
    }

    /// Draws `s` with the pen at `t.x` and the font's ascender line at `t.top`, clipped at `max_x`; returns where
    /// the pen ends.
    pub fn text(&mut self, t: Text, c: u32, s: &str, max_x: u32) -> u32 {
        let f = font::face(t.face);
        let baseline = t.top as i32 + f.ascent;
        let max_x = max_x.min(self.w) as i32;
        let mut pen = i64::from(t.x) * 64;
        for ch in s.chars() {
            let g = f.glyph(ch);
            let x0 = (pen / 64) as i32 + i32::from(g.left);
            self.glyph(f, g, x0, baseline - i32::from(g.top), c, max_x);
            pen += i64::from(g.advance);
        }
        (pen / 64) as u32
    }

    fn glyph(&mut self, f: &Font, g: &font::Glyph, x0: i32, y0: i32, c: u32, max_x: i32) {
        let (gw, gh) = (i32::from(g.w), i32::from(g.h));
        let map = f.map(g);
        for gy in 0..gh {
            let y = y0 + gy;
            if y < 0 || y >= self.h as i32 {
                continue;
            }
            for gx in 0..gw {
                let x = x0 + gx;
                if x < 0 || x >= max_x {
                    continue;
                }
                let a = u32::from(map[(gy * gw + gx) as usize]);
                if a == 0 {
                    continue;
                }
                let at = (y as u32 * self.w + x as u32) as usize;
                self.px[at] = blend(self.px[at], c, a);
            }
        }
    }
}

/// `fg` over `bg` with coverage `a` (0..=255), per channel.
fn blend(bg: u32, fg: u32, a: u32) -> u32 {
    let ch = |shift: u32| {
        let (b, f) = ((bg >> shift) & 0xff, (fg >> shift) & 0xff);
        ((b * (255 - a) + f * a + 127) / 255) << shift
    };
    ch(16) | ch(8) | ch(0)
}

/// A line formatted into a fixed buffer (no heap).
pub struct Line {
    buf: [u8; 160],
    len: usize,
}

impl Line {
    pub fn new() -> Line {
        Line {
            buf: [0; 160],
            len: 0,
        }
    }

    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }
}

impl Write for Line {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for c in s.chars() {
            let mut b = [0; 4];
            let e = c.encode_utf8(&mut b);
            if self.len + e.len() > self.buf.len() {
                break;
            }
            self.buf[self.len..self.len + e.len()].copy_from_slice(e.as_bytes());
            self.len += e.len();
        }
        Ok(())
    }
}

/// The menu as GRUB's gfxmenu drew `theme.txt`: title, subtitle, entries with the selected one on the navy bar
/// with its gold edge, and while counting down the line "Starting in N s." over the gold progress bar.
pub fn menu<'t>(
    c: &mut Canvas<'_>,
    l: &Layout,
    title: &str,
    subtitle: &str,
    items: impl Iterator<Item = &'t str>,
    m: &Menu,
) {
    let right = l.bar.x + l.bar.w;
    c.clear(colour::DESKTOP);
    c.text(l.title, colour::TITLE, title, right);
    c.text(l.subtitle, colour::SUBTITLE, subtitle, right);
    let first = first_visible(m.selected, l.items_fit);
    for (n, item) in items.enumerate().skip(first).take(l.items_fit) {
        let r = l.item_rect(n - first);
        let mut t = Text {
            x: l.item_text_x,
            top: r.y + l.item_text_dy,
            face: lucina_boot::menu::Face::Regular20,
        };
        if n == m.selected {
            c.fill(r, colour::SELECT);
            c.fill(Rect { w: EDGE_W, ..r }, colour::SELECT_EDGE);
            t.face = lucina_boot::menu::Face::Bold20;
            c.text(t, colour::ITEM_SELECTED, item, r.x + r.w);
        } else {
            c.text(t, colour::ITEM, item, r.x + r.w);
        }
    }
    if let (Some(s), Some(fill)) = (m.seconds_left(), m.bar_fill(l.bar.w.saturating_sub(2))) {
        let mut line = Line::new();
        let _ = write!(line, "Starting in {s} s. Arrow keys and Enter to choose.");
        c.text(l.timeout, colour::TIMEOUT, line.as_str(), right);
        c.fill(l.bar, colour::BAR_BACK);
        let inner = Rect {
            x: l.bar.x + 1,
            y: l.bar.y + 1,
            w: fill,
            h: l.bar.h.saturating_sub(2),
        };
        c.fill(inner, colour::BAR);
    }
}

/// The screen while an entry loads, or when it failed: the title and lines of text (GRUB's terminal showed the
/// entry's `echo`, then its errors).
pub fn message(c: &mut Canvas<'_>, l: &Layout, title: &str, lines: &[&str]) {
    let right = l.bar.x + l.bar.w;
    c.clear(colour::DESKTOP);
    c.text(l.title, colour::TITLE, title, right);
    let mut top = l.item.y;
    for s in lines {
        let t = Text {
            x: l.title.x,
            top,
            face: lucina_boot::menu::Face::Regular16,
        };
        c.text(t, colour::MESSAGE, s, right);
        top += 26;
    }
}
