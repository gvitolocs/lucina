//! What the pages say, as flat lists of lines: `TimelineView`, `PermissionCard`, `Pages.tsx` and `PipelinePage.tsx`. On a keyboard
//! every page is one list: the arrows walk it, Enter does what a line offers. Pure, so all of it is tested without a screen.

use std::collections::{BTreeMap, HashSet};

use lucina_account::timeline::parse_rfc3339_ms;
use lucina_agents::bucket::Bucket;
use lucina_agents::display::{ToolCallDisplayInput, tool_call_display};
use lucina_agents::projection::{ProjectedEntry, project_rows};
use lucina_agents::schedule::{Cadence, Schedule, ScheduleStatus, Target};
use lucina_agents::timeline::{TimelineItem, ToolCall, ToolCallDetail, ToolCallStatus};
use lucina_agents::types::{AgentSnapshot, GateResult, GateVerdict};

use crate::diff::{diff_from_request, line_diff};
use crate::model::{
    self, DiffRow, PermissionView, PipelineBoard, Script, Service, UsageProvider, UsageReport,
    agent_title, ago, clock, compact, permission_text, usd,
};
use crate::store::{Store, TlStatus};

/// A long conversation on one CPU: only this many projected entries are on the screen.
pub const CHAT_WINDOW: usize = 400;

