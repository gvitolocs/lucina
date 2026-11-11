//! The screen: Todoist's layout as web/src/views/Tasks.tsx has it at desk width. Sidebar, the open
//! list with quick add on top, and the task's details beside it. Everything drawn comes from what is
//! in memory ([`Store`]); the server only ever corrects it later.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use chrono::{Datelike, NaiveDate, Weekday};
use lucina_account::ui::SignInView;
use lucina_app::keys;
use lucina_app::{App, Face, Id, ListState, Rect, Rgb, TextEdit, Theme, Ui};
use secretary_tasks::{Command, DueView, Parsed, SyncStatus, TaskView, View, ViewQuery};

use crate::cache;
use crate::store::{self, Store, query};
use crate::worker::{Job, Msg};

const TOAST: Duration = Duration::from_secs(7);
const KEY_F5: u16 = 63;

/// Sends work to the server thread.
pub type Link = Box<dyn Fn(Job)>;

/// Whose tasks these are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// No device token yet: the sign-in screen, and nothing to show until the owner approves.
    NotSignedIn,
    /// Invented data, on the fake server, in this process.
    Sample,
    Account,
}

/// What the main column shows.
#[derive(Clone, Debug, PartialEq)]
pub enum Section {
    List,
    FiltersLabels,
    Search,
}

#[derive(Clone, Debug)]
pub enum Row {
    Head {
        title: String,
        overdue: bool,
    },
    Task {
        t: Box<TaskView>,
        depth: usize,
        group: String,
    },
    Named {
        label: String,
        count: u32,
        q: ViewQuery,
        color: String,
    },
}

#[derive(Clone, Debug)]
pub enum Nav {
    Head(String),
    Item {
        label: String,
        count: Option<u32>,
        target: Target,
        color: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    Query(ViewQuery),
    FiltersLabels,
    Search,
}

pub struct Detail {
    pub id: String,
    pub title: TextEdit,
    pub desc: TextEdit,
    pub labels: TextEdit,
    pub subtask: TextEdit,
    pub confirm_delete: bool,
    /// The form's starting values: Save sends what changed.
    from: (String, String, String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum PickFor {
    Task(String),
    Overdue,
}

pub struct DatePicker {
    pub target: PickFor,
    pub phrase: TextEdit,
    pub preview: Option<DueView>,
}

pub struct ProjectPicker {
    pub task: String,
    pub list: ListState,
}

pub struct Toast {
    pub text: String,
    pub until: Instant,
    pub undo: Option<(Command, Option<(String, TaskView)>)>,
}

pub struct Tasks {
    /// A task to open once the list that holds it has loaded (`LUCINA_TASKS_OPEN`, from the home's stat cards).
    open_on_load: Option<String>,
    pub store: Store,
    pub mode: Mode,
    link: Option<Link>,
    /// Starts the sample's server when asked for.
    start_sample: Option<Box<dyn FnMut() -> Link>>,
    /// Starts the account's server once there is a token.
    start_account: Option<Box<dyn FnMut() -> Option<Link>>>,
    begin_sign_in: Option<Box<dyn FnMut() -> Option<SignInView>>>,
    sign_in: Option<SignInView>,
    /// The lists already asked for ahead of time: the server allows 60 requests a minute.
    prefetched: HashSet<String>,
    pub q: ViewQuery,
    pub section: Section,
    pub shown: Option<View>,
    pub rows: Vec<Row>,
    pub nav: Vec<Nav>,
    pub list: ListState,
    pub side: ListState,
    pub add: TextEdit,
    pub parsed: Option<Parsed>,
    pub search: TextEdit,
    pub detail: Option<Detail>,
    pub picker: Option<DatePicker>,
    pub projects: Option<ProjectPicker>,
    pub toast: Option<Toast>,
    pub error: Option<String>,
    pub offline: Option<String>,
    pub syncing: bool,
    /// `g` was pressed: the next key picks a list.
    go: bool,
    want_focus: Option<&'static str>,
    save_cache: bool,
}

/// DejaVu's glyphs, no icon font.
const RING: &str = "○";
const DONE: &str = "●";
const REPEAT: &str = "↻";

pub fn priority_color(t: &Theme, p: u8) -> Rgb {
    match p {
        1 => t.danger,
        2 => Rgb::hex(0xe8912d),
        3 => Rgb::hex(0x4f7cf0),
        _ => t.dim,
    }
}

/// Todoist's colour names (`projectColor` in Tasks.tsx falls back the same way).
pub fn project_color(name: &str, t: &Theme) -> Rgb {
    let hex = match name {
        "berry_red" => 0xb8255f,
        "red" => 0xdb4035,
        "orange" => 0xff9933,
        "yellow" => 0xfad000,
        "olive_green" => 0xafb83b,
        "lime_green" => 0x7ecc49,
        "green" => 0x299438,
        "mint_green" => 0x6accbc,
        "teal" => 0x158fad,
        "sky_blue" => 0x14aaf5,
        "light_blue" => 0x96c3eb,
        "blue" => 0x4073ff,
        "grape" => 0x884dff,
        "violet" => 0xaf38eb,
        "lavender" => 0xeb96eb,
        "magenta" => 0xe05194,
        "salmon" => 0xff8d85,
        "charcoal" => 0x808080,
        "grey" => 0xb8b8b8,
        "taupe" => 0xccac93,
        _ => return t.dim,
    };
    Rgb::hex(hex)
}

fn parse_day(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()
}

fn rel_word(rel: i64) -> Option<&'static str> {
    match rel {
        0 => Some("Today"),
        1 => Some("Tomorrow"),
        -1 => Some("Yesterday"),
        _ => None,
    }
}

fn day_month(d: NaiveDate, today: NaiveDate) -> String {
    if d.year() == today.year() {
        d.format("%-d %b").to_string()
    } else {
        d.format("%-d %b %Y").to_string()
    }
}

/// "Today", "Tomorrow", "Monday", "27 Oct" (+ time): `dueLabel` in Tasks.tsx.
pub fn due_label(due: &DueView, today: NaiveDate) -> String {
    let d = parse_day(&due.date).unwrap_or(today);
    let mut s = match rel_word(due.rel_days) {
        Some(w) => w.to_owned(),
        None if due.rel_days > 1 && due.rel_days < 7 => d.format("%A").to_string(),
        None => day_month(d, today),
    };
    if let Some(t) = &due.time {
        s.push(' ');
        s.push_str(t);
    }
    s
}

fn due_color(t: &Theme, rel: i64) -> Rgb {
    match rel {
        i64::MIN..=-1 => t.danger,
        0 => t.ok,
        1 => Rgb::hex(0xe8912d),
        2..=6 => Rgb::hex(0xa98bf0),
        _ => t.dim,
    }
}

/// "22 Oct · Today · Thursday": `dateHeader` in Tasks.tsx.
fn date_header(date: NaiveDate, rel: i64, today: NaiveDate) -> String {
    let mut parts = vec![day_month(date, today)];
    if let Some(w) = rel_word(rel) {
        parts.push(w.to_owned());
    }
    parts.push(date.format("%A").to_string());
    parts.join(" · ")
}

/// The day of `target` strictly after `from`.
fn next_weekday(from: NaiveDate, target: Weekday) -> NaiveDate {
    let mut d = from.succ_opt().unwrap_or(from);
    while d.weekday() != target {
        d = d.succ_opt().unwrap_or(d);
    }
    d
}

fn relative(ms: u64, now: u64) -> String {
    let s = now.saturating_sub(ms) / 1000;
    if s < 45 {
        "just now".into()
    } else if s < 3600 {
        format!("{} min ago", (s / 60).max(1))
    } else if s < 86_400 {
        format!("{} h ago", s / 3600)
    } else {
        format!("{} d ago", s / 86_400)
    }
}

fn find<'a>(ts: &'a [TaskView], id: &str) -> Option<&'a TaskView> {
    ts.iter().find_map(|t| {
        if t.id == id {
            Some(t)
        } else {
            find(&t.children, id)
        }
    })
}

