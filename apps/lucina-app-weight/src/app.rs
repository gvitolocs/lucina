//! The screen: `web/src/views/Weight.tsx`. The engine owns parsing, statistics and labels; this sends text, shows the
//! engine's labels, and only converts grams to the display unit to place points on the chart.

use std::time::{Duration, Instant};

use chrono::{DateTime, NaiveDateTime, TimeZone};
use chrono_tz::Tz;
use lucina_account::ui::SignInView;
use lucina_app::{App, Face, ListState, Rect, Rgb, TextEdit, Ui, keys};
use secretary_body::{Command, Entry, View};

use crate::server::{Job, Msg, Tag};

/// Weigh-ins change rarely and the account's request limit is shared by every app.
const REFRESH: Duration = Duration::from_secs(300);
const DASH: &str = "—";
const LB_GRAMS: f64 = 453.59237;
const WHEN_FORMAT: &str = "%Y-%m-%d %H:%M";

pub type Link = Box<dyn FnMut(Job)>;

fn weight_value(grams: u64, unit: &str) -> f64 {
    if unit == "lb" { grams as f64 / LB_GRAMS } else { grams as f64 / 1000.0 }
}

/// `formatWeight`: one decimal, without a trailing `.0`.
pub fn format_weight(grams: u64, unit: &str) -> String {
    let s = format!("{:.1}", weight_value(grams, unit));
    s.strip_suffix(".0").map_or(s.clone(), str::to_owned)
}

pub fn format_when(ms: u64, tz: Tz) -> String {
    DateTime::from_timestamp_millis(ms as i64)
        .map_or_else(String::new, |d| d.with_timezone(&tz).format(WHEN_FORMAT).to_string())
}

fn format_date(ms: u64, tz: Tz) -> String {
    DateTime::from_timestamp_millis(ms as i64)
        .map_or_else(String::new, |d| d.with_timezone(&tz).format("%-d %b %Y").to_string())
}

pub fn parse_when(s: &str, tz: Tz) -> Option<u64> {
    let local = NaiveDateTime::parse_from_str(s.trim(), WHEN_FORMAT).ok()?;
    let ms = tz.from_local_datetime(&local).earliest()?.timestamp_millis();
    u64::try_from(ms).ok()
}

pub struct Edit {
    pub entry: Entry,
    pub weight: TextEdit,
    pub when: TextEdit,
    pub note: TextEdit,
    pub error: Option<String>,
    pub pending: bool,
    pub confirm_delete: bool,
    first: bool,
}

pub struct Settings {
    pub unit: String,
    pub goal: TextEdit,
    pub error: Option<String>,
    pub pending: bool,
    first: bool,
}

pub struct Weight {
    tz: Tz,
    link: Option<Link>,
    sign_in: Option<SignInView>,
    /// Starts the sign-in screen again when the token dies.
    start_sign_in: Option<Box<dyn FnMut() -> Option<SignInView>>>,
    /// Starts the worker once there is a token.
    start_worker: Option<Box<dyn FnMut() -> Option<Link>>>,
    pub view: Option<View>,
    pub load_error: Option<String>,
    pub weight: TextEdit,
    pub note: TextEdit,
    pub unit: String,
    unit_chosen: bool,
    pub log_error: Option<String>,
    pub log_pending: bool,
    pub list: ListState,
    pub edit: Option<Edit>,
    pub settings: Option<Settings>,
    last_load: Option<Instant>,
    focus_form: bool,
    save_cache: Option<Box<dyn FnMut(&View, &str)>>,
    /// Shown instead of the account's name for what sample mode shows.
    sample: bool,
}

enum Todo {
    Log,
    Refresh,
    OpenEdit(usize),
    OpenSettings,
    SaveEdit,
    Delete,
    SaveSettings,
}

impl Weight {
    pub fn new(tz: Tz, link: Option<Link>) -> Weight {
        Weight {
            tz,
            link,
            sign_in: None,
            start_sign_in: None,
            start_worker: None,
            view: None,
            load_error: None,
            weight: TextEdit::new(),
            note: TextEdit::new(),
            unit: "kg".into(),
            unit_chosen: false,
            log_error: None,
            log_pending: false,
            list: ListState::new(),
            edit: None,
            settings: None,
            last_load: None,
            focus_form: true,
            save_cache: None,
            sample: false,
        }
    }

