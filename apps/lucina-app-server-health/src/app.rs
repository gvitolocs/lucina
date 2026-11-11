//! The screen: `web/src/views/ServerHealth.tsx`. Sections on the left, one list on the right, and Esc back out of whatever Enter
//! opened. Refreshes every minute while it is open. Commands only reach the clipboard: nothing here runs anything.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use lucina_account::ui::SignInView;
use lucina_app::time::Zone;
use lucina_app::{App, Face, ListState, Rect, Rgb, TextEdit, Theme, Ui, keys};
use lucina_app::gfx::Text;

use crate::model::{Kind, Overview, State, ago};
use crate::pages::{self, Act, Ctx, HOT_C, Line, Metric, Opened, Spark, Tone, disk_state, disk_text};
use crate::server::ServerError;

/// `REFRESH_MS`.
pub const REFRESH: Duration = Duration::from_secs(60);
const TICK: Duration = Duration::from_secs(30);

pub type Link = Box<dyn FnMut(bool)>;

pub enum Msg {
    /// The sign-in screen changed.
    Changed,
    SignedIn,
    Loaded(Box<Overview>),
    Failed(ServerError),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Overview,
    Services,
    Incidents,
    Deploys,
}

const SECTIONS: [(Section, &str); 4] = [(Section::Overview, "Overview"), (Section::Services, "Services"), (Section::Incidents, "Incidents"), (Section::Deploys, "Deploys")];

/// What Enter opened: `?item=`, `?host=` and `?component=` on the web.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Sheet {
    Item(String),
    Host(String),
    Component(String),
}

pub struct ServerApp {
    pub zone: Zone,
    link: Option<Link>,
    sign_in: Option<SignInView>,
    start_sign_in: Option<Box<dyn FnMut() -> SignInView>>,
    start_worker: Option<Box<dyn FnMut() -> Option<Link>>>,
    pub data: Option<Overview>,
    pub error: Option<String>,
    pub loading: bool,
    pub section: Section,
    pub sheets: Vec<Sheet>,
    pub opened: Opened,
    pub search: TextEdit,
    pub list: ListState,
    pub lines: Vec<Line>,
    stale: bool,
    pub note: Option<String>,
    last_load: Option<Instant>,
    built_at: Option<Instant>,
    focus_list: bool,
    focus_search: bool,
    save_cache: Option<Box<dyn FnMut(&Overview)>>,
    sample: bool,
    /// Tests fix the clock.
    pub fixed_now: Option<i64>,
}

fn clock_now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

impl ServerApp {
    pub fn new(zone: Zone, link: Option<Link>) -> ServerApp {
        ServerApp {
            zone,
            link,
            sign_in: None,
            start_sign_in: None,
            start_worker: None,
            data: None,
            error: None,
            loading: false,
            section: Section::Overview,
            sheets: Vec::new(),
            opened: Opened::default(),
            search: TextEdit::new().placeholder("Search services"),
            list: ListState::new(),
            lines: Vec::new(),
            stale: true,
            note: None,
            last_load: None,
            built_at: None,
            focus_list: true,
            focus_search: false,
            save_cache: None,
            sample: false,
            fixed_now: None,
        }
    }

    pub fn sample(mut self) -> ServerApp {
        self.sample = true;
        self
    }

    pub fn with_sign_in(mut self, start_sign_in: Box<dyn FnMut() -> SignInView>, start_worker: Box<dyn FnMut() -> Option<Link>>) -> ServerApp {
        self.start_sign_in = Some(start_sign_in);
        self.start_worker = Some(start_worker);
        if self.link.is_none() {
            self.sign_in = self.start_sign_in.as_mut().map(|f| f());
        }
        self
    }

    pub fn with_cache(mut self, save: Box<dyn FnMut(&Overview)>) -> ServerApp {
        self.save_cache = Some(save);
        self
    }

    /// What was fetched last time, shown before any network.
    pub fn restore(&mut self, o: Overview) {
        self.data = Some(o);
        self.stale = true;
    }