/// Only this many changed lines of a permission card's diff are on the screen.
pub const DIFF_CARD_MAX: usize = 200;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Act {
    None,
    OpenAgent(String),
    /// A tool call or thinking row, by its first seq.
    Toggle(u64),
    Answer {
        key: String,
        allow: bool,
    },
    Schedule {
        id: String,
        action: &'static str,
    },
    Script {
        name: String,
        start: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Text,
    Dim,
    Bad,
    Ok,
    Accent,
    Mono,
    MonoDim,
    Add,
    Del,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Line {
    Heading(String),
    Para(Tone, String),
    /// The owner's message, on its own background.
    User(String),
    Row {
        bucket: Option<Bucket>,
        title: String,
        sub: String,
        meta: String,
        tone: Tone,
        act: Act,
    },
    Button {
        label: String,
        act: Act,
        danger: bool,
    },
    /// One numbered line of a permission card's diff: `add` and `remove` rows carry the new and the old line number.
    Diff(DiffRow),
}

impl Line {
    pub fn act(&self) -> Option<&Act> {
        match self {
            Line::Row { act, .. } | Line::Button { act, .. } => {
                Some(act).filter(|a| **a != Act::None)
            }
            _ => None,
        }
    }

    fn para(t: Tone, s: impl Into<String>) -> Line {
        Line::Para(t, s.into())
    }
}

fn tool_status(s: ToolCallStatus) -> &'static str {
    match s {
        ToolCallStatus::Running => "running",
        ToolCallStatus::Failed => "failed",
        ToolCallStatus::Canceled => "canceled",
        ToolCallStatus::Completed => "",
    }
}

fn first_line(s: &str, max: usize) -> String {
    let l = s.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    if l.chars().count() > max {
        let cut: String = l.chars().take(max - 1).collect();
        format!("{cut}…")
    } else {
        l.to_owned()
    }
}

/// `ToolBody`.
fn tool_body(c: &ToolCall, out: &mut Vec<Line>) {
    let mono = |s: &str| Line::para(Tone::Mono, s);
    let code = |s: &str| Line::para(Tone::MonoDim, s);
    match &c.detail {
        ToolCallDetail::Shell {
            command,
            output,
            exit_code,
            ..
        } => {
            out.push(mono(&format!("$ {command}")));
            if let Some(o) = output.as_deref().filter(|o| !o.is_empty()) {
                out.push(code(o));
            }
            if let Some(Some(n)) = exit_code {
                out.push(Line::para(Tone::Dim, format!("Exit code {n}")));
            }
        }
        ToolCallDetail::Read {
            file_path, content, ..
        }
        | ToolCallDetail::Write { file_path, content } => {
            out.push(Line::para(Tone::Dim, file_path.as_str()));
            if let Some(t) = content.as_deref().filter(|o| !o.is_empty()) {
                out.push(code(t));
            }
        }
        ToolCallDetail::Edit {
            file_path,
            old_string,
            new_string,
            unified_diff,
        } => {
            out.push(Line::para(Tone::Dim, file_path.as_str()));
            if let Some(d) = unified_diff.as_deref().filter(|o| !o.is_empty()) {
                out.push(mono(d));
            } else {
                if let Some(t) = old_string.as_deref().filter(|o| !o.is_empty()) {
                    out.push(Line::para(Tone::Del, t));
                }
                if let Some(t) = new_string.as_deref().filter(|o| !o.is_empty()) {
                    out.push(Line::para(Tone::Add, t));
                }
            }
        }
        ToolCallDetail::Search(s) => {
            out.push(Line::para(Tone::Dim, s.query.as_str()));
            if let Some(t) = s.content.as_deref().filter(|o| !o.is_empty()) {
                out.push(code(t));
            }
            if let Some(p) = &s.file_paths {
                out.push(code(&p.join("\n")));
            }
        }
        ToolCallDetail::Fetch { url, result, .. } => {
            out.push(Line::para(Tone::Dim, url.as_str()));
            if let Some(t) = result.as_deref().filter(|o| !o.is_empty()) {
                out.push(code(t));
            }
        }
        ToolCallDetail::SubAgent {
            description, log, ..
        } => {
            if let Some(d) = description {
                out.push(Line::para(Tone::Dim, d.as_str()));
            }
            if !log.is_empty() {
                out.push(code(log));
            }
        }
        ToolCallDetail::WorktreeSetup { log, .. } => out.push(code(log)),
        ToolCallDetail::Plan { text } => out.push(Line::para(Tone::Text, text.as_str())),
        ToolCallDetail::PlainText { text, .. } => {
            if let Some(t) = text.as_deref().filter(|o| !o.is_empty()) {
                out.push(code(t));
            }
        }
        ToolCallDetail::Unknown { input, output } => {
            if !input.is_null() {
                out.push(mono(
                    &serde_json::to_string_pretty(input).unwrap_or_default(),
                ));
            }
            if !output.is_null() {
                out.push(code(
                    &serde_json::to_string_pretty(output).unwrap_or_default(),
                ));
            }
        }
    }
    if !c.error.is_null() {
        let text = c.error.as_str().map_or_else(
            || serde_json::to_string_pretty(&c.error).unwrap_or_default(),
            str::to_owned,
        );
        out.push(Line::para(Tone::Bad, text));
    }
}

fn entry_lines(
    e: &ProjectedEntry,
    cwd: Option<&str>,
    expanded: &HashSet<u64>,
    out: &mut Vec<Line>,
) {
    match &e.item {
        TimelineItem::UserMessage { text, .. } => out.push(Line::User(text.clone())),
        TimelineItem::AssistantMessage { text, .. } => {
            out.push(Line::para(Tone::Text, text.as_str()))
        }
        TimelineItem::Reasoning { text } => {
            let open = expanded.contains(&e.seq_start);
            out.push(Line::Row {
                bucket: None,
                title: format!("{} Thinking", if open { "▾" } else { "▸" }),
                sub: String::new(),
                meta: String::new(),
                tone: Tone::Dim,
                act: Act::Toggle(e.seq_start),
            });
            if open {
                out.push(Line::para(Tone::Dim, text.as_str()));
            }
        }
        TimelineItem::ToolCall(c) => {
            let d = tool_call_display(&ToolCallDisplayInput {
                name: &c.name,
                detail: &c.detail,
                metadata: c.metadata.as_ref(),
                agent_message: c.agent_message.as_ref(),
                cwd,
                target_agent_title: None,
            });
            let open = expanded.contains(&e.seq_start);
            let summary = d
                .summary
                .as_deref()
                .map_or_else(String::new, |s| first_line(s, 120));
            let tone = match c.status {
                ToolCallStatus::Failed => Tone::Bad,
                ToolCallStatus::Running => Tone::Accent,
                _ => Tone::Dim,
            };
            out.push(Line::Row {
                bucket: None,
                title: format!(
                    "{} {} {summary}",
                    if open { "▾" } else { "▸" },
                    d.display_name
                ),
                sub: String::new(),
                meta: tool_status(c.status).to_owned(),
                tone,
                act: Act::Toggle(e.seq_start),
            });
            if open {
                tool_body(c, out);
            }
        }
        TimelineItem::Todo { items } => {
            for x in items {
                let done = x.completed
                    || format!("{:?}", x.status)
                        .to_lowercase()
                        .contains("completed");
                let going = format!("{:?}", x.status)
                    .to_lowercase()
                    .contains("in_progress")
                    || format!("{:?}", x.status)
                        .to_lowercase()
                        .contains("inprogress");
                let mark = if done {
                    "✓"
                } else if going {
                    "…"
                } else {
                    "○"
                };
                out.push(Line::para(
                    if done { Tone::Dim } else { Tone::Text },
                    format!("{mark} {}", x.text),
                ));
            }
        }
        TimelineItem::Error { message } => out.push(Line::para(Tone::Bad, message.as_str())),
        TimelineItem::Notification { message, .. } => {
            out.push(Line::para(Tone::Dim, message.as_str()))
        }
        TimelineItem::Compaction { .. } => {
            out.push(Line::para(Tone::Dim, "── Conversation compacted ──"))
        }
        TimelineItem::Plugin { .. } => {}
    }
}

/// `gate_*`.
fn gate_lines(g: &GateResult, out: &mut Vec<Line>) {
    let word = match g.verdict {
        GateVerdict::Running => "running",
        GateVerdict::Verified => "verified",
        GateVerdict::Refused => "refused",
        GateVerdict::Suspicious => "suspicious",
        GateVerdict::Unverified => "not verified",
    };
    let tone = match g.verdict {
        GateVerdict::Verified => Tone::Ok,
        GateVerdict::Running => Tone::Accent,
        _ => Tone::Bad,
    };
    out.push(Line::para(tone, format!("Check: {word}")));
    if g.tampered {
        out.push(Line::para(
            Tone::Bad,
            "The run changed the check's own settings.",
        ));
    }
    out.extend(
        g.details
            .iter()
            .map(|d| Line::para(Tone::Text, format!("• {d}"))),
    );
    if let Some(p) = &g.proof_command {
        out.push(Line::para(Tone::Dim, format!("Proof command: {p}")));
    }
}

/// `PermissionCard`. `closed` is what ended it elsewhere, if anything.
pub fn permission_lines(
    p: &PermissionView,
    snap: Option<&AgentSnapshot>,
    who: Option<&str>,
    closed: Option<&str>,
    now: i64,
    busy: bool,
    out: &mut Vec<Line>,
) {
    let pending = p.status == "pending" && closed.is_none();
    let left = parse_rfc3339_ms(&p.expires_at).map(|e| ((e - now) / 1000).max(0));
    let kind = if p.kind == "external" {
        "Request".to_owned()
    } else {
        p.name.clone().unwrap_or_else(|| "Tool".to_owned())
    };
    let mut sub = vec![kind];
    if let Some(w) = who {
        sub.push(w.to_owned());
    }
    out.push(Line::Row {
        bucket: None,
        title: p.title.clone(),
        sub: sub.join(" · "),
        meta: if pending {
            left.map_or_else(String::new, |l| format!("{} left", clock(l)))
        } else {
            String::new()
        },
        tone: if pending { Tone::Accent } else { Tone::Dim },
        act: Act::None,
    });
    if let Some(d) = &p.description {
        out.push(Line::para(Tone::Dim, d.as_str()));
    }
    let request = snap.and_then(|s| s.pending_permissions.iter().find(|r| r.id == p.id));
    let command = request.and_then(|r| match &r.detail {
        Some(ToolCallDetail::Shell { command, .. }) => Some(command.as_str()),
        _ => None,
    });
    // A request that carries a diff (ACP `diff` content, a Claude edit input) shows the change itself, not its JSON.
    let diff = request.and_then(diff_from_request);
    let body = if diff.is_some() {
        String::new()
    } else {
        permission_text(p.details.as_ref(), command)
    };
    if !body.is_empty() {
        out.push(Line::para(Tone::Mono, body));
    }
    if let Some(d) = &diff {
        out.push(Line::para(Tone::Mono, d.path.as_str()));
        let rows = line_diff(&d.old_text, &d.new_text);
        let total = rows.len();
        let shown = total.min(DIFF_CARD_MAX);
        out.extend(rows.into_iter().take(shown).map(Line::Diff));
        if total > shown {
            out.push(Line::para(
                Tone::Dim,
                format!("… {total} more changed lines", total = total - shown),
            ));
        }
    }
    if pending {
        let key = p.key();
        if busy {
            out.push(Line::para(Tone::Dim, "Working…"));
        } else {
            out.push(Line::Button {
                label: "Allow".into(),
                act: Act::Answer {
                    key: key.clone(),
                    allow: true,
                },
                danger: false,
            });
            out.push(Line::Button {
                label: "Deny".into(),
                act: Act::Answer { key, allow: false },
                danger: true,
            });
        }
        if p.approval_id.is_some() {
            out.push(Line::para(Tone::Dim, "Also in Decisions, under approvals."));
        }
    } else {
        let mut text = closed.map_or_else(
            || {
                match p.status.as_str() {
                    "allowed" => "Allowed",
                    "denied" => "Denied",
                    _ => "This request expired.",
                }
                .to_owned()
            },
            str::to_owned,
        );
        if let (None, Some(m)) = (
            closed,
            p.resolution.as_ref().and_then(|r| r.message.as_ref()),
        ) {
            text += &format!(" · {m}");
        }
        out.push(Line::para(Tone::Dim, text));
    }
}

/// `AgentChat`: the timeline, the error, the pending permissions and the finished-run gate.
pub fn chat(
    store: &Store,
    id: &str,
    expanded: &HashSet<u64>,
    seen: &[String],
    closed: &BTreeMap<String, String>,
    busy: &HashSet<String>,
    now: i64,
) -> Vec<Line> {
    let mut out = Vec::new();
    let Some(tl) = store.timelines.get(id) else {
        out.push(Line::para(Tone::Dim, "Loading…"));
        return out;
    };
    if let (TlStatus::Error, Some(e)) = (tl.status, &tl.error) {
        out.push(Line::para(Tone::Bad, e.as_str()));
    }
    let entries = project_rows(&tl.rows);
    if entries.is_empty() {
        out.push(Line::para(
            Tone::Dim,
            if tl.status == TlStatus::Loading {
                "Loading…"
            } else {
                "Nothing here yet."
            },
        ));
    }
    let skip = entries.len().saturating_sub(CHAT_WINDOW);
    if skip > 0 {
        out.push(Line::para(
            Tone::Dim,
            format!("{skip} earlier entries are not shown here."),
        ));
    }
    let snap = store.snapshots.get(id);
    let cwd = snap.map(|s| s.cwd.as_str());
    for e in &entries[skip..] {
        entry_lines(e, cwd, expanded, &mut out);
    }
    if let Some(s) = snap {
        if let (lucina_agents::types::AgentStatus::Error, Some(e)) = (s.status, &s.last_error) {
            out.push(Line::para(Tone::Bad, e.as_str()));
        }
    }
    // `PermissionList`: a request that was shown stays, saying how it ended, until the view is left.
    let mut shown: Vec<&PermissionView> = store
        .permissions
        .values()
        .filter(|p| {
            p.agent_id.as_deref() == Some(id) && (p.status == "pending" || seen.contains(&p.key()))
        })
        .collect();
    shown.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    for p in shown {
        let key = p.key();
        permission_lines(
            p,
            snap,
            None,
            closed.get(&key).map(String::as_str),
            now,
            busy.contains(&key),
            &mut out,
        );
    }
    if let Some(g) = snap.and_then(|s| s.gate.as_ref()) {
        gate_lines(g, &mut out);
    }
    out
}

/// `PermissionsPage`.
pub fn permissions(
    store: &Store,
    seen: &[String],
    closed: &BTreeMap<String, String>,
    busy: &HashSet<String>,
    now: i64,
) -> Vec<Line> {
    let mut out = vec![Line::para(
        Tone::Dim,
        "Agents and other services ask here before they run something risky. Only you can answer.",
    )];
    let who = |p: &PermissionView| -> Option<String> {
        match &p.agent_id {
            Some(a) => Some(store.items.get(a).map_or_else(
                || a.clone(),
                |i| agent_title(i.title.as_deref(), &i.id, &i.cwd),
            )),
            None => p.source.clone(),
        }
    };
    let mut shown: Vec<&PermissionView> = store
        .permissions
        .values()
        .filter(|p| p.status == "pending" || seen.contains(&p.key()))
        .collect();
    shown.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    if shown.is_empty() {
        out.push(Line::para(Tone::Dim, "Nothing is waiting."));
    }
    for p in &shown {
        let key = p.key();
        let w = who(p);
        let snap = p.agent_id.as_ref().and_then(|a| store.snapshots.get(a));
        permission_lines(
            p,
            snap,
            w.as_deref(),
            closed.get(&key).map(String::as_str),
            now,
            busy.contains(&key),
            &mut out,
        );
    }
    let shown_keys: Vec<String> = shown.iter().map(|p| p.key()).collect();
    let mut recent: Vec<&PermissionView> = store
        .permissions
        .values()
        .filter(|p| p.status != "pending" && !shown_keys.contains(&p.key()))
        .collect();
    recent.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    if !recent.is_empty() {
        out.push(Line::Heading("Answered lately".into()));
    }
    for p in recent.into_iter().take(15) {
        let w = who(p);
        permission_lines(p, None, w.as_deref(), None, now, false, &mut out);
    }
    out
}

fn cadence_text(s: &Schedule) -> String {
    match &s.cadence {
        Cadence::Cron {
            expression,
            timezone,
        } => match timezone {
            Some(z) => format!("{expression} ({z})"),
            None => expression.clone(),
        },
        Cadence::Every { every_ms } => format!("every {} min", (every_ms / 60_000).max(1)),
    }
}

/// `SchedulesPage`.
pub fn schedules(
    list: Option<&[Schedule]>,
    available: bool,
    problem: Option<&str>,
    busy: Option<&str>,
    now: i64,
) -> Vec<Line> {
    let mut out = Vec::new();
    if !available {
        out.push(Line::para(Tone::Dim, "Not available on this server yet."));
        return out;
    }
    if let Some(p) = problem {
        out.push(Line::para(Tone::Bad, p));
    }
    let Some(list) = list else {
        out.push(Line::para(Tone::Dim, "Loading…"));
        return out;
    };
    if list.is_empty() {
        out.push(Line::para(Tone::Dim, "No schedules."));
    }
    for s in list {
        let title = s
            .name
            .clone()
            .unwrap_or_else(|| s.prompt.chars().take(60).collect());
        let (word, tone) = match s.status {
            ScheduleStatus::Active => ("Active", Tone::Ok),
            ScheduleStatus::Paused => ("Paused", Tone::Accent),
            ScheduleStatus::Completed => ("Completed", Tone::Dim),
        };
        let mut sub = cadence_text(s);
        if let Some(n) = &s.next_run_at {
            sub += &format!(" · next {}", ago(n, now));
        }
        let act = match &s.target {
            Target::Agent { agent_id } => Act::OpenAgent(agent_id.clone()),
            Target::NewAgent { .. } => Act::None,
        };
        out.push(Line::Row {
            bucket: None,
            title,
            sub,
            meta: word.to_owned(),
            tone,
            act,
        });
        if busy == Some(s.id.as_str()) {
            out.push(Line::para(Tone::Dim, "Working…"));
            continue;
        }
        let action = |label: &str, action: &'static str| Line::Button {
            label: label.to_owned(),
            act: Act::Schedule {
                id: s.id.clone(),
                action,
            },
            danger: false,
        };
        match s.status {
            ScheduleStatus::Active => out.push(action("Pause", "pause")),
            ScheduleStatus::Paused => out.push(action("Resume", "resume")),
            ScheduleStatus::Completed => {}
        }
        if s.status != ScheduleStatus::Completed {
            out.push(action("Run now", "run-once"));
        }
    }
    out
}

