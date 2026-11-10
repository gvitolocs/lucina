//! The app: the Timeline, Problems, a crash group's events and an event's Details, one stack of pages and Esc back out of each.

use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use lucina_app::time::Zone;
use lucina_app::{App, Face, ListState, Rect, Ui, keys};
use lucina_evlog::event::Event;
use lucina_evlog::store::Meta;
use lucina_evlog::timeline::{self, Day, FORMULA};

use crate::chart;
use crate::details::{self, DLine, LOG_ROW};
use crate::look::{BLUE, event_colour, index_colour, pill_right};
use crate::model::{
    Data, event_clock, index_text, long_date, offset_s, problems, short_date, title,
};

pub enum Msg {
    Loaded(Box<(Vec<Event>, Meta)>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum View {
    Timeline,
    Problems,
    /// The events of one crash group, by bucket id.
    Group(String),
    Details(u64),
}

/// A row of an event list: the record, and a heading to draw above it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ERow {
    pub record: u64,
    pub head: Option<&'static str>,
}

#[derive(Default)]
pub struct Detail {
    pub record: u64,
    pub lines: Vec<DLine>,
    pub list: ListState,
    pub log: ListState,
    /// The log line an evidence item picked.
    pub hl: Option<usize>,
    pub tech: bool,
    /// Row of `lines` the highlight was last taken from, so that it moves only when the selection does.
    seen_sel: Option<usize>,
    center: Option<usize>,
}

struct Level {
    view: View,
    list: ListState,
}

pub struct ReliabilityApp {
    pub zone: Zone,
    pub data: Option<Rc<Data>>,
    sample: bool,
    /// Tests fix the clock.
    pub fixed_now: Option<i64>,
    stack: Vec<Level>,
    /// How many days before today the timeline has selected.
    pub back: usize,
    /// Whether the opening day was chosen yet (see `days`).
    day_picked: bool,
    days: Rc<Vec<Day>>,
    days_key: Option<(usize, i64, u64)>,
    generation: u64,
    pub rows: Vec<ERow>,
    rows_stale: bool,
    pub formula: bool,
    pub detail: Detail,
    dir: String,
    want_focus: bool,
}

fn clock_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

impl ReliabilityApp {
    pub fn new(zone: Zone, dir: &str) -> ReliabilityApp {
        ReliabilityApp {
            zone,
            data: None,
            sample: false,
            fixed_now: None,
            stack: vec![Level {
                view: View::Timeline,
                list: ListState::new(),
            }],
            back: 0,
            day_picked: false,
            days: Rc::new(Vec::new()),
            days_key: None,
            generation: 0,
            rows: Vec::new(),
            rows_stale: true,
            formula: false,
            detail: Detail::default(),
            dir: dir.to_owned(),
            want_focus: true,
        }
    }

    pub fn sample(mut self) -> ReliabilityApp {
        self.sample = true;
        self
    }

    pub fn now(&self) -> i64 {
        self.fixed_now.unwrap_or_else(clock_now)
    }

    pub fn view_now(&self) -> &View {
        &self.stack.last().expect("the stack never empties").view
    }

    /// The loaded log, for tests and `--render`.
    pub fn load(&mut self, events: Vec<Event>, meta: Meta) {
        if let Some(d) = &self.data {
            if d.events == events && d.meta == meta {
                return;
            }
        }
        self.data = Some(Rc::new(Data::new(events, meta)));
        self.generation += 1;
        self.days_key = None;
        self.rows_stale = true;
        if let View::Details(r) = self.view_now().clone() {
            self.build_detail(r, true);
        }
    }

    /// The days the chart shows, for `n` columns; the last is today.
    pub fn days(&mut self, n: usize) -> Rc<Vec<Day>> {
        let now = self.now();
        let key = (n, now.div_euclid(60_000), self.generation);
        if self.days_key != Some(key) {
            if let Some(d) = &self.data {
                self.days = Rc::new(timeline::days(&d.events, now, n, offset_s(&self.zone, now)));
                // The first time there is something to show: a today without events opens on the latest day that has
                // some, so that the screen starts on the last problems and Enter has an event to open.
                if !self.day_picked && !d.events.is_empty() {
                    self.day_picked = true;
                    let days = &self.days;
                    if days.last().is_some_and(|t| t.records.is_empty()) {
                        if let Some(i) = days.iter().rposition(|x| !x.records.is_empty()) {
                            self.back = days.len() - 1 - i;
                        }
                    }
                }
            }
            self.days_key = Some(key);
            self.rows_stale = true;
        }
        self.days.clone()
    }

