//! The crash screen: one page that a phone photo can read, about 100 columns wide, drawn straight into the firmware's
//! framebuffer without a console, a lock or a heap.

use core::fmt::{self, Write};

use font8x8::legacy::BASIC_LEGACY;

use crate::{frames::Frame, report::Facts};

/// Where the pixels go. Colours are `0xRRGGBB`; the kernel's side knows the framebuffer's format.
pub trait Canvas {
    /// Width and height in pixels.
    fn size(&self) -> (usize, usize);
    /// Sets a pixel inside the size.
    fn put(&mut self, x: usize, y: usize, rgb: u32);
}

pub const BACKGROUND: u32 = 0x07_0b_18;
pub const FOREGROUND: u32 = 0xff_ff_ff;
const STATUS: u32 = 0xff_9f_0a;

const GLYPH: usize = 8;
/// Text cells of the headline, in normal rows: two lines of the font drawn twice as large.
pub const HEADLINE_ROWS: usize = 4;
/// The first row of the backtrace, under the headline and a blank row.
pub const FRAMES_ROW: usize = HEADLINE_ROWS + 1;
/// Bytes of the headline at most: four rows of 120 columns, on the stack of a crash.
const HEADLINE_SIZE: usize = 512;
const MARGIN: usize = 1;

/// How the text falls on a screen.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Layout {
    /// Pixels per font pixel, across and down.
    pub scale_x: usize,
    pub scale_y: usize,
    /// Text cells that fit, margins taken off.
    pub columns: usize,
    pub rows: usize,
}

impl Layout {
    /// As large as still gives about 100 columns: a 1920-wide screen gets 120 columns, 1366 or 800 stay at the font's
    /// width, and a font pixel is then twice as tall as wide if the height allows it, to stay readable.
    pub fn new(width: usize, height: usize) -> Self {
        let scale_x = (width / (100 * GLYPH)).max(1);
        let scale_y = if scale_x == 1 && height >= 480 {
            2
        } else {
            scale_x
        };
        Self {
            scale_x,
            scale_y,
            columns: (width / (GLYPH * scale_x)).saturating_sub(2 * MARGIN),
            rows: (height / (GLYPH * scale_y)).saturating_sub(2 * MARGIN),
        }
    }

    /// The row of the footer: the last.
    pub fn footer_row(&self) -> usize {
        self.rows.saturating_sub(1)
    }
}

/// Draws one character at a text cell, on the background.
fn glyph<C: Canvas>(
    c: &mut C,
    l: &Layout,
    column: usize,
    row: usize,
    ch: u8,
    rgb: u32,
    big: usize,
) {
    let bitmap = BASIC_LEGACY[if ch < 128 { ch as usize } else { b'?' as usize }];
    let (sx, sy) = (l.scale_x * big, l.scale_y * big);
    let x0 = (MARGIN + column) * GLYPH * l.scale_x;
    let y0 = (MARGIN + row) * GLYPH * l.scale_y;
    let (width, height) = c.size();
    if x0 + GLYPH * sx > width || y0 + GLYPH * sy > height {
        return;
    }
    for (fy, bits) in bitmap.iter().enumerate() {
        for fx in 0..GLYPH {
            let on = bits >> fx & 1 != 0;
            for dy in 0..sy {
                for dx in 0..sx {
                    c.put(
                        x0 + fx * sx + dx,
                        y0 + fy * sy + dy,
                        if on { rgb } else { BACKGROUND },
                    );
                }
            }
        }
    }
}

fn fill<C: Canvas>(c: &mut C, rgb: u32) {
    let (width, height) = c.size();
    for y in 0..height {
        for x in 0..width {
            c.put(x, y, rgb);
        }
    }
}

/// Writes one row of text, cut at the screen's edge; the rest of the row is blanked, so that a shorter text replaces a
/// longer one.
struct Line<'c, C: Canvas> {
    canvas: &'c mut C,
    layout: Layout,
    row: usize,
    column: usize,
    rgb: u32,
}

impl<C: Canvas> Line<'_, C> {
    fn put(&mut self, b: u8) {
        if self.column < self.layout.columns && self.row < self.layout.rows {
            let b = if b == b'\n' || b == b'\t' { b' ' } else { b };
            glyph(self.canvas, &self.layout, self.column, self.row, b, self.rgb, 1);
        }
        self.column += 1;
    }

    fn finish(mut self) {
        while self.column < self.layout.columns {
            self.put(b' ');
        }
    }
}

impl<C: Canvas> Write for Line<'_, C> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        s.bytes().for_each(|b| self.put(b));
        Ok(())
    }
}