fn hit(ui: &mut Ui, id: impl Into<Id>, r: Rect) -> bool {
    ui.hit(id, r).clicked
}

impl Tasks {
    pub fn new(store: Store, mode: Mode, link: Option<Link>) -> Tasks {
        let mut app = Tasks {
            open_on_load: std::env::var("LUCINA_TASKS_OPEN").ok().filter(|v| !v.is_empty()),
            store,
            mode,
            link,
            start_sample: None,
            start_account: None,
            begin_sign_in: None,
            sign_in: None,
            prefetched: HashSet::new(),
            q: query("today", None),
            section: Section::List,
            shown: None,
            rows: Vec::new(),
            nav: Vec::new(),
            list: ListState::new(),
            side: ListState::new(),
            add: TextEdit::new().placeholder("Add a task: Call Marco tomorrow 9am #Work p1"),
            parsed: None,
            search: TextEdit::new().placeholder("Search tasks"),
            detail: None,
            picker: None,
            projects: None,
            toast: None,
            error: None,
            offline: None,
            syncing: false,
            go: false,
            want_focus: Some("rows"),
            save_cache: mode == Mode::Account,
        };
        app.rebuild();
        app.load_everything();
        app
    }

    pub fn with_starters(
        mut self,
        sample: Box<dyn FnMut() -> Link>,
        account: Box<dyn FnMut() -> Option<Link>>,
        sign_in: Box<dyn FnMut() -> Option<SignInView>>,
    ) -> Tasks {
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
        self.save_cache = false;
        self.sign_in = self.begin_sign_in.as_mut().and_then(|f| f());
        self.store.restore(store::Saved::default());
        self.detail = None;
        self.picker = None;
        self.projects = None;
        self.toast = None;
        self.rebuild();
    }

    /// A token arrived: the account's lists from now on.
    fn signed_in(&mut self) {
        let link = self.start_account.as_mut().and_then(|f| f());
        if link.is_some() {
            self.link = link;
            self.mode = Mode::Account;
            self.save_cache = true;
            self.sign_in = None;
            self.want_focus = Some("rows");
            self.load_everything();
        }
    }

    fn send(&self, job: Job) {
        if let Some(l) = &self.link {
            l(job);
        }
    }

    /// The list on screen, then what is one tap away.
    fn load_everything(&mut self) {
        if self.link.is_none() {
            return;
        }
        self.send(Job::Load(self.q.clone()));
        self.prefetch();
    }

    /// What is one tap away, once each, and a few projects: a handful of requests per start.
    fn prefetch(&mut self) {
        let mut queries = vec![
            query("inbox", None),
            query("upcoming", None),
            query("today", None),
        ];
        if let Some(v) = &self.shown {
            queries.extend(
                v.projects
                    .iter()
                    .filter(|p| !p.inbox)
                    .take(5)
                    .map(|p| query("project", Some(&p.id))),
            );
        }
        for q in queries {
            if !self.store.has_list(&q) && self.prefetched.insert(store::key(&q)) {
                self.send(Job::Prefetch(q));
            }
        }
    }

    /// Everything on screen follows the store; called when it changes, not per frame.
    pub fn rebuild(&mut self) {
        let selected = self.list.selected.and_then(|i| match self.rows.get(i) {
            Some(Row::Task { t, .. }) => Some((i, t.id.clone())),
            _ => None,
        });
        self.shown = self.store.shown(&self.q);
        self.rows = self.flatten();
        self.nav = self.build_nav();
        self.list.invalidate();
        self.side.invalidate();
        self.list.selected = match selected {
            Some((i, id)) => self
                .rows
                .iter()
                .position(|r| matches!(r, Row::Task { t, .. } if t.id == id))
                .or_else(|| self.nearest_task(i)),
            None => self.nearest_task(self.list.selected.unwrap_or(0)),
        };
        self.side.selected = self.nav.iter().position(|n| match n {
            Nav::Item { target, .. } => match target {
                Target::Query(q) => {
                    self.section == Section::List && store::key(q) == store::key(&self.q)
                }
                Target::FiltersLabels => self.section == Section::FiltersLabels,
                Target::Search => self.section == Section::Search,
            },
            _ => false,
        });
        if let Some(d) = &self.detail
            && self.task(&d.id).is_none()
        {
            self.detail = None;
        }
    }

    /// The row nearest `i` that can be selected.
    fn nearest_task(&self, i: usize) -> Option<usize> {
        let can = |r: &Row| !matches!(r, Row::Head { .. });
        let i = i.min(self.rows.len().saturating_sub(1));
        (i..self.rows.len())
            .find(|&k| can(&self.rows[k]))
            .or_else(|| (0..i).rev().find(|&k| can(&self.rows[k])))
    }

    fn flatten(&self) -> Vec<Row> {
        let Some(v) = &self.shown else {
            return Vec::new();
        };
        let mut out = Vec::new();
        if self.section == Section::FiltersLabels {
            if !v.filters.is_empty() {
                out.push(Row::Head {
                    title: "Filters".into(),
                    overdue: false,
                });
            }
            for f in &v.filters {
                out.push(Row::Named {
                    label: f.name.clone(),
                    count: f.count,
                    q: query("filter", Some(&f.id)),
                    color: f.color.clone(),
                });
            }
            if !v.labels.is_empty() {
                out.push(Row::Head {
                    title: "Labels".into(),
                    overdue: false,
                });
            }
            for l in &v.labels {
                out.push(Row::Named {
                    label: format!("@{}", l.name),
                    count: l.count,
                    q: query("label", Some(&l.name)),
                    color: l.color.clone(),
                });
            }
            return out;
        }
        let today = self.store.today();
        fn push(out: &mut Vec<Row>, t: &TaskView, depth: usize, group: &str) {
            let mut one = t.clone();
            let kids = std::mem::take(&mut one.children);
            out.push(Row::Task {
                t: Box::new(one),
                depth,
                group: group.to_owned(),
            });
            for k in &kids {
                push(out, k, depth + 1, group);
            }
        }
        // A list of one project doesn't repeat its name on every row.
        let one_project = matches!(self.q.kind.as_deref(), Some("project" | "inbox"));
        for g in &v.list.groups {
            let title = match g.kind.as_str() {
                "overdue" => "Overdue".to_owned(),
                "date" | "completed" => g
                    .date
                    .as_deref()
                    .and_then(parse_day)
                    .map(|d| date_header(d, g.rel_days.unwrap_or(99), today))
                    .unwrap_or_default(),
                _ => g.title.clone(),
            };
            // A day with nothing on it says nothing; a project's section stays, as in Todoist.
            let show = match g.kind.as_str() {
                "none" => false,
                "section" => true,
                _ => !g.tasks.is_empty(),
            };
            if show {
                out.push(Row::Head {
                    title,
                    overdue: g.kind == "overdue",
                });
            }
            for t in &g.tasks {
                push(&mut out, t, 0, &g.key);
            }
        }
        if one_project {
            for r in &mut out {
                if let Row::Task { t, .. } = r {
                    t.project_name.clear();
                }
            }
        }
        out
    }