    #[cfg(test)]
    pub fn selected_day(&self) -> Option<&Day> {
        let n = self.days.len();
        (n > 0).then(|| &self.days[n - 1 - self.back.min(n - 1)])
    }

    /// The headline: today's index as shown, and the status line under it.
    pub fn headline(&self) -> (String, String) {
        let n = self.days.len();
        let Some(today) = self.days.last() else {
            return ("—".to_owned(), String::new());
        };
        let before: u32 = self.days[n.saturating_sub(8)..n - 1]
            .iter()
            .map(problems)
            .sum();
        let now = problems(today);
        let a = match now {
            0 => "No problems today".to_owned(),
            1 => "1 problem today".to_owned(),
            k => format!("{k} problems today"),
        };
        let b = match before {
            0 => "none in the 7 days before".to_owned(),
            k => format!("{k} in the 7 days before"),
        };
        (
            format!("{} / 10", index_text(today.index)),
            format!("{a} · {b}"),
        )
    }

    /// The newest critical event of today (the day the clock is in).
    pub fn newest_critical_today(&self) -> Option<u64> {
        let d = self.data.as_ref()?;
        let today = crate::model::local_day(&self.zone, self.now());
        d.events
            .iter()
            .filter(|e| {
                e.level == lucina_evlog::Level::Critical
                    && e.time_ms
                        .is_some_and(|t| crate::model::local_day(&self.zone, t) == today)
            })
            .max_by_key(|e| (e.time_ms, e.record))
            .map(|e| e.record)
    }

    /// The text of the list area when there is nothing to list.
    pub fn empty_notice(&self) -> Option<&'static str> {
        let d = self.data.as_ref()?;
        d.events.is_empty().then_some("No events yet")
    }

    // ---- Navigation ----

    fn push(&mut self, v: View) {
        self.stack.push(Level {
            view: v,
            list: ListState::new(),
        });
        self.rows_stale = true;
        self.want_focus = true;
    }

    fn root(&mut self, v: View) {
        self.stack.clear();
        self.formula = false;
        self.stack.push(Level {
            view: v,
            list: ListState::new(),
        });
        self.rows_stale = true;
        self.want_focus = true;
    }

    pub fn open_details(&mut self, record: u64) {
        self.push(View::Details(record));
        self.build_detail(record, false);
    }

    fn build_detail(&mut self, record: u64, keep: bool) {
        let Some(d) = self.data.clone() else { return };
        let Some(e) = d.event(record) else { return };
        let tech = if keep && self.detail.record == record {
            self.detail.tech
        } else {
            false
        };
        let lines = details::build(&d, e, &self.zone, self.now(), tech);
        if keep && self.detail.record == record {
            self.detail.lines = lines;
            self.detail.list.invalidate();
        } else {
            self.detail = Detail {
                record,
                lines,
                ..Detail::default()
            };
            self.detail.list.plain = true;
            self.detail.log.plain = true;
        }
    }

    fn toggle_tech(&mut self) {
        self.detail.tech = !self.detail.tech;
        let r = self.detail.record;
        let Some(d) = self.data.clone() else { return };
        if let Some(e) = d.event(r) {
            self.detail.lines = details::build(&d, e, &self.zone, self.now(), self.detail.tech);
            self.detail.list.invalidate();
        }
    }

    /// Selects the `k`th row of the page and takes its log line, as the arrow keys do.
    pub fn select_row(&mut self, k: usize) {
        self.detail.list.select(k);
        self.follow_selection();
    }

    /// The first evidence item that points at a log line.
    pub fn first_linked_row(&self) -> Option<usize> {
        self.detail
            .lines
            .iter()
            .position(|l| l.log_line().is_some())
    }

    fn follow_selection(&mut self) {
        let sel = self.detail.list.selected;
        if sel == self.detail.seen_sel {
            return;
        }
        self.detail.seen_sel = sel;
        if let Some(l) = sel.and_then(|i| self.detail.lines[i].log_line()) {
            self.detail.hl = Some(l);
            self.detail.center = Some(l);
        }
    }

    fn move_selection(&mut self, down: bool) {
        let lines = &self.detail.lines;
        let from = self.detail.list.selected;
        let next = if down {
            (from.map_or(0, |s| s + 1)..lines.len()).find(|&i| lines[i].selectable())
        } else {
            (0..from.unwrap_or(0))
                .rev()
                .find(|&i| lines[i].selectable())
        };
        if let Some(i) = next {
            self.detail.list.select(i);
        }
    }

