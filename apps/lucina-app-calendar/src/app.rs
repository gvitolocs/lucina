//! The screen: month, week and day views as web/src/views/Calendar.tsx has them at desk width; an
//! event's sheet and the editor as dialogs. Everything drawn comes from memory.

use std::time::{Duration, Instant};

use chrono::{Datelike, NaiveDate, Timelike};
use lucina_account::credential;
use lucina_account::ui::SignInView;
use lucina_app::keys;
use lucina_app::{App, Face, Id, ListState, Rect, Rgb, TextEdit, Theme, Ui};
use secretary_calendar::model::local_to_utc;
use secretary_calendar::{Event, Source};

use crate::account::CACHE_FILE;
use crate::store::{
    Change, Form, Store, add_months, calendar_color, calendar_name, days_of, default_start,
    draft_from_form, form_for_event, form_for_range, local, month_of, overlap_columns, plain_notes,
    shift_end, when_label,
};
use crate::worker::{Job, Msg};

const TOAST: Duration = Duration::from_secs(8);
const KEY_F5: u16 = 63;
/// One hour in the time grid, in reference pixels.
const HOUR: f32 = 44.0;

pub type Link = Box<dyn Fn(Job)>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    NotSignedIn,
    Sample,
    Account,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Day,
    Week,
    Month,
}

pub struct Editor {
    /// The event being changed: its id and start. None: a new one.
    pub editing: Option<(String, i64)>,
    pub title: TextEdit,
    pub all_day: bool,
    pub start_date: TextEdit,
    pub start_time: TextEdit,
    pub end_date: TextEdit,
    pub end_time: TextEdit,
    pub location: TextEdit,
    pub video: TextEdit,
    pub notes: TextEdit,
    pub error: Option<String>,
    /// The start as last seen, so that moving it moves the end.
    last: Form,
}

impl Editor {
    fn new(f: &Form, editing: Option<(String, i64)>) -> Editor {
        let mut notes = TextEdit::multiline().placeholder("Notes");
        notes.set(&f.notes);
        Editor {
            editing,
            title: TextEdit::with_text(&f.title).placeholder("Title"),
            all_day: f.all_day,
            start_date: TextEdit::with_text(&f.start_date).placeholder("YYYY-MM-DD"),
            start_time: TextEdit::with_text(&f.start_time).placeholder("HH:MM"),
            end_date: TextEdit::with_text(&f.end_date).placeholder("YYYY-MM-DD"),
            end_time: TextEdit::with_text(&f.end_time).placeholder("HH:MM"),
            location: TextEdit::with_text(&f.location).placeholder("Location"),
            video: TextEdit::with_text(&f.video).placeholder("Video call link"),
            notes,
            error: None,
            last: f.clone(),
        }
    }

    pub fn form(&self) -> Form {
        Form {
            title: self.title.text.clone(),
            all_day: self.all_day,
            start_date: self.start_date.text.clone(),
            start_time: self.start_time.text.clone(),
            end_date: self.end_date.text.clone(),
            end_time: self.end_time.text.clone(),
            location: self.location.text.clone(),
            notes: self.notes.text.clone(),
            video: self.video.text.clone(),
        }
    }
}

pub struct Calendar {
    pub store: Store,
    pub mode: Mode,
    link: Option<Link>,
    start_sample: Option<Box<dyn FnMut() -> Link>>,
    start_account: Option<Box<dyn FnMut() -> Option<Link>>>,
    begin_sign_in: Option<Box<dyn FnMut() -> Option<SignInView>>>,
    sign_in: Option<SignInView>,
    pub kind: Kind,
    pub cursor: NaiveDate,
    /// The event under the keyboard: its index in `store.events`.
    pub marked: Option<usize>,
    /// The time grid's scroll, kept in a list of one tall row.
    grid: ListState,
    grid_set: bool,
    side: ListState,
    /// The event's sheet: id and start, so that it survives a reload.
    pub detail: Option<(String, i64)>,
    pub confirm_delete: bool,
    pub editor: Option<Editor>,
    pub toast: Option<(String, Instant, bool)>,
    pub offline: Option<String>,
    want_focus: Option<&'static str>,
}

fn hit(ui: &mut Ui, id: impl Into<Id>, r: Rect) -> bool {
    ui.hit(id, r).clicked
}

/// The colour on the event's block: its calendar's, over the background.
fn tint(c: Rgb, bg: Rgb, a: f32) -> Rgb {
    let mix = |x: u8, y: u8| (x as f32 * a + y as f32 * (1.0 - a)) as u8;
    let (cx, bx) = (c.0, bg.0);
    let r = mix((cx >> 16) as u8, (bx >> 16) as u8) as u32;
    let g = mix((cx >> 8) as u8, (bx >> 8) as u8) as u32;
    let b = mix(cx as u8, bx as u8) as u32;
    Rgb::hex((r << 16) | (g << 8) | b)
}

/// The Monday on or before `d`, as the owner's week starts (the web app's first day).
fn week_start(d: NaiveDate) -> NaiveDate {
    d - chrono::Duration::days(d.weekday().num_days_from_monday() as i64)
}

impl Calendar {
    pub fn new(mut store: Store, mode: Mode, link: Option<Link>) -> Calendar {
        store.rebuild();
        let today = store.today();
        let app = Calendar {
            store,
            mode,
            link,
            start_sample: None,
            start_account: None,
            begin_sign_in: None,
            sign_in: None,
            kind: Kind::Week,
            cursor: today,
            marked: None,
            grid: ListState::new(),
            grid_set: false,
            side: ListState::new(),
            detail: None,
            confirm_delete: false,
            editor: None,
            toast: None,
            offline: None,
            want_focus: None,
        };
        app.load();
        app
    }

    pub fn with_starters(
        mut self,
        sample: Box<dyn FnMut() -> Link>,
        account: Box<dyn FnMut() -> Option<Link>>,
        sign_in: Box<dyn FnMut() -> Option<SignInView>>,
    ) -> Calendar {
        self.start_sample = Some(sample);
        self.start_account = Some(account);
        self.begin_sign_in = Some(sign_in);
        if self.mode == Mode::NotSignedIn {
            self.show_sign_in();
        }
        self
    }

    fn show_sign_in(&mut self) {
        self.mode = Mode::NotSignedIn;
        self.link = None;
        self.sign_in = self.begin_sign_in.as_mut().and_then(|f| f());
        self.store.months.clear();
        self.store.changes.clear();
        self.store.rebuild();
        self.detail = None;
        self.editor = None;
        self.toast = None;
    }