    fn now(&self) -> i64 {
        self.fixed_now.unwrap_or_else(clock_now)
    }

    /// `load`: never two at a time.
    pub fn refresh(&mut self, fresh: bool) {
        if self.loading {
            return;
        }
        if let Some(l) = &mut self.link {
            self.loading = true;
            self.last_load = Some(Instant::now());
            l(fresh);
        }
    }

    fn switch(&mut self, s: Section) {
        self.section = s;
        self.sheets.clear();
        self.note = None;
        self.reset_list();
    }

    fn reset_list(&mut self) {
        self.list = ListState::new();
        self.stale = true;
        self.focus_list = true;
    }

    fn rebuild(&mut self) {
        self.stale = false;
        self.built_at = Some(Instant::now());
        self.list.invalidate();
        let Some(o) = &self.data else {
            self.lines.clear();
            return;
        };
        let c = Ctx { o, zone: &self.zone, now: self.fixed_now.unwrap_or_else(clock_now) };
        self.lines = match self.sheets.last() {
            Some(Sheet::Item(k)) => o.attention.iter().find(|a| &a.key == k).map(|a| pages::item(&c, a)),
            Some(Sheet::Host(id)) => o.hosts.iter().find(|h| &h.id == id).map(|h| pages::host(&c, h)),
            Some(Sheet::Component(id)) => o.all_components().into_iter().find(|(x, _, _)| &x.id == id).map(|(x, g, p)| pages::component(x, g, p)),
            None => Some(match self.section {
                Section::Overview => pages::overview(&c, &self.opened),
                Section::Services => pages::services(&c, &self.opened),
                Section::Incidents => pages::timeline(&c, &self.opened, &[Kind::Incident, Kind::Change], "No incidents or changes recorded."),
                Section::Deploys => pages::timeline(&c, &self.opened, &[Kind::Deploy, Kind::Decision], "No deploys or decisions recorded."),
            }),
        }
        .unwrap_or_else(|| vec![Line::Para(Tone::Dim, "This item is no longer in the overview.".into())]);
        if let Some(i) = self.list.selected {
            if i >= self.lines.len() {
                self.list.selected = self.lines.len().checked_sub(1);
            }
        }
        // Enter must do something from the first key.
        if self.list.selected.is_none() && !self.lines.is_empty() {
            // A host's page opens on its charts, as on the web, not on a row further down.
            let host = matches!(self.sheets.last(), Some(Sheet::Host(_)));
            let first = if host { 0 } else { self.lines.iter().position(|l| l.act().is_some()).unwrap_or(0) };
            self.list.select(first);
        }
    }

    pub fn act(&mut self, act: Act, ui: Option<&mut Ui>) {
        self.note = None;
        match act {
            Act::None => return,
            Act::OpenItem(k) => self.open(Sheet::Item(k)),
            Act::OpenHost(h) => self.open(Sheet::Host(h)),
            Act::OpenComponent(c) => self.open(Sheet::Component(c)),
            Act::Toggle(k) => {
                if !self.opened.toggled.remove(&k) {
                    self.opened.toggled.insert(k);
                }
            }
            Act::ToggleRestarted => self.opened.restarted = !self.opened.restarted,
            Act::ToggleHealthy => self.opened.healthy = !self.opened.healthy,
            Act::ToggleDetail(k) => {
                if !self.opened.details.remove(&k) {
                    self.opened.details.insert(k);
                }
            }
            Act::Copy { what, text } => {
                if let Some(ui) = ui {
                    ui.copy(&text);
                }
                self.note = Some(format!("{what} is on the clipboard."));
            }
        }
        self.stale = true;
    }

    fn open(&mut self, s: Sheet) {
        self.sheets.push(s);
        self.reset_list();
    }