    pub fn sample(mut self) -> Weight {
        self.sample = true;
        self
    }

    pub fn with_sign_in(
        mut self,
        start_sign_in: Box<dyn FnMut() -> Option<SignInView>>,
        start_worker: Box<dyn FnMut() -> Option<Link>>,
    ) -> Weight {
        self.start_sign_in = Some(start_sign_in);
        self.start_worker = Some(start_worker);
        if self.link.is_none() {
            self.sign_in = self.start_sign_in.as_mut().and_then(|f| f());
        }
        self
    }

    pub fn with_cache(mut self, save: Box<dyn FnMut(&View, &str)>) -> Weight {
        self.save_cache = Some(save);
        self
    }

    /// What was fetched last time, shown before any network.
    pub fn restore(&mut self, view: View, unit: Option<String>) {
        if let Some(u) = unit {
            self.unit = u;
            self.unit_chosen = true;
        } else {
            self.unit.clone_from(&view.settings.unit);
        }
        self.view = Some(view);
    }

    fn send(&mut self, job: Job) {
        if let Some(l) = &mut self.link {
            l(job);
        }
    }

    pub fn refresh(&mut self) {
        self.last_load = Some(Instant::now());
        self.send(Job::Refresh);
    }

    pub fn choose_unit(&mut self, unit: &str) {
        self.unit_chosen = true;
        self.unit = unit.to_owned();
    }

    pub fn submit_log(&mut self) {
        if self.log_pending {
            return;
        }
        let text = self.weight.text.trim().to_owned();
        if text.is_empty() {
            self.log_error = Some("Enter your weight.".into());
            return;
        }
        let note = self.note.text.trim();
        let note = (!note.is_empty()).then(|| note.to_owned());
        self.log_error = None;
        self.log_pending = true;
        let unit = self.unit.clone();
        self.send(Job::Log { text, unit, note });
    }

    pub fn open_edit(&mut self, entry: Entry) {
        let unit = self.view_unit().to_owned();
        self.edit = Some(Edit {
            weight: TextEdit::with_text(&format_weight(entry.weight_grams, &unit)),
            when: TextEdit::with_text(&format_when(entry.measured_ms, self.tz)),
            note: TextEdit::with_text(&entry.note),
            entry,
            error: None,
            pending: false,
            confirm_delete: false,
            first: true,
        });
    }

    /// Only what changed is sent; what is left out keeps its value.
    pub fn save_edit(&mut self) {
        let unit = self.view_unit().to_owned();
        let tz = self.tz;
        let Some(e) = &mut self.edit else { return };
        if e.pending {
            return;
        }
        let Some(measured) = parse_when(&e.when.text, tz) else {
            e.error = Some("Enter a valid date and time.".into());
            return;
        };
        let weight = e.weight.text.trim();
        let weight = (weight != format_weight(e.entry.weight_grams, &unit)).then(|| weight.to_owned());
        let when_changed = e.when.text.trim() != format_when(e.entry.measured_ms, tz);
        let measured_ms = when_changed.then_some(measured);
        let note = e.note.text.trim();
        let note = (note != e.entry.note).then(|| note.to_owned());
        if weight.is_none() && measured_ms.is_none() && note.is_none() {
            self.edit = None;
            return;
        }
        e.error = None;
        e.pending = true;
        let entry_id = e.entry.id.clone();
        self.send(Job::Run(
            Tag::Edit,
            Command::EditEntry { entry_id, weight_grams: None, weight, measured_ms, note },
        ));
    }

    pub fn delete_entry(&mut self) {
        let Some(e) = &mut self.edit else { return };
        if e.pending {
            return;
        }
        e.pending = true;
        let entry_id = e.entry.id.clone();
        self.send(Job::Run(Tag::Delete, Command::DeleteEntry { entry_id }));
    }

