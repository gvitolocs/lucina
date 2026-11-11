//! One picture on a Juno screen, built once for every app that shows pictures: the viewer, Gallery and Drive.
//!
//! [`load`] turns a file's bytes into an [`Image`] the right way up, on a thread. [`PictureView`] draws it fitted or zoomed and takes the
//! keys, wheel and drag for it. What surrounds the picture (walking between files, info panels, the server) belongs to the app.
//!
//! The zoom and pan themselves are `secretary_image::view`, shared with every Secretary app.

use lucina_app::image::ImageError;
use lucina_app::{Image, Input, Rect, Ui, keys};
use secretary_image::view::{TAP_SCALE, View};

/// Below this scale a picture counts as fitted.
const FITTED: f32 = 1.01;
/// One press of a zoom key, or one notch of the wheel.
const STEP: f32 = 1.25;
/// A press that moves further than this is a drag, not a click.
const DRAG_PX: i32 = 4;

// Evdev codes the toolkit's `keys` does not name yet.
const KEY_0: u16 = 11;
const KEY_MINUS: u16 = 12;
const KEY_EQUAL: u16 = 13;
const KEY_KPMINUS: u16 = 74;
const KEY_KPPLUS: u16 = 78;

/// How a file's bytes were turned into a picture, for the log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Jpeg,
    Png,
    Webp,
    JpegXl,
}

impl Format {
    pub fn name(self) -> &'static str {
        match self {
            Format::Jpeg => "JPEG",
            Format::Png => "PNG",
            Format::Webp => "WebP",
            Format::JpegXl => "JPEG XL",
        }
    }
}

pub fn sniff(bytes: &[u8]) -> Option<Format> {
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some(Format::Jpeg)
    } else if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        Some(Format::Png)
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some(Format::Webp)
    } else if bytes.starts_with(&[0xff, 0x0a]) || bytes.starts_with(&[0, 0, 0, 0x0c, b'J', b'X', b'L', b' ']) {
        Some(Format::JpegXl)
    } else {
        None
    }
}

/// Whether a file name looks like a picture this can read. The first bytes decide in the end.
pub fn is_picture_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    [".jpg", ".jpeg", ".png", ".webp", ".jxl"].iter().any(|e| name.ends_with(e))
}

/// The EXIF orientation of a JPEG, 1 to 8. 1, the upright value, when there is none.
pub fn jpeg_orientation(b: &[u8]) -> u8 {
    let mut at = 2;
    while at + 4 <= b.len() && b[at] == 0xff {
        let marker = b[at + 1];
        // The scan starts here: nothing further can hold EXIF.
        if marker == 0xda {
            break;
        }
        let len = u16::from_be_bytes([b[at + 2], b[at + 3]]) as usize;
        if len < 2 || at + 2 + len > b.len() {
            break;
        }
        if marker == 0xe1 && b[at + 4..at + 2 + len].starts_with(b"Exif\0\0") {
            return tiff_orientation(&b[at + 10..at + 2 + len]).unwrap_or(1);
        }
        at += 2 + len;
    }
    1
}