    fn build_nav(&self) -> Vec<Nav> {
        let v = self.shown.as_ref();
        let item = |label: &str, count: Option<u32>, target: Target| Nav::Item {
            label: label.into(),
            count: count.filter(|&n| n > 0),
            target,
            color: None,
        };
        let mut out = vec![
            item("Search", None, Target::Search),
            item(
                "Inbox",
                v.map(|v| v.counts.inbox),
                Target::Query(query("inbox", None)),
            ),
            item(
                "Today",
                v.map(|v| v.counts.today),
                Target::Query(query("today", None)),
            ),
            item(
                "Upcoming",
                v.map(|v| v.counts.upcoming),
                Target::Query(query("upcoming", None)),
            ),
            item("Filters & Labels", None, Target::FiltersLabels),
            item("Completed", None, Target::Query(query("completed", None))),
        ];
        if let Some(v) = v {
            let projects: Vec<_> = v.projects.iter().filter(|p| !p.inbox).collect();
            if !projects.is_empty() {
                out.push(Nav::Head("My Projects".into()));
            }
            for p in projects {
                out.push(Nav::Item {
                    label: p.name.clone(),
                    count: Some(p.count).filter(|&n| n > 0),
                    target: Target::Query(query("project", Some(&p.id))),
                    color: Some(p.color.clone()),
                });
            }
        }
        out
    }

    pub fn task(&self, id: &str) -> Option<&TaskView> {
        let v = self.shown.as_ref()?;
        v.list.groups.iter().find_map(|g| find(&g.tasks, id))
    }

    /// Show a list: what is known about it now, the server's answer when it comes.
    pub fn open_list(&mut self, q: ViewQuery) {
        self.section = Section::List;
        self.q = q.clone();
        self.detail = None;
        self.list.scroll = 0;
        self.list.selected = None;
        self.rebuild();
        self.send(Job::Load(q));
    }

    fn open_target(&mut self, t: Target) {
        match t {
            Target::Query(q) => self.open_list(q),
            Target::FiltersLabels => {
                self.section = Section::FiltersLabels;
                self.detail = None;
                self.list.scroll = 0;
                self.list.selected = None;
                self.rebuild();
            }
            Target::Search => {
                self.section = Section::Search;
                self.detail = None;
                self.list.scroll = 0;
                self.list.selected = None;
                self.want_focus = Some("search");
                self.run_search();
            }
        }
    }

    fn run_search(&mut self) {
        self.q = ViewQuery {
            kind: Some("search".into()),
            query: Some(self.search.text.clone()),
            ..ViewQuery::default()
        };
        self.rebuild();
        self.send(Job::Load(self.q.clone()));
    }

    /// The command shows at once, and goes to the server behind whatever waits.
    pub fn run(&mut self, command: Command, restore: Option<(String, TaskView)>) {
        self.error = None;
        let q = self.store.enqueue(command, restore);
        self.rebuild();
        self.send(Job::Command(q, self.q.clone()));
    }

    fn toast(&mut self, text: &str, undo: Option<(Command, Option<(String, TaskView)>)>) {
        self.toast = Some(Toast {
            text: text.to_owned(),
            until: Instant::now() + TOAST,
            undo,
        });
    }

    fn group_of(&self, id: &str) -> Option<String> {
        self.rows.iter().find_map(|r| match r {
            Row::Task { t, group, .. } if t.id == id => Some(group.clone()),
            _ => None,
        })
    }

    /// Tick a task's box: done, or back from Completed.
    pub fn toggle(&mut self, id: &str) {
        let Some(t) = self.task(id).cloned() else {
            return;
        };
        if t.checked {
            self.run(Command::Uncomplete { task_id: t.id }, None);
            return;
        }
        let group = self.group_of(id).unwrap_or_default();
        let task_id = t.id.clone();
        self.run(
            Command::Complete {
                task_id: task_id.clone(),
            },
            None,
        );
        self.toast(
            "1 task completed",
            Some((Command::Uncomplete { task_id }, Some((group, t)))),
        );
    }

    pub fn delete(&mut self, id: &str) {
        let Some(t) = self.task(id).cloned() else {
            return;
        };
        let group = self.group_of(id).unwrap_or_default();
        let task_id = t.id.clone();
        self.detail = None;
        self.run(
            Command::Delete {
                task_id: task_id.clone(),
            },
            None,
        );
        self.toast(
            "Task deleted",
            Some((Command::UndoDelete { task_id }, Some((group, t)))),
        );
    }

    pub fn undo(&mut self) {
        if let Some(Toast {
            undo: Some((c, restore)),
            ..
        }) = self.toast.take()
        {
            self.run(c, restore);
        }
    }

    pub fn quick_add(&mut self) {
        let text = self.add.text.trim().to_owned();
        if text.is_empty() {
            return;
        }
        // The open list is the default, as InlineAdd in Tasks.tsx: the Today list means due today,
        // a project's list means that project.
        let kind = self.q.kind.as_deref().unwrap_or("today");
        let (project_id, default_due) = match kind {
            "project" => (self.q.id.clone(), None),
            "today" => (None, Some("today".to_owned())),
            _ => (None, None),
        };
        self.add.clear();
        self.parsed = None;
        self.run(
            Command::QuickAdd {
                text,
                lang: None,
                project_id,
                section_id: None,
                default_due,
            },
            None,
        );
        self.toast("Task added", None);
    }

    pub fn open_detail(&mut self, id: &str) {
        let Some(t) = self.task(id) else {
            return;
        };
        let labels = t.labels.join(" ");
        let mut title = TextEdit::with_text(&t.content);
        title.placeholder = "Task name".into();
        let mut desc = TextEdit::multiline().placeholder("Description");
        desc.set(&t.description);
        self.detail = Some(Detail {
            id: t.id.clone(),
            from: (t.content.clone(), t.description.clone(), labels.clone()),
            title,
            desc,
            labels: TextEdit::with_text(&labels).placeholder("Labels, separated by spaces"),
            subtask: TextEdit::new().placeholder("Add a sub-task"),
            confirm_delete: false,
        });
    }

    /// Save the details: only what changed goes to the server.
    pub fn save_detail(&mut self) {
        let Some(d) = self.detail.take() else {
            return;
        };
        let (title, desc, labels) = (
            d.title.text.trim().to_owned(),
            d.desc.text.trim().to_owned(),
            d.labels.text.trim().to_owned(),
        );
        if title.is_empty() {
            self.error = Some("A task needs a name.".into());
            self.detail = Some(d);
            return;
        }
        let content = (title != d.from.0).then_some(title);
        let description = (desc != d.from.1).then_some(desc);
        let labels = (labels != d.from.2).then(|| {
            labels
                .split_whitespace()
                .map(|l| l.trim_start_matches('@').to_owned())
                .collect::<Vec<_>>()
        });
        if content.is_some() || description.is_some() || labels.is_some() {
            self.run(
                Command::Update {
                    task_id: d.id,
                    content,
                    description,
                    priority: None,
                    labels,
                    due_string: None,
                    due_date: None,
                    lang: None,
                    clear_due: false,
                },
                None,
            );
        }
    }