    fn signed_in(&mut self) {
        let link = self.start_account.as_mut().and_then(|f| f());
        if link.is_some() {
            self.link = link;
            self.mode = Mode::Account;
            self.sign_in = None;
            self.load();
        }
    }

    fn send(&self, j: Job) {
        if let Some(l) = &self.link {
            l(j);
        }
    }

    /// The days on screen.
    pub fn range(&self) -> (NaiveDate, NaiveDate) {
        match self.kind {
            Kind::Day => (self.cursor, self.cursor),
            Kind::Week => {
                let s = week_start(self.cursor);
                (s, s + chrono::Duration::days(6))
            }
            Kind::Month => {
                let s = week_start(month_of(self.cursor));
                (s, s + chrono::Duration::days(41))
            }
        }
    }

    pub fn visible_months(&self) -> Vec<NaiveDate> {
        let (a, b) = self.range();
        let mut out = Vec::new();
        let mut m = month_of(a);
        while m <= month_of(b) {
            out.push(m);
            m = add_months(m, 1);
        }
        out
    }

    /// Load what's on screen, and have the months around it ready.
    fn load(&self) {
        if self.link.is_none() {
            return;
        }
        let months = self.visible_months();
        let next: Vec<NaiveDate> = [-1, 1, 2, -2]
            .into_iter()
            .map(|n| add_months(month_of(self.cursor), n))
            .filter(|m| !months.contains(m) && !self.store.months.contains_key(m))
            .collect();
        self.send(Job::Visible(months));
        self.send(Job::Prefetch(next));
    }

    pub fn go(&mut self, d: NaiveDate) {
        self.cursor = d;
        self.marked = None;
        self.load();
    }

    pub fn set_kind(&mut self, k: Kind) {
        self.kind = k;
        self.marked = None;
        self.load();
    }

    /// Back or forward one day, week or month.
    pub fn page(&mut self, n: i32) {
        let d = match self.kind {
            Kind::Day => self.cursor + chrono::Duration::days(n as i64),
            Kind::Week => self.cursor + chrono::Duration::days(7 * n as i64),
            Kind::Month => {
                let m = add_months(month_of(self.cursor), n);
                let last = (add_months(m, 1) - chrono::Duration::days(1)).day();
                m.with_day(self.cursor.day().min(last)).unwrap_or(m)
            }
        };
        self.go(d);
    }

    fn title(&self) -> String {
        match self.kind {
            Kind::Day => self.cursor.format("%A %-d %B %Y").to_string(),
            Kind::Week => {
                let (a, b) = self.range();
                if a.month() == b.month() {
                    format!("{} – {}", a.format("%-d"), b.format("%-d %B %Y"))
                } else {
                    format!("{} – {}", a.format("%-d %b"), b.format("%-d %b %Y"))
                }
            }
            Kind::Month => self.cursor.format("%B %Y").to_string(),
        }
    }

    pub fn event(&self, id: &str, start: i64) -> Option<&Event> {
        self.store
            .events
            .iter()
            .find(|e| e.id == id && e.start_ms == start)
    }

    pub fn open_event(&mut self, i: usize) {
        if let Some(e) = self.store.events.get(i) {
            self.detail = Some((e.id.clone(), e.start_ms));
            self.confirm_delete = false;
        }
    }

    /// `openNew`: from a slot or the + button.
    pub fn new_event(&mut self, start: i64, end: i64, all_day: bool) {
        self.detail = None;
        let f = form_for_range(start, end, all_day, self.store.tz);
        self.editor = Some(Editor::new(&f, None));
        self.want_focus = Some("ed-title");
    }

    pub fn new_event_default(&mut self) {
        let s = default_start(self.cursor, (self.store.now)(), self.store.tz);
        self.new_event(s, s + 3_600_000, false);
    }

    /// Only an editable event ever gets here; the server refuses the others anyway.
    pub fn edit_event(&mut self, id: &str, start: i64) {
        let Some(e) = self.event(id, start).filter(|e| e.editable) else {
            return;
        };
        let f = form_for_event(e, self.store.tz);
        self.editor = Some(Editor::new(&f, Some((id.to_owned(), start))));
        self.detail = None;
        self.want_focus = Some("ed-title");
    }

    /// `saveEditor`, without the wait: the event is on screen as the dialog closes. If the server
    /// says no, the dialog comes back as it was, with the reason.
    pub fn save_editor(&mut self) {
        let Some(ed) = &mut self.editor else {
            return;
        };
        let form = ed.form();
        let draft = match draft_from_form(&form, self.store.tz) {
            Ok(d) => d,
            Err(e) => {
                ed.error = Some(e.text().to_owned());
                return;
            }
        };
        let editing = ed.editing.clone();
        if ed.error.is_none()
            && let Some(dup) = self
                .store
                .duplicate_of(&draft, editing.as_ref().map(|x| x.0.as_str()))
        {
            let name = calendar_name(dup);
            let Some(ed) = &mut self.editor else {
                return;
            };
            ed.error = Some(format!(
                "This is already in {name}, at the same time. Save again to add it anyway."
            ));
            return;
        }
        self.editor = None;
        let op = self.store.next_op();
        match editing {
            None => {
                let ghost = self.store.ghost(&draft, op, None);
                self.store.changes.push(Change::Create { op, ghost, form });
                self.store.rebuild();
                self.send(Job::Create(op, draft.clone()));
                self.toast(&format!("Added “{}”", draft.title));
            }
            Some((id, start_ms)) => {
                let base = self.event(&id, start_ms).cloned();
                let ghost = self.store.ghost(&draft, op, base.as_ref());
                self.store.changes.push(Change::Update {
                    op,
                    id: id.clone(),
                    start_ms,
                    ghost,
                    form,
                });
                self.store.rebuild();
                self.send(Job::Update(op, id, draft.clone()));
                self.toast(&format!("Saved “{}”", draft.title));
            }
        }
        self.marked = None;
    }

    pub fn delete_event(&mut self, id: &str, start_ms: i64) {
        let Some(e) = self.event(id, start_ms).filter(|e| e.editable) else {
            return;
        };
        let title = e.title.clone();
        let op = self.store.next_op();
        self.store.changes.push(Change::Delete {
            op,
            id: id.to_owned(),
            start_ms,
        });
        self.store.rebuild();
        self.detail = None;
        self.marked = None;
        self.send(Job::Delete(op, id.to_owned()));
        self.toast(&format!("Deleted “{title}”"));
    }