    /// Tab: the next evidence item that has a log line, round the end.
    fn next_linked(&mut self, back: bool) {
        let linked: Vec<usize> = (0..self.detail.lines.len())
            .filter(|&i| self.detail.lines[i].log_line().is_some())
            .collect();
        if linked.is_empty() {
            return;
        }
        let cur = self.detail.list.selected;
        let at = cur.and_then(|c| linked.iter().position(|&i| i == c));
        let k = match (at, back) {
            (Some(k), false) => (k + 1) % linked.len(),
            (Some(k), true) => (k + linked.len() - 1) % linked.len(),
            (None, false) => cur
                .and_then(|c| linked.iter().position(|&i| i > c))
                .unwrap_or(0),
            (None, true) => linked.len() - 1,
        };
        self.detail.list.select(linked[k]);
    }

    fn rebuild_rows(&mut self) {
        self.rows_stale = false;
        self.rows.clear();
        let Some(d) = self.data.clone() else { return };
        match self.view_now().clone() {
            View::Timeline => {
                let n = self.days.len();
                if n == 0 {
                    return;
                }
                let sel = n - 1 - self.back.min(n - 1);
                let mut recs: Vec<u64> = self.days[sel].records.clone();
                recs.reverse();
                self.rows = recs
                    .into_iter()
                    .map(|record| ERow { record, head: None })
                    .collect();
                if sel == n - 1 {
                    let mut first = true;
                    for e in d.events.iter().rev().filter(|e| e.time_ms.is_none()) {
                        self.rows.push(ERow {
                            record: e.record,
                            head: first.then_some("Time unknown"),
                        });
                        first = false;
                    }
                }
            }
            View::Group(id) => {
                let mut v: Vec<&Event> = d
                    .events
                    .iter()
                    .filter(|e| e.bucket.as_ref().is_some_and(|b| b.id == id))
                    .collect();
                v.sort_by_key(|e| std::cmp::Reverse((e.time_ms, e.record)));
                self.rows = v
                    .into_iter()
                    .map(|e| ERow {
                        record: e.record,
                        head: None,
                    })
                    .collect();
            }
            View::Problems | View::Details(_) => {}
        }
    }

    fn with_list<R>(&mut self, f: impl FnOnce(&mut Self, &mut ListState) -> R) -> R {
        let mut list = std::mem::take(&mut self.stack.last_mut().expect("never empty").list);
        let r = f(self, &mut list);
        self.stack.last_mut().expect("never empty").list = list;
        r
    }

    // ---- Drawing ----

    fn tabs(&self, ui: &mut Ui, r: Rect) -> Option<View> {
        let t = ui.theme();
        let mut chosen = None;
        let root = &self.stack[0].view;
        let mut x = r.x;
        for (i, (v, label)) in [
            (View::Timeline, "1  Timeline"),
            (View::Problems, "2  Problems"),
        ]
        .into_iter()
        .enumerate()
        {
            let w = ui.measure(Face::Bold, t.pt(14.0), label) + t.px(28.0);
            let tab = Rect::new(x, r.y + t.px(3.0), w, r.h - t.px(6.0));
            let hit = ui.hit(("tab", i), tab);
            let on = *root == v && self.stack.len() == 1;
            let part = *root == v;
            if on {
                ui.fill_round(tab, t.px(6.0), t.accent);
            } else if part || hit.hovered {
                ui.fill_round(tab, t.px(6.0), t.panel_hi);
            } else {
                ui.fill_round(tab, t.px(6.0), t.panel);
            }
            ui.text_in(
                Rect::new(tab.x + t.px(14.0), tab.y, tab.w, tab.h),
                if on { Face::Bold } else { Face::Sans },
                t.pt(14.0),
                if on { t.bg } else { t.text },
                label,
            );
            if hit.clicked {
                chosen = Some(v);
            }
            x += w + t.px(8.0);
        }
        chosen
    }

