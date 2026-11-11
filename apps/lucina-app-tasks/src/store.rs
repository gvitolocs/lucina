//! What is in memory: the server's answers per list, and the commands it has not confirmed yet, laid
//! over them. So a tap shows at once, whatever the network does; the engine's answer replaces the
//! guess when it arrives.
//!
//! The behaviour is web/src/state/tasks.ts's: commands go in order with an op_id and no expected
//! revision, a queued completion or deletion hides its task, a queued addition shows as a waiting
//! row. Beyond the web app: an edit, a reschedule or an undo also shows before the answer, and quick
//! add is parsed here by the engine's own parser instead of over the network.

use std::collections::HashMap;

use chrono::{NaiveDate, NaiveTime};
use chrono_tz::Tz;
use secretary_tasks::nlp;
use secretary_tasks::{
    Command, DueView, Group, ListView, Parsed, TaskView, View, ViewQuery, now_ms, today_in,
};
use serde::{Deserialize, Serialize};

/// The list a query names, as a map key.
pub fn key(q: &ViewQuery) -> String {
    format!(
        "{}|{}|{}|{}",
        q.kind.as_deref().unwrap_or("today"),
        q.id.as_deref().unwrap_or(""),
        q.query.as_deref().unwrap_or(""),
        q.days.unwrap_or(0)
    )
}

pub fn query(kind: &str, id: Option<&str>) -> ViewQuery {
    ViewQuery {
        kind: Some(kind.into()),
        id: id.map(str::to_owned),
        ..ViewQuery::default()
    }
}

/// A command on its way to the server.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Queued {
    pub op_id: String,
    pub command: Command,
    pub at_ms: u64,
    /// What an undo puts back, so that it shows before the answer.
    #[serde(skip)]
    pub restore: Option<(String, TaskView)>,
}

pub struct Store {
    pub tz: Tz,
    pub now: fn() -> u64,
    views: HashMap<String, View>,
    pub pending: Vec<Queued>,
    /// The newest answer: the sidebar comes from it while a list has never been loaded.
    latest: Option<String>,
    next_op: u64,
}

fn walk_mut(tasks: &mut Vec<TaskView>, f: &mut dyn FnMut(&mut TaskView) -> bool) {
    tasks.retain_mut(|t| {
        if !f(t) {
            return false;
        }
        walk_mut(&mut t.children, f);
        true
    });
}

/// What removing or restoring a task does to the sidebar's numbers (`view` in the engine's views.rs).
fn adjust_counts(v: &mut View, t: &TaskView, sign: i32) {
    let add = |n: &mut u32| *n = n.saturating_add_signed(sign);
    if Some(&t.project_id) == v.inbox_id.as_ref() {
        add(&mut v.counts.inbox);
    }
    if let Some(p) = v.projects.iter_mut().find(|p| p.id == t.project_id) {
        add(&mut p.count);
    }
    for l in &t.labels {
        if let Some(n) = v.labels.iter_mut().find(|n| n.name.eq_ignore_ascii_case(l)) {
            add(&mut n.count);
        }
    }
    if let Some(d) = &t.due {
        if d.rel_days < 0 {
            add(&mut v.counts.overdue);
            add(&mut v.counts.today);
        } else if d.rel_days == 0 {
            add(&mut v.counts.today);
        } else if d.rel_days < 7 {
            add(&mut v.counts.upcoming);
        }
    }
}

fn remove_task(v: &mut View, id: &str) -> Option<TaskView> {
    let mut gone = None;
    for g in &mut v.list.groups {
        walk_mut(&mut g.tasks, &mut |t| {
            if t.id == id {
                gone = Some(t.clone());
                false
            } else {
                true
            }
        });
    }
    if let Some(t) = &gone {
        v.list.count = v.list.count.saturating_sub(1);
        adjust_counts(v, t, -1);
    }
    gone
}

fn find_task<'a>(tasks: &'a [TaskView], id: &str) -> Option<&'a TaskView> {
    tasks.iter().find_map(|t| {
        if t.id == id {
            Some(t)
        } else {
            find_task(&t.children, id)
        }
    })
}

fn has_task(v: &View, id: &str) -> bool {
    v.list
        .groups
        .iter()
        .any(|g| find_task(&g.tasks, id).is_some())
}

/// A due as the engine would show it: `date` from a phrase or `YYYY-MM-DD`.
fn due_view(
    today: NaiveDate,
    date: NaiveDate,
    time: Option<NaiveTime>,
    string: &str,
    is_recurring: bool,
) -> DueView {
    let rel = (date - today).num_days();
    DueView {
        date: date.format("%Y-%m-%d").to_string(),
        time: time.map(|t| t.format("%H:%M").to_string()),
        string: string.to_owned(),
        is_recurring,
        rel_days: rel,
        overdue: rel < 0,
        provisional: false,
    }
}