fn num(n: &Option<serde_json::Number>) -> f64 {
    n.as_ref()
        .and_then(serde_json::Number::as_f64)
        .unwrap_or(0.0)
}

/// `UsagePage`: the server's report, else summed from the agents' last usage, and per agent.
pub fn usage(
    snaps: &[AgentSnapshot],
    report: Option<&UsageReport>,
    loading: bool,
    problem: Option<&str>,
) -> Vec<Line> {
    let mut out = Vec::new();
    if let Some(p) = problem {
        out.push(Line::para(Tone::Bad, p));
    }
    if loading {
        out.push(Line::para(Tone::Dim, "Loading…"));
        return out;
    }
    let providers: Vec<UsageProvider> = match report.and_then(|r| r.providers.clone()) {
        Some(p) => p,
        None => {
            let mut by: BTreeMap<String, UsageProvider> = BTreeMap::new();
            for s in snaps {
                let p = by
                    .entry(s.provider.clone())
                    .or_insert_with(|| UsageProvider {
                        provider: s.provider.clone(),
                        ..UsageProvider::default()
                    });
                p.agents += 1;
                if s.status == lucina_agents::types::AgentStatus::Running {
                    p.running += 1;
                }
                if let Some(u) = &s.last_usage {
                    p.input_tokens += num(&u.input_tokens);
                    p.cached_input_tokens += num(&u.cached_input_tokens);
                    p.output_tokens += num(&u.output_tokens);
                    p.total_cost_usd += num(&u.total_cost_usd);
                }
            }
            by.into_values().collect()
        }
    };
    if providers.is_empty() {
        out.push(Line::para(Tone::Dim, "No usage yet."));
    } else {
        out.push(Line::Heading("Usage by provider".into()));
    }
    for p in &providers {
        let agents = if p.running > 0 {
            format!("{} agents ({} running)", p.agents, p.running)
        } else {
            format!("{} agents", p.agents)
        };
        out.push(Line::Row {
            bucket: None,
            title: p.provider.clone(),
            sub: format!(
                "{agents} · Input {} · Cached {} · Output {}",
                compact(p.input_tokens),
                compact(p.cached_input_tokens),
                compact(p.output_tokens)
            ),
            meta: usd(p.total_cost_usd),
            tone: Tone::Text,
            act: Act::None,
        });
    }
    let mut per: Vec<&AgentSnapshot> = snaps.iter().filter(|s| s.last_usage.is_some()).collect();
    per.sort_by(|a, b| {
        num(&b.last_usage.as_ref().expect("filtered").total_cost_usd).total_cmp(&num(&a
            .last_usage
            .as_ref()
            .expect("filtered")
            .total_cost_usd))
    });
    if !per.is_empty() {
        out.push(Line::Heading("By agent".into()));
    }
    for s in per.into_iter().take(20) {
        let u = s.last_usage.as_ref().expect("filtered");
        out.push(Line::Row {
            bucket: None,
            title: agent_title(s.title.as_deref(), &s.id, &s.cwd),
            sub: format!(
                "{} · {} / {}",
                s.provider,
                compact(num(&u.input_tokens)),
                compact(num(&u.output_tokens))
            ),
            meta: usd(num(&u.total_cost_usd)),
            tone: Tone::Text,
            act: Act::OpenAgent(s.id.clone()),
        });
    }
    if let Some(c) = report.and_then(|r| r.schedules.as_ref()) {
        out.push(Line::para(
            Tone::Dim,
            format!(
                "Schedules: {} active, {} paused, {} completed.",
                c.active, c.paused, c.completed
            ),
        ));
    }
    out
}

