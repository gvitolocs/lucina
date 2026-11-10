//! The Details page: one event told from the top (what, why, how we know, what to do, how often) as a list of lines on the left,
//! and its kernel log on the right. An evidence item that points at a log line selects it there.

use lucina_app::gfx::Text;
use lucina_app::time::Zone;
use lucina_app::{Face, Rect, Theme, Ui};
use lucina_evlog::event::{Category, Confidence, Event};

use crate::look::{BLUE, category_colour, confidence_colour, confidence_word, pill, pill_right};
use crate::model::{Data, boots_text, event_clock, local_day, long_date, short_date, title};

#[derive(Clone, Debug, PartialEq)]
pub enum DLine {
    Title(String, Category),
    Meta(String),
    Summary(String),
    Heading(&'static str),
    Cause {
        confidence: Confidence,
        text: String,
    },
    Evidence {
        observed: bool,
        fact: String,
        source: String,
        line: Option<usize>,
    },
    Step(String),
    /// The event's crash group: Enter lists its events.
    Same {
        label: String,
        text: String,
        bucket: String,
    },
    TechToggle(bool),
    Mono(String),
    Gap,
}

impl DLine {
    /// Rows the arrow keys stop on.
    pub fn selectable(&self) -> bool {
        matches!(
            self,
            DLine::Evidence { .. } | DLine::Same { .. } | DLine::TechToggle(_)
        )
    }