fn tiff_orientation(t: &[u8]) -> Option<u8> {
    let little = match t.get(..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let u16_at = |i: usize| -> Option<u16> {
        let s: [u8; 2] = t.get(i..i + 2)?.try_into().ok()?;
        Some(if little { u16::from_le_bytes(s) } else { u16::from_be_bytes(s) })
    };
    let u32_at = |i: usize| -> Option<u32> {
        let s: [u8; 4] = t.get(i..i + 4)?.try_into().ok()?;
        Some(if little { u32::from_le_bytes(s) } else { u32::from_be_bytes(s) })
    };
    let ifd = u32_at(4)? as usize;
    let n = u16_at(ifd)? as usize;
    for i in 0..n {
        let e = ifd + 2 + i * 12;
        if u16_at(e)? == 0x0112 {
            let v = u16_at(e + 8)?;
            return (1..=8).contains(&v).then_some(v as u8);
        }
    }
    None
}

/// `img` as EXIF orientation `o` says it should look.
pub fn oriented(img: Image, o: u8) -> Image {
    if !(2..=8).contains(&o) {
        return img;
    }
    let (w, h) = (img.w as usize, img.h as usize);
    let (nw, nh) = if o >= 5 { (h, w) } else { (w, h) };
    let mut px = vec![0u32; w * h];
    for y in 0..h {
        for x in 0..w {
            let (nx, ny) = match o {
                2 => (w - 1 - x, y),
                3 => (w - 1 - x, h - 1 - y),
                4 => (x, h - 1 - y),
                5 => (y, x),
                6 => (h - 1 - y, x),
                7 => (h - 1 - y, w - 1 - x),
                _ => (y, w - 1 - x),
            };
            px[ny * nw + nx] = img.px[y * w + x];
        }
    }
    Image { w: nw as u32, h: nh as u32, px }
}

/// Pixels with alpha, laid on `bg`, so that everything after this may copy them.
pub fn flattened(mut img: Image, bg: u32) -> Image {
    if img.px.iter().all(|p| p >> 24 == 0xff) {
        return img;
    }
    for p in &mut img.px {
        let a = *p >> 24;
        let over = |shift: u32| {
            let (s, d) = ((*p >> shift) & 0xff, (bg >> shift) & 0xff);
            ((s * a + d * (255 - a)) / 255) << shift
        };
        *p = 0xff00_0000 | over(16) | over(8) | over(0);
    }
    img
}

fn from_rgba(f: &secretary_image::Frame) -> Image {
    let px = f.rgba.chunks_exact(4).map(|p| (p[3] as u32) << 24 | (p[0] as u32) << 16 | (p[1] as u32) << 8 | p[2] as u32).collect();
    Image { w: f.width, h: f.height, px }
}

/// A whole picture from its bytes, the right way up. On a thread: this is the slow part.
///
/// `on_pass` sees each better picture of a progressive JPEG XL as it renders, for a viewer that wants to show something early. A large
/// file does a few passes, not one per chunk: each render costs about as much as the last.
pub fn load(bytes: &[u8], mut on_pass: impl FnMut(Image)) -> Result<Image, String> {
    match sniff(bytes) {
        Some(Format::JpegXl) => {
            let mut d = secretary_image::Decoder::new();
            let chunk = (bytes.len() / 4).max(64 * 1024);
            d.render_step = chunk;
            // The header first, and nothing rendered yet: a JPEG XL names its own size, and the decoder allocates what it says when it renders.
            let (head, mut rest) = bytes.split_at(bytes.len().min(4096));
            d.feed(head, false).map_err(|e| e.to_string())?;
            if let Some((w, h)) = d.dimensions()
                && w as u64 * h as u64 > lucina_app::image::MAX_PIXELS
            {
                return Err(format!("image too large: {w} x {h}"));
            }
            loop {
                let (now, later) = rest.split_at(chunk.min(rest.len()));
                rest = later;
                let better = d.feed(now, rest.is_empty()).map_err(|e| e.to_string())?;
                // The header may have been cut across 4096 bytes: the same check as soon as it is whole.
                if let Some((w, h)) = d.dimensions()
                    && w as u64 * h as u64 > lucina_app::image::MAX_PIXELS
                {
                    return Err(format!("image too large: {w} x {h}"));
                }
                if rest.is_empty() {
                    break;
                }
                if better && let Some(f) = d.frame() {
                    on_pass(from_rgba(f));
                }
            }
            d.frame().map(from_rgba).ok_or_else(|| "nothing to show".to_owned())
        }
        Some(Format::Jpeg) => {
            let o = jpeg_orientation(bytes);
            Image::decode(bytes).map(|i| oriented(i, o)).map_err(|e| e.to_string())
        }
        None => Err("not a JPEG, PNG, WebP or JPEG XL file".to_owned()),
        _ => Image::decode(bytes).map_err(|e: ImageError| e.to_string()),
    }
}

/// What the input did, for the app to act on what is not the picture's.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Response {
    /// The picture took the event: it moved, or the zoom changed.
    pub used: bool,
    /// A click that was not a drag, on a picture that stays as it is.
    pub clicked: bool,
}