/// `phrase` read by the engine's parser.
pub fn parse_phrase(phrase: &str, today: NaiveDate) -> Option<DueView> {
    if let Ok(d) = NaiveDate::parse_from_str(phrase.trim(), "%Y-%m-%d") {
        return Some(due_view(today, d, None, phrase, false));
    }
    let m = nlp::parse_due(phrase, nlp::Ctx::new(today))?;
    Some(due_view(
        today,
        m.date,
        m.time,
        phrase,
        m.recurrence.is_some(),
    ))
}

/// A moved due in a list grouped by dates goes to its day's group, or out of the list.
fn place_by_date(v: &mut View, id: &str) {
    let kind = v.list.kind.clone();
    if kind != "today" && kind != "upcoming" {
        return;
    }
    let mut moved: Option<TaskView> = None;
    for g in &mut v.list.groups {
        if let Some(pos) = g.tasks.iter().position(|t| t.id == id) {
            let t = &g.tasks[pos];
            let date = t.due.as_ref().map(|d| d.date.clone());
            let overdue = t.due.as_ref().is_some_and(|d| d.overdue);
            let stays = if overdue {
                g.kind == "overdue"
            } else {
                date.as_deref() == g.date.as_deref()
            };
            if !stays {
                moved = Some(g.tasks.remove(pos));
            }
            break;
        }
    }
    let Some(t) = moved else {
        return;
    };
    let date = t.due.as_ref().map(|d| d.date.clone());
    let target = v.list.groups.iter_mut().find(|g| {
        if t.due.as_ref().is_some_and(|d| d.overdue) {
            g.kind == "overdue"
        } else {
            g.date.is_some() && g.date == date
        }
    });
    match target {
        Some(g) => g.tasks.push(t),
        None => v.list.count = v.list.count.saturating_sub(1),
    }
}