fn short_session(s: &str) -> String {
    if s.starts_with("local_") {
        let cut: String = s.chars().take(14).collect();
        format!("{cut}…")
    } else {
        s.to_owned()
    }
}

/// `PipelinePage`: what `lagent board` prints.
pub fn pipeline(
    board: Option<&PipelineBoard>,
    updated: Option<&str>,
    problem: Option<&str>,
    now: i64,
) -> Vec<Line> {
    let mut out = Vec::new();
    if let Some(p) = problem {
        out.push(Line::para(
            Tone::Bad,
            format!("Couldn't read the pipeline: {p}"),
        ));
    }
    let Some(b) = board else {
        if problem.is_none() {
            out.push(Line::para(Tone::Dim, "Loading…"));
        }
        return out;
    };
    let running: Vec<_> = b
        .queue
        .iter()
        .filter(|j| j.state == "queued" || j.state == "running")
        .collect();
    let mut summary = format!(
        "{} workspaces · {} in the queue",
        b.workspaces.len(),
        running.len()
    );
    if let Some(u) = updated {
        summary += &format!(" · Updated {}", ago(u, now));
    }
    out.push(Line::para(Tone::Dim, summary));
    let job_line = |j: &crate::model::BoardJob| {
        let kind = if j.kind == "deploy" {
            "Deploy"
        } else {
            "Merge"
        };
        let what = j.branch.clone().or(j.runtime.clone()).unwrap_or_default();
        let mut sub: Vec<String> = [j.repo.clone()].into_iter().flatten().collect();
        sub.push(ago(j.finished_at.as_deref().unwrap_or(&j.created_at), now));
        if j.state == "failed" {
            sub.extend(j.outcome.as_ref().and_then(|o| o.message.clone()));
        }
        let tone = match j.state.as_str() {
            "failed" => Tone::Bad,
            "running" | "queued" => Tone::Accent,
            _ => Tone::Dim,
        };
        Line::Row {
            bucket: None,
            title: format!("{kind} {what}"),
            sub: sub.join(" · "),
            meta: model::job_state(&j.state).to_owned(),
            tone,
            act: Act::None,
        }
    };
    out.push(Line::Heading("Merges and deploys".into()));
    if running.is_empty() {
        out.push(Line::para(Tone::Dim, "Nothing is merging or deploying."));
    }
    out.extend(running.iter().map(|j| job_line(j)));
    out.push(Line::Heading("Workspaces".into()));
    if b.workspaces.is_empty() {
        out.push(Line::para(Tone::Dim, "No workspaces."));
    }
    const ORDER: [&str; 5] = ["merging", "merge_queued", "open", "merged", "stale"];
    let mut states: Vec<&str> = ORDER
        .iter()
        .copied()
        .filter(|s| b.workspaces.iter().any(|w| w.state == *s))
        .collect();
    for w in &b.workspaces {
        if !states.contains(&w.state.as_str()) {
            states.push(&w.state);
        }
    }
    for s in states {
        let rows: Vec<_> = b.workspaces.iter().filter(|w| w.state == s).collect();
        out.push(Line::para(
            Tone::Accent,
            format!("{} ({})", model::workspace_state(s), rows.len()),
        ));
        for w in rows {
            let title = w
                .branch
                .clone()
                .or(w.name.clone())
                .unwrap_or_else(|| model::base_name(&w.cwd).to_owned());
            let mut parts: Vec<String> = [w.project.clone()].into_iter().flatten().collect();
            match (w.ahead, w.behind) {
                (Some(a), Some(c)) => parts.push(format!("+{a} −{c}")),
                (Some(a), None) => parts.push(format!("+{a}")),
                _ => {}
            }
            if w.dirty {
                parts.push("uncommitted changes".into());
            }
            if let Some(d) = w.idle_days.filter(|d| *d > 0.0) {
                parts.push(format!("idle {d} d"));
            }
            let session = w
                .task
                .as_ref()
                .and_then(|t| t.session.as_deref())
                .map_or_else(|| "no session".to_owned(), short_session);
            parts.push(session);
            if let Some(p) = w.procs.filter(|p| *p > 0) {
                parts.push(format!("{p} processes · {} MB", w.rss_mb.unwrap_or(0.0)));
            }
            out.push(Line::Row {
                bucket: None,
                title,
                sub: parts.join(" · "),
                meta: String::new(),
                tone: Tone::Text,
                act: Act::None,
            });
        }
    }
    if !b.main_checkouts.is_empty() {
        out.push(Line::para(
            Tone::Dim,
            format!(
                "{} main checkouts have uncommitted files or running processes",
                b.main_checkouts.len()
            ),
        ));
    }
    if !b.unmanaged.is_empty() {
        out.push(Line::para(
            Tone::Dim,
            format!("{} worktrees are not registered", b.unmanaged.len()),
        ));
    }
    let leases: Vec<_> = b.leases.iter().filter(|d| d.lease.is_some()).collect();
    if !leases.is_empty() {
        out.push(Line::Heading("Devices in use".into()));
    }
    for d in leases {
        let l = d.lease.as_ref().expect("filtered");
        let sub: Vec<String> = [l.holder.as_deref().map(short_session), l.purpose.clone()]
            .into_iter()
            .flatten()
            .collect();
        let until = l
            .until
            .as_deref()
            .or(l.expires_at.as_deref())
            .map_or_else(String::new, |u| format!("until {}", ago(u, now)));
        out.push(Line::Row {
            bucket: None,
            title: d.device.clone(),
            sub: sub.join(" · "),
            meta: until,
            tone: Tone::Text,
            act: Act::None,
        });
    }
    let recent: Vec<_> = b
        .recent_jobs
        .iter()
        .filter(|j| j.state != "queued" && j.state != "running")
        .take(8)
        .collect();
    if !recent.is_empty() {
        out.push(Line::Heading("Recent jobs".into()));
    }
    out.extend(recent.iter().map(|j| job_line(j)));
    if !b.deploys.is_empty() {
        out.push(Line::Heading("Recent deploys".into()));
    }
    for d in b.deploys.iter().take(5) {
        let mut sub = vec![
            d.commit.chars().take(10).collect::<String>(),
            ago(&d.recorded, now),
        ];
        sub.extend(d.by.clone());
        out.push(Line::Row {
            bucket: None,
            title: d.runtime.clone(),
            sub: sub.join(" · "),
            meta: String::new(),
            tone: Tone::Text,
            act: Act::None,
        });
    }
    out
}

