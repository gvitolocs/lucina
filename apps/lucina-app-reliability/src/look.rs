//! Colours and small shapes shared by the screens. Red is a failure, orange an app failure, amber a warning, blue information;
//! the toolkit's palette has no blue, so the one here is a step lighter than its navy ground and reads on it.

use lucina_app::{Face, Rect, Rgb, Theme, Ui};
use lucina_evlog::event::{Category, Confidence, Event, Level};

pub const BLUE: Rgb = Rgb::hex(0x7aa8ff);
pub const ORANGE: Rgb = Rgb::hex(0xe8975a);

pub fn category_colour(c: Category, t: &Theme) -> Rgb {
    match c {
        Category::JunoFailure => t.danger,
        Category::AppFailure => ORANGE,
        Category::Warning => t.accent,
        Category::Info => BLUE,
    }
}

pub fn event_colour(e: &Event, t: &Theme) -> Rgb {
    match e.level {
        Level::Verbose => t.dim,
        _ => category_colour(e.category(), t),
    }
}

/// The index's colour: good, wary, bad.
pub fn index_colour(i: Option<f64>, t: &Theme) -> Rgb {
    match i {
        None => t.dim,
        Some(v) if v >= 8.0 => t.ok,
        Some(v) if v >= 5.0 => t.accent,
        Some(_) => t.danger,
    }
}

pub fn confidence_colour(c: Confidence, t: &Theme) -> Rgb {
    match c {
        Confidence::Certain => t.ok,
        Confidence::Likely => BLUE,
        Confidence::Possible => t.accent,
        Confidence::Unknown => t.dim,
    }
}

pub fn confidence_word(c: Confidence) -> &'static str {
    match c {
        Confidence::Certain => "Certain",
        Confidence::Likely => "Likely",
        Confidence::Possible => "Possible",
        Confidence::Unknown => "Unknown",
    }
}

/// A small filled label with `text` in the ground colour; returns its width.
pub fn pill(ui: &mut Ui, x: i32, y: i32, h: i32, px: u16, face: Face, bg: Rgb, text: &str) -> i32 {
    let t = ui.theme();
    let w = ui.measure(face, px, text) + t.px(14.0);
    let r = Rect::new(x, y, w, h);
    ui.fill_round(r, h / 2, bg);
    ui.text_in(Rect::new(x + t.px(7.0), y, w, h), face, px, t.bg, text);
    w
}

/// Like [`pill`] but ending at `right`.
pub fn pill_right(
    ui: &mut Ui,
    right: i32,
    y: i32,
    h: i32,
    px: u16,
    face: Face,
    bg: Rgb,
    text: &str,
) -> i32 {
    let t = ui.theme();
    let w = ui.measure(face, px, text) + t.px(14.0);
    pill(ui, right - w, y, h, px, face, bg, text)
}

pub fn disc(ui: &mut Ui, cx: i32, cy: i32, side: i32, c: Rgb) {
    ui.fill_round(
        Rect::new(cx - side / 2, cy - side / 2, side, side),
        side / 2,
        c,
    );
}

/// A line of `th` pixels from (x0, y0) to (x1, y1), as small squares along it.
pub fn line(ui: &mut Ui, x0: i32, y0: i32, x1: i32, y1: i32, th: i32, c: Rgb) {
    if !ui.painting() {
        return;
    }
    let steps = (x1 - x0).abs().max((y1 - y0).abs()).max(1);
    for s in 0..=steps {
        let x = x0 + (x1 - x0) * s / steps;
        let y = y0 + (y1 - y0) * s / steps;
        ui.fill(Rect::new(x - th / 2, y - th / 2, th, th), c);
    }
}

/// One line of text centred on `cx`.
pub fn centred(ui: &mut Ui, cx: i32, y: i32, h: i32, face: Face, px: u16, c: Rgb, text: &str) {
    let w = ui.measure(face, px, text) + 2;
    ui.text_in(Rect::new(cx - w / 2, y, w, h), face, px, c, text);
}
