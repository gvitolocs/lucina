//! The screen: `web/src/views/Library.tsx`. The books on the left, the one selected on the right. Its details are fetched once
//! the selection has settled and kept: arrows held down must not spend the account's request limit.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use chrono::DateTime;
use chrono_tz::Tz;
use lucina_account::timeline::parse_rfc3339_ms;
use lucina_account::ui::SignInView;
use lucina_app::{App, Face, ListState, Rect, Rgb, TextEdit, Theme, Ui, keys};
use serde_json::json;

use crate::server::{Activity, BookRow, Detail, Filters, Hit, Job, Msg, Progress};

pub const STATUSES: [&str; 5] = ["reading", "completed", "abandoned", "not_started", "unknown"];
const FILTERS: [Option<&str>; 5] = [None, Some("reading"), Some("completed"), Some("not_started"), Some("unknown")];
pub const SEARCH_WAIT: Duration = Duration::from_millis(250);
pub const SETTLE: Duration = Duration::from_millis(350);
const MAX_TAGS: usize = 16;
const MAX_TAG_CHARS: usize = 32;

pub type Link = Box<dyn FnMut(Job)>;

pub fn status_label(s: &str) -> &'static str {
    match s {
        "reading" => "Reading",
        "completed" => "Completed",
        "abandoned" => "Abandoned",
        "not_started" => "Not started",
        _ => "Unknown",
    }
}

/// `percent`: at most one decimal.
pub fn percent(p: Option<&Progress>) -> String {
    match p.and_then(|p| p.percent) {
        None => "—".into(),
        Some(n) => {
            let s = format!("{n:.1}");
            format!("{}%", s.strip_suffix(".0").unwrap_or(&s))
        }
    }
}

/// `parseTags`: split by commas, trimmed, no empties, no repeats.
pub fn parse_tags(s: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in s.split(',') {
        let t = t.trim();
        if !t.is_empty() && !out.iter().any(|x| x == t) {
            out.push(t.to_owned());
        }
    }
    out
}

pub fn tags_valid(tags: &[String]) -> bool {
    tags.len() <= MAX_TAGS && tags.iter().all(|t| t.chars().count() <= MAX_TAG_CHARS)
}