    fn toast(&mut self, s: &str) {
        self.toast = Some((s.to_owned(), Instant::now() + TOAST, false));
    }

    fn take_change(&mut self, op: u64) -> Option<Change> {
        let pos = self.store.changes.iter().position(|c| c.op() == op)?;
        Some(self.store.changes.remove(pos))
    }

    // ---- Drawing ----

    fn draw_not_signed_in(&mut self, ui: &mut Ui) {
        let t = ui.theme();
        let foot = ui.cut_bottom(t.px(60.0));
        const WHY: &str = "Your calendar lives in your Secretary account. Approve this laptop on your phone and \
                           your iPhone, Google and Lucina calendars appear here.";
        match &mut self.sign_in {
            Some(v) => v.view(ui, WHY),
            None => {
                ui.pad(40.0);
                ui.heading("Sign in to Secretary");
                ui.paragraph(WHY);
            }
        }
        let w = t.px(220.0);
        let b = Rect::new(
            foot.x + (foot.w - w) / 2,
            foot.y + t.px(10.0),
            w,
            t.px(34.0),
        );
        if ui.button_at(b, "sample", "Try it with sample data")
            && let Some(start) = &mut self.start_sample
        {
            self.sign_in = None;
            self.link = Some(start());
            self.mode = Mode::Sample;
            self.load();
        }
    }

    fn draw_toolbar(&mut self, ui: &mut Ui) {
        let t = ui.theme();
        let r = ui.cut_top(t.px(42.0));
        let h = t.px(30.0);
        let y = r.y + (r.h - h) / 2;
        let mut x = r.x;
        let button = |ui: &mut Ui, x: &mut i32, id: &str, s: &str, w: f32| {
            let w = t.px(w);
            let b = ui.button_at(Rect::new(*x, y, w, h), id, s);
            *x += w + t.px(6.0);
            b
        };
        if button(ui, &mut x, "prev", "‹", 34.0) {
            self.page(-1);
        }
        if button(ui, &mut x, "today", "Today", 64.0) {
            let d = self.store.today();
            self.go(d);
        }
        if button(ui, &mut x, "next", "›", 34.0) {
            self.page(1);
        }
        x += t.px(8.0);
        let title = self.title();
        let tw = ui.measure(Face::Bold, t.pt(19.0), &title);
        ui.text_in(
            Rect::new(x, r.y, tw, r.h),
            Face::Bold,
            t.pt(19.0),
            t.text,
            &title,
        );
        x += tw + t.px(14.0);
        let (note, c) = if self.mode == Mode::Sample {
            ("Sample data, not your account".to_owned(), t.accent)
        } else if let Some(why) = &self.offline {
            (
                format!("Offline, showing what's in memory: {why}"),
                t.danger,
            )
        } else {
            (String::new(), t.dim)
        };
        // Between the title and the view buttons, cut to what's left.
        let room = r.right() - t.px(322.0) - x;
        let nw = ui.measure(Face::Sans, t.pt(13.0), &note).min(room.max(0));
        ui.text_in(Rect::new(x, r.y, nw, r.h), Face::Sans, t.pt(13.0), c, &note);

        // On the right: the views, and the new event.
        let bw = t.px(64.0);
        let nw = t.px(100.0);
        let mut x = r.right() - nw - 3 * (bw + t.px(4.0)) - t.px(10.0);
        for (k, id, label) in [
            (Kind::Day, "v-day", "Day"),
            (Kind::Week, "v-week", "Week"),
            (Kind::Month, "v-month", "Month"),
        ] {
            let rect = Rect::new(x, y, bw, h);
            if self.kind == k {
                ui.fill_round(rect, t.px(6.0), t.selection);
                let w = ui.measure(Face::Sans, t.pt(15.0), label);
                ui.text_in(
                    Rect::new(x + (bw - w) / 2, y, w, h),
                    Face::Sans,
                    t.pt(15.0),
                    t.text,
                    label,
                );
                if hit(ui, id, rect) {
                    self.set_kind(k);
                }
            } else if ui.button_at(rect, id, label) {
                self.set_kind(k);
            }
            x += bw + t.px(4.0);
        }
        let rect = Rect::new(r.right() - nw, y, nw, h);
        ui.fill_round(rect, t.px(6.0), t.accent);
        let w = ui.measure(Face::Bold, t.pt(14.0), "+ New");
        ui.text_in(
            Rect::new(rect.x + (nw - w) / 2, y, w, h),
            Face::Bold,
            t.pt(14.0),
            t.bg,
            "+ New",
        );
        if hit(ui, "new", rect) {
            self.new_event_default();
        }
    }