/// Draws the whole page: the headline (what crashed and why), the frames that matter, innermost first, and the footer
/// (boot, uptime, what happens next, where the full record is), which [`draw_status`] replaces. The registers, the log
/// and every address stay in the saved record and on the console, not here.
pub fn draw<C: Canvas>(canvas: &mut C, facts: &Facts<'_>, frames: &[Frame<'_>], footer: &str) -> Layout {
    let (width, height) = canvas.size();
    let layout = Layout::new(width, height);
    fill(canvas, BACKGROUND);

    let mut headline = [0u8; HEADLINE_SIZE];
    let mut buf = crate::text::Buf::new(&mut headline);
    let _ = facts.write_headline(&mut buf);
    // Two large lines, broken at the screen's width; a longer one in the normal font, over the same four rows.
    let text = buf.as_bytes();
    let big = if text.len() <= 2 * (layout.columns / 2) { 2 } else { 1 };
    let per_line = (layout.columns / big).max(1);
    for (i, &b) in text.iter().take(HEADLINE_ROWS / big * per_line).enumerate() {
        let (line, column) = (i / per_line, i % per_line);
        glyph(canvas, &layout, column * big, line * big, b, FOREGROUND, big);
    }

    // The frames, one a row, down to the row above the footer's blank one.
    let last = layout.footer_row().saturating_sub(1);
    let frames = crate::frames::that_matter(frames);
    if frames.is_empty() && FRAMES_ROW < last {
        let mut line = Line { canvas: &mut *canvas, layout, row: FRAMES_ROW, column: 0, rgb: FOREGROUND };
        let _ = line.write_str("(no backtrace)");
    }
    for (i, frame) in frames.iter().enumerate() {
        let row = FRAMES_ROW + i;
        if row >= last {
            break;
        }
        let mut text = [0u8; FRAME_SIZE];
        let mut buf = crate::text::Buf::new(&mut text);
        let _ = write!(buf, "{frame}");
        let mut line = Line { canvas: &mut *canvas, layout, row, column: 0, rgb: FOREGROUND };
        let _ = line.write_str("  ");
        write_cut(&mut line, buf.as_bytes(), layout.columns.saturating_sub(2));
    }
    draw_status(canvas, &layout, footer);
    layout
}

/// Bytes of one frame's text at most.
const FRAME_SIZE: usize = 256;

/// Writes `text` in `room` cells: whole if it fits, else its start, `...` and its end, so that a long qualified name
/// keeps both what it is and the method and offset at its end.
fn write_cut<C: Canvas>(line: &mut Line<'_, C>, text: &[u8], room: usize) {
    if text.len() <= room {
        text.iter().for_each(|&b| line.put(b));
        return;
    }
    let tail = room / 3;
    let head = room.saturating_sub(tail + 3);
    text[..head].iter().for_each(|&b| line.put(b));
    b"...".iter().take(room - head).for_each(|&b| line.put(b));
    text[text.len() - tail.min(text.len())..].iter().for_each(|&b| line.put(b));
}

/// Replaces the footer. A countdown calls this once a second.
pub fn draw_status<C: Canvas>(canvas: &mut C, layout: &Layout, footer: &str) {
    if layout.rows <= FRAMES_ROW {
        return;
    }
    let mut line = Line { canvas, layout: *layout, row: layout.footer_row(), column: 0, rgb: STATUS };
    let _ = line.write_str(footer);
    line.finish();
}

/// Reads a text row back from the pixels, by comparing each cell with the font: what a phone photo of the page would show,
/// for tests. `pixel` gives the colour at a position. Writes the characters to `out` (a cell that is no character becomes
/// `~`) and returns how many, without the blanks at the end. `big` is 2 for the title, 1 for the rest.
pub fn read_row(
    pixel: impl Fn(usize, usize) -> u32,
    l: &Layout,
    row: usize,
    big: usize,
    out: &mut [u8],
) -> usize {
    let mut end = 0;
    for column in 0..(l.columns / big).min(out.len()) {
        let x0 = (MARGIN + column * big) * GLYPH * l.scale_x;
        let y0 = (MARGIN + row) * GLYPH * l.scale_y;
        let mut seen = [0u8; GLYPH];
        for (fy, bits) in seen.iter_mut().enumerate() {
            for fx in 0..GLYPH {
                if pixel(x0 + fx * big * l.scale_x, y0 + fy * big * l.scale_y) != BACKGROUND {
                    *bits |= 1 << fx;
                }
            }
        }
        let ch = BASIC_LEGACY[..128]
            .iter()
            .position(|glyph| *glyph == seen)
            .map_or(b'~', |c| c as u8);
        // The font draws several control characters as blanks: the first of them is found first.
        let ch = if seen == [0; GLYPH] { b' ' } else { ch };
        out[column] = ch;
        if ch != b' ' {
            end = column + 1;
        }
    }
    end
}