    /// The log line an evidence item points at.
    pub fn log_line(&self) -> Option<usize> {
        match self {
            DLine::Evidence { line, .. } => *line,
            _ => None,
        }
    }
}

/// The lines of one event's page.
pub fn build(data: &Data, e: &Event, zone: &Zone, now_ms: i64, tech: bool) -> Vec<DLine> {
    let mut out = vec![
        DLine::Title(title(e), e.category()),
        DLine::Meta(meta_line(e, zone, now_ms)),
    ];
    out.push(DLine::Summary(e.summary.clone()));
    out.push(DLine::Gap);
    // A normal step (a boot, a clean shutdown) has nothing to explain: no empty cause or evidence boxes for it, only for
    // problems, where "unknown" is itself worth saying.
    let problem = e.level <= lucina_evlog::Level::Warning;
    if problem || e.cause.is_some() {
        out.push(DLine::Heading("Likely cause"));
        match &e.cause {
            Some(c) => out.push(DLine::Cause {
                confidence: c.confidence,
                text: c.text.clone(),
            }),
            None => out.push(DLine::Cause {
                confidence: Confidence::Unknown,
                text: "Nothing points at a cause for this event.".into(),
            }),
        }
    }
    if problem || !e.evidence.is_empty() {
        out.push(DLine::Heading("Evidence"));
    }
    if problem && e.evidence.is_empty() {
        out.push(DLine::Mono("No evidence was kept with this event.".into()));
    }
    for ev in &e.evidence {
        out.push(DLine::Evidence {
            observed: ev.observed,
            fact: ev.fact.clone(),
            source: ev.source.clone(),
            line: ev.line.filter(|&l| l < e.log.len()),
        });
    }
    if let Some(step) = &e.next_step {
        out.push(DLine::Heading("What to do"));
        out.push(DLine::Step(step.clone()));
    }
    if let Some(b) = &e.bucket {
        out.push(DLine::Heading("Same problem"));
        let text = match data.group(&b.id) {
            Some(g) => {
                let first = g
                    .first_ms
                    .map_or_else(|| "unknown".to_owned(), |t| short_date(zone, t));
                let last = g
                    .last_ms
                    .map_or_else(|| "unknown".to_owned(), |t| short_date(zone, t));
                let times = if g.count == 1 {
                    "seen 1 time".to_owned()
                } else {
                    format!("seen {} times", g.count)
                };
                format!(
                    "{times}, first {first}, last {last}, {}",
                    boots_text(&g.boots)
                )
            }
            None => "seen 1 time".to_owned(),
        };
        out.push(DLine::Same {
            label: b.label.clone(),
            text,
            bucket: b.id.clone(),
        });
    }
    out.push(DLine::Gap);
    out.push(DLine::TechToggle(tech));
    if tech {
        out.extend(technical(e).into_iter().map(DLine::Mono));
    }
    out.push(DLine::Gap);
    out
}

fn meta_line(e: &Event, zone: &Zone, now_ms: i64) -> String {
    let mut parts = Vec::new();
    match e.time_ms {
        Some(t) => {
            let when = event_clock(zone, e);
            if local_day(zone, t) == local_day(zone, now_ms) {
                parts.push(when);
            } else {
                parts.push(format!("{}, {when}", long_date(zone, t)));
            }
        }
        None => parts.push("time unknown".to_owned()),
    }
    if let Some(b) = e.boot {
        parts.push(format!("boot {b}"));
    }
    if let Some(b) = &e.build {
        parts.push(format!("build {b}"));
    }
    parts.join(", ")
}

/// The technical details, one monospace line each.
pub fn technical(e: &Event) -> Vec<String> {
    let mut v = Vec::new();
    let mut kv = |k: &str, val: String| v.push(format!("{k:<10} {val}"));
    kv("provider", e.provider.clone());
    kv("id", e.id.to_string());
    kv("level", e.level.name().to_owned());
    kv("channel", e.channel.name().to_owned());
    kv("record", e.record.to_string());
    if let Some(k) = &e.key {
        kv("key", k.clone());
    }
    if let Some(s) = &e.source {
        kv("source", s.clone());
    }
    if let Some(a) = &e.activity {
        kv("activity", a.clone());
    }
    if let Some(u) = e.uptime_ms {
        kv("uptime", format!("{:.3} s", u as f64 / 1000.0));
    }
    if let Some(b) = &e.bucket {
        kv("bucket", format!("{}  {}", b.id, b.label));
        for (k, val) in &b.inputs {
            kv(&format!("  {k}"), val.clone());
        }
    }
    if !e.fields.is_empty() {
        v.push("fields".to_owned());
        let json = serde_json::to_string_pretty(&e.fields).unwrap_or_default();
        v.extend(json.lines().map(|l| format!("  {l}")));
    }
    v
}

const PAD: f32 = 10.0;

/// A line's height for a width.
pub fn height(text: &mut Text, line: &DLine, w: i32, t: &Theme) -> i32 {
    let cw = w - t.px(2.0 * PAD);
    match line {
        DLine::Title(..) => t.px(44.0),
        DLine::Meta(_) => t.px(26.0),
        DLine::Summary(s) => {
            Ui::wrapped_height(text, Face::Sans, t.pt(19.0), s, cw - t.px(14.0)) + t.px(16.0)
        }
        DLine::Heading(_) => t.px(40.0),
        DLine::Cause { text: s, .. } => {
            t.px(40.0)
                + Ui::wrapped_height(text, Face::Sans, t.pt(15.0), s, cw - t.px(24.0))
                + t.px(10.0)
        }
        DLine::Evidence { fact, .. } => {
            Ui::wrapped_height(text, Face::Sans, t.pt(14.0), fact, cw - t.px(96.0)) + t.px(40.0)
        }
        DLine::Step(s) => {
            Ui::wrapped_height(text, Face::Sans, t.pt(15.0), s, cw - t.px(24.0)) + t.px(24.0)
        }
        DLine::Same { label, text: s, .. } => {
            Ui::wrapped_height(text, Face::Bold, t.pt(14.0), label, cw - t.px(24.0))
                + Ui::wrapped_height(text, Face::Sans, t.pt(13.0), s, cw - t.px(24.0))
                + t.px(22.0)
        }
        DLine::TechToggle(_) => t.px(36.0),
        DLine::Mono(_) => t.px(20.0),
        DLine::Gap => t.px(10.0),
    }
}

pub fn draw(ui: &mut Ui, line: &DLine, selected: bool) {
    let t = ui.theme();
    let row = ui.area();
    let x = row.x + t.px(PAD);
    let w = row.w - t.px(2.0 * PAD);
    if selected && line.selectable() {
        ui.fill_round(
            Rect::new(row.x + 2, row.y + 1, row.w - 4, row.h - 2),
            t.px(6.0),
            t.selection,
        );
    }
    match line {
        DLine::Title(s, cat) => {
            let c = category_colour(*cat, &t);
            ui.fill_round(
                Rect::new(x, row.y + t.px(8.0), t.px(5.0), row.h - t.px(14.0)),
                t.px(2.5),
                c,
            );
            ui.text_in(
                Rect::new(
                    x + t.px(14.0),
                    row.y + t.px(4.0),
                    w - t.px(14.0),
                    row.h - t.px(8.0),
                ),
                Face::Bold,
                t.pt(24.0),
                t.text,
                s,
            );
        }
        DLine::Meta(s) => ui.text_in(
            Rect::new(x + t.px(14.0), row.y, w - t.px(14.0), row.h),
            Face::Sans,
            t.pt(13.0),
            t.dim,
            s,
        ),
        DLine::Summary(s) => {
            ui.within(
                Rect::new(
                    x + t.px(14.0),
                    row.y + t.px(4.0),
                    w - t.px(14.0),
                    row.h - t.px(4.0),
                ),
                |ui| ui.wrapped(Face::Sans, t.pt(19.0), t.text, s),
            );
        }
        DLine::Heading(s) => {
            ui.text_in(
                Rect::new(x, row.bottom() - t.px(28.0), w, t.px(24.0)),
                Face::Bold,
                t.pt(14.0),
                t.accent,
                s,
            );
            ui.fill(Rect::new(x, row.bottom() - t.px(3.0), w, 1), t.panel_hi);
        }
        DLine::Cause {
            confidence,
            text: s,
        } => {
            let card = Rect::new(x, row.y + t.px(2.0), w, row.h - t.px(6.0));
            ui.fill_round(card, t.px(8.0), t.panel);
            let c = confidence_colour(*confidence, &t);
            ui.fill_round(
                Rect::new(card.x, card.y + t.px(6.0), t.px(4.0), card.h - t.px(12.0)),
                t.px(2.0),
                c,
            );
            pill(
                ui,
                card.x + t.px(14.0),
                card.y + t.px(9.0),
                t.px(22.0),
                t.pt(12.0),
                Face::Bold,
                c,
                confidence_word(*confidence),
            );
            ui.within(
                Rect::new(
                    card.x + t.px(14.0),
                    card.y + t.px(38.0),
                    card.w - t.px(24.0),
                    card.h - t.px(40.0),
                ),
                |ui| ui.wrapped(Face::Sans, t.pt(15.0), t.item, s),
            );
        }
        DLine::Evidence {
            observed,
            fact,
            source,
            line,
        } => {
            let (tag, c) = if *observed {
                ("seen", t.ok)
            } else {
                ("reasoned", BLUE)
            };
            let tw = t.px(78.0);
            let tag_h = t.px(20.0);
            let tag_r = Rect::new(x + t.px(6.0), row.y + t.px(6.0), tw, tag_h);
            ui.fill_round(tag_r, tag_h / 2, t.panel);
            crate::look::centred(
                ui,
                tag_r.x + tw / 2,
                tag_r.y,
                tag_h,
                Face::Bold,
                t.pt(11.0),
                c,
                tag,
            );
            let tx = x + t.px(96.0);
            let fh = wrapped_h(ui, Face::Sans, t.pt(14.0), fact, w - t.px(96.0));
            ui.within(Rect::new(tx, row.y + t.px(5.0), w - t.px(96.0), fh), |ui| {
                ui.wrapped(Face::Sans, t.pt(14.0), t.text, fact)
            });
            let sy = row.y + t.px(5.0) + fh + t.px(2.0);
            let mut sw = w - t.px(96.0);
            if let Some(l) = line {
                let label = format!("→ log line {}", l + 1);
                let lw = ui.measure(Face::Bold, t.pt(12.0), &label) + t.px(8.0);
                ui.text_in(
                    Rect::new(x + w - lw, sy, lw, t.px(20.0)),
                    Face::Bold,
                    t.pt(12.0),
                    t.accent,
                    &label,
                );
                sw -= lw + t.px(8.0);
            }
            ui.text_in(
                Rect::new(tx, sy, sw, t.px(20.0)),
                Face::Sans,
                t.pt(12.0),
                if selected { t.item } else { t.dim },
                source,
            );
        }
        DLine::Step(s) => {
            let card = Rect::new(x, row.y + t.px(2.0), w, row.h - t.px(6.0));
            ui.fill_round(card, t.px(8.0), t.panel);
            ui.fill_round(
                Rect::new(card.x, card.y + t.px(6.0), t.px(4.0), card.h - t.px(12.0)),
                t.px(2.0),
                t.ok,
            );
            ui.within(
                Rect::new(
                    card.x + t.px(14.0),
                    card.y + t.px(10.0),
                    card.w - t.px(24.0),
                    card.h - t.px(12.0),
                ),
                |ui| ui.wrapped(Face::Sans, t.pt(15.0), t.text, s),
            );
        }
        DLine::Same { label, text: s, .. } => {
            let card = Rect::new(x, row.y + t.px(2.0), w, row.h - t.px(6.0));
            if !selected {
                ui.fill_round(card, t.px(8.0), t.panel);
            }
            let lh = wrapped_h(ui, Face::Bold, t.pt(14.0), label, card.w - t.px(24.0));
            ui.within(
                Rect::new(
                    card.x + t.px(12.0),
                    card.y + t.px(8.0),
                    card.w - t.px(24.0),
                    lh,
                ),
                |ui| ui.wrapped(Face::Bold, t.pt(14.0), t.text, label),
            );
            ui.within(
                Rect::new(
                    card.x + t.px(12.0),
                    card.y + t.px(10.0) + lh,
                    card.w - t.px(24.0),
                    card.h - lh - t.px(10.0),
                ),
                |ui| ui.wrapped(Face::Sans, t.pt(13.0), t.item, s),
            );
        }
        DLine::TechToggle(open) => {
            let label = format!("{} Technical details  (t)", if *open { "▾" } else { "▸" });
            ui.text_in(
                Rect::new(x + t.px(8.0), row.y, w, row.h),
                Face::Bold,
                t.pt(14.0),
                if selected { t.text } else { t.item },
                &label,
            );
        }
        DLine::Mono(s) => ui.text_in(
            Rect::new(x + t.px(8.0), row.y, w - t.px(8.0), row.h),
            Face::Mono,
            t.pt(12.5),
            t.item,
            s,
        ),
        DLine::Gap => {}
    }
}

/// What `Ui::wrapped` will take for `s` at width `w`: lines times the line height.
fn wrapped_h(ui: &mut Ui, f: Face, px: u16, s: &str, w: i32) -> i32 {
    ui.wrap_count(f, px, s, w) * ui.line_height(f, px)
}

/// The kernel log pane: a heading, then the event's lines. `hl` is the line an evidence item picked.
pub fn log_row(ui: &mut Ui, e: &Event, i: usize, hl: Option<usize>) {
    let t = ui.theme();
    let row = ui.area();
    let l = &e.log[i];
    let marked = l.mark.is_some();
    if hl == Some(i) {
        ui.fill(Rect::new(row.x, row.y, row.w, row.h), t.selection);
    }
    if marked || hl == Some(i) {
        let c = l.mark.as_deref().map_or(t.accent, |m| mark_colour(m, &t));
        ui.fill(Rect::new(row.x, row.y, t.px(4.0).max(3), row.h), c);
    }
    let px = t.pt(12.5);
    let up = l.uptime_ms.map_or_else(
        || " ".repeat(10),
        |u| format!("[{:>8.3}]", u as f64 / 1000.0),
    );
    let up_w = ui.measure(Face::Mono, px, &format!("{up} "));
    let x = row.x + t.px(12.0);
    ui.text_in(Rect::new(x, row.y, up_w, row.h), Face::Mono, px, t.dim, &up);
    let mut right = row.right() - t.px(8.0);
    if let Some(m) = &l.mark {
        // A long mark is cut: the line's text matters more.
        let m = cut(ui, Face::Bold, t.pt(10.5), m, row.w * 2 / 5);
        let w = pill_right(
            ui,
            right,
            row.y + t.px(3.0),
            row.h - t.px(6.0),
            t.pt(10.5),
            Face::Bold,
            mark_colour(&m, &t),
            &m,
        );
        right -= w + t.px(8.0);
    }
    ui.text_in(
        Rect::new(x + up_w, row.y, (right - x - up_w).max(0), row.h),
        Face::Mono,
        px,
        t.text,
        &l.text,
    );
}

/// `s` cut with an ellipsis to at most `max` pixels.
fn cut(ui: &mut Ui, f: Face, px: u16, s: &str, max: i32) -> String {
    if ui.measure(f, px, s) <= max {
        return s.to_owned();
    }
    let mut out: String = s.to_owned();
    while !out.is_empty() && ui.measure(f, px, &format!("{out}…")) > max {
        out.pop();
    }
    format!("{}…", out.trim_end())
}

fn mark_colour(m: &str, t: &Theme) -> lucina_app::Rgb {
    if m.contains("last") || m.contains("panic") || m.contains("fault") {
        t.danger
    } else {
        t.accent
    }
}

pub const LOG_ROW: f32 = 22.0;
