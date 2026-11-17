//! Colours and small shapes shared by the screens. Red is a failure, the warning's amber an app failure, the accent a warning,
//! info's blue information: all four are the kit's tokens (rules 9 and 10), read as `Rgb` through the toolkit's `theme`. `BLUE`
//! and `ORANGE` keep the names the screens already say.

use lucina_app::theme;
use lucina_app::{Face, Rect, Rgb, Theme, Ui};
use lucina_evlog::event::{Category, Confidence, Event, Level};
use lucina_glass::tokens;

pub const BLUE: Rgb = theme::rgb(tokens::color::SIGNAL);
pub const ORANGE: Rgb = theme::rgb(tokens::color::WARNING);

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

/// A face and a size: the toolkit's text calls take them apart, the helpers below take them as one.
#[derive(Clone, Copy)]
pub struct Label {
    pub face: Face,
    pub px: u16,
}

/// A small filled label with `text` in the ground colour; returns its width.
pub fn pill(ui: &mut Ui, x: i32, y: i32, h: i32, label: Label, bg: Rgb, text: &str) -> i32 {
    let t = ui.theme();
    let w = ui.measure(label.face, label.px, text) + t.px(14.0);
    let r = Rect::new(x, y, w, h);
    ui.fill_round(r, h / 2, bg);
    ui.text_in(
        Rect::new(x + t.px(7.0), y, w, h),
        label.face,
        label.px,
        t.bg,
        text,
    );
    w
}

/// Like [`pill`] but ending at `right`.
pub fn pill_right(
    ui: &mut Ui,
    right: i32,
    y: i32,
    h: i32,
    label: Label,
    bg: Rgb,
    text: &str,
) -> i32 {
    let t = ui.theme();
    let w = ui.measure(label.face, label.px, text) + t.px(14.0);
    pill(ui, right - w, y, h, label, bg, text)
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
pub fn centred(ui: &mut Ui, cx: i32, y: i32, h: i32, label: Label, c: Rgb, text: &str) {
    let w = ui.measure(label.face, label.px, text) + 2;
    ui.text_in(
        Rect::new(cx - w / 2, y, w, h),
        label.face,
        label.px,
        c,
        text,
    );
}