/// The list as it will be once the server has `q`.
fn overlay(v: &mut View, q: &Queued, today: NaiveDate, projects: &[String]) {
    match &q.command {
        Command::Complete { task_id } | Command::Delete { task_id } => {
            let gone = remove_task(v, task_id);
            if gone.is_some() && matches!(q.command, Command::Complete { .. }) {
                v.goal.completed_today += 1;
                v.goal.reached =
                    v.goal.daily_goal > 0 && v.goal.completed_today >= v.goal.daily_goal;
            }
        }
        Command::Uncomplete { task_id } | Command::UndoDelete { task_id } => {
            if v.list.kind == "completed" {
                remove_task(v, task_id);
            } else if let Some((group, task)) = &q.restore
                && !has_task(v, task_id)
                && let Some(g) = v.list.groups.iter_mut().find(|g| &g.key == group)
            {
                g.tasks.push(task.clone());
                v.list.count += 1;
                let t = task.clone();
                adjust_counts(v, &t, 1);
            }
        }
        Command::Update {
            task_id,
            content,
            description,
            priority,
            labels,
            due_string,
            due_date,
            clear_due,
            ..
        } => {
            let phrase = due_string.as_deref().or(due_date.as_deref());
            let new_due = phrase.and_then(|p| parse_phrase(p, today));
            for g in &mut v.list.groups {
                walk_mut(&mut g.tasks, &mut |t| {
                    if &t.id == task_id {
                        if let Some(c) = content {
                            t.content = c.clone();
                        }
                        if let Some(d) = description {
                            t.description = d.clone();
                        }
                        if let Some(p) = priority {
                            t.priority = *p;
                        }
                        if let Some(l) = labels {
                            t.labels = l.clone();
                        }
                        if *clear_due {
                            t.due = None;
                        } else if new_due.is_some() {
                            t.due = new_due.clone();
                        }
                        t.pending = true;
                    }
                    true
                });
            }
            if new_due.is_some() || *clear_due {
                place_by_date(v, task_id);
            }
        }
        Command::Reschedule { task_id, date } => {
            let new_due = parse_phrase(date, today);
            for g in &mut v.list.groups {
                walk_mut(&mut g.tasks, &mut |t| {
                    if &t.id == task_id {
                        // The time and the recurrence stay, as Reschedule keeps them.
                        if let (Some(old), Some(new)) = (&t.due, &new_due) {
                            let mut d = new.clone();
                            d.time = old.time.clone();
                            d.string = old.string.clone();
                            d.is_recurring = old.is_recurring;
                            t.due = Some(d);
                        } else if new_due.is_some() {
                            t.due = new_due.clone();
                        }
                        t.pending = true;
                    }
                    true
                });
            }
            place_by_date(v, task_id);
        }
        Command::RescheduleOverdue { date } => {
            let target = parse_phrase(date.as_deref().unwrap_or("today"), today);
            let ids: Vec<String> = v
                .list
                .groups
                .iter()
                .filter(|g| g.kind == "overdue")
                .flat_map(|g| g.tasks.iter().map(|t| t.id.clone()))
                .collect();
            for id in ids {
                if let Some(new) = &target {
                    for g in &mut v.list.groups {
                        for t in &mut g.tasks {
                            if t.id == id {
                                let mut d = new.clone();
                                if let Some(old) = &t.due {
                                    d.time = old.time.clone();
                                    d.string = old.string.clone();
                                    d.is_recurring = old.is_recurring;
                                }
                                t.due = Some(d);
                                t.pending = true;
                            }
                        }
                    }
                    place_by_date(v, &id);
                }
            }
            v.list
                .groups
                .retain(|g| g.kind != "overdue" || !g.tasks.is_empty());
        }
        Command::QuickAdd { text, .. } => {
            let parsed = nlp::quick_add(text, "en", nlp::Ctx::new(today), projects);
            push_waiting(
                v,
                &q.op_id,
                &parsed.content,
                parsed.priority,
                parsed.due_string.as_deref(),
                today,
                parsed.labels,
            );
        }
        Command::Add {
            content,
            priority,
            due_string,
            labels,
            parent_id: None,
            ..
        } => {
            push_waiting(
                v,
                &q.op_id,
                content,
                *priority,
                due_string.as_deref(),
                today,
                labels.clone().unwrap_or_default(),
            );
        }
        Command::Add {
            content,
            parent_id: Some(parent),
            ..
        } => {
            let waiting = waiting_task(&q.op_id, content, None, None, today, vec![]);
            for g in &mut v.list.groups {
                walk_mut(&mut g.tasks, &mut |t| {
                    if &t.id == parent && !t.children.iter().any(|c| c.id == waiting.id) {
                        t.children.push(waiting.clone());
                        t.subtasks += 1;
                    }
                    true
                });
            }
        }
        Command::Move {
            task_id,
            project_id: Some(project),
            ..
        } => {
            if v.list.kind == "project" && v.list.id.as_ref() != Some(project)
                || v.list.kind == "inbox" && v.inbox_id.as_ref() != Some(project)
            {
                remove_task(v, task_id);
            } else {
                let name = v
                    .projects
                    .iter()
                    .find(|p| &p.id == project)
                    .map(|p| (p.name.clone(), p.color.clone()));
                for g in &mut v.list.groups {
                    walk_mut(&mut g.tasks, &mut |t| {
                        if &t.id == task_id {
                            t.project_id = project.clone();
                            if let Some((n, c)) = &name {
                                t.project_name = n.clone();
                                t.project_color = c.clone();
                            }
                            t.pending = true;
                        }
                        true
                    });
                }
            }
        }
        Command::DismissConflict { conflict_id } => {
            v.conflicts.retain(|c| &c.id != conflict_id);
        }
        _ => {}
    }
}

fn waiting_task(
    op_id: &str,
    content: &str,
    priority: Option<u8>,
    due: Option<&str>,
    today: NaiveDate,
    labels: Vec<String>,
) -> TaskView {
    TaskView {
        id: op_id.to_owned(),
        content: content.to_owned(),
        description: String::new(),
        project_id: String::new(),
        project_name: String::new(),
        project_color: String::new(),
        section_id: None,
        section_name: None,
        parent_id: None,
        parent_content: None,
        priority: priority.unwrap_or(4),
        due: due.and_then(|p| parse_phrase(p, today)),
        deadline: None,
        labels,
        checked: false,
        subtasks: 0,
        subtasks_done: 0,
        pending: true,
        duration_minutes: None,
        children: vec![],
        completed_ms: None,
    }
}

/// Waiting rows go in a group of their own at the top; the engine's answer puts them in their place.
fn push_waiting(
    v: &mut View,
    op_id: &str,
    content: &str,
    priority: Option<u8>,
    due: Option<&str>,
    today: NaiveDate,
    labels: Vec<String>,
) {
    let t = waiting_task(op_id, content, priority, due, today, labels);
    match v.list.groups.iter_mut().find(|g| g.kind == "waiting") {
        Some(g) => g.tasks.push(t),
        None => v.list.groups.insert(
            0,
            Group {
                key: "waiting".into(),
                kind: "waiting".into(),
                title: "Waiting to be sent".into(),
                date: None,
                rel_days: None,
                tasks: vec![t],
            },
        ),
    }
    v.list.count += 1;
}