    fn crumbs(&self, ui: &mut Ui, r: Rect) {
        let t = ui.theme();
        let mut parts = vec!["Esc back".to_owned()];
        for l in &self.stack {
            parts.push(match &l.view {
                View::Timeline => "Timeline".to_owned(),
                View::Problems => "Problems".to_owned(),
                View::Group(id) => self
                    .data
                    .as_ref()
                    .and_then(|d| d.group(id))
                    .map_or("Same problem".to_owned(), |g| g.bucket.label.clone()),
                View::Details(rec) => self
                    .data
                    .as_ref()
                    .and_then(|d| d.event(*rec))
                    .map_or("Details".to_owned(), title),
            });
        }
        let text = format!("{}   ‹   {}", parts[0], parts[1..].join("  ›  "));
        ui.text_in(
            Rect::new(r.x + t.px(4.0), r.y, r.w, r.h),
            Face::Sans,
            t.pt(13.0),
            t.dim,
            &text,
        );
    }

    fn footer(&self, ui: &mut Ui) {
        let t = ui.theme();
        let r = ui.cut_bottom(t.px(26.0));
        ui.fill(r, t.bar);
        let Some(d) = &self.data else { return };
        let place = if self.sample {
            "sample data, nothing is saved".to_owned()
        } else {
            self.dir.clone()
        };
        let mut text = format!(
            "{} events · {} routine events dropped by rotation",
            d.events.len(),
            d.meta.dropped_routine
        );
        if d.meta.dropped_important > 0 {
            text.push_str(&format!(
                " · {} important events dropped",
                d.meta.dropped_important
            ));
        }
        text.push_str(&format!(" · {place}"));
        ui.text_in(
            Rect::new(r.x + t.px(14.0), r.y, r.w - t.px(28.0), r.h),
            Face::Sans,
            t.pt(12.0),
            t.dim,
            &text,
        );
    }
}

/// How many day columns fit: 21 at most, 10 at least.
fn columns(width: i32, t: &lucina_app::Theme) -> usize {
    let room = width - t.px(chart::GUTTER) - t.px(8.0);
    ((room / t.px(52.0).max(1)).max(0) as usize).clamp(10, 21)
}

impl App for ReliabilityApp {
    type Msg = Msg;

    fn message(&mut self, m: Msg) {
        let Msg::Loaded(b) = m;
        let (events, meta) = *b;
        self.load(events, meta);
    }

    fn view(&mut self, ui: &mut Ui) {
        ui.header(if self.sample {
            "Reliability (sample)"
        } else {
            "Reliability"
        });
        ui.wake_after(Duration::from_secs(60));
        let t = ui.theme();
        self.footer(ui);
        let Some(data) = self.data.clone() else {
            ui.space(30.0);
            ui.within(
                Rect::new(
                    ui.area().x + t.px(20.0),
                    ui.area().y,
                    ui.area().w,
                    t.px(30.0),
                ),
                |ui| ui.dim("Reading the event log…"),
            );
            return;
        };
        let area = ui.area();
        let inner = Rect::new(
            area.x + t.px(14.0),
            area.y + t.px(4.0),
            area.w - t.px(28.0),
            area.h - t.px(6.0),
        );
        let mut goto: Option<View> = None;
        let mut open: Option<u64> = None;
        let mut group: Option<String> = None;
        ui.within(inner, |ui| {
            let strip = ui.cut_top(t.px(36.0));
            match self.view_now().clone() {
                View::Timeline | View::Problems if self.stack.len() == 1 => {
                    goto = self.tabs(ui, strip)
                }
                _ => self.crumbs(ui, strip),
            }
            match self.view_now().clone() {
                View::Timeline => open = self.timeline_page(ui, &data),
                View::Problems => group = self.problems_page(ui, &data),
                View::Group(_) => open = self.group_page(ui, &data),
                View::Details(_) => {
                    if let Some(g) = self.details_page(ui, &data) {
                        group = Some(g);
                    }
                }
            }
        });
        if self.formula {
            ui.modal(640.0, 270.0, |ui| {
                ui.heading("How the index is worked out");
                ui.space(6.0);
                ui.wrapped(Face::Sans, ui.theme().pt(16.0), ui.theme().item, FORMULA);
                ui.space(10.0);
                ui.dim("Press ? or Esc to close.");
            });
        }

        // Pages.
        if let Some(v) = goto {
            self.root(v);
        }
        if let Some(r) = open {
            self.open_details(r);
        }
        if let Some(id) = group {
            self.push(View::Group(id));
        }
        // Keys no widget took.
        let on_details = matches!(self.view_now(), View::Details(_));
        let on_timeline = *self.view_now() == View::Timeline;
        if let Some(p) = ui.press() {
            if p.code == keys::KEY_SLASH && p.mods.shift && !p.mods.ctrl && !p.mods.alt {
                ui.take_press();
                if on_timeline || self.formula {
                    self.formula = !self.formula;
                }
            } else if on_details && p.code == keys::KEY_TAB && !p.mods.ctrl && !p.mods.alt {
                ui.take_press();
                self.next_linked(p.mods.shift);
            }
        }
        if ui.key(keys::KEY_ESC) {
            if self.formula {
                self.formula = false;
            } else if self.stack.len() > 1 {
                self.stack.pop();
                self.rows_stale = true;
                self.want_focus = true;
            }
        }
        if !self.formula {
            if ui.key(keys::KEY_1) {
                self.root(View::Timeline);
            }
            if ui.key(keys::KEY_2) {
                self.root(View::Problems);
            }
        }
        if on_details && ui.key(keys::KEY_T) {
            self.toggle_tech();
        }
        if on_timeline && !self.formula {
            let n = self.days.len();
            let (left, right) = (ui.key(keys::KEY_LEFT), ui.key(keys::KEY_RIGHT));
            if left && self.back + 1 < n {
                self.back += 1;
                self.rows_stale = true;
                self.stack[0].list = ListState::new();
            }
            if right && self.back > 0 {
                self.back -= 1;
                self.rows_stale = true;
                self.stack[0].list = ListState::new();
            }
            // The toolkit also walks the focus on Left and Right: the day's events take it back, so that Enter opens the
            // selected event and not the button next to the chart (seen on QEMU through the shell).
            if left || right {
                self.want_focus = true;
            }
        }
    }
}