    pub fn open_settings(&mut self) {
        let Some(v) = &self.view else { return };
        let unit = v.settings.unit.clone();
        let goal = v.settings.goal_grams.map_or_else(String::new, |g| format_weight(g, &unit));
        self.settings = Some(Settings {
            unit,
            goal: TextEdit::with_text(&goal).placeholder("No goal"),
            error: None,
            pending: false,
            first: true,
        });
    }

    /// The engine reads the goal in the new unit, so an unchanged goal is written again in it.
    pub fn settings_unit(&mut self, next: &str) {
        let goal = self.view.as_ref().and_then(|v| v.settings.goal_grams);
        let Some(s) = &mut self.settings else { return };
        if let Some(g) = goal
            && s.goal.text.trim() == format_weight(g, &s.unit)
        {
            s.goal.set(&format_weight(g, next));
        }
        s.unit = next.to_owned();
    }

    pub fn save_settings(&mut self) {
        let Some(s) = &mut self.settings else { return };
        if s.pending {
            return;
        }
        s.error = None;
        s.pending = true;
        // No goal clears it.
        let goal = s.goal.text.trim();
        let goal = (!goal.is_empty()).then(|| goal.to_owned());
        let unit = s.unit.clone();
        self.send(Job::Run(Tag::Settings, Command::UpdateSettings { unit, goal_grams: None, goal }));
    }

    pub fn view_unit(&self) -> &str {
        self.view.as_ref().map_or("kg", |v| v.settings.unit.as_str())
    }

    fn stat(&self, key: &str) -> String {
        self.view.as_ref().and_then(|v| v.stat_labels.get(key)).cloned().unwrap_or_else(|| DASH.to_owned())
    }

    fn signed_out(&mut self) {
        crate::account::rejected();
        self.link = None;
        self.view = None;
        self.edit = None;
        self.settings = None;
        self.log_pending = false;
        self.sign_in = self.start_sign_in.as_mut().and_then(|f| f());
    }

    fn signed_in(&mut self) {
        self.sign_in = None;
        self.link = self.start_worker.as_mut().and_then(|f| f());
        self.focus_form = true;
        self.refresh();
    }

    fn done(&mut self, tag: Tag, result: Result<(), String>) {
        match tag {
            Tag::Log => {
                self.log_pending = false;
                match result {
                    Ok(()) => {
                        self.weight.clear();
                        self.note.clear();
                        self.log_error = None;
                    }
                    Err(m) => self.log_error = Some(m),
                }
            }
            Tag::Edit | Tag::Delete => match result {
                Ok(()) => self.edit = None,
                Err(m) => {
                    if let Some(e) = &mut self.edit {
                        e.pending = false;
                        e.error = Some(m);
                    }
                }
            },
            Tag::Settings => match result {
                Ok(()) => self.settings = None,
                Err(m) => {
                    if let Some(s) = &mut self.settings {
                        s.pending = false;
                        s.error = Some(m);
                    }
                }
            },
        }
    }

    fn view_signed(&mut self, ui: &mut Ui, todo: &mut Option<Todo>) {
        let t = ui.theme();
        ui.pad(20.0);
        let area = ui.area();
        if self.view.is_none() {
            ui.within(area, |ui| match &self.load_error {
                Some(e) => {
                    ui.wrapped(Face::Sans, t.pt(16.0), t.danger, e);
                    ui.space(8.0);
                    if ui.button("Try again") {
                        *todo = Some(Todo::Refresh);
                    }
                }
                None => ui.dim("Loading your weigh-ins…"),
            });
            return;
        }
        let left = ui.cut_left(t.px(380.0));
        ui.cut_left(t.px(20.0));
        self.form(ui, left, todo);

        let stats = ui.cut_top(t.px(96.0));
        self.stat_tiles(ui, stats);
        ui.cut_top(t.px(6.0));
        let detail = ui.cut_top(t.px(24.0));
        let line = self.detail_line();
        ui.text_in(detail, Face::Sans, t.pt(13.0), t.dim, &line);
        ui.cut_top(t.px(6.0));
        let rest = ui.area();
        let chart_r = ui.cut_top((rest.h * 45 / 100).max(t.px(120.0)));
        self.chart(ui, chart_r);
        ui.cut_top(t.px(12.0));
        self.history(ui, todo);
    }