/// One picture on the screen, fitted or zoomed. Keep one in the app's state.
#[derive(Default)]
pub struct PictureView {
    /// The largest form of the picture that has arrived, for zooming.
    source: Option<Image>,
    /// `source` at half size, a quarter, and so on, made when first drawn that small.
    mips: Vec<Image>,
    view: Option<View>,
    buf: Vec<u32>,
    rect: Option<Rect>,
    dirty: bool,
    drag: Option<(i32, i32)>,
    dragged: bool,
    /// Smooth-scroll pixels not yet a whole notch (40 logical px), kept between events.
    scroll_rem: i32,
}

impl PictureView {
    pub fn new() -> PictureView {
        PictureView::default()
    }

    /// Forget everything: another picture is coming.
    pub fn clear(&mut self) {
        *self = PictureView::new();
    }

    /// The form of the picture to zoom into. Pass the original when it arrives; the zoom keeps its place.
    pub fn set_source(&mut self, img: Image) {
        self.source = Some(img);
        self.mips.clear();
        self.dirty = true;
    }

    pub fn source(&self) -> Option<&Image> {
        self.source.as_ref()
    }

    pub fn is_zoomed(&self) -> bool {
        self.view.is_some_and(|v| v.scale > FITTED)
    }

    /// The zoom, for an app that asks for a larger form when the drawn size has outrun the one it has.
    pub fn view(&self) -> Option<&View> {
        self.view.as_ref()
    }

    /// Back to fitted. The memory for the zoomed pixels goes too.
    pub fn fit(&mut self) {
        self.view = None;
        self.buf = Vec::new();
        self.mips.clear();
        self.dirty = true;
    }

    /// Multiply the zoom by `factor`, keeping the point at (`ax`, `ay`) inside `r` still. `base` is the picture's size if nothing has
    /// arrived for zooming yet.
    pub fn zoom_by(&mut self, factor: f32, ax: f32, ay: f32, base: (u32, u32), r: Rect) {
        let v = self.view.unwrap_or_else(|| View::new(base.0 as f32, base.1 as f32, r.w as f32, r.h as f32));
        let v = v.pinch(factor, ax, ay);
        if v.scale > FITTED {
            self.view = Some(v);
            self.dirty = true;
        } else {
            self.fit();
        }
    }

    /// Fitted: in on this point. Zoomed: out again.
    pub fn toggle(&mut self, ax: f32, ay: f32, base: (u32, u32), r: Rect) {
        if self.is_zoomed() {
            self.fit();
        } else {
            self.zoom_by(TAP_SCALE, ax, ay, base, r);
        }
    }

    pub fn pan(&mut self, dx: f32, dy: f32) {
        if let Some(v) = self.view {
            self.view = Some(v.pan(dx, dy));
            self.dirty = true;
        }
    }