    /// Left: a small month to jump with, and the calendars to show.
    fn draw_side(&mut self, ui: &mut Ui, r: Rect) {
        let t = ui.theme();
        ui.fill(r, t.bar);
        let inner = r.inset(t.px(10.0));
        let today = self.store.today();
        let month = month_of(self.cursor);
        let label = month.format("%B %Y").to_string();
        ui.text_in(
            Rect::new(inner.x, inner.y, inner.w, t.px(26.0)),
            Face::Bold,
            t.pt(14.0),
            t.item,
            &label,
        );
        let top = inner.y + t.px(28.0);
        let cw = inner.w / 7;
        let ch = t.px(24.0);
        for (i, name) in ["M", "T", "W", "T", "F", "S", "S"].iter().enumerate() {
            let w = ui.measure(Face::Sans, t.pt(12.0), name);
            ui.text_in(
                Rect::new(inner.x + i as i32 * cw + (cw - w) / 2, top, w, ch),
                Face::Sans,
                t.pt(12.0),
                t.dim,
                name,
            );
        }
        let start = week_start(month);
        let mut pick = None;
        for i in 0..42 {
            let d = start + chrono::Duration::days(i as i64);
            let x = inner.x + (i % 7) * cw;
            let y = top + ch + (i / 7) * ch;
            let cell = Rect::new(x, y, cw, ch);
            let (lo, hi) = self.range();
            if self.kind != Kind::Month && d >= lo && d <= hi {
                ui.fill(cell, t.panel_hi);
            }
            if d == today {
                let s = ch - t.px(2.0);
                ui.fill_round(
                    Rect::new(x + (cw - s) / 2, y + t.px(1.0), s, s),
                    s / 2,
                    t.accent,
                );
            }
            let c = if d == today {
                t.bg
            } else if d.month() != month.month() {
                t.selection
            } else {
                t.item
            };
            let s = d.day().to_string();
            let w = ui.measure(Face::Sans, t.pt(12.0), &s);
            ui.text_in(
                Rect::new(x + (cw - w) / 2, y, w, ch),
                Face::Sans,
                t.pt(12.0),
                c,
                &s,
            );
            if !self.store.on_day(d).is_empty() && d != today {
                let dot = t.px(3.0).max(2);
                ui.fill_round(
                    Rect::new(x + (cw - dot) / 2, y + ch - dot - 1, dot, dot),
                    dot / 2,
                    t.dim,
                );
            }
            if hit(ui, ("mini", i as usize), cell) {
                pick = Some(d);
            }
        }
        if let Some(d) = pick {
            self.go(d);
        }

        let list_top = top + ch * 7 + t.px(14.0);
        let list = Rect::new(inner.x, list_top, inner.w, inner.bottom() - list_top);
        ui.text_in(
            Rect::new(list.x, list.y, list.w, t.px(24.0)),
            Face::Bold,
            t.pt(13.0),
            t.dim,
            "Calendars",
        );
        let rows = Rect::new(list.x, list.y + t.px(26.0), list.w, list.h - t.px(26.0));
        let cals = self.store.calendars.clone();
        let hidden = &self.store.hidden;
        let res = ui.list(
            "calendars",
            &mut self.side,
            rows,
            cals.len(),
            28.0,
            |ui, i, _| {
                let a = ui.area();
                let (name, source) = &cals[i];
                let on = !hidden.contains(name);
                let s = t.px(12.0);
                let c = calendar_color(name, *source, &t);
                let swatch = Rect::new(a.x + t.px(4.0), a.y + (a.h - s) / 2, s, s);
                ui.fill_round(swatch, t.px(3.0), if on { c } else { t.panel });
                let tc = if on { t.item } else { t.dim };
                ui.text_in(
                    Rect::new(a.x + t.px(24.0), a.y, a.w - t.px(26.0), a.h),
                    Face::Sans,
                    t.pt(14.0),
                    tc,
                    name,
                );
            },
        );
        self.side.selected = None;
        if let Some(i) = res.activated {
            let name = cals[i].0.clone();
            if !self.store.hidden.remove(&name) {
                self.store.hidden.insert(name);
            }
            self.store.rebuild();
            self.marked = None;
        }
    }

    fn event_rect_colors(&self, e: &Event, t: &Theme) -> (Rgb, Rgb, Rgb) {
        let name = calendar_name(e);
        let c = calendar_color(&name, e.source, t);
        let past = e.end_ms <= (self.store.now)();
        let pending = e.id.starts_with("pending:");
        let a = if pending || e.free {
            0.25
        } else if past {
            0.22
        } else {
            0.42
        };
        let text = if past { t.item } else { t.text };
        (c, tint(c, t.bg, a), text)
    }

    /// A chip on one line: all-day events, and events in a month's cell.
    fn draw_chip(
        &mut self,
        ui: &mut Ui,
        r: Rect,
        i: usize,
        timed_prefix: bool,
        day: NaiveDate,
    ) -> bool {
        let t = ui.theme();
        let e = &self.store.events[i];
        let (c, fill, text) = self.event_rect_colors(e, &t);
        let marked = self.marked == Some(i);
        ui.fill_round(r, t.px(3.0), if marked { t.selection } else { fill });
        ui.fill(Rect::new(r.x, r.y + 1, t.px(3.0).max(2), r.h - 2), c);
        let label = if timed_prefix && !e.all_day {
            let at = local(e.start_ms, self.store.tz);
            let start = if at.date() == day {
                at.format("%H:%M ").to_string()
            } else {
                String::new()
            };
            format!("{start}{}", e.title)
        } else {
            e.title.clone()
        };
        ui.text_in(
            Rect::new(r.x + t.px(6.0), r.y, r.w - t.px(8.0), r.h),
            Face::Sans,
            t.pt(12.0),
            text,
            &label,
        );
        hit(ui, ("chip", i ^ (day.num_days_from_ce() as usize) << 16), r)
    }