    fn form(&mut self, ui: &mut Ui, r: Rect, todo: &mut Option<Todo>) {
        let t = ui.theme();
        ui.fill_round(r, t.px(10.0), t.bar);
        if std::mem::take(&mut self.focus_form) {
            ui.focus("weight");
        }
        let chosen = self.unit.clone();
        let mut pick = None;
        ui.within(r.inset(t.px(16.0)), |ui| {
            ui.heading(if self.sample { "Weight (sample data)" } else { "Weight" });
            ui.space(6.0);
            ui.dim(&format!("Weight ({chosen})"));
            if ui.text_field("weight", &mut self.weight).submitted {
                *todo = Some(Todo::Log);
            }
            ui.space(6.0);
            ui.row(38.0, |ui| {
                for u in ["kg", "lb"] {
                    let hit = if u == chosen {
                        ui.primary_button(u)
                    } else {
                        ui.button_id(("unit", usize::from(u == "lb")), u)
                    };
                    if hit {
                        pick = Some(u);
                    }
                }
            });
            ui.space(6.0);
            ui.dim("Note (optional)");
            if ui.text_field("note", &mut self.note).submitted {
                *todo = Some(Todo::Log);
            }
            if let Some(e) = &self.log_error {
                ui.space(4.0);
                ui.wrapped(Face::Sans, t.pt(14.0), t.danger, e);
            }
            ui.space(10.0);
            if ui.primary_button(if self.log_pending { "Logging…" } else { "Log weigh-in" }) {
                *todo = Some(Todo::Log);
            }
            if let Some(e) = &self.load_error {
                ui.space(8.0);
                ui.wrapped(Face::Sans, t.pt(13.0), t.dim, &format!("{e} Showing the last copy."));
            }
            ui.space(14.0);
            if ui.button("Settings") {
                *todo = Some(Todo::OpenSettings);
            }
        });
        if let Some(u) = pick {
            self.choose_unit(u);
        }
    }

    fn detail_line(&self) -> String {
        match &self.view {
            Some(v) if v.stats.count > 0 => format!(
                "Min {} · Max {} · 30d {} · 90d {}",
                self.stat("min"),
                self.stat("max"),
                self.stat("trend30"),
                self.stat("trend90")
            ),
            _ => String::new(),
        }
    }

    fn goal_value(&self) -> String {
        let Some(v) = &self.view else { return "Set a goal".into() };
        if v.stats.goal_grams.is_none() {
            return "Set a goal".into();
        }
        if v.stats.goal_reached == Some(true) {
            return "Goal reached".into();
        }
        self.stat("goal_distance")
    }

    fn stat_tiles(&self, ui: &mut Ui, r: Rect) {
        let t = ui.theme();
        let goal_label = self
            .view
            .as_ref()
            .and_then(|v| v.stat_labels.get("goal"))
            .map_or("Goal distance".to_owned(), |g| format!("Goal distance · {g}"));
        let tiles = [
            (self.stat("current"), "Current".to_owned()),
            (self.stat("avg7"), "7-day average".to_owned()),
            (self.stat("weekly_change"), "Weekly change".to_owned()),
            (self.goal_value(), goal_label),
        ];
        let gap = t.px(12.0);
        let w = (r.w - gap * 3) / 4;
        for (i, (value, label)) in tiles.iter().enumerate() {
            let tile = Rect::new(r.x + (w + gap) * i as i32, r.y, w, r.h);
            ui.fill_round(tile, t.px(10.0), t.panel);
            let inner = tile.inset(t.px(12.0));
            let top = Rect::new(inner.x, inner.y, inner.w, inner.h * 3 / 5);
            let bottom = Rect::new(inner.x, top.bottom(), inner.w, inner.h - top.h);
            ui.text_in(top, Face::Bold, t.pt(22.0), t.text, value);
            ui.text_in(bottom, Face::Sans, t.pt(13.0), t.dim, label);
        }
    }