    /// The wheel, a drag and a click on `r`, the zoom keys, and the arrows while zoomed. Call before drawing, in every pass. An app whose
    /// arrows walk between pictures sees them here when not zoomed.
    pub fn input(&mut self, ui: &mut Ui, id: &'static str, r: Rect, base: (u32, u32)) -> Response {
        let mut out = Response::default();
        let (px, py) = ui.pointer();
        let hit = ui.hit(id, r);
        let at = (px - r.x, py - r.y);

        if let Some(Input::Wheel(d)) = ui.input()
            && d != 0
            && r.contains(px, py)
        {
            self.zoom_by(if d > 0 { STEP } else { 1.0 / STEP }, at.0 as f32, at.1 as f32, base, r);
            ui.take_input();
            out.used = true;
        }

        // Smooth scroll: one zoom notch per 40 logical pixels of it, the rest kept for the next event.
        if let Some(Input::Scroll { dy, .. }) = ui.input()
            && dy != 0
            && r.contains(px, py)
        {
            self.scroll_rem += dy;
            let notch = ui.theme().px(40.0);
            let d = self.scroll_rem / notch;
            if d != 0 {
                self.scroll_rem -= d * notch;
                self.zoom_by(if d > 0 { STEP } else { 1.0 / STEP }, at.0 as f32, at.1 as f32, base, r);
            }
            ui.take_input();
            out.used = true;
        }

        if hit.pressed {
            if let Some((x0, y0)) = self.drag {
                if (px - x0).abs() + (py - y0).abs() >= DRAG_PX || self.dragged {
                    self.dragged = true;
                    if (px, py) != (x0, y0) {
                        self.pan((px - x0) as f32, (py - y0) as f32);
                        self.drag = Some((px, py));
                        out.used = true;
                    }
                }
            } else {
                self.drag = Some((px, py));
                self.dragged = false;
            }
        } else {
            self.drag = None;
        }
        if hit.clicked {
            if self.dragged {
                self.dragged = false;
            } else {
                out.clicked = true;
            }
        }

        if let Some(p) = ui.press() {
            let plain = !p.mods.ctrl && !p.mods.alt && !p.mods.meta;
            let centre = (r.w as f32 / 2.0, r.h as f32 / 2.0);
            let step = r.w.min(r.h) as f32 / 8.0;
            let used = plain
                && match p.code {
                    KEY_EQUAL | KEY_KPPLUS => {
                        self.zoom_by(STEP, centre.0, centre.1, base, r);
                        true
                    }
                    KEY_MINUS | KEY_KPMINUS => {
                        self.zoom_by(1.0 / STEP, centre.0, centre.1, base, r);
                        true
                    }
                    KEY_0 => {
                        self.fit();
                        true
                    }
                    keys::KEY_LEFT if self.is_zoomed() => {
                        self.pan(step, 0.0);
                        true
                    }
                    keys::KEY_RIGHT if self.is_zoomed() => {
                        self.pan(-step, 0.0);
                        true
                    }
                    keys::KEY_UP if self.is_zoomed() => {
                        self.pan(0.0, step);
                        true
                    }
                    keys::KEY_DOWN if self.is_zoomed() => {
                        self.pan(0.0, -step);
                        true
                    }
                    _ => false,
                };
            if used {
                ui.take_press();
                out.used = true;
            }
        }
        out
    }

    /// Draw into `r`: `fitted`, made once with [`Image::fit_within`], or the zoomed pixels. `bg` shows around a zoomed picture.
    pub fn show(&mut self, ui: &mut Ui, r: Rect, fitted: Option<&Image>, bg: u32) {
        if self.is_zoomed() {
            if ui.painting() && (self.dirty || self.rect != Some(r)) {
                self.paint(r, fitted, bg);
            }
            if self.rect == Some(r) {
                ui.pixels(r, r.w.max(0) as usize, &self.buf);
            }
        } else if let Some(f) = fitted {
            ui.image_in(r, f);
        }
    }