impl Store {
    pub fn new(tz: Tz) -> Store {
        Store {
            tz,
            now: now_ms,
            views: HashMap::new(),
            pending: Vec::new(),
            latest: None,
            next_op: 0,
        }
    }

    pub fn today(&self) -> NaiveDate {
        today_in(self.tz, (self.now)())
    }

    pub fn has_list(&self, q: &ViewQuery) -> bool {
        self.views.contains_key(&key(q))
    }

    /// A server answer. A list is replaced whole, and the sidebar in the others follows it.
    pub fn answered(&mut self, q: &ViewQuery, view: View) {
        let k = key(q);
        for other in self.views.values_mut() {
            other.counts = view.counts.clone();
            other.goal = view.goal.clone();
            other.projects = view.projects.clone();
            other.labels = view.labels.clone();
            other.filters = view.filters.clone();
            other.sync = view.sync.clone();
            other.conflicts = view.conflicts.clone();
            other.undo = view.undo.clone();
        }
        self.views.insert(k.clone(), view);
        self.latest = Some(k);
    }

    /// A confirmed command has already changed the lists that are not on screen: apply it to them
    /// for good, so that opening one never shows the old row. The next load corrects any guess.
    pub fn settle(&mut self, q: &Queued, except: &ViewQuery) {
        let today = self.today();
        let skip = key(except);
        let names = self.project_names();
        for (k, v) in self.views.iter_mut() {
            if *k != skip && !matches!(q.command, Command::QuickAdd { .. } | Command::Add { .. }) {
                let (counts, goal, projects, labels) = (
                    v.counts.clone(),
                    v.goal.clone(),
                    v.projects.clone(),
                    v.labels.clone(),
                );
                overlay(v, q, today, &names);
                // `answered` has just brought the true sidebar.
                v.counts = counts;
                v.goal = goal;
                v.projects = projects;
                v.labels = labels;
            }
        }
    }

    fn project_names(&self) -> Vec<String> {
        self.latest
            .as_ref()
            .and_then(|k| self.views.get(k))
            .map(|v| v.projects.iter().map(|p| p.name.clone()).collect())
            .unwrap_or_default()
    }

    /// The list on screen: the last answer with everything unconfirmed laid over it. A list never
    /// loaded has the newest sidebar and no rows yet.
    pub fn shown(&self, q: &ViewQuery) -> Option<View> {
        let mut v = match self.views.get(&key(q)) {
            Some(v) => v.clone(),
            None => {
                let mut v = self.views.get(self.latest.as_ref()?)?.clone();
                v.list = ListView {
                    kind: q.kind.clone().unwrap_or_else(|| "today".into()),
                    id: q.id.clone(),
                    title: String::new(),
                    query: q.query.clone(),
                    groups: vec![],
                    count: 0,
                    warning: None,
                };
                v
            }
        };
        let today = self.today();
        let names = self.project_names();
        for p in &self.pending {
            overlay(&mut v, p, today, &names);
        }
        Some(v)
    }

    /// A new command: it shows from now on.
    pub fn enqueue(&mut self, command: Command, restore: Option<(String, TaskView)>) -> Queued {
        self.next_op += 1;
        let at_ms = (self.now)();
        let q = Queued {
            op_id: format!("juno-tasks-{}-{at_ms}-{}", std::process::id(), self.next_op),
            command,
            at_ms,
            restore,
        };
        self.pending.push(q.clone());
        q
    }

    /// The command is done with: answered or refused.
    pub fn take(&mut self, op_id: &str) -> Option<Queued> {
        let pos = self.pending.iter().position(|q| q.op_id == op_id)?;
        Some(self.pending.remove(pos))
    }

    /// The quick add preview, by the engine's parser, on every key: no network.
    pub fn parse_quick(&self, text: &str) -> Option<Parsed> {
        if text.trim().is_empty() {
            return None;
        }
        Some(nlp::quick_add(
            text,
            "en",
            nlp::Ctx::new(self.today()),
            &self.project_names(),
        ))
    }

    /// Everything worth keeping for the next start of the app.
    pub fn snapshot(&self) -> Saved {
        Saved {
            views: self.views.clone(),
            latest: self.latest.clone(),
            pending: self.pending.clone(),
        }
    }

    /// Also empties it: `Saved::default()`.
    pub fn restore(&mut self, s: Saved) {
        self.views = s.views;
        self.latest = s.latest;
        self.pending = s.pending;
    }
}

/// The file the app starts from (`crate::cache`).
#[derive(Serialize, Deserialize, Default)]
pub struct Saved {
    pub views: HashMap<String, View>,
    pub latest: Option<String>,
    pub pending: Vec<Queued>,
}