    fn draw_grid(&mut self, ui: &mut Ui, days: &[NaiveDate]) {
        let t = ui.theme();
        let today = self.store.today();
        let area = ui.area();
        let gutter = t.px(52.0);
        let bar = t.px(6.0).max(4); // The list's scroll bar.
        let cw = (area.w - gutter - bar) / days.len() as i32;

        // The day names.
        let head = ui.cut_top(t.px(36.0));
        let mut go = None;
        for (i, d) in days.iter().enumerate() {
            let x = area.x + gutter + i as i32 * cw;
            let cell = Rect::new(x, head.y, cw, head.h);
            let is_today = *d == today;
            let c = if is_today { t.accent } else { t.item };
            let label = d.format("%a %-d").to_string();
            let f = if is_today { Face::Bold } else { Face::Sans };
            let w = ui.measure(f, t.pt(15.0), &label);
            ui.text_in(
                Rect::new(x + (cw - w) / 2, head.y, w, head.h),
                f,
                t.pt(15.0),
                c,
                &label,
            );
            if days.len() > 1 && hit(ui, ("head", i), cell) {
                go = Some(*d);
            }
            if *d == self.cursor && days.len() > 1 {
                ui.fill(
                    Rect::new(x + t.px(10.0), head.bottom() - 2, cw - t.px(20.0), 2),
                    t.accent,
                );
            }
        }

        // All day: a band as tall as the fullest day.
        let per_day: Vec<Vec<usize>> = days
            .iter()
            .map(|d| {
                self.store
                    .on_day(*d)
                    .iter()
                    .copied()
                    .filter(|&i| self.store.events[i].all_day)
                    .collect()
            })
            .collect();
        let most = per_day.iter().map(Vec::len).max().unwrap_or(0).min(4) as i32;
        let line = t.px(21.0);
        let mut open = None;
        let mut slot_all_day = None;
        if most > 0 {
            let band = ui.cut_top(most * line + t.px(6.0));
            ui.fill(band, t.panel);
            let w = ui.measure(Face::Sans, t.pt(11.0), "all day");
            ui.text_in(
                Rect::new(band.x + gutter - w - t.px(6.0), band.y, w, line),
                Face::Sans,
                t.pt(11.0),
                t.dim,
                "all day",
            );
            for (col, list) in per_day.iter().enumerate() {
                let x = band.x + gutter + col as i32 * cw;
                for (n, &i) in list.iter().take(most as usize).enumerate() {
                    let r = Rect::new(
                        x + 1,
                        band.y + t.px(3.0) + n as i32 * line,
                        cw - 3,
                        line - 2,
                    );
                    if self.draw_chip(ui, r, i, false, days[col]) {
                        open = Some(i);
                    }
                }
                if hit(ui, ("allday", col), Rect::new(x, band.y, cw, band.h)) {
                    slot_all_day = Some(days[col]);
                }
            }
        }
        ui.fill(Rect::new(area.x, ui.area().y - 1, area.w, 1), t.panel_hi);

        // The hours: one tall row of a list, which brings the scrolling and the clip.
        let hour = t.px(HOUR);
        let body = ui.area();
        if !self.grid_set && body.h > 0 {
            // Start where the day starts, or an hour before now.
            let now = local((self.store.now)(), self.store.tz);
            let from = if days.contains(&today) {
                (now.hour() as i32 - 1).clamp(0, 16)
            } else {
                7
            };
            self.grid.scroll = from * hour;
            self.grid_set = true;
        }
        let now_ms = (self.store.now)();
        let tz = self.store.tz;
        let mut slot = None;
        let store = &self.store;
        let marked = self.marked;
        let columns: Vec<Vec<(usize, usize, usize)>> = days
            .iter()
            .map(|d| {
                let timed: Vec<usize> = store
                    .on_day(*d)
                    .iter()
                    .copied()
                    .filter(|&i| !store.events[i].all_day)
                    .collect();
                let spans: Vec<(i64, i64)> = timed
                    .iter()
                    .map(|&i| {
                        let e = &store.events[i];
                        (e.start_ms, e.end_ms.max(e.start_ms + 900_000))
                    })
                    .collect();
                overlap_columns(&spans)
                    .into_iter()
                    .zip(timed)
                    .map(|((col, n), i)| (i, col, n))
                    .collect()
            })
            .collect();
        ui.list("grid", &mut self.grid, body, 1, HOUR * 24.0, |ui, _, _| {
            let a = ui.area();
            for h in 0..24 {
                let y = a.y + h * hour;
                ui.fill(Rect::new(a.x + gutter, y, a.w - gutter, 1), t.panel);
                if h > 0 {
                    let s = format!("{h:02}:00");
                    let w = ui.measure(Face::Sans, t.pt(11.0), &s);
                    ui.text_in(
                        Rect::new(a.x + gutter - w - t.px(6.0), y - t.px(8.0), w, t.px(16.0)),
                        Face::Sans,
                        t.pt(11.0),
                        t.dim,
                        &s,
                    );
                }
            }
            for (col, d) in days.iter().enumerate() {
                let x = a.x + gutter + col as i32 * cw;
                ui.fill(Rect::new(x, a.y, 1, a.h), t.panel);
                if *d == today {
                    ui.fill(
                        Rect::new(x + 1, a.y, cw - 1, a.h),
                        tint(t.accent, t.bg, 0.04),
                    );
                }
                // The blocks first: a click on one isn't a click on the slot under it.
                for &(i, c, n) in &columns[col] {
                    let e = &store.events[i];
                    let (first, _) = days_of(e, tz);
                    let (s, en) = (local(e.start_ms, tz), local(e.end_ms, tz));
                    let top_min = if first == *d {
                        s.hour() * 60 + s.minute()
                    } else {
                        0
                    };
                    let end_min = if en.date() == *d {
                        en.hour() * 60 + en.minute()
                    } else {
                        24 * 60
                    };
                    let end_min = end_min.max(top_min + 15);
                    let y = a.y + top_min as i32 * hour / 60;
                    let h = ((end_min - top_min) as i32 * hour / 60).max(t.px(16.0));
                    let w = (cw - 4) / n as i32;
                    let r = Rect::new(x + 2 + c as i32 * w, y + 1, w - 2, h - 2);
                    let name = calendar_name(e);
                    let cc = calendar_color(&name, e.source, &t);
                    let past = e.end_ms <= now_ms;
                    let alpha = if e.id.starts_with("pending:") || e.free {
                        0.25
                    } else if past {
                        0.22
                    } else {
                        0.42
                    };
                    let fill = if marked == Some(i) {
                        t.selection
                    } else {
                        tint(cc, t.bg, alpha)
                    };
                    ui.fill_round(r, t.px(4.0), fill);
                    ui.fill(Rect::new(r.x, r.y + 2, t.px(3.0).max(2), r.h - 4), cc);
                    let tx = Rect::new(
                        r.x + t.px(7.0),
                        r.y + 1,
                        r.w - t.px(9.0),
                        t.px(18.0).min(r.h),
                    );
                    let tc = if past { t.item } else { t.text };
                    ui.text_in(tx, Face::Bold, t.pt(12.0), tc, &e.title);
                    if r.h > t.px(38.0) {
                        let when = format!("{}–{}", s.format("%H:%M"), en.format("%H:%M"));
                        ui.text_in(
                            Rect::new(tx.x, tx.y + t.px(17.0), tx.w, t.px(15.0)),
                            Face::Sans,
                            t.pt(11.0),
                            t.item,
                            &when,
                        );
                    }
                    if r.h > t.px(56.0)
                        && let Some(l) = &e.location
                    {
                        ui.text_in(
                            Rect::new(tx.x, tx.y + t.px(32.0), tx.w, t.px(15.0)),
                            Face::Sans,
                            t.pt(11.0),
                            t.item,
                            l,
                        );
                    }
                    if hit(ui, ("ev", i), r) {
                        open = Some(i);
                    }
                }
                // The slot under the pointer, to the half hour.
                if hit(ui, ("slot", col), Rect::new(x, a.y, cw, a.h)) {
                    let half = (ui.pointer().1 - a.y) * 2 / hour.max(1);
                    slot = Some((*d, half.clamp(0, 47) as u32));
                }
                if *d == today {
                    let n = local(now_ms, tz);
                    let y = a.y + (n.hour() * 60 + n.minute()) as i32 * hour / 60;
                    ui.fill(Rect::new(x, y - 1, cw, 2), t.danger);
                    let dot = t.px(7.0);
                    ui.fill_round(
                        Rect::new(x - dot / 2, y - dot / 2, dot, dot),
                        dot / 2,
                        t.danger,
                    );
                }
            }
        });
        self.grid.selected = None;
        if let Some(i) = open {
            self.open_event(i);
        }
        if let Some(d) = go {
            self.cursor = d;
            self.set_kind(Kind::Day);
        }
        if let Some(d) = slot_all_day {
            let next = d + chrono::Duration::days(1);
            let s = local_to_utc(tz, d.and_hms_opt(0, 0, 0).expect("midnight")).timestamp_millis();
            let e =
                local_to_utc(tz, next.and_hms_opt(0, 0, 0).expect("midnight")).timestamp_millis();
            self.cursor = d;
            self.new_event(s, e, true);
        }
        if let Some((d, half)) = slot {
            let s = local_to_utc(
                tz,
                d.and_hms_opt(half / 2, half % 2 * 30, 0).expect("a time"),
            )
            .timestamp_millis();
            self.cursor = d;
            self.new_event(s, s + 3_600_000, false);
        }
    }