    fn sheet_title(&self) -> String {
        let Some(o) = &self.data else { return String::new() };
        match self.sheets.last() {
            Some(Sheet::Item(k)) => o.attention.iter().find(|a| &a.key == k).map_or_else(|| k.clone(), |a| a.title.clone()),
            Some(Sheet::Host(h)) => o.hosts.iter().find(|x| &x.id == h).map_or_else(|| h.clone(), |x| x.name.clone()),
            Some(Sheet::Component(c)) => o.name_of(c),
            None => String::new(),
        }
    }

    /// The page's header: "All systems normal · updated 2 min ago".
    pub fn status(&self) -> (State, String) {
        if self.error.is_some() {
            return (State::Crit, "Can't reach the server".to_owned());
        }
        let Some(d) = &self.data else { return (State::Unknown, "Loading…".to_owned()) };
        let updated = format!(" · updated {}", ago(&d.generated_at, &self.zone, self.now()));
        if d.attention_count == 0 {
            return (State::Ok, format!("All systems normal{updated}"));
        }
        let verb = if d.attention_count == 1 { "needs" } else { "need" };
        (if d.overall == State::Crit { State::Crit } else { State::Warn }, format!("{} {verb} attention{updated}", d.attention_count))
    }
}

impl App for ServerApp {
    type Msg = Msg;

    fn message(&mut self, m: Msg) {
        match m {
            Msg::Changed => {}
            Msg::SignedIn => {
                self.sign_in = None;
                if self.link.is_none() {
                    self.link = self.start_worker.as_mut().and_then(|f| f());
                }
                self.refresh(false);
            }
            Msg::Loaded(o) => {
                self.loading = false;
                self.error = None;
                self.data = Some(*o);
                self.stale = true;
                if let (Some(save), Some(o)) = (&mut self.save_cache, &self.data) {
                    save(o);
                }
            }
            Msg::Failed(ServerError::SignedOut) => {
                self.loading = false;
                self.link = None;
                crate::account::rejected();
                self.sign_in = self.start_sign_in.as_mut().map(|f| f());
            }
            Msg::Failed(ServerError::Failed(e)) => {
                self.loading = false;
                self.error = Some(e);
            }
        }
    }