// ---- Pages ----

impl ReliabilityApp {
    fn timeline_page(&mut self, ui: &mut Ui, data: &Rc<Data>) -> Option<u64> {
        let t = ui.theme();
        let n = columns(ui.area().w, &t);
        let days = self.days(n);
        let n = days.len();
        if self.rows_stale {
            self.rebuild_rows();
        }
        self.back = self.back.min(n.saturating_sub(1));
        let sel = n - 1 - self.back;
        let today = &days[n - 1];

        // Today's index, large, and what it says.
        let (big, status) = self.headline();
        let top = ui.cut_top(t.px(56.0));
        let idx = index_colour(today.index, &t);
        let (num, rest) = big.split_once(' ').unwrap_or((&big, ""));
        let nw = ui.measure(Face::Bold, t.pt(46.0), num);
        ui.text_in(
            Rect::new(top.x + t.px(6.0), top.y, nw + 4, top.h),
            Face::Bold,
            t.pt(46.0),
            idx,
            num,
        );
        ui.text_in(
            Rect::new(
                top.x + t.px(12.0) + nw,
                top.y + t.px(22.0),
                t.px(70.0),
                t.px(30.0),
            ),
            Face::Sans,
            t.pt(20.0),
            t.dim,
            rest,
        );
        let sx = top.x + t.px(12.0) + nw + t.px(80.0);
        ui.text_in(
            Rect::new(sx, top.y + t.px(6.0), top.w - sx + top.x, t.px(26.0)),
            Face::Bold,
            t.pt(16.0),
            t.text,
            &status,
        );
        ui.text_in(
            Rect::new(sx, top.y + t.px(30.0), top.w - sx + top.x, t.px(22.0)),
            Face::Sans,
            t.pt(13.0),
            t.dim,
            "Stability index today, from the problems of the last four weeks",
        );

        // The chart.
        let ch = chart::height(ui);
        let chart_r = ui.cut_top(ch);
        if let Some(i) = chart::draw(ui, chart_r, &days, sel) {
            self.back = n - 1 - i;
            self.rows_stale = true;
            self.stack[0].list = ListState::new();
        }
        ui.space(6.0);

        // Why this day has its index.
        let day = &days[sel];
        let why_h = t.px(24.0) + t.px(21.0) * day.why.len().max(1) as i32 + t.px(6.0);
        let why = ui.cut_top(why_h);
        let head = match day.index {
            Some(v) => format!("Why {v:.1}"),
            None => "Why —".to_owned(),
        };
        let when = if sel == n - 1 {
            "today".to_owned()
        } else {
            format!("on {}", pretty_date(&day.date))
        };
        ui.text_in(
            Rect::new(why.x + t.px(6.0), why.y, t.px(400.0), t.px(24.0)),
            Face::Bold,
            t.pt(14.0),
            t.accent,
            &format!("{head}, {when}"),
        );
        let bw = t.px(260.0);
        if ui.button_at(
            Rect::new(why.right() - bw, why.y, bw, t.px(26.0)),
            "formula",
            "How is this worked out?  (?)",
        ) {
            self.formula = true;
        }
        if day.index.is_none() {
            ui.text_in(
                Rect::new(why.x + t.px(16.0), why.y + t.px(24.0), why.w, t.px(21.0)),
                Face::Sans,
                t.pt(13.0),
                t.dim,
                "Nothing was watching on this day: the log begins later.",
            );
        } else if day.why.is_empty() {
            ui.text_in(
                Rect::new(why.x + t.px(16.0), why.y + t.px(24.0), why.w, t.px(21.0)),
                Face::Sans,
                t.pt(13.0),
                t.dim,
                "No problems are counted against this day.",
            );
        }
        for (i, c) in day.why.iter().enumerate() {
            let y = why.y + t.px(24.0) + t.px(21.0) * i as i32;
            let cost = format!("−{:.2} points", c.weight);
            let cw = ui.measure(Face::Sans, t.pt(13.0), &cost) + t.px(6.0);
            ui.fill_round(
                Rect::new(why.x + t.px(8.0), y + t.px(7.0), t.px(7.0), t.px(7.0)),
                t.px(3.5),
                t.danger,
            );
            ui.text_in(
                Rect::new(why.x + t.px(24.0), y, why.w - cw - t.px(34.0), t.px(21.0)),
                Face::Sans,
                t.pt(13.0),
                t.item,
                &c.summary,
            );
            ui.text_in(
                Rect::new(why.right() - cw, y, cw, t.px(21.0)),
                Face::Sans,
                t.pt(13.0),
                t.dim,
                &cost,
            );
        }

        // The day's events.
        let title_r = ui.cut_top(t.px(28.0));
        let label = if sel == n - 1 {
            "Today".to_owned()
        } else {
            long_date(&self.zone, day.start_ms + 1)
        };
        ui.text_in(
            Rect::new(title_r.x + t.px(6.0), title_r.y, title_r.w, title_r.h),
            Face::Bold,
            t.pt(14.0),
            t.accent,
            &format!("{label}: {} events, newest first", self.rows.len()),
        );
        self.event_list(ui, data, false, "events")
    }