    pub fn set_priority(&mut self, id: &str, p: u8) {
        self.run(
            Command::Update {
                task_id: id.to_owned(),
                content: None,
                description: None,
                priority: Some(p),
                labels: None,
                due_string: None,
                due_date: None,
                lang: None,
                clear_due: false,
            },
            None,
        );
    }

    fn pick_date(&mut self, target: PickFor) {
        self.picker = Some(DatePicker {
            target,
            phrase: TextEdit::new().placeholder("Type a date: fri 3pm, every monday, domani"),
            preview: None,
        });
        self.want_focus = Some("date-phrase");
    }

    /// `date`: `YYYY-MM-DD` or a phrase; None clears the date.
    pub fn apply_date(&mut self, date: Option<String>) {
        let Some(p) = self.picker.take() else {
            return;
        };
        match (p.target, date) {
            (PickFor::Overdue, d) => self.run(Command::RescheduleOverdue { date: d }, None),
            (PickFor::Task(task_id), None) => self.run(
                Command::Update {
                    task_id,
                    content: None,
                    description: None,
                    priority: None,
                    labels: None,
                    due_string: None,
                    due_date: None,
                    lang: None,
                    clear_due: true,
                },
                None,
            ),
            (PickFor::Task(task_id), Some(d)) => {
                // A plain day keeps the time and the recurrence; a phrase with its own replaces them.
                let parsed = store::parse_phrase(&d, self.store.today());
                let plain = parsed
                    .as_ref()
                    .is_none_or(|x| x.time.is_none() && !x.is_recurring);
                if plain {
                    self.run(Command::Reschedule { task_id, date: d }, None);
                } else {
                    self.run(
                        Command::Update {
                            task_id,
                            content: None,
                            description: None,
                            priority: None,
                            labels: None,
                            due_string: Some(d),
                            due_date: None,
                            lang: None,
                            clear_due: false,
                        },
                        None,
                    );
                }
            }
        }
    }

    fn selected_task(&self) -> Option<&TaskView> {
        match self.rows.get(self.list.selected?)? {
            Row::Task { t, .. } => Some(t),
            _ => None,
        }
    }

    fn selected_is_waiting(&self) -> bool {
        self.selected_task()
            .is_some_and(|t| t.project_id.is_empty() && t.pending)
    }

    // ---- Drawing ----