    fn chart(&self, ui: &mut Ui, r: Rect) {
        let t = ui.theme();
        ui.fill_round(r, t.px(10.0), t.panel);
        let inner = r.inset(t.px(14.0));
        let title = Rect::new(inner.x, inner.y, inner.w, t.px(24.0));
        ui.text_in(title, Face::Bold, t.pt(15.0), t.text, "Trend");
        let legend_x = inner.right() - t.px(240.0);
        legend(ui, legend_x, title, t.dim, "Weigh-ins");
        legend(ui, legend_x + t.px(120.0), title, t.accent, "7-day average");
        let Some(v) = &self.view else { return };
        let plot = Rect::new(
            inner.x + t.px(52.0),
            inner.y + t.px(32.0),
            inner.w - t.px(60.0),
            inner.h - t.px(32.0) - t.px(24.0),
        );
        if v.series.is_empty() || plot.w <= 0 || plot.h <= 0 {
            ui.text_in(plot, Face::Sans, t.pt(14.0), t.dim, "Nothing to show yet.");
            return;
        }
        if !ui.painting() {
            return;
        }
        let unit = v.settings.unit.as_str();
        let (mut lo, mut hi) = (f64::MAX, f64::MIN);
        for p in &v.series {
            let (a, b) = (weight_value(p.weight_grams, unit), weight_value(p.avg7_grams, unit));
            lo = lo.min(a).min(b);
            hi = hi.max(a).max(b);
        }
        let margin = ((hi - lo) * 0.08).max(0.5);
        let (lo, hi) = (lo - margin, hi + margin);
        let first = v.series[0].measured_ms;
        let last = v.series[v.series.len() - 1].measured_ms;
        let span = last.saturating_sub(first).max(1) as f64;
        let to_x = |ms: u64| plot.x + ((ms.saturating_sub(first)) as f64 / span * f64::from(plot.w - 1)) as i32;
        let to_y = |w: f64| plot.y + ((hi - w) / (hi - lo) * f64::from(plot.h - 1)) as i32;

        for i in 0..3 {
            let value = hi - (hi - lo) * f64::from(i) / 2.0;
            let y = to_y(value);
            ui.fill(Rect::new(plot.x, y, plot.w, 1), t.panel_hi);
            let label = Rect::new(inner.x, y - t.px(10.0), t.px(48.0), t.px(20.0));
            ui.text_in(label, Face::Sans, t.pt(12.0), t.dim, &format!("{value:.1}"));
        }
        let weights: Vec<(i32, i32)> =
            v.series.iter().map(|p| (to_x(p.measured_ms), to_y(weight_value(p.weight_grams, unit)))).collect();
        let averages: Vec<(i32, i32)> =
            v.series.iter().map(|p| (to_x(p.measured_ms), to_y(weight_value(p.avg7_grams, unit)))).collect();
        polyline(ui, &weights, t.dim, 1);
        polyline(ui, &averages, t.accent, t.px(2.0).max(2));
        if v.series.len() > 1 {
            let y = plot.bottom() + t.px(2.0);
            let w = t.px(160.0);
            let start = Rect::new(plot.x, y, w, t.px(20.0));
            ui.text_in(start, Face::Sans, t.pt(12.0), t.dim, &format_date(first, self.tz));
            let end_text = format_date(last, self.tz);
            let tw = ui.measure(Face::Sans, t.pt(12.0), &end_text);
            let end = Rect::new(plot.right() - tw, y, tw, t.px(20.0));
            ui.text_in(end, Face::Sans, t.pt(12.0), t.dim, &end_text);
        }
    }