    fn view(&mut self, ui: &mut Ui) {
        ui.header(if self.sample { "Server (sample data)" } else { "Server" });
        if let Some(s) = &mut self.sign_in {
            s.view(ui, "Your server's health is in your account.");
            return;
        }
        if self.last_load.is_none_or(|t| t.elapsed() >= REFRESH) {
            self.refresh(false);
        }
        ui.wake_after(TICK);
        if self.built_at.is_none_or(|t| t.elapsed() >= TICK) {
            self.stale = true;
        }
        if self.stale {
            self.rebuild();
        }
        let t = ui.theme();

        let top = ui.cut_top(t.px(30.0));
        let (state, text) = self.status();
        dot(ui, Rect::new(top.x + t.px(14.0), top.y + t.px(10.0), t.px(10.0), t.px(10.0)), state, &t);
        ui.text_in(Rect::new(top.x + t.px(32.0), top.y, top.w - t.px(40.0), top.h), Face::Sans, t.pt(14.0), t.item, &text);

        let rail = ui.cut_left(t.px(132.0));
        ui.fill(rail, t.bar);
        let mut chosen = None;
        let mut refresh = false;
        ui.within(rail.inset(t.px(8.0)), |ui| {
            for (i, (s, label)) in SECTIONS.iter().enumerate() {
                let text = if self.section == *s { format!("› {label}") } else { (*label).to_owned() };
                if ui.button_id(("section", i), &text) {
                    chosen = Some(*s);
                }
            }
            ui.space(10.0);
            if ui.button_id("refresh", if self.loading { "Refreshing…" } else { "Refresh (R)" }) {
                refresh = true;
            }
        });
        let mut todo = None;
        let pane = ui.area();
        let pane = Rect::new(pane.x + t.px(10.0), pane.y + t.px(4.0), pane.w - t.px(16.0), pane.h - t.px(8.0));
        ui.within(pane, |ui| {
            if let (Some(e), true) = (&self.error, self.data.is_some()) {
                ui.wrapped(Face::Sans, t.pt(13.0), t.danger, &format!("Couldn't refresh: {e}"));
            }
            if let Some(o) = &self.data {
                for s in o.broken_sources() {
                    ui.wrapped(Face::Sans, t.pt(13.0), t.item, s);
                }
            } else {
                if self.error.is_some() {
                    ui.wrapped(Face::Sans, t.pt(14.0), t.danger, "Couldn't load the server overview.");
                    if ui.button("Try again") {
                        refresh = true;
                    }
                } else {
                    ui.dim("Loading…");
                }
                return;
            }
            if !self.sheets.is_empty() {
                let title = self.sheet_title();
                let head = ui.cut_top(t.px(28.0));
                ui.text_in(head, Face::Bold, t.pt(16.0), t.text, &title);
                let hint = "Esc back";
                let w = ui.measure(Face::Sans, t.pt(12.0), hint);
                ui.text_in(Rect::new(head.right() - w, head.y, w, head.h), Face::Sans, t.pt(12.0), t.dim, hint);
            } else if self.section == Section::Services {
                let o = self.data.as_ref().expect("checked above");
                let repos = pages::repo_count(o);
                let row = ui.cut_top(t.px(36.0));
                let bw = t.px(150.0);
                if std::mem::take(&mut self.focus_search) {
                    ui.focus("search");
                }
                let res = ui.text_field_at(Rect::new(row.x, row.y + t.px(2.0), row.w - bw - t.px(8.0), t.px(30.0)), "search", &mut self.search);
                if res.changed {
                    self.opened.query = self.search.text.clone();
                    self.stale = true;
                }
                if res.submitted {
                    self.focus_list = true;
                }
                let label = format!("{} repos ({repos})", if self.opened.show_repos { "Hide" } else { "Show" });
                if ui.button_at(Rect::new(row.right() - bw, row.y + t.px(2.0), bw, t.px(30.0)), "repos", &label) {
                    self.opened.show_repos = !self.opened.show_repos;
                    self.stale = true;
                }
            }
            if let Some(n) = &self.note {
                ui.wrapped(Face::Sans, t.pt(13.0), t.ok, n);
            }
            let r = ui.area();
            if self.focus_list && !self.lines.is_empty() {
                self.focus_list = false;
                ui.focus("lines");
            }
            let lines = &self.lines;
            let res = ui.list_measured(
                "lines",
                &mut self.list,
                r,
                lines.len(),
                |text, i, w| height(text, &lines[i], w, &t),
                |ui, i, selected| draw(ui, &lines[i], selected),
            );
            if let Some(i) = res.activated {
                todo = lines[i].act().cloned();
            }
        });

        if let Some(s) = chosen {
            self.switch(s);
        }
        if let Some(a) = todo {
            self.act(a, Some(ui));
        }
        if refresh {
            self.refresh(true);
        }
        if ui.key(keys::KEY_ESC) {
            if self.search.text.is_empty() {
                if self.sheets.pop().is_some() {
                    self.note = None;
                    self.reset_list();
                }
            } else {
                self.search.clear();
                self.opened.query.clear();
                self.reset_list();
            }
        }
        if ui.key(keys::KEY_R) {
            self.refresh(true);
        }
        for (i, code) in [keys::KEY_1, keys::KEY_2, keys::KEY_3, keys::KEY_4].into_iter().enumerate() {
            if ui.key(code) {
                self.switch(SECTIONS[i].0);
            }
        }
        // `/` focuses the search, as on the web.
        if ui.key(keys::KEY_SLASH) {
            self.switch(Section::Services);
            self.focus_list = false;
            self.focus_search = true;
        }
    }
}

fn state_colour(s: State, t: &Theme) -> Rgb {
    match s {
        State::Ok => t.ok,
        State::Warn => t.accent,
        State::Crit => t.danger,
        State::Idle | State::Unknown => t.dim,
    }
}

fn dot(ui: &mut Ui, r: Rect, s: State, t: &Theme) {
    ui.fill_round(r, r.w / 2, state_colour(s, t));
}

const INSET: f32 = 8.0;