    fn draw_month(&mut self, ui: &mut Ui) {
        let t = ui.theme();
        let area = ui.area();
        let today = self.store.today();
        let start = week_start(month_of(self.cursor));
        let month = month_of(self.cursor).month();
        let head = t.px(24.0);
        let cw = area.w / 7;
        let ch = (area.h - head) / 6;
        for (i, name) in ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"]
            .iter()
            .enumerate()
        {
            let x = area.x + i as i32 * cw;
            ui.text_in(
                Rect::new(x + t.px(6.0), area.y, cw, head),
                Face::Sans,
                t.pt(13.0),
                t.dim,
                name,
            );
        }
        let line = t.px(19.0);
        let mut open = None;
        let mut pick = None;
        let mut more = None;
        for i in 0..42i32 {
            let d = start + chrono::Duration::days(i as i64);
            let cell = Rect::new(area.x + (i % 7) * cw, area.y + head + (i / 7) * ch, cw, ch);
            let bg = if d == self.cursor {
                t.panel_hi
            } else if d.month() != month {
                t.bar
            } else {
                t.bg
            };
            ui.fill(cell, bg);
            ui.fill(Rect::new(cell.x, cell.y, cell.w, 1), t.panel);
            ui.fill(Rect::new(cell.x, cell.y, 1, cell.h), t.panel);
            let s = d.day().to_string();
            let num = t.px(22.0);
            let w = ui.measure(Face::Sans, t.pt(13.0), &s);
            if d == today {
                ui.fill_round(
                    Rect::new(cell.x + t.px(4.0), cell.y + t.px(2.0), num, num),
                    num / 2,
                    t.accent,
                );
            }
            let c = if d == today {
                t.bg
            } else if d.month() != month {
                t.dim
            } else {
                t.item
            };
            ui.text_in(
                Rect::new(
                    cell.x + t.px(4.0) + (num - w) / 2,
                    cell.y + t.px(2.0),
                    w,
                    num,
                ),
                Face::Sans,
                t.pt(13.0),
                c,
                &s,
            );
            if hit(ui, ("cell", i as usize), cell) {
                pick = Some(d);
            }
            let list: Vec<usize> = self.store.on_day(d).to_vec();
            let room = ((cell.h - t.px(28.0)) / line).max(0) as usize;
            let show = if list.len() > room {
                room.saturating_sub(1)
            } else {
                list.len()
            };
            for (n, &ev) in list.iter().take(show).enumerate() {
                let r = Rect::new(
                    cell.x + 2,
                    cell.y + t.px(26.0) + n as i32 * line,
                    cell.w - 4,
                    line - 2,
                );
                if self.draw_chip(ui, r, ev, true, d) {
                    open = Some(ev);
                }
            }
            if list.len() > show {
                let r = Rect::new(
                    cell.x + t.px(6.0),
                    cell.y + t.px(26.0) + show as i32 * line,
                    cell.w - t.px(8.0),
                    line,
                );
                ui.text_in(
                    r,
                    Face::Sans,
                    t.pt(12.0),
                    t.dim,
                    &format!("+{} more", list.len() - show),
                );
                if hit(ui, ("more", i as usize), r) {
                    more = Some(d);
                }
            }
        }
        if let Some(i) = open {
            self.open_event(i);
        }
        if let Some(d) = more {
            self.cursor = d;
            self.set_kind(Kind::Day);
        } else if let Some(d) = pick {
            // A second click on the day opens it.
            if d == self.cursor {
                self.set_kind(Kind::Day);
            } else {
                self.go(d);
            }
        }
    }