#[cfg(test)]
mod tests {
    extern crate std;
    use std::{string::String, vec, vec::Vec};

    use super::*;
    use crate::{frames, report::{Kind, Regs}};

    struct Frame {
        width: usize,
        height: usize,
        pixels: Vec<u32>,
    }

    impl Frame {
        fn new(width: usize, height: usize) -> Self {
            Self {
                width,
                height,
                pixels: vec![0; width * height],
            }
        }

        fn read_row(&self, l: &Layout, row: usize) -> String {
            let mut out = [0u8; 300];
            let len = read_row(|x, y| self.pixels[y * self.width + x], l, row, 1, &mut out);
            String::from_utf8_lossy(&out[..len]).into_owned()
        }

        fn read_all(&self, l: &Layout) -> Vec<String> {
            (0..l.rows).map(|row| self.read_row(l, row)).collect()
        }
    }

    impl Canvas for Frame {
        fn size(&self) -> (usize, usize) {
            (self.width, self.height)
        }

        fn put(&mut self, x: usize, y: usize, rgb: u32) {
            assert!(
                x < self.width && y < self.height,
                "pixel off the screen: {x},{y}"
            );
            self.pixels[y * self.width + x] = rgb;
        }
    }

    fn facts<'a>(reason: &'a str, regs: &'a Regs, backtrace: &'a [u64]) -> Facts<'a> {
        Facts {
            kernel_name: "LucinaJuno",
            kind: Kind::Panic,
            reason,
            cpu: 0,
            tid: Some(3),
            uptime_ms: 1500,
            boot: Some(4),
            build: "0.19.0 juno-abc",
            regs: Some(regs),
            backtrace,
        }
    }

    /// The headline as a photo reads it: the two large lines, or the four normal ones.
    fn headline(frame: &Frame, l: &Layout) -> String {
        for (big, rows) in [(2, [0, 2].as_slice()), (1, [0, 1, 2, 3].as_slice())] {
            let mut text = String::new();
            for &row in rows {
                let mut out = [0u8; 300];
                let len = read_row(|x, y| frame.pixels[y * frame.width + x], l, row, big, &mut out);
                text.push_str(&String::from_utf8_lossy(&out[..len]));
            }
            if !text.contains('~') {
                return text;
            }
        }
        String::from("~")
    }

    fn named(names: &[&'static str]) -> Vec<frames::Frame<'static>> {
        names
            .iter()
            .enumerate()
            .map(|(i, &name)| frames::Frame { pc: 0xffff_ffff_a000_0000 + i as u64 * 0x40, name: Some(name), offset: 0x1c })
            .collect()
    }

    const FOOTER: &str = "boot 4 | uptime 1.5 s | Restarting in 30 s | full record: lucina-crash list";

    #[test]
    fn layouts_of_real_screens() {
        let wide = Layout::new(1920, 1080);
        assert_eq!((wide.scale_x, wide.scale_y, wide.columns), (2, 2, 118));
        let laptop = Layout::new(1366, 768);
        assert_eq!(
            (laptop.scale_x, laptop.scale_y, laptop.columns),
            (1, 2, 168)
        );
        let qemu = Layout::new(800, 600);
        assert_eq!(
            (qemu.scale_x, qemu.scale_y, qemu.columns, qemu.rows),
            (1, 2, 98, 35)
        );
        assert_eq!(Layout::new(640, 400).scale_y, 1);
    }

    #[test]
    fn page_has_the_headline_the_frames_and_the_footer_on_every_size() {
        let regs = Regs {
            pc: 0xffff_8000_0001_2345,
            ..Regs::default()
        };
        let backtrace = [0xffff_8000_0000_0001u64; 14];
        let f = facts("a panic on request at crash_test.rs:12:9", &regs, &backtrace);
        let frames = named(&["ostd::crash::crash", "rust_begin_unwind", "core::panicking::panic_fmt", "aster_kernel::fs::procfs::crash_test::write", "aster_kernel::syscall::write::sys_write", "trap_handler", "trap_common"]);
        for (w, h) in [(800, 600), (1280, 800), (1366, 768), (1920, 1080), (640, 400)] {
            let mut frame = Frame::new(w, h);
            let l = draw(&mut frame, &f, &frames, FOOTER);
            let head = headline(&frame, &l);
            assert!(head.starts_with("Kernel panic: a panic on request"), "{w}x{h}: {head}");
            let text = frame.read_all(&l);
            assert_eq!(text[FRAMES_ROW], "  aster_kernel::fs::procfs::crash_test::write+0x1c", "{w}x{h}: {text:#?}");
            assert_eq!(text[FRAMES_ROW + 1], "  aster_kernel::syscall::write::sys_write+0x1c", "{w}x{h}");
            assert!(text[FRAMES_ROW + 2].is_empty(), "{w}x{h}: the trap entry is cut");
            assert!(!text.iter().any(|r| r.contains("rust_begin_unwind")), "{w}x{h}: the machinery is cut");
            assert_eq!(text[l.footer_row()], &FOOTER[..FOOTER.len().min(l.columns)], "{w}x{h}");
            // Nothing else: no registers, no log.
            assert!(!text.iter().any(|r| r.contains("ffff800000012345") || r.contains("x19")), "{w}x{h}");
        }
    }

    #[test]
    fn a_long_headline_takes_the_normal_font_and_is_cut() {
        let reason = "r".repeat(1000);
        let regs = Regs::default();
        let f = facts(&reason, &regs, &[]);
        let mut frame = Frame::new(800, 600);
        let l = draw(&mut frame, &f, &[], FOOTER);
        let head = headline(&frame, &l);
        assert_eq!(head.len(), (HEADLINE_ROWS * l.columns).min(HEADLINE_SIZE));
        assert!(head.starts_with("Kernel panic: rrr"));
        // One that fits two large lines stays large.
        let short = "s".repeat(2 * (l.columns / 2) - "Kernel panic: ".len());
        let f = facts(&short, &regs, &[]);
        let mut frame = Frame::new(800, 600);
        draw(&mut frame, &f, &[], FOOTER);
        let mut out = [0u8; 300];
        let len = read_row(|x, y| frame.pixels[y * frame.width + x], &l, 2, 2, &mut out);
        assert_eq!(len, l.columns / 2);
        let text = frame.read_all(&l);
        assert_eq!(text[FRAMES_ROW], "(no backtrace)");
    }

    #[test]
    fn without_names_the_addresses_show() {
        let regs = Regs::default();
        let f = facts("x", &regs, &[]);
        let bare = [frames::Frame { pc: 0xffff_ffff_a08c_fe40, name: None, offset: 0 }; 3];
        let mut frame = Frame::new(800, 600);
        let l = draw(&mut frame, &f, &bare, FOOTER);
        let text = frame.read_all(&l);
        assert_eq!(text[FRAMES_ROW], "  ffffffffa08cfe40");
        assert_eq!(text[FRAMES_ROW + 2], "  ffffffffa08cfe40");
    }

    #[test]
    fn a_long_frame_keeps_its_start_and_its_end() {
        let regs = Regs::default();
        let f = facts("x", &regs, &[]);
        let long = "<aster_core::fs::fs_impls::procfs::crash_test::CrashTestFileOps as aster_core::fs::file::file_handle::FileOps>::write";
        let frames = named(&[long]);
        let mut frame = Frame::new(800, 600);
        let l = draw(&mut frame, &f, &frames, FOOTER);
        let text = frame.read_all(&l);
        let row = &text[FRAMES_ROW];
        assert_eq!(row.len(), l.columns, "{row}");
        assert!(row.starts_with("  <aster_core::fs::fs_impls::procfs::crash_test::"), "{row}");
        assert!(row.contains("..."), "{row}");
        assert!(row.ends_with("FileOps>::write+0x1c"), "{row}");
    }

    #[test]
    fn many_frames_stop_above_the_footer() {
        let regs = Regs::default();
        let f = facts("x", &regs, &[]);
        let names: Vec<&'static str> = (0..100).map(|i| &*std::boxed::Box::leak(std::format!("aster_kernel::f{i}").into_boxed_str())).collect();
        let frames = named(&names);
        let mut frame = Frame::new(800, 600);
        let l = draw(&mut frame, &f, &frames, FOOTER);
        let text = frame.read_all(&l);
        assert!(text[l.footer_row() - 1].is_empty(), "a blank row above the footer");
        assert_eq!(text[l.footer_row()], FOOTER);
        assert!(text[l.footer_row() - 2].starts_with("  aster_kernel::f"));
    }

    #[test]
    fn tiny_screen_does_not_draw_off_it() {
        let regs = Regs::default();
        let f = facts("x", &regs, &[1, 2, 3]);
        let frames = named(&["a::b", "c::d"]);
        for (w, h) in [(160, 100), (8, 8), (1, 1), (64, 48), (0, 0)] {
            let mut frame = Frame::new(w, h);
            draw(&mut frame, &f, &frames, "status");
        }
    }

    #[test]
    fn status_line_is_replaced() {
        let regs = Regs::default();
        let f = facts("x", &regs, &[]);
        let mut frame = Frame::new(800, 600);
        let l = draw(&mut frame, &f, &[], FOOTER);
        draw_status(&mut frame, &l, "boot 4 | Restarting in 9 s");
        assert_eq!(frame.read_row(&l, l.footer_row()), "boot 4 | Restarting in 9 s");
    }
}