    fn draw_not_signed_in(&mut self, ui: &mut Ui) {
        let t = ui.theme();
        let foot = ui.cut_bottom(t.px(60.0));
        const WHY: &str = "Your tasks live in your Secretary account. Approve this laptop on your phone and your \
                           lists appear here; changes go back to Todoist as they do from the web app.";
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
            self.load_everything();
        }
    }

    fn draw_sidebar(&mut self, ui: &mut Ui, r: Rect) {
        let t = ui.theme();
        ui.fill(r, t.bar);
        let mut go: Option<Target> = None;
        let mut sync = false;
        ui.within(r.inset(t.px(10.0)), |ui| {
            let foot = ui.cut_bottom(t.px(66.0));
            let nav = &self.nav;
            let area = ui.area();
            let measure = |_: &mut lucina_app::gfx::Text, i: usize, _w: i32| match nav[i] {
                Nav::Head(_) => t.px(38.0),
                Nav::Item { .. } => t.px(30.0),
            };
            let res = ui.list_measured(
                "nav",
                &mut self.side,
                area,
                nav.len(),
                measure,
                |ui, i, selected| {
                    let a = ui.area();
                    match &nav[i] {
                        Nav::Head(s) => {
                            let tr = Rect::new(
                                a.x + t.px(8.0),
                                a.y + t.px(12.0),
                                a.w - t.px(8.0),
                                a.h - t.px(12.0),
                            );
                            ui.text_in(tr, Face::Bold, t.pt(13.0), t.dim, s);
                        }
                        Nav::Item {
                            label,
                            count,
                            color,
                            ..
                        } => {
                            let mut x = a.x + t.px(8.0);
                            if let Some(c) = color {
                                let s = t.px(8.0);
                                ui.fill_round(
                                    Rect::new(x, a.y + (a.h - s) / 2, s, s),
                                    s / 2,
                                    project_color(c, &t),
                                );
                                x += t.px(16.0);
                            }
                            let c = if selected { t.text } else { t.item };
                            let num = count.map(|n| n.to_string()).unwrap_or_default();
                            let nw = ui.measure(Face::Sans, t.pt(13.0), &num);
                            ui.text_in(
                                Rect::new(x, a.y, a.right() - x - nw - t.px(14.0), a.h),
                                Face::Sans,
                                t.pt(15.0),
                                c,
                                label,
                            );
                            ui.text_in(
                                Rect::new(a.right() - nw - t.px(8.0), a.y, nw, a.h),
                                Face::Sans,
                                t.pt(13.0),
                                t.dim,
                                &num,
                            );
                        }
                    }
                },
            );
            if let Some(i) = res.activated
                && let Nav::Item { target, .. } = &nav[i]
            {
                go = Some(target.clone());
            }
            ui.within(foot, |ui| {
                if let Some(v) = &self.shown {
                    let now = (self.store.now)();
                    let line = sync_line(v, now, self.syncing);
                    ui.wrapped(Face::Sans, t.pt(12.0), t.dim, &line);
                    if v.sync.connected && v.sync.status != SyncStatus::Reconnect {
                        let label = if self.syncing {
                            "Syncing…"
                        } else {
                            "Sync now"
                        };
                        ui.row(26.0, |ui| {
                            if ui.button_id("sync", label) {
                                sync = true;
                            }
                        });
                    }
                }
            });
        });
        if let Some(target) = go {
            self.open_target(target);
        }
        if sync && !self.syncing {
            self.syncing = true;
            self.send(Job::Sync);
        }
    }

    fn draw_title(&mut self, ui: &mut Ui) {
        let t = ui.theme();
        let r = ui.cut_top(t.px(36.0));
        let title = match self.section {
            Section::FiltersLabels => "Filters & Labels".to_owned(),
            Section::Search => "Search".to_owned(),
            Section::List => self.shown.as_ref().map_or(String::new(), |v| {
                if v.list.title.is_empty() {
                    list_name(&self.q)
                } else {
                    v.list.title.clone()
                }
            }),
        };
        let tw = ui.measure(Face::Bold, t.pt(22.0), &title).min(r.w / 2);
        ui.text_in(
            Rect::new(r.x, r.y, tw, r.h),
            Face::Bold,
            t.pt(22.0),
            t.text,
            &title,
        );
        let mut x = r.x + tw + t.px(12.0);
        if let Some(v) = &self.shown
            && self.section == Section::List
        {
            let n = match v.list.count {
                1 => "1 task".to_owned(),
                n => format!("{n} tasks"),
            };
            let nw = ui.measure(Face::Sans, t.pt(13.0), &n);
            ui.text_in(
                Rect::new(x, r.y + t.px(3.0), nw, r.h),
                Face::Sans,
                t.pt(13.0),
                t.dim,
                &n,
            );
            x += nw + t.px(12.0);
            if v.goal.daily_goal > 0 {
                let g = format!(
                    "· {} of {} done today",
                    v.goal.completed_today, v.goal.daily_goal
                );
                let c = if v.goal.reached { t.ok } else { t.dim };
                let gw = ui.measure(Face::Sans, t.pt(13.0), &g);
                ui.text_in(
                    Rect::new(x, r.y + t.px(3.0), gw, r.h),
                    Face::Sans,
                    t.pt(13.0),
                    c,
                    &g,
                );
            }
        }
        // On the right: whose data, and whether it has reached the server.
        let (note, color) = if self.mode == Mode::Sample {
            ("Sample data, not your account".to_owned(), t.accent)
        } else if let Some(why) = &self.offline {
            let n = self.store.pending.len();
            if n > 0 {
                (format!("Offline · {n} waiting to be sent"), t.danger)
            } else {
                (format!("Offline: {why}"), t.danger)
            }
        } else if !self.store.pending.is_empty() {
            ("Sending…".to_owned(), t.dim)
        } else {
            (String::new(), t.dim)
        };
        let nw = ui.measure(Face::Sans, t.pt(13.0), &note);
        ui.text_in(
            Rect::new(r.right() - nw, r.y + t.px(3.0), nw, r.h),
            Face::Sans,
            t.pt(13.0),
            color,
            &note,
        );
    }

    fn draw_quick_add(&mut self, ui: &mut Ui) {
        let t = ui.theme();
        let res = ui.text_field("quick-add", &mut self.add);
        if res.changed {
            self.parsed = self.store.parse_quick(&self.add.text);
        }
        if res.submitted {
            self.quick_add();
        }
        let r = ui.cut_top(t.px(24.0));
        let today = self.store.today();
        match &self.parsed {
            Some(p) => {
                let mut x = r.x;
                let chip = |ui: &mut Ui, x: &mut i32, s: &str, c: Rgb| {
                    let w = ui.measure(Face::Sans, t.pt(12.0), s) + t.px(14.0);
                    let h = t.px(18.0);
                    let y = r.y + (r.h - h) / 2;
                    ui.fill_round(Rect::new(*x, y, w, h), h / 2, t.panel_hi);
                    ui.text_in(
                        Rect::new(*x + t.px(7.0), y, w, h),
                        Face::Sans,
                        t.pt(12.0),
                        c,
                        s,
                    );
                    *x += w + t.px(6.0);
                };
                if let Some(s) = &p.due_string
                    && let Some(d) = store::parse_phrase(s, today)
                {
                    let mut label = due_label(&d, today);
                    if d.is_recurring {
                        label = format!("{REPEAT} {label}");
                    }
                    chip(ui, &mut x, &label, due_color(&t, d.rel_days));
                }
                if let Some(n) = &p.project {
                    chip(ui, &mut x, &format!("#{n}"), t.item);
                }
                if let Some(n) = &p.section {
                    chip(ui, &mut x, &format!("/{n}"), t.item);
                }
                for l in &p.labels {
                    chip(ui, &mut x, &format!("@{l}"), t.item);
                }
                if let Some(n) = p.priority {
                    chip(ui, &mut x, &format!("P{n}"), priority_color(&t, n));
                }
            }
            None => {
                ui.text_in(
                    r,
                    Face::Sans,
                    t.pt(12.0),
                    t.dim,
                    "q add · / search · Space done · d date · Del delete · 1–4 priority · g then t, i, u go to a list",
                );
            }
        }
    }

    fn draw_rows(&mut self, ui: &mut Ui) {
        let t = ui.theme();
        let today = self.store.today();
        let area = ui.area();
        let loaded = self.store.has_list(&self.q) || self.section == Section::FiltersLabels;
        if self.rows.is_empty() {
            // The empty list keeps its place in the focus, so the keys still work when rows arrive.
            let mut none = |_: &mut Ui, _: usize, _: bool| {};
            ui.list("rows", &mut self.list, area, 0, 30.0, &mut none);
            let text = if !loaded && self.link.is_some() {
                ("Loading…", "")
            } else {
                match self.q.kind.as_deref() {
                    _ if self.section == Section::Search && self.search.text.trim().is_empty() => {
                        ("Type to search your tasks", "")
                    }
                    _ if self.section == Section::FiltersLabels => ("No filters or labels yet", ""),
                    Some("today") => (
                        "You're all done for today",
                        "Enjoy the rest of your day. New tasks for today show up here.",
                    ),
                    Some("inbox") => ("Your inbox is clear", ""),
                    Some("completed") => ("Nothing completed in the last 7 days.", ""),
                    _ => ("No tasks here.", ""),
                }
            };
            let y = area.y + area.h / 3;
            let w = ui.measure(Face::Bold, t.pt(17.0), text.0);
            let h = t.px(30.0);
            ui.text_in(
                Rect::new(area.x + (area.w - w) / 2, y, w, h),
                Face::Bold,
                t.pt(17.0),
                t.item,
                text.0,
            );
            let w2 = ui.measure(Face::Sans, t.pt(14.0), text.1).min(area.w);
            ui.text_in(
                Rect::new(area.x + (area.w - w2) / 2, y + h, w2, h),
                Face::Sans,
                t.pt(14.0),
                t.dim,
                text.1,
            );
            return;
        }
        let rows = &self.rows;
        let mut toggled: Option<String> = None;
        let mut reschedule = false;
        let before = self.list.selected;
        let measure = |_: &mut lucina_app::gfx::Text, i: usize, _w: i32| match &rows[i] {
            Row::Head { .. } => t.px(38.0),
            Row::Named { .. } => t.px(32.0),
            Row::Task { t: x, .. } => {
                let mut h = 34.0;
                if !x.description.is_empty() {
                    h += 16.0;
                }
                if has_meta(x) {
                    h += 18.0;
                }
                t.px(h)
            }
        };
        let res = ui.list_measured(
            "rows",
            &mut self.list,
            area,
            rows.len(),
            measure,
            |ui, i, selected| {
                let a = ui.area();
                match &rows[i] {
                    Row::Head { title, overdue } => {
                        let h = t.px(26.0);
                        let tr = Rect::new(a.x + t.px(4.0), a.bottom() - h, a.w - t.px(120.0), h);
                        let c = if *overdue { t.danger } else { t.item };
                        ui.text_in(tr, Face::Bold, t.pt(14.0), c, title);
                        ui.fill(Rect::new(a.x, a.bottom() - 1, a.w, 1), t.panel_hi);
                        if *overdue {
                            let bw = t.px(112.0);
                            let b =
                                Rect::new(a.right() - bw, a.bottom() - t.px(25.0), bw, t.px(23.0));
                            if ui.button_at(b, "reschedule-overdue", "Reschedule") {
                                reschedule = true;
                            }
                        }
                    }
                    Row::Named {
                        label,
                        count,
                        color,
                        ..
                    } => {
                        let s = t.px(10.0);
                        ui.fill_round(
                            Rect::new(a.x + t.px(6.0), a.y + (a.h - s) / 2, s, s),
                            s / 2,
                            project_color(color, &t),
                        );
                        let tr = Rect::new(a.x + t.px(24.0), a.y, a.w - t.px(70.0), a.h);
                        ui.text_in(
                            tr,
                            Face::Sans,
                            t.pt(15.0),
                            if selected { t.text } else { t.item },
                            label,
                        );
                        let n = count.to_string();
                        let w = ui.measure(Face::Sans, t.pt(13.0), &n);
                        ui.text_in(
                            Rect::new(a.right() - w - t.px(8.0), a.y, w, a.h),
                            Face::Sans,
                            t.pt(13.0),
                            t.dim,
                            &n,
                        );
                    }
                    Row::Task { t: x, depth, .. } => {
                        let left = a.x + t.px(4.0) + *depth as i32 * t.px(24.0);
                        // The round box, in the priority's colour.
                        let bs = t.px(30.0);
                        let b = Rect::new(left, a.y + t.px(2.0), bs, bs);
                        if hit(ui, ("tick", i), b) {
                            toggled = Some(x.id.clone());
                        }
                        let glyph = if x.checked { DONE } else { RING };
                        ui.text_in(
                            Rect::new(b.x + t.px(5.0), b.y, bs, bs),
                            Face::Sans,
                            t.pt(18.0),
                            priority_color(&t, x.priority),
                            glyph,
                        );
                        let tx = left + bs + t.px(4.0);
                        let tw = a.right() - tx - t.px(6.0);
                        let mut y = a.y + t.px(4.0);
                        let c = if x.checked {
                            t.dim
                        } else if selected {
                            t.text
                        } else {
                            Rgb::hex(0xeef0fb)
                        };
                        ui.text_in(
                            Rect::new(tx, y, tw, t.px(26.0)),
                            Face::Sans,
                            t.pt(15.0),
                            c,
                            &x.content,
                        );
                        y += t.px(26.0);
                        if !x.description.is_empty() {
                            let first = x.description.lines().next().unwrap_or("");
                            ui.text_in(
                                Rect::new(tx, y, tw, t.px(16.0)),
                                Face::Sans,
                                t.pt(12.0),
                                t.dim,
                                first,
                            );
                            y += t.px(16.0);
                        }
                        if has_meta(x) {
                            draw_meta(ui, &t, x, Rect::new(tx, y, tw, t.px(16.0)), today);
                        }
                        ui.fill(Rect::new(tx, a.bottom() - 1, a.right() - tx, 1), t.panel);
                    }
                }
            },
        );
        let mut open: Option<String> = None;
        let mut go: Option<ViewQuery> = None;
        if let Some(i) = res.activated {
            match &rows[i] {
                Row::Task { t, .. } => open = Some(t.id.clone()),
                Row::Named { q, .. } => go = Some(q.clone()),
                Row::Head { overdue: true, .. } => reschedule = true,
                _ => {}
            }
        }
        // The arrows walk over the group titles.
        if res.selection_changed
            && let Some(s) = self.list.selected
            && matches!(rows.get(s), Some(Row::Head { .. }))
        {
            let down = before.is_none_or(|b| s >= b);
            let next = if down && s + 1 < rows.len() {
                s + 1
            } else if s > 0 {
                s - 1
            } else {
                s + 1
            };
            self.list.select(next.min(rows.len() - 1));
        }
        if let Some(id) = toggled {
            self.toggle(&id);
            ui.focus("rows");
        }
        if reschedule {
            self.pick_date(PickFor::Overdue);
        }
        if let Some(id) = open {
            self.open_detail(&id);
        }
        if let Some(q) = go {
            self.open_list(q);
        }
    }

    fn draw_detail(&mut self, ui: &mut Ui, r: Rect) {
        let t = ui.theme();
        ui.fill(r, t.bar);
        let today = self.store.today();
        let Some(id) = self.detail.as_ref().map(|d| d.id.clone()) else {
            return;
        };
        let Some(task) = self.task(&id).cloned() else {
            return;
        };
        let (mut close, mut save, mut del, mut pick, mut project) =
            (false, false, false, false, false);
        let mut prio = None;
        let mut sub = None;
        let mut tick_sub = None;
        let Some(d) = &mut self.detail else {
            return;
        };
        ui.within(r.inset(t.px(14.0)), |ui| {
            ui.dim(&format!(
                "{}{}",
                task.project_name,
                task.section_name
                    .as_deref()
                    .map(|s| format!(" / {s}"))
                    .unwrap_or_default()
            ));
            if ui.text_field("d-title", &mut d.title).submitted {
                save = true;
            }
            ui.space(6.0);
            ui.text_area("d-desc", &mut d.desc, 4);
            ui.space(8.0);
            ui.row(30.0, |ui| {
                let label = task.due.as_ref().map_or("No date".to_owned(), |x| {
                    let mut s = due_label(x, today);
                    if x.is_recurring {
                        s = format!("{REPEAT} {s}");
                    }
                    s
                });
                ui.label("Due");
                if ui.button_id("d-due", &label) {
                    pick = true;
                }
            });
            ui.space(4.0);
            ui.row(30.0, |ui| {
                ui.label("Priority");
                for p in 1..=4u8 {
                    let s = if task.priority == p {
                        format!("● P{p}")
                    } else {
                        format!("P{p}")
                    };
                    if ui.button_id(("d-prio", p as usize), &s) {
                        prio = Some(p);
                    }
                }
            });
            ui.space(4.0);
            ui.row(30.0, |ui| {
                ui.label("Project");
                if ui.button_id("d-project", &task.project_name) {
                    project = true;
                }
            });
            ui.space(6.0);
            if ui.text_field("d-labels", &mut d.labels).submitted {
                save = true;
            }
            ui.space(8.0);
            if !task.children.is_empty() || task.subtasks > 0 {
                ui.heading("Sub-tasks");
                for (i, c) in task.children.iter().enumerate() {
                    ui.row(26.0, |ui| {
                        let mut on = c.checked;
                        let a = ui.area();
                        if ui.button_at(
                            Rect::new(a.x, a.y, t.px(26.0), t.px(24.0)),
                            ("d-sub", i),
                            RING,
                        ) {
                            on = true;
                            tick_sub = Some(c.id.clone());
                        }
                        let _ = on;
                        ui.cut_left(t.px(32.0));
                        ui.label(&c.content);
                    });
                }
            }
            let sr = ui.text_field("d-subtask", &mut d.subtask);
            if sr.submitted && !d.subtask.text.trim().is_empty() {
                sub = Some(d.subtask.text.trim().to_owned());
                d.subtask.clear();
            }
            ui.space(10.0);
            ui.row(34.0, |ui| {
                if ui.primary_button("Save") {
                    save = true;
                }
                if ui.button("Close") {
                    close = true;
                }
                if d.confirm_delete {
                    if ui.button_id("d-delete-sure", "Really delete") {
                        del = true;
                    }
                } else if ui.button("Delete") {
                    d.confirm_delete = true;
                }
            });
        });
        if let Some(p) = prio {
            self.set_priority(&id, p);
        }
        if let Some(text) = sub {
            self.run(
                Command::Add {
                    content: text,
                    description: None,
                    project_id: None,
                    section_id: None,
                    parent_id: Some(id.clone()),
                    due_string: None,
                    due_date: None,
                    lang: None,
                    priority: None,
                    labels: None,
                },
                None,
            );
        }
        if let Some(c) = tick_sub {
            self.run(Command::Complete { task_id: c }, None);
        }
        if pick {
            self.pick_date(PickFor::Task(id.clone()));
        }
        if project {
            let at = self
                .shown
                .as_ref()
                .and_then(|v| v.projects.iter().position(|p| p.id == task.project_id));
            let mut list = ListState::new();
            list.selected = at;
            self.projects = Some(ProjectPicker {
                task: id.clone(),
                list,
            });
            self.want_focus = Some("project-list");
        }
        if save {
            self.save_detail();
            ui.focus("rows");
        }
        if close {
            self.detail = None;
            ui.focus("rows");
        }
        if del {
            self.delete(&id);
            ui.focus("rows");
        }
    }

    fn draw_date_picker(&mut self, ui: &mut Ui) {
        let today = self.store.today();
        let weekend = {
            let mut d = today;
            while d.weekday() != Weekday::Sat {
                d = d.succ_opt().unwrap_or(d);
            }
            d
        };
        let options = [
            ("Today", today),
            ("Tomorrow", today.succ_opt().unwrap_or(today)),
            ("This weekend", weekend),
            ("Next week", next_weekday(today, Weekday::Mon)),
        ];
        let mut chosen: Option<Option<String>> = None;
        let Some(p) = &mut self.picker else {
            return;
        };
        let overdue = p.target == PickFor::Overdue;
        ui.modal(460.0, 356.0, |ui| {
            ui.heading(if overdue {
                "Reschedule overdue tasks"
            } else {
                "Due date"
            });
            for (name, date) in options {
                ui.row(34.0, |ui| {
                    if ui.button_id(("date", date.num_days_from_ce() as usize), name) {
                        chosen = Some(Some(date.format("%Y-%m-%d").to_string()));
                    }
                    ui.dim(&date.format("%a %-d %b").to_string());
                });
            }
            if !overdue {
                ui.row(34.0, |ui| {
                    if ui.button("No date") {
                        chosen = Some(None);
                    }
                });
            }
            ui.space(4.0);
            ui.dim("Or type: fri 3pm, every monday, domani");
            let r = ui.text_field("date-phrase", &mut p.phrase);
            if r.changed {
                p.preview = store::parse_phrase(&p.phrase.text, today);
            }
            if r.submitted && p.preview.is_some() {
                chosen = Some(Some(p.phrase.text.trim().to_owned()));
            }
            match &p.preview {
                Some(d) => {
                    let mut s = due_label(d, today);
                    if d.is_recurring {
                        s = format!("{REPEAT} {s} (repeats)");
                    }
                    ui.label(&s);
                }
                None if !p.phrase.text.trim().is_empty() => ui.dim("Not understood yet"),
                None => {}
            }
        });
        if ui.key(keys::KEY_ESC) {
            self.picker = None;
            ui.focus("rows");
        }
        if let Some(c) = chosen {
            self.apply_date(c);
            ui.focus("rows");
        }
    }

    fn draw_project_picker(&mut self, ui: &mut Ui) {
        let t = ui.theme();
        let Some(pp) = &mut self.projects else {
            return;
        };
        let Some(v) = &self.shown else {
            return;
        };
        let projects = &v.projects;
        let mut go = None;
        ui.modal(380.0, 340.0, |ui| {
            ui.heading("Move to project");
            let r = ui.area();
            let res = ui.list(
                "project-list",
                &mut pp.list,
                r,
                projects.len(),
                30.0,
                |ui, i, selected| {
                    let a = ui.area();
                    let s = t.px(10.0);
                    ui.fill_round(
                        Rect::new(a.x + t.px(6.0), a.y + (a.h - s) / 2, s, s),
                        s / 2,
                        project_color(&projects[i].color, &t),
                    );
                    ui.text_in(
                        Rect::new(a.x + t.px(24.0), a.y, a.w - t.px(30.0), a.h),
                        Face::Sans,
                        t.pt(15.0),
                        if selected { t.text } else { t.item },
                        &projects[i].name,
                    );
                },
            );
            if let Some(i) = res.activated {
                go = Some(projects[i].id.clone());
            }
        });
        let task = pp.task.clone();
        if ui.key(keys::KEY_ESC) {
            self.projects = None;
            ui.focus("rows");
        }
        if let Some(project) = go {
            self.projects = None;
            self.run(
                Command::Move {
                    task_id: task,
                    project_id: Some(project),
                    section_id: None,
                    parent_id: None,
                },
                None,
            );
            ui.focus("rows");
        }
    }

    fn draw_toast(&mut self, ui: &mut Ui, r: Rect) {
        let Some(toast) = &self.toast else {
            return;
        };
        let now = Instant::now();
        if now >= toast.until {
            self.toast = None;
            ui.repaint();
            return;
        }
        ui.wake_after(toast.until - now);
        let t = ui.theme();
        let tw = ui.measure(Face::Sans, t.pt(14.0), &toast.text);
        let has_undo = toast.undo.is_some();
        let bw = if has_undo { t.px(70.0) } else { 0 };
        let w = tw + bw + t.px(34.0);
        let h = t.px(38.0);
        let box_r = Rect::new(r.x + (r.w - w) / 2, r.bottom() - h - t.px(12.0), w, h);
        ui.fill_round(box_r, t.px(8.0), t.selection);
        ui.text_in(
            Rect::new(box_r.x + t.px(14.0), box_r.y, tw, h),
            Face::Sans,
            t.pt(14.0),
            t.text,
            &toast.text,
        );
        if has_undo
            && ui.button_at(
                Rect::new(
                    box_r.right() - bw - t.px(6.0),
                    box_r.y + t.px(5.0),
                    bw,
                    h - t.px(10.0),
                ),
                "undo",
                "Undo",
            )
        {
            self.undo();
            ui.focus("rows");
        }
    }

    fn keys(&mut self, ui: &mut Ui) {
        let Some(p) = ui.press() else {
            return;
        };
        if p.mods.ctrl && keys::char_for(p.code, keys::Mods::default()) == Some('z') {
            ui.take_press();
            self.undo();
            return;
        }
        if p.mods.ctrl || p.mods.alt || p.mods.meta {
            return;
        }
        match p.code {
            keys::KEY_ESC => {
                ui.take_press();
                if self.detail.is_some() {
                    self.detail = None;
                    ui.focus("rows");
                } else if !self.add.text.is_empty() {
                    self.add.clear();
                    self.parsed = None;
                } else {
                    ui.focus("rows");
                }
                return;
            }
            KEY_F5 => {
                ui.take_press();
                self.send(Job::Load(self.q.clone()));
                return;
            }
            keys::KEY_SPACE => {
                ui.take_press();
                if let Some(id) = self.selected_task().map(|t| t.id.clone())
                    && !self.selected_is_waiting()
                {
                    self.toggle(&id);
                }
                return;
            }
            keys::KEY_DELETE => {
                ui.take_press();
                if let Some(id) = self.selected_task().map(|t| t.id.clone())
                    && !self.selected_is_waiting()
                {
                    self.delete(&id);
                }
                return;
            }
            _ => {}
        }
        let Some(c) = keys::char_for(p.code, p.mods) else {
            return;
        };
        ui.take_press();
        if self.go {
            self.go = false;
            match c {
                't' => self.open_list(query("today", None)),
                'i' => self.open_list(query("inbox", None)),
                'u' => self.open_list(query("upcoming", None)),
                'c' => self.open_list(query("completed", None)),
                'f' => self.open_target(Target::FiltersLabels),
                _ => {}
            }
            return;
        }
        let selected = self
            .selected_task()
            .filter(|_| !self.selected_is_waiting())
            .map(|t| t.id.clone());
        match c {
            'q' => ui.focus("quick-add"),
            '/' => self.open_target(Target::Search),
            'g' => self.go = true,
            'd' => {
                if let Some(id) = selected {
                    self.pick_date(PickFor::Task(id));
                }
            }
            'e' => {
                if let Some(id) = selected {
                    self.open_detail(&id);
                    ui.focus("d-title");
                }
            }
            '1'..='4' => {
                if let Some(id) = selected {
                    self.set_priority(&id, c as u8 - b'0');
                }
            }
            _ => {}
        }
    }
}