    /// A list of events as rows. Returns the record Enter or a click opened.
    fn event_list(
        &mut self,
        ui: &mut Ui,
        data: &Rc<Data>,
        with_date: bool,
        id: &'static str,
    ) -> Option<u64> {
        let t = ui.theme();
        let area = ui.area();
        if self.rows.is_empty() {
            let msg = self.empty_notice().unwrap_or(if with_date {
                "No events here."
            } else {
                "No events on this day."
            });
            ui.text_in(
                Rect::new(area.x + t.px(10.0), area.y + t.px(6.0), area.w, t.px(30.0)),
                Face::Sans,
                t.pt(16.0),
                t.dim,
                msg,
            );
            return None;
        }
        let rows = self.rows.clone();
        let zone = self.zone.clone();
        let activated = self.with_list(|s, list| {
            if list.selected.is_none() {
                list.select(0);
            }
            if std::mem::take(&mut s.want_focus) {
                ui.focus(id);
            }
            let res = ui.list_measured(
                id,
                list,
                area,
                rows.len(),
                |_, i, _| t.px(if rows[i].head.is_some() { 62.0 } else { 36.0 }),
                |ui, i, selected| {
                    if let Some(e) = data.event(rows[i].record) {
                        event_row(ui, e, rows[i].head, with_date, selected, &zone);
                    }
                },
            );
            res.activated
        });
        activated.map(|i| rows[i].record)
    }

    fn group_page(&mut self, ui: &mut Ui, data: &Rc<Data>) -> Option<u64> {
        let t = ui.theme();
        if self.rows_stale {
            self.rebuild_rows();
            let n = self.rows.len();
            self.with_list(|_, l| {
                *l = ListState::new();
                if n > 0 {
                    l.select(0);
                }
            });
        }
        let head = ui.cut_top(t.px(30.0));
        if let View::Group(id) = self.view_now().clone() {
            if let Some(g) = data.group(&id) {
                ui.text_in(
                    Rect::new(head.x + t.px(6.0), head.y, head.w, head.h),
                    Face::Bold,
                    t.pt(16.0),
                    t.text,
                    &format!("{}  ·  {} events", g.bucket.label, self.rows.len()),
                );
            }
        }
        self.event_list(ui, data, true, "group")
    }

