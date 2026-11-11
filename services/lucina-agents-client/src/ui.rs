//! Drawing the flat `pages` lines on the toolkit's screen: `draw`, `height`, `tone_of`, `bucket_colour`. Moved here from
//! lucina-app-agents/src/app.rs so CodeCor draws the same chat and permission cards as the Agents app.

use lucina_agents::bucket::Bucket;
use lucina_app::gfx::Text;
use lucina_app::{Face, Rect, Rgb, Theme, Ui};

use crate::pages::{Line, Tone};

pub fn bucket_colour(b: Bucket, t: &Theme) -> Rgb {
    match b {
        Bucket::NeedsInput => t.accent,
        Bucket::Failed => t.danger,
        Bucket::Running => t.ok,
        Bucket::Attention => t.item,
        Bucket::Done => t.dim,
    }
}

const INSET: f32 = 8.0;

fn tone_of(tone: Tone, t: &Theme) -> (Face, Rgb) {
    match tone {
        Tone::Text => (Face::Sans, t.text),
        Tone::Dim => (Face::Sans, t.dim),
        Tone::Bad => (Face::Sans, t.danger),
        Tone::Ok => (Face::Sans, t.ok),
        Tone::Accent => (Face::Sans, t.accent),
        Tone::Mono => (Face::Mono, t.text),
        Tone::MonoDim => (Face::Mono, t.dim),
        Tone::Add => (Face::Mono, t.ok),
        Tone::Del => (Face::Mono, t.danger),
    }
}

pub fn height(text: &mut Text, line: &Line, w: i32, t: &Theme) -> i32 {
    match line {
        Line::Heading(_) => t.px(30.0),
        Line::Para(tone, s) => {
            let (f, _) = tone_of(*tone, t);
            let px = t.pt(if f == Face::Mono { 12.0 } else { 14.0 });
            Ui::wrapped_height(text, f, px, s, w - t.px(2.0 * INSET)) + t.px(8.0)
        }
        Line::User(s) => {
            Ui::wrapped_height(text, Face::Sans, t.pt(14.0), s, w - t.px(4.0 * INSET)) + t.px(18.0)
        }
        Line::Row { sub, meta, .. } => {
            if sub.is_empty() && meta.is_empty() {
                t.px(28.0)
            } else {
                t.px(50.0)
            }
        }
        Line::Button { .. } => t.px(36.0),
        Line::Diff(_) => t.px(19.0),
    }
}

pub fn draw(ui: &mut Ui, line: &Line, selected: bool) {
    let t = ui.theme();
    let row = ui.area();
    if selected {
        ui.fill_round(
            Rect::new(row.x, row.y + 1, row.w, row.h - 2),
            t.px(6.0),
            t.selection,
        );
    }
    let x = row.x + t.px(INSET);
    let w = row.w - t.px(2.0 * INSET);
    match line {
        Line::Heading(s) => ui.text_in(
            Rect::new(x, row.bottom() - t.px(25.0), w, t.px(23.0)),
            Face::Bold,
            t.pt(14.0),
            t.accent,
            s,
        ),
        Line::Para(tone, s) => {
            let (f, c) = tone_of(*tone, &t);
            let px = t.pt(if f == Face::Mono { 12.0 } else { 14.0 });
            ui.within(
                Rect::new(x, row.y + t.px(3.0), w, row.h - t.px(4.0)),
                |ui| ui.wrapped(f, px, c, s),
            );
        }
        Line::User(s) => {
            let r = Rect::new(
                row.x + t.px(2.0),
                row.y + t.px(3.0),
                row.w - t.px(4.0),
                row.h - t.px(6.0),
            );
            if !selected {
                ui.fill_round(r, t.px(7.0), t.panel_hi);
            }
            ui.within(
                Rect::new(
                    r.x + t.px(INSET),
                    r.y + t.px(5.0),
                    r.w - t.px(2.0 * INSET),
                    r.h - t.px(6.0),
                ),
                |ui| ui.wrapped(Face::Sans, t.pt(14.0), t.text, s),
            );
        }
        Line::Row {
            title,
            sub,
            meta,
            tone,
            bucket,
            ..
        } => {
            let (_, c) = tone_of(*tone, &t);
            let mut left = x;
            if let Some(b) = bucket {
                let side = t.px(10.0);
                ui.fill_round(
                    Rect::new(left, row.y + t.px(9.0), side, side),
                    side / 2,
                    bucket_colour(*b, &t),
                );
                left += side + t.px(8.0);
            }
            let mw = if meta.is_empty() {
                0
            } else {
                ui.measure(Face::Sans, t.pt(12.0), meta) + t.px(8.0)
            };
            let title_colour = if *tone == Tone::Text { t.text } else { c };
            let top_h = if sub.is_empty() { row.h } else { t.px(24.0) };
            let top_y = if sub.is_empty() {
                row.y
            } else {
                row.y + t.px(3.0)
            };
            ui.text_in(
                Rect::new(left, top_y, row.right() - t.px(INSET) - mw - left, top_h),
                Face::Sans,
                t.pt(14.0),
                title_colour,
                title,
            );
            if mw > 0 {
                ui.text_in(
                    Rect::new(row.right() - t.px(INSET) - mw + t.px(8.0), top_y, mw, top_h),
                    Face::Sans,
                    t.pt(12.0),
                    c,
                    meta,
                );
            }
            if !sub.is_empty() {
                ui.text_in(
                    Rect::new(
                        left,
                        row.y + t.px(26.0),
                        row.right() - t.px(INSET) - left,
                        t.px(20.0),
                    ),
                    Face::Sans,
                    t.pt(12.0),
                    if selected { t.item } else { t.dim },
                    sub,
                );
            }
        }
        Line::Button { label, danger, .. } => {
            let r = Rect::new(x, row.y + t.px(3.0), w.min(t.px(280.0)), row.h - t.px(6.0));
            let bg = if selected {
                if *danger { t.danger } else { t.accent }
            } else {
                t.panel_hi
            };
            ui.fill_round(r, t.px(6.0), bg);
            ui.text_in(
                Rect::new(r.x + t.px(10.0), r.y, r.w - t.px(20.0), r.h),
                Face::Sans,
                t.pt(14.0),
                if selected { t.bg } else { t.text },
                label,
            );
        }
        Line::Diff(d) => {
            // The Changes tab's numbered rows: old lines for `remove`, new ones for `add` and `context`.
            let (bg, fg, sign) = match d.kind.as_str() {
                "add" => (Some(Rgb::hex(0x103824)), t.text, "+"),
                "remove" => (Some(Rgb::hex(0x461a1e)), t.text, "−"),
                _ => (None, t.dim, ""),
            };
            if let Some(c) = bg {
                ui.fill(row, c);
            }
            if selected {
                ui.fill(Rect::new(row.x, row.y, t.px(3.0), row.h), t.accent);
            }
            let nw = t.px(42.0);
            ui.text_in(
                Rect::new(x, row.y, nw, row.h),
                Face::Mono,
                t.pt(11.0),
                t.dim,
                &d.n,
            );
            ui.text_in(
                Rect::new(x + nw, row.y, t.px(12.0), row.h),
                Face::Mono,
                t.pt(12.0),
                fg,
                sign,
            );
            ui.text_in(
                Rect::new(
                    x + nw + t.px(12.0),
                    row.y,
                    row.right() - x - nw - t.px(12.0),
                    row.h,
                ),
                Face::Mono,
                t.pt(12.0),
                fg,
                &d.content,
            );
        }
    }
}