    /// The screen's pixels of the zoomed picture, once per change. A picture drawn smaller than it is reads the mip level just larger
    /// than the drawn size, so nothing shimmers; between pixels it is bilinear, and drawn larger than it is, nearest, so that the pixels show.
    fn paint(&mut self, r: Rect, fallback: Option<&Image>, bg: u32) {
        let Some(mut v) = self.view else { return };
        let (w, h) = (r.w.max(0) as usize, r.h.max(0) as usize);
        self.buf.clear();
        self.buf.resize(w * h, bg);
        self.rect = Some(r);
        self.dirty = false;
        let Some(top) = self.source.as_ref().or(fallback) else { return };
        v = v.resize(top.w as f32, top.h as f32, r.w as f32, r.h as f32);
        self.view = Some(v);
        let d = v.rect();
        if d.w < 1.0 || d.h < 1.0 {
            return;
        }
        // Source pixels per screen pixel, and the mip level that brings it under 2.
        let k = top.w as f32 / d.w;
        let mut level = 0;
        while k / ((1u32 << (level + 1)) as f32) >= 1.0 && level < 6 {
            level += 1;
        }
        if self.source.is_some() {
            while self.mips.len() < level {
                let next = half(self.mips.last().or(self.source.as_ref()).expect("the source is there"));
                self.mips.push(next);
            }
        } else {
            level = 0;
        }
        let src = if level == 0 { self.source.as_ref().or(fallback).expect("checked above") } else { &self.mips[level - 1] };
        let (kx, ky) = (src.w as f32 / d.w, src.h as f32 / d.h);
        let x0 = (d.x.max(0.0) as usize).min(w);
        let x1 = ((d.x + d.w).ceil().max(0.0) as usize).min(w);
        let (sw, sh) = (src.w as usize, src.h as usize);
        if kx >= 1.0 {
            bilinear(src, (sw, sh), (kx, ky), (d.x, d.y, d.h), (x0, x1), (w, h), &mut self.buf);
        } else {
            let columns: Vec<usize> = (x0..x1).map(|x| (((x as f32 + 0.5 - d.x) * kx) as usize).min(sw - 1)).collect();
            for y in 0..h {
                let fy = y as f32 + 0.5 - d.y;
                if fy < 0.0 || fy >= d.h {
                    continue;
                }
                let sy = ((fy * ky) as usize).min(sh - 1) * sw;
                let row = &mut self.buf[y * w + x0..y * w + x1];
                for (p, sx) in row.iter_mut().zip(&columns) {
                    *p = src.px[sy + sx] & 0x00ff_ffff;
                }
            }
        }
    }
}

/// Half the size, each pixel the average of four.
fn half(img: &Image) -> Image {
    let (w, h) = ((img.w as usize / 2).max(1), (img.h as usize / 2).max(1));
    let sw = img.w as usize;
    let mut px = Vec::with_capacity(w * h);
    for y in 0..h {
        let a = &img.px[(y * 2).min(img.h as usize - 1) * sw..];
        let b = &img.px[(y * 2 + 1).min(img.h as usize - 1) * sw..];
        for x in 0..w {
            let (x1, x2) = ((x * 2).min(sw - 1), (x * 2 + 1).min(sw - 1));
            let (p, q, s, t) = (a[x1], a[x2], b[x1], b[x2]);
            // Two channels at a time: red and blue, then green.
            let rb = (((p & 0xff00ff) + (q & 0xff00ff) + (s & 0xff00ff) + (t & 0xff00ff)) >> 2) & 0xff00ff;
            let g = (((p & 0xff00) + (q & 0xff00) + (s & 0xff00) + (t & 0xff00)) >> 2) & 0xff00;
            px.push(rb | g);
        }
    }
    Image { w: w as u32, h: h as u32, px }
}

/// Fills the rows of `out` the picture covers, 8 bits of position between pixels.
#[allow(clippy::too_many_arguments)]
fn bilinear(src: &Image, (sw, sh): (usize, usize), (kx, ky): (f32, f32), (dx, dy, dh): (f32, f32, f32), (x0, x1): (usize, usize), (w, h): (usize, usize), out: &mut [u32]) {
    let columns: Vec<(u32, u32)> = (x0..x1)
        .map(|x| {
            let s = ((x as f32 + 0.5 - dx) * kx - 0.5).max(0.0);
            ((s as u32).min(sw as u32 - 1), ((s.fract() * 256.0) as u32).min(255))
        })
        .collect();
    for y in 0..h {
        let fy = y as f32 + 0.5 - dy;
        if fy < 0.0 || fy >= dh {
            continue;
        }
        let s = (fy * ky - 0.5).max(0.0);
        let (r0, ty) = ((s as usize).min(sh - 1), ((s.fract() * 256.0) as u32).min(255));
        let r1 = (r0 + 1).min(sh - 1);
        let (a, b) = (&src.px[r0 * sw..r0 * sw + sw], &src.px[r1 * sw..r1 * sw + sw]);
        let row = &mut out[y * w + x0..y * w + x1];
        for (p, &(sx, tx)) in row.iter_mut().zip(&columns) {
            let (i, j) = (sx as usize, (sx as usize + 1).min(sw - 1));
            let top = mix(a[i], a[j], tx);
            let bottom = mix(b[i], b[j], tx);
            *p = mix(top, bottom, ty);
        }
    }
}