/// `ScriptsPane`.
pub fn scripts(list: &[Script], services: &[Service], error: Option<&str>) -> Vec<Line> {
    let mut out = Vec::new();
    if let Some(e) = error {
        out.push(Line::para(Tone::Bad, e));
    }
    if list.is_empty() {
        out.push(Line::para(Tone::Dim, "This workspace has no scripts."));
    }
    for s in list {
        let running = s.lifecycle == "running";
        let mut sub = vec![
            if s.kind == "service" {
                "service"
            } else {
                "script"
            }
            .to_owned(),
            if running { "Running" } else { "Stopped" }.to_owned(),
        ];
        if let Some(h) = &s.health {
            sub.push(h.clone());
        }
        if let Some(p) = s.port {
            sub.push(format!("port {p}"));
        }
        if let Some(c) = s.exit_code {
            sub.push(format!("exit code {c}"));
        }
        out.push(Line::Row {
            bucket: None,
            title: s.script_name.clone(),
            sub: sub.join(" · "),
            meta: String::new(),
            tone: if running { Tone::Ok } else { Tone::Text },
            act: Act::None,
        });
        if let Some(c) = &s.command {
            out.push(Line::para(Tone::Mono, c.as_str()));
        }
        out.push(Line::Button {
            label: format!(
                "{} {}",
                if running { "Stop" } else { "Start" },
                s.script_name
            ),
            act: Act::Script {
                name: s.script_name.clone(),
                start: !running,
            },
            danger: false,
        });
    }
    if !services.is_empty() {
        out.push(Line::Heading("Open ports".into()));
        for s in services {
            out.push(Line::Row {
                bucket: None,
                title: s.name.clone(),
                sub: format!("port {}", s.port),
                meta: s.health.clone().unwrap_or_default(),
                tone: Tone::Text,
                act: Act::None,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Timeline;
    use serde_json::json;

    /// A store with one running agent whose pending permission carries an ACP `diff` content block.
    fn store_with_diff_card() -> Store {
        let mut store = Store::default();
        let snap: AgentSnapshot = serde_json::from_value(json!({
            "id": "a1", "provider": "oc-cli", "cwd": "/w", "createdAt": "2026-11-02T10:00:00.000Z",
            "updatedAt": "2026-11-02T10:00:00.000Z", "status": "running",
            "capabilities": {"supportsStreaming": true, "supportsSessionPersistence": true, "supportsDynamicModes": false,
                "supportsMcpServers": true, "supportsReasoningStream": true, "supportsToolInvocations": true},
            "currentModeId": null, "availableModes": [],
            "pendingPermissions": [
                {"id": "p1", "provider": "oc-cli", "name": "edit", "kind": "tool",
                 "input": {"content": [{"type": "diff", "path": "/w/src/a.rs", "oldText": "a\nb\n", "newText": "a\nc\n"}]},
                 "detail": {"type": "unknown", "input": {}, "output": null}}
            ],
        }))
        .unwrap();
        store.apply_snapshot(snap);
        store.timelines.insert(
            "a1".to_owned(),
            Timeline {
                epoch: "e".to_owned(),
                rows: Vec::new(),
                status: TlStatus::Ready,
                error: None,
            },
        );
        let p: PermissionView = serde_json::from_value(json!({
            "id": "p1", "kind": "agent", "agentId": "a1", "name": "edit", "title": "Edit a.rs",
            "details": {"content": [{"type": "diff", "path": "/w/src/a.rs", "oldText": "a\nb\n", "newText": "a\nc\n"}]},
            "status": "pending", "createdAt": "2026-11-02T10:00:00.000Z", "expiresAt": "2026-11-02T11:00:00.000Z",
        }))
        .unwrap();
        store.permissions.insert(p.key(), p);
        store
    }

    #[test]
    fn a_permission_card_with_a_diff_shows_numbered_lines_above_allow_and_deny() {
        let store = store_with_diff_card();
        let lines = chat(
            &store,
            "a1",
            &HashSet::new(),
            &[],
            &BTreeMap::new(),
            &HashSet::new(),
            1_793_620_800_000,
        );
        let first_diff = lines
            .iter()
            .position(|l| matches!(l, Line::Diff(_)))
            .expect("the diff rows");
        let allow = lines
            .iter()
            .position(|l| {
                matches!(
                    l,
                    Line::Button {
                        act: Act::Answer { allow: true, .. },
                        ..
                    }
                )
            })
            .expect("Allow");
        let deny = lines
            .iter()
            .position(|l| {
                matches!(
                    l,
                    Line::Button {
                        act: Act::Answer { allow: false, .. },
                        ..
                    }
                )
            })
            .expect("Deny");
        assert!(
            first_diff < allow && allow < deny,
            "the diff is above Allow and Deny"
        );
        let rows: Vec<(&str, &str, &str)> = lines
            .iter()
            .filter_map(|l| match l {
                Line::Diff(d) => Some((d.kind.as_str(), d.n.as_str(), d.content.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(
            rows,
            [
                ("context", "1", "a"),
                ("remove", "2", "b"),
                ("add", "2", "c")
            ],
            "numbered old/new lines"
        );
        let text = format!("{lines:?}");
        assert!(text.contains("/w/src/a.rs"), "the path is shown: {text}");
        assert!(
            !text.contains("oldText"),
            "the raw JSON is not shown: {text}"
        );
    }

    #[test]
    fn a_permission_card_without_a_diff_keeps_its_body_and_no_diff_rows() {
        let mut store = store_with_diff_card();
        // A shell request: no diff content anywhere.
        let snap = store.snapshots.get_mut("a1").unwrap();
        snap.pending_permissions = serde_json::from_value(json!([
            {"id": "p1", "provider": "claude", "name": "Bash", "kind": "tool",
             "input": {"command": "git push"}, "detail": {"type": "shell", "command": "git push"}}
        ]))
        .unwrap();
        let lines = chat(
            &store,
            "a1",
            &HashSet::new(),
            &[],
            &BTreeMap::new(),
            &HashSet::new(),
            1_793_620_800_000,
        );
        assert!(!lines.iter().any(|l| matches!(l, Line::Diff(_))));
        let text = format!("{lines:?}");
        assert!(text.contains("git push"), "{text}");
    }
}