fn when(s: &str, tz: Tz) -> String {
    parse_rfc3339_ms(s)
        .and_then(|ms| DateTime::from_timestamp_millis(ms as i64))
        .map_or_else(|| s.to_owned(), |d| d.with_timezone(&tz).format("%-d %b %Y %H:%M").to_string())
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Style {
    Heading,
    Text,
    Dim,
}

/// The scrolling part of the right side: every row a piece of wrapped text.
pub fn detail_rows(d: &Detail, tz: Tz) -> Vec<(Style, String)> {
    let mut r = Vec::new();
    r.push((Style::Heading, "Highlights".to_owned()));
    if d.highlights.is_empty() {
        r.push((Style::Dim, "No highlights yet".to_owned()));
    }
    for h in &d.highlights {
        r.push((Style::Text, h.text.clone()));
        if let Some(n) = &h.note {
            r.push((Style::Text, n.clone()));
        }
        let mut meta = Vec::new();
        if let Some((a, b)) = h.location {
            meta.push(if a == b { format!("Location {a}") } else { format!("Location {a}-{b}") });
        }
        if let Some(p) = &h.page {
            meta.push(format!("Page {p}"));
        }
        // Kindle's date as supplied: converting it would invent a timezone.
        meta.push(h.added.clone().unwrap_or_else(|| "Unknown date".to_owned()));
        meta.push(format!("Source: {}", h.source));
        r.push((Style::Dim, meta.join(" · ")));
    }
    r.push((Style::Heading, "Brief".to_owned()));
    if d.briefs.is_empty() {
        r.push((Style::Dim, "No brief yet".to_owned()));
    }
    for (text, at) in &d.briefs {
        r.push((Style::Dim, "Generated from your highlights".to_owned()));
        r.push((Style::Text, text.clone()));
        r.push((Style::Dim, when(at, tz)));
    }
    r.push((Style::Heading, "Metadata".to_owned()));
    let formats = if d.formats.is_empty() { "—".to_owned() } else { d.formats.join(", ") };
    r.push((Style::Text, format!("Formats: {formats}")));
    for (label, v) in &d.ids {
        r.push((Style::Text, format!("{label}: {v}")));
    }
    r.push((Style::Heading, "Presence".to_owned()));
    let mut presence = Vec::new();
    if d.owned {
        presence.push("Owned".to_owned());
    }
    if d.row.on_kindle {
        presence.push("On Kindle".to_owned());
    }
    if let Some(at) = &d.removed_at {
        presence.push(format!("Removed from Kindle: {}", when(at, tz)));
    }
    if !d.owned && !d.row.on_kindle && !d.highlights.is_empty() {
        presence.push("Clippings only".to_owned());
    }
    if presence.is_empty() {
        presence.push("Unknown".to_owned());
    }
    r.push((Style::Text, presence.join(" · ")));
    r.push((Style::Heading, "Related notes".to_owned()));
    if d.related.is_empty() {
        r.push((Style::Dim, "Obsidian is not connected yet".to_owned()));
    }
    for (title, place) in &d.related {
        r.push((Style::Text, title.clone()));
        r.push((Style::Dim, place.clone()));
    }
    r
}

fn progress_lines(d: &Detail) -> String {
    let Some(p) = &d.row.progress else {
        return "Progress unknown: the stock Kindle does not expose it over USB".to_owned();
    };
    if p.percent.is_none() {
        return "Progress unknown: the stock Kindle does not expose it over USB".to_owned();
    }
    let mut s = format!("Progress: {}", percent(Some(p)));
    if let Some(l) = p.location {
        s += &format!(" · Location {l}");
    }
    if let Some(n) = p.page {
        s += &format!(" · Page {n}");
    }
    s
}

pub struct Library {
    pub tz: Tz,
    link: Option<Link>,
    pub sign_in: Option<SignInView>,
    start_sign_in: Option<Box<dyn FnMut() -> Option<SignInView>>>,
    start_worker: Option<Box<dyn FnMut() -> Option<Link>>>,
    pub sample: bool,

    pub query: TextEdit,
    pub status: Option<&'static str>,
    pub on_kindle: bool,
    pub books: Vec<BookRow>,
    pub hits: Vec<Hit>,
    pub capped: bool,
    pub list: ListState,
    seq: u64,
    pub loaded: bool,
    pub loading: bool,
    pub error: Option<String>,
    /// The search typed, waiting to be sent.
    pub search_at: Option<Instant>,

    pub details: HashMap<String, Detail>,
    /// The book selected, since when, and whether it has been asked for.
    pub want: Option<(String, Instant, bool)>,
    pub detail_error: Option<String>,
    pub message: Option<String>,
    pub busy: bool,
    pub tags: TextEdit,
    tags_of: Option<String>,
    pub detail_list: ListState,
    rows: Vec<(Style, String)>,
    rows_of: Option<String>,
    pub activity: Option<Activity>,
    pub showing_activity: bool,
    pub activity_error: Option<String>,
    focus_first: bool,
}

enum Todo {
    Status(&'static str),
    SaveTags,
    Brief,
    Filter(Option<&'static str>),
    Kindle,
    ShowActivity,
    Retry,
    RetryDetail,
}

impl Library {
    pub fn new(tz: Tz, link: Option<Link>) -> Library {
        Library {
            tz,
            link,
            sign_in: None,
            start_sign_in: None,
            start_worker: None,
            sample: false,
            query: TextEdit::new().placeholder("Search library"),
            status: None,
            on_kindle: false,
            books: Vec::new(),
            hits: Vec::new(),
            capped: false,
            list: ListState::new(),
            seq: 0,
            loaded: false,
            loading: false,
            error: None,
            search_at: None,
            details: HashMap::new(),
            want: None,
            detail_error: None,
            message: None,
            busy: false,
            tags: TextEdit::new(),
            tags_of: None,
            detail_list: ListState::new(),
            rows: Vec::new(),
            rows_of: None,
            activity: None,
            showing_activity: false,
            activity_error: None,
            focus_first: true,
        }
    }

    pub fn sample(mut self) -> Library {
        self.sample = true;
        self
    }

    pub fn with_sign_in(
        mut self,
        start_sign_in: Box<dyn FnMut() -> Option<SignInView>>,
        start_worker: Box<dyn FnMut() -> Option<Link>>,
    ) -> Library {
        self.start_sign_in = Some(start_sign_in);
        self.start_worker = Some(start_worker);
        if self.link.is_none() {
            self.sign_in = self.start_sign_in.as_mut().and_then(|f| f());
        }
        self
    }

    fn send(&mut self, job: Job) {
        if let Some(l) = &mut self.link {
            l(job);
        }
    }

    /// Asks for the list as the filters now say. A later answer replaces an earlier one, never the other way round.
    pub fn reload(&mut self) {
        self.seq += 1;
        self.loading = true;
        self.error = None;
        self.search_at = None;
        let f = Filters { q: self.query.text.trim().to_owned(), status: self.status, on_kindle: self.on_kindle };
        let seq = self.seq;
        self.send(Job::List(seq, f));
    }

    pub fn set_status_filter(&mut self, s: Option<&'static str>) {
        self.status = s;
        self.reload();
    }

    pub fn toggle_kindle(&mut self) {
        self.on_kindle = !self.on_kindle;
        self.reload();
    }

    pub fn selected(&self) -> Option<&BookRow> {
        self.list.selected.and_then(|i| self.books.get(i))
    }

    pub fn open(&self) -> Option<&Detail> {
        self.selected().and_then(|b| self.details.get(&b.id))
    }

    /// The selection decides what is asked for, once it has rested.
    pub fn follow_selection(&mut self) {
        let Some(id) = self.selected().map(|b| b.id.clone()) else {
            self.want = None;
            return;
        };
        if self.want.as_ref().is_none_or(|(w, _, _)| *w != id) {
            self.want = Some((id, Instant::now(), false));
            self.detail_error = None;
            self.detail_list = ListState::new();
        }
    }

    /// `at`: now, which a test can move on.
    pub fn ask_if_settled(&mut self, at: Instant) {
        let Some((id, since, asked)) = self.want.clone() else { return };
        if asked || self.details.contains_key(&id) || at.duration_since(since) < SETTLE {
            return;
        }
        if let Some(w) = &mut self.want {
            w.2 = true;
        }
        self.send(Job::Detail(id));
    }

    pub fn retry_detail(&mut self) {
        self.want = None;
        self.detail_error = None;
    }

    pub fn save_status(&mut self, status: &str) {
        let Some(d) = self.open() else { return };
        if self.busy || d.row.status == status {
            return;
        }
        let (id, base) = (d.row.id.clone(), d.updated_at.clone());
        self.busy = true;
        self.message = None;
        self.send(Job::Patch { id, patch: json!({"status": status}), base });
    }

    pub fn save_tags(&mut self) {
        let parsed = parse_tags(&self.tags.text);
        if !tags_valid(&parsed) {
            self.message = Some("Use at most 16 tags, with at most 32 characters each.".into());
            return;
        }
        let Some(d) = self.open() else { return };
        if self.busy {
            return;
        }
        let (id, base) = (d.row.id.clone(), d.updated_at.clone());
        self.busy = true;
        self.message = None;
        self.send(Job::Patch { id, patch: json!({"tags": parsed}), base });
    }

    pub fn make_brief(&mut self) {
        let Some(d) = self.open() else { return };
        if self.busy || d.highlights.is_empty() {
            return;
        }
        let id = d.row.id.clone();
        self.busy = true;
        self.message = None;
        self.send(Job::Brief(id));
    }

    pub fn show_activity(&mut self) {
        self.showing_activity = !self.showing_activity;
        if self.showing_activity {
            self.activity_error = None;
            self.send(Job::Activity);
        }
    }

    fn keep(&mut self, id: &str, detail: Detail) {
        if let Some(b) = self.books.iter_mut().find(|b| b.id == id) {
            b.status.clone_from(&detail.row.status);
            b.highlight_count = detail.row.highlight_count;
        }
        self.details.insert(id.to_owned(), detail);
        self.rows_of = None;
        self.tags_of = None;
    }

    /// What the right side draws, rebuilt when the book changes. Tags follow it, unless something is being typed for it.
    fn sync_open(&mut self) {
        let tz = self.tz;
        let Some(d) = self.open().cloned() else {
            self.rows.clear();
            self.rows_of = None;
            return;
        };
        if self.rows_of.as_deref() != Some(d.row.id.as_str()) {
            self.rows = detail_rows(&d, tz);
            self.detail_list.invalidate();
            self.rows_of = Some(d.row.id.clone());
        }
        if self.tags_of.as_deref() != Some(d.row.id.as_str()) {
            self.tags.set(&d.tags.join(", "));
            self.tags_of = Some(d.row.id.clone());
        }
    }

    fn left(&mut self, ui: &mut Ui, r: Rect, todo: &mut Option<Todo>) {
        let t = ui.theme();
        ui.fill(r, t.bar);
        let inner = r.inset(t.px(12.0));
        let foot = Rect::new(inner.x, inner.bottom() - t.px(40.0), inner.w, t.px(40.0));
        let mut top = Rect::new(inner.x, inner.y, inner.w, t.px(180.0));
        ui.within(top, |ui| {
            if self.sample {
                ui.dim("Sample data");
            }
            // The list has to exist first: the keyboard starts on it, once there are books.
            if !self.books.is_empty() && std::mem::take(&mut self.focus_first) {
                ui.focus("books");
            }
            if ui.text_field("search", &mut self.query).changed {
                self.search_at = Some(Instant::now());
            }
            ui.space(4.0);
            ui.row(32.0, |ui| {
                for (i, f) in FILTERS.iter().enumerate().take(3) {
                    filter_button(ui, i, *f, self.status, todo);
                }
            });
            ui.space(2.0);
            ui.row(32.0, |ui| {
                for (i, f) in FILTERS.iter().enumerate().skip(3) {
                    filter_button(ui, i, *f, self.status, todo);
                }
                let label = if self.on_kindle { "• On Kindle" } else { "On Kindle" };
                if ui.button_id("kindle", label) {
                    *todo = Some(Todo::Kindle);
                }
            });
        });
        top.h = t.px(if self.sample { 148.0 } else { 124.0 });
        let list_r = Rect::new(inner.x, top.bottom(), inner.w, foot.y - top.bottom() - t.px(6.0));
        let mut below = list_r;
        if self.capped {
            below.h -= t.px(24.0);
            let note = Rect::new(inner.x, below.bottom(), inner.w, t.px(22.0));
            ui.text_in(note, Face::Sans, t.pt(12.0), t.dim, "Showing the first 500. Refine your search to see more.");
        }
        self.book_list(ui, below, &t, todo);
        ui.within(foot, |ui| {
            let label = if self.showing_activity { "Back to the book" } else { "Activity / stats" };
            if ui.button_id("activity", label) {
                *todo = Some(Todo::ShowActivity);
            }
        });
    }

    fn book_list(&mut self, ui: &mut Ui, r: Rect, t: &Theme, todo: &mut Option<Todo>) {
        if self.books.is_empty() {
            ui.within(r, |ui| {
                if let Some(e) = &self.error {
                    ui.wrapped(Face::Sans, t.pt(14.0), t.danger, e);
                    ui.space(6.0);
                    if ui.button("Try again") {
                        *todo = Some(Todo::Retry);
                    }
                } else if self.loading || !self.loaded {
                    ui.dim("Loading…");
                } else if !self.query.text.trim().is_empty() || self.status.is_some() || self.on_kindle {
                    ui.dim("No books match these filters.");
                } else {
                    ui.label("Your library is empty");
                    ui.wrapped(Face::Sans, t.pt(13.0), t.dim, "Connect your Kindle by USB and import it from your computer.");
                }
            });
            return;
        }
        let (books, hits) = (&self.books, &self.hits);
        ui.list("books", &mut self.list, r, books.len(), 74.0, |ui, i, selected| {
            let row = ui.area();
            if selected {
                ui.fill_round(row.inset(t.px(1.0)), t.px(6.0), t.panel_hi);
            }
            let b = &books[i];
            let x = row.x + t.px(10.0);
            let w = row.w - t.px(20.0);
            ui.text_in(Rect::new(x, row.y + t.px(4.0), w, t.px(24.0)), Face::Bold, t.pt(14.0), t.text, &b.title);
            let second = hits
                .iter()
                .find(|h| h.highlight && h.book_id == b.id)
                .map_or_else(|| b.authors.join(", "), |h| h.snippet.clone());
            ui.text_in(Rect::new(x, row.y + t.px(27.0), w, t.px(20.0)), Face::Sans, t.pt(13.0), t.item, &second);
            let mut meta = vec![status_label(&b.status).to_owned(), percent(b.progress.as_ref())];
            meta.push(if b.highlight_count == 1 { "1 highlight".into() } else { format!("{} highlights", b.highlight_count) });
            if b.on_kindle {
                meta.push("On Kindle".into());
            }
            if b.protected {
                meta.push("Protected".into());
            }
            ui.text_in(Rect::new(x, row.y + t.px(48.0), w, t.px(20.0)), Face::Sans, t.pt(12.0), t.dim, &meta.join(" · "));
        });
    }

    fn right(&mut self, ui: &mut Ui, r: Rect, todo: &mut Option<Todo>) {
        let t = ui.theme();
        let inner = r.inset(t.px(18.0));
        if self.showing_activity {
            ui.within(inner, |ui| self.activity_view(ui));
            return;
        }
        let Some(d) = self.open().cloned() else {
            ui.within(inner, |ui| {
                if let Some(e) = &self.detail_error {
                    ui.wrapped(Face::Sans, t.pt(14.0), t.danger, e);
                    ui.space(6.0);
                    if ui.button("Try again") {
                        *todo = Some(Todo::RetryDetail);
                    }
                } else if self.selected().is_some() {
                    ui.dim("Loading…");
                }
            });
            return;
        };
        let head = Rect::new(inner.x, inner.y, inner.w, t.px(214.0));
        let tags_save = ui.within(head, |ui| {
            ui.heading(&d.row.title);
            ui.dim(&d.row.authors.join(", "));
            if d.row.protected {
                ui.dim("Protected");
            }
            ui.space(4.0);
            ui.dim("Reading status");
            ui.row(36.0, |ui| {
                for (i, s) in STATUSES.iter().enumerate() {
                    let label = status_label(s);
                    let hit = if d.row.status == *s { ui.primary_button(label) } else { ui.button_id(("status", i), label) };
                    if hit {
                        *todo = Some(Todo::Status(s));
                    }
                }
            });
            ui.space(2.0);
            ui.dim("Tags, separated by commas");
            ui.text_field("tags", &mut self.tags).submitted
        });
        if tags_save {
            *todo = Some(Todo::SaveTags);
        }
        let actions = Rect::new(inner.x, head.bottom(), inner.w, t.px(44.0));
        ui.within(actions, |ui| {
            ui.row(38.0, |ui| {
                if ui.button_id("save-tags", if self.busy { "Saving…" } else { "Save tags" }) {
                    *todo = Some(Todo::SaveTags);
                }
                if !d.highlights.is_empty() && ui.primary_button("Make brief") {
                    *todo = Some(Todo::Brief);
                }
            });
        });
        let line = Rect::new(inner.x, actions.bottom(), inner.w, t.px(24.0));
        ui.text_in(line, Face::Sans, t.pt(13.0), t.dim, &progress_lines(&d));
        let mut top = line.bottom() + t.px(2.0);
        if let Some(m) = &self.message {
            let note = Rect::new(inner.x, top, inner.w, t.px(26.0));
            ui.text_in(note, Face::Sans, t.pt(13.0), t.danger, m);
            top += t.px(28.0);
        }
        let list_r = Rect::new(inner.x, top, inner.w, inner.bottom() - top);
        let rows = &self.rows;
        let (sans, bold, small) = (t.pt(14.0), t.pt(15.0), t.pt(12.0));
        let measure = |text: &mut lucina_app::gfx::Text, i: usize, w: i32| {
            let (style, s) = &rows[i];
            let (f, px, pad) = match style {
                Style::Heading => (Face::Bold, bold, t.px(16.0)),
                Style::Text => (Face::Sans, sans, t.px(8.0)),
                Style::Dim => (Face::Sans, small, t.px(10.0)),
            };
            Ui::wrapped_height(text, f, px, s, w - t.px(10.0)) + pad
        };
        ui.list_measured("detail", &mut self.detail_list, list_r, rows.len(), measure, |ui, i, _| {
            let (style, s) = &rows[i];
            let (f, px, c, top_pad): (Face, u16, Rgb, f32) = match style {
                Style::Heading => (Face::Bold, bold, t.text, 12.0),
                Style::Text => (Face::Sans, sans, t.item, 4.0),
                Style::Dim => (Face::Sans, small, t.dim, 2.0),
            };
            ui.space(top_pad);
            ui.wrapped(f, px, c, s);
        });
    }

    fn activity_view(&self, ui: &mut Ui) {
        let t = ui.theme();
        ui.heading("Activity / stats");
        ui.space(6.0);
        if let Some(e) = &self.activity_error {
            ui.wrapped(Face::Sans, t.pt(14.0), t.danger, e);
            return;
        }
        let Some(a) = &self.activity else {
            ui.dim("Loading…");
            return;
        };
        let last = a.last_import.as_ref().map_or_else(|| "No imports yet".to_owned(), |s| when(s, self.tz));
        ui.label(&format!("Last import: {last}"));
        ui.space(6.0);
        for (s, n) in &a.by_status {
            ui.label(&format!("{}: {n}", status_label(s)));
        }
        ui.space(6.0);
        ui.label(&format!("Highlights in the last 7 days: {}", a.week));
        ui.label(&format!("Highlights in the last 30 days: {}", a.month));
        if a.capped {
            ui.dim("Showing up to 500 highlights per window.");
        }
        ui.space(6.0);
        ui.wrapped(Face::Sans, t.pt(13.0), t.dim, "Kindle dates have no timezone; highlights with unknown dates are excluded.");
        if a.no_sessions {
            ui.wrapped(Face::Sans, t.pt(13.0), t.dim, "Reading sessions are unavailable on a stock Kindle.");
        }
    }
}

fn filter_button(ui: &mut Ui, i: usize, f: Option<&'static str>, on: Option<&'static str>, todo: &mut Option<Todo>) {
    let label = f.map_or("All", status_label);
    let hit = if on == f { ui.primary_button(label) } else { ui.button_id(("filter", i), label) };
    if hit {
        *todo = Some(Todo::Filter(f));
    }
}

impl App for Library {
    type Msg = Msg;

    fn view(&mut self, ui: &mut Ui) {
        ui.header("Library");
        if let Some(v) = &mut self.sign_in {
            v.view(ui, "Your library waits in your account. This laptop asks to be added as a device.");
            return;
        }
        if !self.loaded && !self.loading {
            self.reload();
        }
        let t = ui.theme();
        let mut todo = None;
        let left = ui.cut_left(t.px(460.0));
        self.left(ui, left, &mut todo);
        let right = ui.area();
        self.follow_selection();
        self.sync_open();
        self.right(ui, right, &mut todo);
        if ui.key(keys::KEY_ESC) {
            ui.focus("books");
        }
        match todo {
            Some(Todo::Status(s)) => self.save_status(s),
            Some(Todo::SaveTags) => self.save_tags(),
            Some(Todo::Brief) => self.make_brief(),
            Some(Todo::Filter(f)) => self.set_status_filter(f),
            Some(Todo::Kindle) => self.toggle_kindle(),
            Some(Todo::ShowActivity) => self.show_activity(),
            Some(Todo::Retry) => self.reload(),
            Some(Todo::RetryDetail) => self.retry_detail(),
            None => {}
        }
        // Two short waits while something is pending; otherwise nothing wakes this.
        let now = Instant::now();
        if self.search_at.is_some_and(|s| now.duration_since(s) >= SEARCH_WAIT) {
            self.reload();
        }
        self.ask_if_settled(now);
        if self.search_at.is_some() {
            ui.wake_after(SEARCH_WAIT);
        }
        if let Some((id, since, false)) = &self.want
            && !self.details.contains_key(id)
        {
            ui.wake_after(SETTLE.saturating_sub(now.duration_since(*since)).max(Duration::from_millis(10)));
        }
    }

    fn message(&mut self, msg: Msg) {
        match msg {
            Msg::Changed => {}
            Msg::SignedIn => {
                self.sign_in = None;
                self.link = self.start_worker.as_mut().and_then(|f| f());
                self.focus_first = true;
                self.reload();
            }
            Msg::SignedOut => {
                crate::account::rejected();
                self.link = None;
                self.books.clear();
                self.details.clear();
                self.loaded = false;
                self.loading = false;
                self.busy = false;
                self.sign_in = self.start_sign_in.as_mut().and_then(|f| f());
            }
            Msg::Listed { seq, books, hits, capped } => {
                if seq != self.seq {
                    return;
                }
                let was = self.selected().map(|b| b.id.clone());
                self.books = books;
                self.hits = hits;
                self.capped = capped;
                self.loaded = true;
                self.loading = false;
                self.error = None;
                self.list.invalidate();
                let at = was.and_then(|id| self.books.iter().position(|b| b.id == id));
                if !self.books.is_empty() {
                    self.list.select(at.unwrap_or(0));
                }
            }
            Msg::ListFailed { seq, message } => {
                if seq == self.seq {
                    self.loading = false;
                    self.loaded = true;
                    self.error = Some(message);
                }
            }
            Msg::Detail(id, d) => self.keep(&id, *d),
            Msg::DetailFailed(id, m) => {
                // The book stays marked as asked: no retrying by itself, which would spend the account's request limit.
                // Selecting it again, or Try again, asks again.
                if self.want.as_ref().is_some_and(|w| w.0 == id) {
                    self.detail_error = Some(m);
                }
            }
            Msg::Saved { id, detail, conflict } => {
                self.busy = false;
                self.keep(&id, *detail);
                self.message = conflict.then(|| {
                    "This book changed while you were editing. The latest version has been reloaded.".to_owned()
                });
            }
            Msg::SaveFailed(m) => {
                self.busy = false;
                self.message = Some(m);
            }
            Msg::Activity(a) => self.activity = Some(*a),
            Msg::ActivityFailed(m) => self.activity_error = Some(m),
        }
    }
}