fn tone_face(tone: Tone, t: &Theme) -> (Face, Rgb) {
    match tone {
        Tone::Text => (Face::Sans, t.text),
        Tone::Dim => (Face::Sans, t.dim),
        Tone::Mono => (Face::Mono, t.item),
    }
}

/// A line's height for a width: `list_measured` keeps the answer until the lines change.
fn height(text: &mut Text, line: &Line, w: i32, t: &Theme) -> i32 {
    match line {
        Line::Heading(_) => t.px(32.0),
        Line::Para(tone, s) => {
            let (f, _) = tone_face(*tone, t);
            Ui::wrapped_height(text, f, t.pt(14.0), s, w - t.px(2.0 * INSET)) + t.px(8.0)
        }
        Line::Row { sub, meta, .. } => {
            if sub.is_empty() && meta.is_empty() {
                t.px(32.0)
            } else {
                t.px(54.0)
            }
        }
        Line::Host { sparks, .. } => {
            if sparks.is_empty() {
                t.px(72.0)
            } else {
                t.px(108.0)
            }
        }
        Line::Disk { .. } => t.px(40.0),
        Line::Chart { .. } => t.px(150.0),
        Line::Button { .. } => t.px(38.0),
    }
}

fn draw(ui: &mut Ui, line: &Line, selected: bool) {
    let t = ui.theme();
    let row = ui.area();
    if selected {
        ui.fill_round(Rect::new(row.x, row.y + 1, row.w, row.h - 2), t.px(6.0), t.selection);
    }
    let x = row.x + t.px(INSET);
    let w = row.w - t.px(2.0 * INSET);
    match line {
        Line::Heading(s) => {
            ui.text_in(Rect::new(x, row.bottom() - t.px(26.0), w, t.px(24.0)), Face::Bold, t.pt(14.0), t.accent, s);
        }
        Line::Para(tone, s) => {
            let (f, c) = tone_face(*tone, &t);
            ui.within(Rect::new(x, row.y + t.px(3.0), w, row.h - t.px(4.0)), |ui| ui.wrapped(f, t.pt(14.0), c, s));
        }
        Line::Row { state, title, badge, sub, meta, chips, indent, .. } => {
            let x = x + t.px(14.0 * f32::from(*indent));
            let mut left = x;
            let title_h = t.px(24.0);
            let top = if sub.is_empty() && meta.is_empty() { row.y + (row.h - title_h) / 2 } else { row.y + t.px(4.0) };
            if let Some(s) = state {
                let side = t.px(10.0);
                dot(ui, Rect::new(left, top + (title_h - side) / 2, side, side), *s, &t);
                left += side + t.px(8.0);
            }
            // Chips and meta from the right, so a long title gives way.
            let mut right = row.right() - t.px(INSET);
            for c in chips.iter().rev() {
                let cw = ui.measure(Face::Sans, t.pt(12.0), c) + t.px(12.0);
                if right - cw < left + t.px(120.0) {
                    break;
                }
                let r = Rect::new(right - cw, top + t.px(2.0), cw, t.px(20.0));
                ui.fill_round(r, t.px(5.0), t.panel_hi);
                ui.text_in(Rect::new(r.x + t.px(6.0), r.y, cw, r.h), Face::Sans, t.pt(12.0), t.item, c);
                right -= cw + t.px(5.0);
            }
            if let (Some(b), Some(s)) = (badge, state) {
                let bw = ui.measure(Face::Sans, t.pt(12.0), b) + t.px(6.0);
                if right - bw > left + t.px(120.0) {
                    let c = if selected { t.text } else { state_colour(*s, &t) };
                    ui.text_in(Rect::new(right - bw, top, bw, title_h), Face::Sans, t.pt(12.0), c, b);
                    right -= bw + t.px(5.0);
                }
            }
            ui.text_in(Rect::new(left, top, right - left, title_h), Face::Sans, t.pt(15.0), t.text, title);
            if !sub.is_empty() || !meta.is_empty() {
                let y = top + title_h;
                let right = row.right() - t.px(INSET);
                let mw = if meta.is_empty() { 0 } else { ui.measure(Face::Sans, t.pt(12.0), meta) + t.px(8.0) };
                let dim = if selected { t.item } else { t.dim };
                ui.text_in(Rect::new(left, y, (right - mw - left).max(0), t.px(20.0)), Face::Sans, t.pt(12.0), dim, sub);
                if mw > 0 {
                    ui.text_in(Rect::new(right - mw + t.px(8.0), y, mw, t.px(20.0)), Face::Sans, t.pt(12.0), dim, meta);
                }
            }
        }
        Line::Host { state, name, role, uptime, os, note, sparks, disk, .. } => {
            let top = row.y + t.px(4.0);
            let card = Rect::new(row.x + 1, row.y + t.px(2.0), row.w - 2, row.h - t.px(4.0));
            if !selected {
                ui.fill_round(card, t.px(8.0), t.panel);
            }
            let side = t.px(10.0);
            dot(ui, Rect::new(x, top + t.px(7.0), side, side), *state, &t);
            let head = if role.is_empty() { name.clone() } else { format!("{name}  ·  {role}") };
            ui.text_in(Rect::new(x + side + t.px(8.0), top, w - t.px(120.0), t.px(24.0)), Face::Bold, t.pt(15.0), t.text, &head);
            let uw = ui.measure(Face::Sans, t.pt(12.0), uptime);
            ui.text_in(Rect::new(x + w - uw, top, uw, t.px(24.0)), Face::Sans, t.pt(12.0), t.dim, uptime);
            ui.text_in(Rect::new(x, top + t.px(24.0), w, t.px(20.0)), Face::Sans, t.pt(12.0), t.dim, os);
            let y = top + t.px(44.0);
            if let Some(n) = note {
                ui.text_in(Rect::new(x, y, w, t.px(20.0)), Face::Sans, t.pt(13.0), state_colour(*state, &t), n);
            } else {
                let cell = (w - t.px(12.0)) / 3;
                for (i, s) in sparks.iter().enumerate() {
                    let r = Rect::new(x + (cell + t.px(6.0)) * i as i32, y, cell, t.px(38.0));
                    draw_spark(ui, s, r, &t);
                }
                if let Some((d, more)) = disk {
                    let dy = y + t.px(38.0);
                    draw_disk(ui, d, *more, Rect::new(x, dy, w, t.px(18.0)), &t, false);
                }
            }
        }
        Line::Disk { disk, more } => draw_disk(ui, disk, *more, Rect::new(x, row.y + t.px(2.0), w, row.h - t.px(4.0)), &t, true),
        Line::Chart { spark, stats, ticks } => {
            ui.text_in(Rect::new(x, row.y + t.px(2.0), w, t.px(22.0)), Face::Sans, t.pt(13.0), t.item, stats);
            let plot = Rect::new(x, row.y + t.px(26.0), w, row.h - t.px(46.0));
            ui.fill(plot, t.panel);
            if spark.values.iter().flatten().count() < 2 {
                ui.text_in(plot.inset(t.px(8.0)), Face::Sans, t.pt(13.0), t.dim, "collecting…");
            } else {
                draw_series(ui, spark, plot, &t);
                if spark.metric == Metric::Tctl {
                    let (lo, hi) = spark.scale();
                    let y = plot.y + ((1.0 - (HOT_C - lo) / (hi - lo)) * f64::from(plot.h)) as i32;
                    ui.fill(Rect::new(plot.x, y, plot.w, 1), t.danger);
                    ui.text_in(Rect::new(plot.right() - t.px(44.0), y, t.px(44.0), t.px(16.0)), Face::Sans, t.pt(11.0), t.danger, "90 °C");
                }
                for (at, label) in ticks {
                    let tx = plot.x + (at * plot.w as f32) as i32;
                    ui.fill(Rect::new(tx, plot.y, 1, plot.h), t.panel_hi);
                    // At the edges a label moves inside the chart rather than being cut.
                    let lx = (tx - t.px(18.0)).clamp(plot.x, plot.right() - t.px(40.0));
                    ui.text_in(Rect::new(lx, plot.bottom(), t.px(40.0), t.px(18.0)), Face::Sans, t.pt(11.0), t.dim, label);
                }
            }
        }
        Line::Button { label, .. } => {
            let r = Rect::new(x, row.y + t.px(3.0), w.min(t.px(320.0)), row.h - t.px(6.0));
            ui.fill_round(r, t.px(6.0), if selected { t.accent } else { t.panel_hi });
            ui.text_in(Rect::new(r.x + t.px(10.0), r.y, r.w - t.px(20.0), r.h), Face::Sans, t.pt(14.0), if selected { t.bg } else { t.text }, label);
        }
    }
}