fn list_name(q: &ViewQuery) -> String {
    match q.kind.as_deref() {
        Some("inbox") => "Inbox",
        Some("upcoming") => "Upcoming",
        Some("completed") => "Completed",
        Some("today") | None => "Today",
        _ => "",
    }
    .to_owned()
}

fn has_meta(x: &TaskView) -> bool {
    x.due.is_some()
        || !x.labels.is_empty()
        || x.pending
        || x.subtasks > 0
        || !x.project_name.is_empty()
}

/// One line under the task: `TaskMetadata` in Tasks.tsx.
fn draw_meta(ui: &mut Ui, t: &Theme, x: &TaskView, r: Rect, today: NaiveDate) {
    let px = t.pt(12.0);
    let mut left = r.x;
    let mut put = |ui: &mut Ui, s: &str, c: Rgb| {
        let w = ui.measure(Face::Sans, px, s);
        if left + w < r.right() {
            ui.text_in(Rect::new(left, r.y, w, r.h), Face::Sans, px, c, s);
        }
        left += w + t.px(10.0);
    };
    if let Some(d) = &x.due {
        let mut s = due_label(d, today);
        if d.is_recurring {
            s = format!("{REPEAT} {s}");
        }
        if d.provisional {
            s.push_str(" (estimated)");
        }
        put(ui, &s, due_color(t, d.rel_days));
    }
    if x.subtasks > 0 {
        put(
            ui,
            &format!("{}/{} sub-tasks", x.subtasks_done, x.subtasks),
            t.dim,
        );
    }
    for l in &x.labels {
        put(ui, &format!("@{l}"), t.item);
    }
    if x.pending {
        put(ui, "waiting", t.dim);
    }
    if !x.project_name.is_empty() {
        let w = ui.measure(Face::Sans, px, &x.project_name);
        if left + w < r.right() {
            ui.text_in(
                Rect::new(r.right() - w, r.y, w, r.h),
                Face::Sans,
                px,
                t.dim,
                &x.project_name,
            );
        }
    }
}