    fn history(&mut self, ui: &mut Ui, todo: &mut Option<Todo>) {
        let t = ui.theme();
        let title = ui.cut_top(t.px(26.0));
        ui.text_in(title, Face::Bold, t.pt(15.0), t.text, "Weigh-ins");
        let r = ui.area();
        let Some(view) = self.view.as_ref() else { return };
        if view.entries.is_empty() {
            ui.within(r, |ui| ui.dim("No weigh-ins yet. Log your first one on the left."));
            return;
        }
        let tz = self.tz;
        let res = ui.list("entries", &mut self.list, r, view.entries.len(), 44.0, |ui, i, selected| {
            let row = ui.area();
            let e = &view.entries[i];
            if selected {
                ui.fill_round(row.inset(t.px(1.0)), t.px(6.0), t.panel_hi);
            }
            let x = row.x + t.px(10.0);
            let label = view.labels.get(&e.id).map_or(DASH, String::as_str);
            let weight = Rect::new(x, row.y, t.px(130.0), row.h);
            ui.text_in(weight, Face::Bold, t.pt(15.0), t.text, label);
            let when = Rect::new(x + t.px(130.0), row.y, t.px(190.0), row.h);
            ui.text_in(when, Face::Sans, t.pt(14.0), t.dim, &format_when(e.measured_ms, tz));
            let note = Rect::new(x + t.px(320.0), row.y, row.w - t.px(340.0), row.h);
            ui.text_in(note, Face::Sans, t.pt(14.0), t.item, &e.note);
        });
        if let Some(i) = res.activated {
            *todo = Some(Todo::OpenEdit(i));
        }
    }

    fn edit_modal(&mut self, ui: &mut Ui, todo: &mut Option<Todo>) {
        let unit = self.view_unit().to_owned();
        let Some(e) = &mut self.edit else { return };
        let t = ui.theme();
        let mut close = ui.key(keys::KEY_ESC);
        ui.modal(520.0, 345.0, |ui| {
            ui.heading("Edit weigh-in");
            ui.space(6.0);
            if std::mem::take(&mut e.first) {
                ui.focus("edit-weight");
            }
            ui.dim(&format!("Weight ({unit})"));
            if ui.text_field("edit-weight", &mut e.weight).submitted {
                *todo = Some(Todo::SaveEdit);
            }
            ui.dim("When (year-month-day hour:minute)");
            if ui.text_field("edit-when", &mut e.when).submitted {
                *todo = Some(Todo::SaveEdit);
            }
            ui.dim("Note");
            if ui.text_field("edit-note", &mut e.note).submitted {
                *todo = Some(Todo::SaveEdit);
            }
            if let Some(m) = &e.error {
                ui.space(4.0);
                ui.wrapped(Face::Sans, t.pt(14.0), t.danger, m);
            }
            ui.space(10.0);
            ui.row(40.0, |ui| {
                if e.confirm_delete {
                    if ui.button_id("edit-keep", "Keep it") {
                        e.confirm_delete = false;
                    }
                    if ui.primary_button("Delete this weigh-in") {
                        *todo = Some(Todo::Delete);
                    }
                } else {
                    if ui.button_id("edit-cancel", "Cancel") {
                        close = true;
                    }
                    if ui.primary_button(if e.pending { "Saving…" } else { "Save" }) {
                        *todo = Some(Todo::SaveEdit);
                    }
                    if ui.button_id("edit-delete", "Delete") {
                        e.confirm_delete = true;
                    }
                }
            });
        });
        if close {
            self.edit = None;
        }
    }

    fn settings_modal(&mut self, ui: &mut Ui, todo: &mut Option<Todo>) {
        let Some(s) = &mut self.settings else { return };
        let t = ui.theme();
        let mut close = ui.key(keys::KEY_ESC);
        let mut pick = None;
        ui.modal(480.0, 265.0, |ui| {
            ui.heading("Settings");
            ui.space(6.0);
            ui.dim("Unit");
            ui.row(38.0, |ui| {
                for (u, label) in [("kg", "Kilograms (kg)"), ("lb", "Pounds (lb)")] {
                    let hit = if s.unit == u {
                        ui.primary_button(label)
                    } else {
                        ui.button_id(("settings-unit", usize::from(u == "lb")), label)
                    };
                    if hit {
                        pick = Some(u);
                    }
                }
            });
            ui.space(6.0);
            ui.dim(&format!("Goal weight ({})", s.unit));
            if std::mem::take(&mut s.first) {
                ui.focus("settings-goal");
            }
            if ui.text_field("settings-goal", &mut s.goal).submitted {
                *todo = Some(Todo::SaveSettings);
            }
            if let Some(m) = &s.error {
                ui.space(4.0);
                ui.wrapped(Face::Sans, t.pt(14.0), t.danger, m);
            }
            ui.space(10.0);
            ui.row(40.0, |ui| {
                if ui.button_id("settings-cancel", "Cancel") {
                    close = true;
                }
                if ui.primary_button(if s.pending { "Saving…" } else { "Save settings" }) {
                    *todo = Some(Todo::SaveSettings);
                }
            });
        });
        if let Some(u) = pick {
            self.settings_unit(u);
        }
        if close {
            self.settings = None;
        }
    }
}