fn draw_disk(ui: &mut Ui, d: &crate::model::Disk, more: usize, r: Rect, t: &Theme, tall: bool) {
    let colour = match disk_state(d) {
        State::Ok => t.ok,
        s => state_colour(s, t),
    };
    let line = t.px(if tall { 20.0 } else { 14.0 });
    let text = disk_text(d, more);
    let px = t.pt(if tall { 13.0 } else { 11.0 });
    let tw = ui.measure(Face::Sans, px, &text);
    ui.text_in(Rect::new(r.x, r.y, r.w - tw, line), Face::Sans, px, t.item, &d.mount);
    ui.text_in(Rect::new(r.right() - tw, r.y, tw, line), Face::Sans, px, t.dim, &text);
    let bar = Rect::new(r.x, r.y + line + 1, r.w, t.px(3.0).max(2));
    ui.fill(bar, t.panel_hi);
    let w = (f64::from(r.w) * d.used_pct.clamp(0.0, 100.0) / 100.0) as i32;
    ui.fill(Rect::new(bar.x, bar.y, w, bar.h), colour);
}

fn draw_spark(ui: &mut Ui, s: &Spark, r: Rect, t: &Theme) {
    let line = t.px(16.0);
    let value = s.text();
    let vw = ui.measure(Face::Sans, t.pt(12.0), &value);
    ui.text_in(Rect::new(r.x, r.y, r.w - vw, line), Face::Sans, t.pt(12.0), t.dim, s.metric.short());
    ui.text_in(Rect::new(r.right() - vw, r.y, vw, line), Face::Sans, t.pt(12.0), t.text, &value);
    let plot = Rect::new(r.x, r.y + line + 1, r.w, r.h - line - 2);
    ui.fill(plot, t.panel_hi);
    if s.values.iter().flatten().count() >= 2 {
        draw_series(ui, s, plot, t);
    }
}