/// `SyncLine` in Tasks.tsx.
fn sync_line(v: &View, now: u64, syncing: bool) -> String {
    if syncing {
        return "Syncing with Todoist…".into();
    }
    match v.sync.status {
        SyncStatus::Disconnected => {
            "Todoist isn't connected. These tasks stay on the server.".into()
        }
        SyncStatus::Reconnect => "Todoist needs reconnecting; your changes wait until then.".into(),
        SyncStatus::RateLimited => "Todoist asked to slow down; retrying soon.".into(),
        SyncStatus::Error => v
            .sync
            .error
            .clone()
            .unwrap_or_else(|| "Todoist isn't answering; retrying.".into()),
        SyncStatus::Syncing => "Syncing with Todoist…".into(),
        SyncStatus::Ok => match v.sync.last_sync_ms {
            Some(ms) => format!("Synced {}", relative(ms, now)),
            None => "Not synced yet".into(),
        },
    }
}

impl App for Tasks {
    type Msg = Msg;

    fn message(&mut self, m: Msg) {
        match m {
            Msg::Loaded(q, v) => {
                self.store.answered(&q, *v);
                self.rebuild();
                if let Some(id) = self.open_on_load.clone()
                    && self.task(&id).is_some()
                {
                    self.open_on_load = None;
                    self.open_detail(&id);
                }
                // The first answer names the projects: have their lists ready too.
                self.prefetch();
            }
            Msg::Acked(op_id, q, r) => {
                let done = self.store.take(&op_id);
                if let Some(v) = r.view {
                    self.store.answered(&q, v);
                }
                if let Some(done) = &done {
                    self.store.settle(done, &q);
                }
                self.offline = None;
                self.rebuild();
            }
            Msg::Refused(op_id, text) => {
                self.store.take(&op_id);
                self.error = Some(text);
                self.rebuild();
                // What the server thinks now.
                self.send(Job::Load(self.q.clone()));
            }
            Msg::Offline(text) => self.offline = Some(text),
            Msg::Online => self.offline = None,
            Msg::SignedOut(m) => {
                eprintln!("lucina-app-tasks: signed out: {m}");
                self.show_sign_in();
            }
            Msg::SignedIn => self.signed_in(),
            Msg::Changed => {}
            Msg::Synced(r) => {
                self.syncing = false;
                if let Err(e) = r {
                    self.error = Some(e);
                }
            }
        }
    }