fn legend(ui: &mut Ui, x: i32, row: Rect, c: Rgb, label: &str) {
    let t = ui.theme();
    ui.fill(Rect::new(x, row.y + row.h / 2 - 1, t.px(14.0), 2), c);
    let r = Rect::new(x + t.px(20.0), row.y, t.px(100.0), row.h);
    ui.text_in(r, Face::Sans, t.pt(12.0), t.dim, label);
}

/// Straight lines through the points, left to right. Many points on one column cost one short span each.
fn polyline(ui: &mut Ui, points: &[(i32, i32)], c: Rgb, thick: i32) {
    if let [only] = points {
        ui.fill(Rect::new(only.0 - thick, only.1 - thick, thick * 2 + 1, thick * 2 + 1), c);
    }
    for pair in points.windows(2) {
        let ((x0, y0), (x1, y1)) = (pair[0], pair[1]);
        let steps = (x1 - x0).max(1);
        let mut last_y = y0;
        for s in 1..=steps {
            let y = y0 + (y1 - y0) * s / steps;
            let (top, bottom) = (last_y.min(y), last_y.max(y));
            ui.fill(Rect::new(x0 + s - 1, top, thick, bottom - top + thick), c);
            last_y = y;
        }
    }
}

impl App for Weight {
    type Msg = Msg;

    fn view(&mut self, ui: &mut Ui) {
        ui.header("Weight");
        if let Some(v) = &mut self.sign_in {
            v.view(ui, "Your weigh-ins wait in your account. This laptop asks to be added as a device.");
            return;
        }
        let mut todo = None;
        self.view_signed(ui, &mut todo);
        self.edit_modal(ui, &mut todo);
        self.settings_modal(ui, &mut todo);
        if self.last_load.is_none_or(|t| t.elapsed() >= REFRESH) {
            todo = todo.or(Some(Todo::Refresh));
        }
        ui.wake_after(REFRESH);
        match todo {
            Some(Todo::Log) => self.submit_log(),
            Some(Todo::Refresh) => self.refresh(),
            Some(Todo::OpenEdit(i)) => {
                if let Some(e) = self.view.as_ref().and_then(|v| v.entries.get(i)).cloned() {
                    self.open_edit(e);
                }
            }
            Some(Todo::OpenSettings) => self.open_settings(),
            Some(Todo::SaveEdit) => self.save_edit(),
            Some(Todo::Delete) => self.delete_entry(),
            Some(Todo::SaveSettings) => self.save_settings(),
            None => {}
        }
    }

    fn message(&mut self, msg: Msg) {
        match msg {
            Msg::Changed => {}
            Msg::SignedIn => self.signed_in(),
            Msg::SignedOut => self.signed_out(),
            Msg::LoadFailed(e) => self.load_error = Some(e),
            Msg::Done(tag, result) => self.done(tag, result),
            Msg::View(v) => {
                self.load_error = None;
                if !self.unit_chosen {
                    self.unit.clone_from(&v.settings.unit);
                }
                self.list.invalidate();
                // Enter acts on the selected row: without one, the keyboard could not open a weigh-in.
                match self.list.selected {
                    None if !v.entries.is_empty() => self.list.select(0),
                    Some(i) if i >= v.entries.len() => self.list.select(v.entries.len().saturating_sub(1)),
                    _ => {}
                }
                self.view = Some(*v);
                if let (Some(save), Some(v)) = (&mut self.save_cache, &self.view) {
                    let unit = self.unit_chosen.then_some(self.unit.as_str()).unwrap_or_default();
                    save(v, unit);
                }
            }
        }
    }
}