    fn problems_page(&mut self, ui: &mut Ui, data: &Rc<Data>) -> Option<String> {
        let t = ui.theme();
        let head = ui.cut_top(t.px(30.0));
        ui.text_in(
            Rect::new(head.x + t.px(6.0), head.y, head.w, head.h),
            Face::Bold,
            t.pt(16.0),
            t.text,
            &format!("Problems: {} kinds, most recent first", data.groups.len()),
        );
        let area = ui.area();
        if data.groups.is_empty() {
            ui.text_in(
                Rect::new(area.x + t.px(10.0), area.y + t.px(6.0), area.w, t.px(30.0)),
                Face::Sans,
                t.pt(16.0),
                t.dim,
                if data.events.is_empty() {
                    "No events yet"
                } else {
                    "No problems are grouped yet."
                },
            );
            return None;
        }
        let zone = self.zone.clone();
        let activated = self.with_list(|s, list| {
            if list.selected.is_none() {
                list.select(0);
            }
            if std::mem::take(&mut s.want_focus) {
                ui.focus("problems");
            }
            ui.list(
                "problems",
                list,
                area,
                data.groups.len(),
                64.0,
                |ui, i, selected| group_row(ui, &data.groups[i], selected, &zone),
            )
            .activated
        });
        activated.map(|i| data.groups[i].bucket.id.clone())
    }

    fn details_page(&mut self, ui: &mut Ui, data: &Rc<Data>) -> Option<String> {
        let t = ui.theme();
        let Some(e) = data.event(self.detail.record) else {
            ui.dim("This event is no longer in the log.");
            return None;
        };
        let area = ui.area();
        let left = Rect::new(area.x, area.y, area.w * 11 / 20, area.h);
        let right = Rect::new(
            left.right() + t.px(10.0),
            area.y,
            area.w - left.w - t.px(10.0),
            area.h,
        );

        // Arrow keys stop on evidence, the group and the toggle only.
        if ui.is_focused("lines") {
            if let Some(p) = ui.press() {
                if !p.mods.ctrl && !p.mods.alt && !p.mods.shift {
                    match p.code {
                        keys::KEY_DOWN => {
                            ui.take_press();
                            self.move_selection(true);
                        }
                        keys::KEY_UP => {
                            ui.take_press();
                            self.move_selection(false);
                        }
                        _ => {}
                    }
                }
            }
        }

        if std::mem::take(&mut self.want_focus) {
            ui.focus("lines");
        }
        self.follow_selection();

        // The kernel log on the right.
        let head = Rect::new(right.x, right.y, right.w, t.px(30.0));
        ui.fill_round(right, t.px(8.0), lucina_app::theme::TERM_BG);
        let nlog = e.log.len();
        let log_title = if nlog == 0 {
            "Kernel log".to_owned()
        } else {
            format!("Kernel log  ·  {nlog} lines")
        };
        ui.text_in(
            Rect::new(head.x + t.px(12.0), head.y, head.w, head.h),
            Face::Bold,
            t.pt(14.0),
            t.accent,
            &log_title,
        );
        let log_r = Rect::new(
            right.x + 2,
            head.bottom(),
            right.w - 4,
            right.h - head.h - 2,
        );
        if nlog == 0 {
            ui.text_in(
                Rect::new(
                    log_r.x + t.px(12.0),
                    log_r.y + t.px(4.0),
                    log_r.w,
                    t.px(24.0),
                ),
                Face::Sans,
                t.pt(13.0),
                t.dim,
                "No kernel log lines were kept with this event.",
            );
        } else {
            if let Some(c) = self.detail.center.take() {
                let row = t.px(LOG_ROW);
                self.detail.log.scroll = (c as i32 * row - (log_r.h - row) / 2).max(0);
                self.detail.log.selected = Some(c);
            }
            let hl = self.detail.hl;
            let res = ui.list(
                "log",
                &mut self.detail.log,
                log_r,
                nlog,
                LOG_ROW,
                |ui, i, _| details::log_row(ui, e, i, hl),
            );
            if res.selection_changed || res.activated.is_some() {
                self.detail.hl = self.detail.log.selected;
            }
        }

        // The page on the left.
        let lines = &self.detail.lines;
        let mut todo = None;
        let res = ui.list_measured(
            "lines",
            &mut self.detail.list,
            left,
            lines.len(),
            |text, i, w| details::height(text, &lines[i], w, &t),
            |ui, i, selected| details::draw(ui, &lines[i], selected),
        );
        if let Some(i) = res.activated {
            todo = Some(i);
        }
        self.follow_selection();
        if self.detail.center.is_some() {
            ui.repaint();
        }
        match todo.map(|i| self.detail.lines[i].clone()) {
            Some(DLine::TechToggle(_)) => self.toggle_tech(),
            Some(DLine::Same { bucket, .. }) => return Some(bucket),
            _ => {}
        }
        None
    }
}