    fn on_quit(&mut self) {
        if self.save_cache {
            cache::save(&self.store.snapshot());
        }
    }

    fn view(&mut self, ui: &mut Ui) {
        let t = ui.theme();
        ui.header("Tasks");
        if let Some(f) = self.want_focus.take() {
            ui.focus(f);
        }
        if self.mode == Mode::NotSignedIn {
            self.draw_not_signed_in(ui);
            return;
        }
        let side = ui.cut_left(t.px(230.0));
        self.draw_sidebar(ui, side);
        let screen = ui.area();
        let pane = self.detail.is_some().then(|| ui.cut_right(t.px(370.0)));
        ui.pad(14.0);
        ui.within(ui.area(), |ui| {
            self.draw_title(ui);
            if let Some(e) = self.error.clone() {
                let r = ui.cut_top(t.px(26.0));
                ui.text_in(r, Face::Sans, t.pt(13.0), t.danger, &e);
            }
            if let Some(v) = self.shown.as_ref().filter(|v| !v.conflicts.is_empty()) {
                let c = v.conflicts[0].clone();
                let r = ui.cut_top(t.px(30.0));
                let bw = t.px(80.0);
                ui.text_in(
                    Rect::new(r.x, r.y, r.w - bw - t.px(8.0), r.h),
                    Face::Sans,
                    t.pt(13.0),
                    Rgb::hex(0xe8912d),
                    &format!("{}: {}", c.content, c.message),
                );
                if ui.button_at(
                    Rect::new(r.right() - bw, r.y + t.px(2.0), bw, r.h - t.px(4.0)),
                    "dismiss",
                    "Dismiss",
                ) {
                    self.run(Command::DismissConflict { conflict_id: c.id }, None);
                }
            }
            if self.section == Section::Search {
                if ui.text_field("search", &mut self.search).changed {
                    self.list.scroll = 0;
                    self.list.selected = None;
                    self.run_search();
                }
                ui.space(6.0);
            } else if self.section == Section::List && self.q.kind.as_deref() != Some("completed") {
                self.draw_quick_add(ui);
            }
            self.draw_rows(ui);
        });
        if let Some(r) = pane {
            self.draw_detail(ui, r);
        }
        self.draw_toast(ui, screen);
        if self.picker.is_some() {
            self.draw_date_picker(ui);
        } else if self.projects.is_some() {
            self.draw_project_picker(ui);
        } else {
            self.keys(ui);
        }
    }
}