/// The series as columns under a brighter top: `chartPaths`, in pixels. A null leaves a gap; values are clamped to the scale.
fn draw_series(ui: &mut Ui, s: &Spark, plot: Rect, t: &Theme) {
    if !ui.painting() || plot.w <= 0 || plot.h <= 0 {
        return;
    }
    let (lo, hi) = s.scale();
    let n = s.values.len();
    let thick = t.px(2.0).max(1);
    for x in 0..plot.w {
        // A column shows the worst sample under it, so a short spike is not lost on a small chart.
        let a = x as usize * n / plot.w as usize;
        let b = ((x as usize + 1) * n / plot.w as usize).clamp(a + 1, n.max(a + 1)).min(n);
        let v = s.values[a..b].iter().flatten().copied().fold(None, |m: Option<f64>, v| Some(m.map_or(v, |x| x.max(v))));
        let Some(v) = v else { continue };
        let f = if hi > lo { (v.clamp(lo, hi) - lo) / (hi - lo) } else { 0.5 };
        let h = (f * f64::from(plot.h)).round() as i32;
        let y = plot.bottom() - h;
        ui.fill(Rect::new(plot.x + x, y, 1, h), t.selection);
        ui.fill(Rect::new(plot.x + x, y, 1, thick.min(h.max(1))), t.accent);
    }
}