fn pretty_date(date: &str) -> String {
    let (wd, d, m) = crate::model::parts_of_date(date);
    format!("{wd} {d} {m}")
}

/// One event in a list: time, a level mark, its name and its summary. `head` draws a heading above it.
fn event_row(
    ui: &mut Ui,
    e: &Event,
    head: Option<&'static str>,
    with_date: bool,
    selected: bool,
    zone: &Zone,
) {
    let t = ui.theme();
    let row = ui.area();
    let mut y = row.y;
    if let Some(h) = head {
        ui.text_in(
            Rect::new(row.x + t.px(8.0), row.y + t.px(4.0), row.w, t.px(22.0)),
            Face::Bold,
            t.pt(13.0),
            t.accent,
            h,
        );
        y += t.px(26.0);
    }
    let h = row.bottom() - y;
    let x = row.x + t.px(10.0);
    let time = event_clock(zone, e);
    let when = match (with_date, e.time_ms) {
        (true, Some(ms)) => format!("{}  {time}", short_date(zone, ms)),
        _ => time,
    };
    let tw = t.px(if with_date { 132.0 } else { 62.0 });
    ui.text_in(
        Rect::new(x, y, tw, h),
        Face::Mono,
        t.pt(13.0),
        if selected { t.text } else { t.item },
        &when,
    );
    let c = event_colour(e, &t);
    let cx = x + tw + t.px(12.0);
    ui.fill_round(
        Rect::new(
            cx - t.px(6.0),
            y + h / 2 - t.px(6.0),
            t.px(12.0),
            t.px(12.0),
        ),
        t.px(6.0),
        c,
    );
    let name = title(e);
    let nw = t.px(230.0);
    ui.text_in(
        Rect::new(cx + t.px(14.0), y, nw, h),
        Face::Bold,
        t.pt(14.0),
        t.text,
        &name,
    );
    let sx = cx + t.px(14.0) + nw + t.px(8.0);
    ui.text_in(
        Rect::new(sx, y, row.right() - sx - t.px(10.0), h),
        Face::Sans,
        t.pt(13.0),
        if selected { t.item } else { t.dim },
        &e.summary,
    );
}

fn group_row(ui: &mut Ui, g: &lucina_evlog::bucket::Group, selected: bool, zone: &Zone) {
    let t = ui.theme();
    let row = ui.area();
    let x = row.x + t.px(12.0);
    let count = if g.count == 1 {
        "seen once".to_owned()
    } else {
        format!("seen {} times", g.count)
    };
    let w = pill_right(
        ui,
        row.right() - t.px(14.0),
        row.y + t.px(8.0),
        t.px(22.0),
        t.pt(12.0),
        Face::Bold,
        if g.count >= 2 { t.danger } else { BLUE },
        &count,
    );
    ui.text_in(
        Rect::new(x, row.y + t.px(4.0), row.w - w - t.px(40.0), t.px(28.0)),
        Face::Bold,
        t.pt(15.0),
        t.text,
        &g.bucket.label,
    );
    let first = g
        .first_ms
        .map_or("unknown".to_owned(), |ms| long_date(zone, ms));
    let last = g.last_ms.map_or("unknown".to_owned(), |ms| {
        format!("{} {}", long_date(zone, ms), crate::model::hhmm(zone, ms))
    });
    let builds = if g.builds.is_empty() {
        "build unknown".to_owned()
    } else {
        format!("builds: {}", g.builds.join(", "))
    };
    let line = format!(
        "first {first}  ·  last {last}  ·  {}  ·  {builds}",
        crate::model::boots_text(&g.boots)
    );
    ui.text_in(
        Rect::new(x, row.y + t.px(34.0), row.w - t.px(26.0), t.px(24.0)),
        Face::Sans,
        t.pt(13.0),
        if selected { t.item } else { t.dim },
        &line,
    );
}