    fn draw_detail(&mut self, ui: &mut Ui) {
        let t = ui.theme();
        let Some((id, start)) = self.detail.clone() else {
            return;
        };
        let Some(e) = self.event(&id, start).cloned() else {
            self.detail = None;
            return;
        };
        let name = calendar_name(&e);
        let color = calendar_color(&name, e.source, &t);
        let tz = self.store.tz;
        let (mut close, mut edit, mut del) = (false, false, false);
        let confirm = self.confirm_delete;
        let mut ask = false;
        // As tall as what it has to say.
        let mut lines = 3
            + e.recurring as u32
            + e.location.is_some() as u32
            + e.conference_url.is_some() as u32;
        lines += !e.attendees.is_empty() as u32
            + e.free as u32
            + !e.alarms.is_empty() as u32
            + e.added_by.is_some() as u32;
        let notes = e.description.as_deref().map_or(0, |n| {
            plain_notes(n)
                .lines()
                .map(|l| l.chars().count() as u32 / 62 + 1)
                .sum::<u32>()
                + 1
        });
        let h = (100 + (lines + notes) * 25).clamp(230, 620) as f32;
        ui.modal(560.0, h, |ui| {
            let foot = ui.cut_bottom(t.px(44.0));
            let a = ui.area();
            let s = t.px(14.0);
            ui.fill_round(Rect::new(a.x, a.y + t.px(6.0), s, s), s / 2, color);
            let head = ui.cut_top(t.px(28.0));
            ui.text_in(
                Rect::new(head.x + t.px(22.0), head.y, head.w - t.px(22.0), head.h),
                Face::Bold,
                t.pt(19.0),
                t.text,
                &e.title,
            );
            ui.label(&when_label(&e, tz));
            let source = match e.source {
                Source::Iphone => "iPhone calendar · read-only, change it on the phone".to_owned(),
                Source::Ics => format!("{name} · a subscription, read-only"),
                Source::Lucina => "Lucina's calendar".to_owned(),
                Source::Google => format!("{name} · Google"),
            };
            ui.dim(&source);
            if e.recurring {
                ui.dim("Repeats");
            }
            ui.space(6.0);
            if let Some(l) = &e.location {
                ui.label(&format!("Where: {l}"));
            }
            if let Some(v) = &e.conference_url {
                ui.label(&format!("Video call: {v}"));
            }
            if !e.attendees.is_empty() {
                ui.label(&format!("With: {}", e.attendees.join(", ")));
            }
            if e.free {
                ui.dim("Shown as free");
            }
            if !e.alarms.is_empty() {
                let alerts: Vec<String> = e
                    .alarms
                    .iter()
                    .map(|m| match m {
                        0 => "at the start".to_owned(),
                        m if m % 1440 == 0 => format!("{} d before", m / 1440),
                        m if m % 60 == 0 => format!("{} h before", m / 60),
                        m => format!("{m} min before"),
                    })
                    .collect();
                ui.dim(&format!("Alerts: {}", alerts.join(", ")));
            }
            if let Some(w) = &e.added_by {
                ui.dim(&format!("Added by {w}"));
            }
            if let Some(n) = &e.description {
                ui.space(6.0);
                ui.paragraph(&plain_notes(n));
            }
            ui.within(foot, |ui| {
                ui.row(34.0, |ui| {
                    if e.editable {
                        if ui.button("Edit") {
                            edit = true;
                        }
                        if confirm {
                            if ui.button_id("sure", "Really delete") {
                                del = true;
                            }
                        } else if ui.button("Delete") {
                            ask = true;
                        }
                    }
                    if ui.button("Close") {
                        close = true;
                    }
                });
            });
        });
        if ask {
            self.confirm_delete = true;
        }
        if edit {
            self.edit_event(&id, start);
        }
        if del {
            self.delete_event(&id, start);
        }
        if close || ui.key(keys::KEY_ESC) {
            self.detail = None;
        }
    }

    fn draw_editor(&mut self, ui: &mut Ui) {
        let t = ui.theme();
        let tz = self.store.tz;
        let (mut save, mut cancel) = (false, false);
        let Some(ed) = &mut self.editor else {
            return;
        };
        let new = ed.editing.is_none();
        ui.modal(560.0, 520.0, |ui| {
            let foot = ui.cut_bottom(t.px(44.0));
            ui.heading(if new { "New event" } else { "Edit event" });
            if new {
                ui.dim("Goes to Lucina's calendar. Your iPhone's calendar stays as it is.");
            }
            ui.space(4.0);
            if ui.text_field("ed-title", &mut ed.title).submitted {
                save = true;
            }
            ui.space(4.0);
            ui.checkbox("All day", &mut ed.all_day);
            ui.space(4.0);
            let row = |ui: &mut Ui,
                       label: &str,
                       ids: (&'static str, &'static str),
                       date: &mut TextEdit,
                       time: &mut TextEdit,
                       all_day: bool|
             -> bool {
                let r = ui.cut_top(t.px(36.0));
                ui.text_in(
                    Rect::new(r.x, r.y, t.px(50.0), r.h),
                    Face::Sans,
                    t.pt(14.0),
                    t.item,
                    label,
                );
                let h = t.px(32.0);
                let y = r.y + (r.h - h) / 2;
                let dx = r.x + t.px(52.0);
                let dw = t.px(150.0);
                let mut sub = ui
                    .text_field_at(Rect::new(dx, y, dw, h), ids.0, date)
                    .submitted;
                if !all_day {
                    sub |= ui
                        .text_field_at(
                            Rect::new(dx + dw + t.px(8.0), y, t.px(90.0), h),
                            ids.1,
                            time,
                        )
                        .submitted;
                }
                sub
            };
            let all = ed.all_day;
            save |= row(
                ui,
                "Start",
                ("ed-sd", "ed-st"),
                &mut ed.start_date,
                &mut ed.start_time,
                all,
            );
            // Moving the start moves the end, keeping the length.
            let now = ed.form();
            if now.start_date != ed.last.start_date || now.start_time != ed.last.start_time {
                let moved = shift_end(&ed.last, &now, tz);
                if moved.end_date != now.end_date {
                    ed.end_date.set(&moved.end_date);
                }
                if moved.end_time != now.end_time {
                    ed.end_time.set(&moved.end_time);
                }
            }
            save |= row(
                ui,
                "End",
                ("ed-ed", "ed-et"),
                &mut ed.end_date,
                &mut ed.end_time,
                all,
            );
            ed.last = ed.form();
            ui.space(4.0);
            if ui.text_field("ed-loc", &mut ed.location).submitted {
                save = true;
            }
            ui.space(4.0);
            if ui.text_field("ed-video", &mut ed.video).submitted {
                save = true;
            }
            ui.space(4.0);
            ui.text_area("ed-notes", &mut ed.notes, 2);
            if let Some(e) = &ed.error {
                let r = ui.area();
                ui.text_in(
                    Rect::new(r.x, r.y, r.w, t.px(22.0)),
                    Face::Sans,
                    t.pt(13.0),
                    t.danger,
                    e,
                );
            }
            ui.within(foot, |ui| {
                ui.row(34.0, |ui| {
                    if ui.primary_button("Save") {
                        save = true;
                    }
                    if ui.button("Cancel") {
                        cancel = true;
                    }
                });
            });
        });
        if cancel || ui.key(keys::KEY_ESC) {
            self.editor = None;
        } else if save {
            self.save_editor();
        }
    }

    fn draw_toast(&mut self, ui: &mut Ui, r: Rect) {
        let Some((text, until, bad)) = self.toast.clone() else {
            return;
        };
        let now = Instant::now();
        if now >= until {
            self.toast = None;
            ui.repaint();
            return;
        }
        ui.wake_after(until - now);
        let t = ui.theme();
        let tw = ui
            .measure(Face::Sans, t.pt(14.0), &text)
            .min(r.w - t.px(40.0));
        let h = t.px(36.0);
        let w = tw + t.px(28.0);
        let b = Rect::new(r.x + (r.w - w) / 2, r.bottom() - h - t.px(12.0), w, h);
        ui.fill_round(b, t.px(8.0), if bad { t.danger } else { t.selection });
        ui.text_in(
            Rect::new(b.x + t.px(14.0), b.y, tw, h),
            Face::Sans,
            t.pt(14.0),
            t.text,
            &text,
        );
    }