#[inline]
fn mix(p: u32, q: u32, t: u32) -> u32 {
    let rb = (((p & 0xff00ff) * (256 - t) + (q & 0xff00ff) * t) >> 8) & 0xff00ff;
    let g = (((p & 0xff00) * (256 - t) + (q & 0xff00) * t) >> 8) & 0xff00;
    rb | g
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exif_jpeg(orientation: u16, little: bool) -> Vec<u8> {
        let mut t = Vec::new();
        let w16 = |v: u16| if little { v.to_le_bytes() } else { v.to_be_bytes() };
        let w32 = |v: u32| if little { v.to_le_bytes() } else { v.to_be_bytes() };
        t.extend_from_slice(if little { b"II" } else { b"MM" });
        t.extend_from_slice(&w16(42));
        t.extend_from_slice(&w32(8));
        t.extend_from_slice(&w16(2));
        // Another tag first, as cameras write them.
        t.extend_from_slice(&w16(0x010f));
        t.extend_from_slice(&w16(2));
        t.extend_from_slice(&w32(4));
        t.extend_from_slice(&w32(0));
        t.extend_from_slice(&w16(0x0112));
        t.extend_from_slice(&w16(3));
        t.extend_from_slice(&w32(1));
        t.extend_from_slice(&w16(orientation));
        t.extend_from_slice(&[0, 0]);
        let mut out = vec![0xff, 0xd8, 0xff, 0xe0, 0, 4, 0, 0, 0xff, 0xe1];
        out.extend_from_slice(&((t.len() + 8) as u16).to_be_bytes());
        out.extend_from_slice(b"Exif\0\0");
        out.extend_from_slice(&t);
        out.extend_from_slice(&[0xff, 0xda, 0, 2]);
        out
    }

    #[test]
    fn reads_the_exif_turn_in_either_byte_order() {
        assert_eq!(jpeg_orientation(&exif_jpeg(6, true)), 6);
        assert_eq!(jpeg_orientation(&exif_jpeg(8, false)), 8);
    }

    #[test]
    fn a_missing_or_absurd_turn_is_upright() {
        assert_eq!(jpeg_orientation(&exif_jpeg(9, true)), 1);
        assert_eq!(jpeg_orientation(&[0xff, 0xd8, 0xff, 0xda, 0, 2]), 1);
        // Cut short inside EXIF: no panic, no turn.
        let cut = exif_jpeg(6, true);
        for n in 0..cut.len() {
            let _ = jpeg_orientation(&cut[..n]);
        }
        assert_eq!(jpeg_orientation(&cut[..20]), 1);
    }

    #[test]
    fn each_orientation_lands_the_right_way_up() {
        // 2 x 3, numbered row by row: 1 2 / 3 4 / 5 6.
        let img = || Image { w: 2, h: 3, px: vec![1, 2, 3, 4, 5, 6] };
        assert_eq!(oriented(img(), 1).px, [1, 2, 3, 4, 5, 6]);
        assert_eq!(oriented(img(), 2).px, [2, 1, 4, 3, 6, 5]);
        assert_eq!(oriented(img(), 3).px, [6, 5, 4, 3, 2, 1]);
        assert_eq!(oriented(img(), 4).px, [5, 6, 3, 4, 1, 2]);
        let t = oriented(img(), 5);
        assert_eq!((t.w, t.h, t.px), (3, 2, vec![1, 3, 5, 2, 4, 6]));
        // 6 is what a phone held upright writes: a quarter turn clockwise.
        let t = oriented(img(), 6);
        assert_eq!((t.w, t.h, t.px), (3, 2, vec![5, 3, 1, 6, 4, 2]));
        let t = oriented(img(), 7);
        assert_eq!((t.w, t.h, t.px), (3, 2, vec![6, 4, 2, 5, 3, 1]));
        let t = oriented(img(), 8);
        assert_eq!((t.w, t.h, t.px), (3, 2, vec![2, 4, 6, 1, 3, 5]));
    }

    #[test]
    fn knows_the_formats_by_their_first_bytes() {
        assert_eq!(sniff(&[0xff, 0x0a, 1]), Some(Format::JpegXl));
        assert_eq!(sniff(&[0, 0, 0, 0x0c, b'J', b'X', b'L', b' ', 0xd, 0xa]), Some(Format::JpegXl));
        assert_eq!(sniff(&[0xff, 0xd8, 0xff, 0xe0]), Some(Format::Jpeg));
        assert_eq!(sniff(b"hello"), None);
        assert!(load(b"hello", |_| ()).is_err());
        assert!(is_picture_name("IMG_0001.JPG") && is_picture_name("a.jxl") && !is_picture_name("a.heic"));
    }

    /// A JPEG XL naming 2^30 x 2^30, and nothing behind it: signature, then SizeHeader with the longest size fields. With no frame after it
    /// the decoder never reports the size, so this shows only that such a file ends in an error with nothing rendered. The size check in
    /// `load` itself has not met a whole hostile file: no encoder here writes one, and making one by hand was not worth it.
    #[test]
    fn rejects_a_hostile_jpeg_xl_size_without_rendering_it() {
        // small = 0, height selector 3 (30 bits), height-1; ratio 0... written bit by bit, low bits first as JPEG XL does.
        let mut bits: Vec<bool> = Vec::new();
        let mut put = |v: u32, n: u32| {
            for i in 0..n {
                bits.push(v >> i & 1 == 1);
            }
        };
        put(0, 1);
        put(3, 2);
        put((1 << 30) - 1, 30);
        put(0, 3);
        put(3, 2);
        put((1 << 30) - 1, 30);
        // ImageMetadata: all_default, then the codestream goes on with frames this never reaches.
        put(1, 1);
        let mut f = vec![0xff, 0x0a];
        for byte in bits.chunks(8) {
            f.push(byte.iter().enumerate().fold(0u8, |v, (i, b)| v | (*b as u8) << i));
        }
        f.extend_from_slice(&[0u8; 32]);
        match load(&f, |_| ()) {
            Ok(i) => panic!("decoded {} x {}", i.w, i.h),
            Err(e) => assert!(!e.is_empty()),
        }
    }

    #[test]
    fn alpha_is_laid_on_the_background() {
        let img = Image { w: 2, h: 1, px: vec![0x00ff_ffff, 0x80ff_0000] };
        let f = flattened(img, 0xff00_0000);
        assert_eq!(f.px[0], 0xff00_0000);
        assert_eq!(f.px[1] >> 16 & 0xff, 0x80);
    }

    #[test]
    fn half_averages_four_pixels() {
        let i = Image { w: 2, h: 2, px: vec![0x000000, 0xffffff, 0xffffff, 0x000000] };
        assert_eq!(half(&i).px, vec![0x7f7f7f]);
        // An odd last row or column does not read past the picture.
        let i = Image { w: 3, h: 3, px: vec![0x101010; 9] };
        assert_eq!(half(&i).px, vec![0x101010]);
    }

    #[test]
    fn mixing_stays_in_range() {
        assert_eq!(mix(0xffffff, 0xffffff, 255), 0xffffff);
        assert_eq!(mix(0x000000, 0x000000, 128), 0);
        assert_eq!(mix(0x102030, 0x102030, 0), 0x102030);
    }

    #[derive(Default)]
    struct One {
        pic: PictureView,
        fitted: Option<Image>,
        resp: Response,
    }

    impl lucina_app::App for One {
        type Msg = ();
        fn view(&mut self, ui: &mut Ui) {
            let r = ui.area();
            let base = self.pic.source().map_or((1, 1), |s| (s.w, s.h));
            let resp = self.pic.input(ui, "pic", r, base);
            self.resp.used |= resp.used;
            self.resp.clicked |= resp.clicked;
            self.pic.show(ui, r, self.fitted.as_ref(), 0x101010);
        }
    }

    /// Left half red, right half blue.
    fn sides(w: u32, h: u32) -> Image {
        let px = (0..h).flat_map(|_| (0..w).map(move |x| if x < w / 2 { 0xffff0000 } else { 0xff0000ff })).collect();
        Image { w, h, px }
    }

    fn one(w: u32, h: u32, screen: (u32, u32)) -> lucina_app::Harness<One> {
        let src = sides(w, h);
        let mut o = One::default();
        o.fitted = Some(src.fit_within(screen.0, screen.1));
        o.pic.set_source(src);
        lucina_app::Harness::new(o, screen.0, screen.1)
    }

    fn pixel(h: &lucina_app::Harness<One>, x: usize, y: usize) -> u32 {
        h.core.frame.rows(Rect::new(x as i32, y as i32, 1, 1)).next().unwrap().1[0] & 0xffffff
    }

    #[test]
    fn zoom_keys_and_the_arrows_move_the_picture() {
        let mut h = one(800, 600, (400, 300));
        assert!(!h.app().pic.is_zoomed());
        // Fitted, the middle of the screen is where the sides meet.
        assert_eq!(pixel(&h, 100, 150), 0xff0000);
        assert_eq!(pixel(&h, 300, 150), 0x0000ff);
        // The arrows are the app's while fitted.
        h.key(keys::KEY_LEFT);
        assert!(!h.app().resp.used);
        for _ in 0..4 {
            h.key(KEY_EQUAL);
        }
        assert!(h.app().pic.is_zoomed());
        assert!(h.app().resp.used);
        // Zoomed in on the centre, then panned to the left edge: nothing but red on screen.
        for _ in 0..12 {
            h.key(keys::KEY_LEFT);
        }
        assert_eq!(pixel(&h, 200, 150), 0xff0000);
        assert_eq!(pixel(&h, 390, 150), 0xff0000);
        h.key(KEY_0);
        assert!(!h.app().pic.is_zoomed());
        assert_eq!(pixel(&h, 300, 150), 0x0000ff);
    }

    #[test]
    fn the_wheel_zooms_where_the_pointer_is_and_a_drag_is_not_a_click() {
        let mut h = one(800, 600, (400, 300));
        h.core.input(Input::Move(100, 150));
        for _ in 0..3 {
            h.wheel(1);
        }
        assert!(h.app().pic.is_zoomed());
        // The red point under the pointer stayed under it.
        assert_eq!(pixel(&h, 100, 150), 0xff0000);
        let before = *h.app().pic.view().unwrap();
        h.core.input(Input::Move(200, 150));
        h.core.input(Input::Down(200, 150));
        h.core.input(Input::Move(150, 150));
        h.core.input(Input::Up(150, 150));
        h.core.paint();
        assert!(h.app().pic.view().unwrap().offset_x < before.offset_x);
        assert!(!h.app().resp.clicked);
        // A click that stays put is one.
        h.click(150, 150);
        assert!(h.app().resp.clicked);
        // Wheel all the way out: fitted, and the zoomed pixels are gone.
        for _ in 0..20 {
            h.wheel(-1);
        }
        assert!(!h.app().pic.is_zoomed());
        assert!(h.app().pic.buf.is_empty());
    }

    #[test]
    fn a_large_picture_is_read_from_a_smaller_level() {
        let mut h = one(4000, 3000, (400, 300));
        // 10 source pixels per screen pixel, 8 at scale 1.25: level 3, an eighth of the size.
        h.key(KEY_EQUAL);
        assert_eq!(h.app().pic.mips.len(), 3);
        assert_eq!(pixel(&h, 10, 150), 0xff0000);
        h.key(KEY_0);
        assert!(h.app().pic.mips.is_empty());
    }

    #[test]
    fn zooming_before_the_source_arrives_uses_the_fitted_picture() {
        let src = sides(400, 300);
        let mut o = One::default();
        o.fitted = Some(src);
        let mut h = lucina_app::Harness::new(o, 400, 300);
        h.app().pic.zoom_by(2.0, 0.0, 150.0, (400, 300), Rect::new(0, 0, 400, 300));
        h.core.input(Input::Wheel(0));
        h.core.paint();
        assert_eq!(pixel(&h, 10, 150), 0xff0000);
    }
}