    fn keys(&mut self, ui: &mut Ui) {
        let Some(p) = ui.press() else {
            return;
        };
        if p.mods.ctrl || p.mods.alt || p.mods.meta {
            return;
        }
        let tall = self.kind != Kind::Month;
        let hour = ui.theme().px(HOUR);
        let days = |n: i64, s: &mut Self| {
            let d = s.cursor + chrono::Duration::days(n);
            s.go(d);
        };
        let used = match p.code {
            keys::KEY_LEFT => {
                days(-1, self);
                true
            }
            keys::KEY_RIGHT => {
                days(1, self);
                true
            }
            keys::KEY_UP if tall => {
                self.grid.scroll = (self.grid.scroll - hour).max(0);
                true
            }
            keys::KEY_DOWN if tall => {
                self.grid.scroll += hour;
                true
            }
            keys::KEY_UP => {
                days(-7, self);
                true
            }
            keys::KEY_DOWN => {
                days(7, self);
                true
            }
            keys::KEY_PAGEUP => {
                self.page(-1);
                true
            }
            keys::KEY_PAGEDOWN => {
                self.page(1);
                true
            }
            keys::KEY_ENTER | keys::KEY_KPENTER => match self.marked {
                Some(i) => {
                    self.open_event(i);
                    true
                }
                None => false,
            },
            KEY_F5 => {
                self.load();
                true
            }
            keys::KEY_ESC => {
                self.marked = None;
                true
            }
            _ => match keys::char_for(p.code, p.mods) {
                Some('t') => {
                    let d = self.store.today();
                    self.go(d);
                    true
                }
                Some('d') => {
                    self.set_kind(Kind::Day);
                    true
                }
                Some('w') => {
                    self.set_kind(Kind::Week);
                    true
                }
                Some('m') => {
                    self.set_kind(Kind::Month);
                    true
                }
                Some('n') => {
                    self.new_event_default();
                    true
                }
                // The next event on the cursor's day; Enter opens it.
                Some('e') => {
                    let list = self.store.on_day(self.cursor);
                    self.marked = match self.marked.and_then(|m| list.iter().position(|&x| x == m))
                    {
                        Some(i) => list.get(i + 1).copied().or(list.first().copied()),
                        None => list.first().copied(),
                    };
                    true
                }
                _ => false,
            },
        };
        if used {
            ui.take_press();
        }
    }
}

impl App for Calendar {
    type Msg = Msg;

    fn message(&mut self, m: Msg) {
        match m {
            Msg::Month(month, events) => {
                self.store.months.insert(month, events);
                self.store.rebuild();
                self.marked = None;
            }
            Msg::Created(op, e) | Msg::Updated(op, e) => {
                let old = self.take_change(op);
                if let Some(Change::Update { id, start_ms, .. }) = old {
                    self.store.remove(&id, start_ms);
                }
                if let Some(e) = e {
                    self.store.confirmed(*e);
                }
                self.store.rebuild();
                self.load();
            }
            Msg::Deleted(op) => {
                if let Some(Change::Delete { id, start_ms, .. }) = self.take_change(op) {
                    self.store.remove(&id, start_ms);
                }
                self.store.rebuild();
                self.load();
            }
            Msg::WriteFailed(op, reason) => {
                let change = self.take_change(op);
                self.store.rebuild();
                let text = format!("Not saved: {reason}");
                match change {
                    Some(Change::Create { form, .. }) => {
                        let mut ed = Editor::new(&form, None);
                        ed.error = Some(text);
                        self.editor = Some(ed);
                    }
                    Some(Change::Update {
                        id, start_ms, form, ..
                    }) => {
                        let mut ed = Editor::new(&form, Some((id, start_ms)));
                        ed.error = Some(text);
                        self.editor = Some(ed);
                    }
                    _ => {
                        self.toast = Some((
                            format!("Not deleted: {reason}"),
                            Instant::now() + TOAST,
                            true,
                        ));
                    }
                }
            }
            Msg::Offline(m) => self.offline = Some(m),
            Msg::Online => self.offline = None,
            Msg::SignedOut(m) => {
                eprintln!("lucina-app-calendar: signed out: {m}");
                self.show_sign_in();
            }
            Msg::SignedIn => self.signed_in(),
            Msg::Changed => {}
        }
    }

    fn on_quit(&mut self) {
        if self.mode == Mode::Account
            && let Ok(bytes) = serde_json::to_vec(&self.store.months)
        {
            let _ = credential::write_private(CACHE_FILE, &bytes);
        }
    }

    fn view(&mut self, ui: &mut Ui) {
        let t = ui.theme();
        ui.header("Calendar");
        // The laptop has no battery clock: it can start in 1970 until the network sets the time.
        let today = self.store.today();
        if self.cursor.year() < 2020 && today.year() >= 2020 {
            self.go(today);
            self.grid_set = false;
        }
        if let Some(f) = self.want_focus.take() {
            ui.focus(f);
        }
        if self.mode == Mode::NotSignedIn {
            self.draw_not_signed_in(ui);
            return;
        }
        // No text field on this screen: the keys are the calendar's before any widget's. A focused
        // list or button keeps what isn't a shortcut here (Tab, Space, Enter on a button).
        if self.detail.is_none() && self.editor.is_none() {
            self.keys(ui);
        }
        let side = ui.cut_left(t.px(210.0));
        self.draw_side(ui, side);
        let screen = ui.area();
        ui.pad(10.0);
        self.draw_toolbar(ui);
        match self.kind {
            Kind::Day => {
                let d = [self.cursor];
                self.draw_grid(ui, &d);
            }
            Kind::Week => {
                let s = week_start(self.cursor);
                let d: Vec<NaiveDate> = (0..7).map(|i| s + chrono::Duration::days(i)).collect();
                self.draw_grid(ui, &d);
            }
            Kind::Month => self.draw_month(ui),
        }
        self.draw_toast(ui, screen);
        // The clock moves the line in the grid.
        if self.kind != Kind::Month {
            ui.wake_after(Duration::from_secs(60));
        }
        if self.detail.is_some() {
            self.draw_detail(ui);
        } else if self.editor.is_some() {
            self.draw_editor(ui);
        }
    }
}
