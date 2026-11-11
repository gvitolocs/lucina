//! The shapes of `web/src/api/agents.ts` that `crates/lucina-agents` does not have (the proxy's own views), and the small rules of
//! `web/src/lib/agentsTimeline.ts`. Every field defaults, so a newer server still draws.

use std::collections::BTreeMap;

use lucina_account::timeline::parse_rfc3339_ms;
use lucina_agents::bucket::Bucket;
use lucina_agents::types::AgentStatus;
use serde::Deserialize;
use serde_json::{Map, Value};

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Resolution {
    pub behavior: String,
    pub by: String,
    pub at: String,
    pub message: Option<String>,
}

/// The permission book's view, of agents' and external requests. `approvalId` joins the owner-approvals pop-up.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PermissionView {
    pub id: String,
    pub kind: String,
    pub agent_id: Option<String>,
    pub source: Option<String>,
    pub name: Option<String>,
    pub title: String,
    pub description: Option<String>,
    pub details: Option<Map<String, Value>>,
    pub status: String,
    pub created_at: String,
    pub expires_at: String,
    pub resolution: Option<Resolution>,
    pub approval_id: Option<String>,
}

impl PermissionView {
    /// `permKey`.
    pub fn key(&self) -> String {
        match &self.agent_id {
            Some(a) => format!("{a}/{}", self.id),
            None => self.id.clone(),
        }
    }
}

/// `WorkspaceRecord`, as the proxy gives it.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Workspace {
    pub workspace_id: String,
    pub project_id: String,
    pub cwd: String,
    pub name: String,
    pub display_name: Option<String>,
    pub project_display_name: Option<String>,
    pub branch: Option<String>,
    pub background: Option<bool>,
    pub archived_at: Option<String>,
}

impl Workspace {
    pub fn label(&self) -> String {
        let name = if self.name.is_empty() { self.display_name.clone().unwrap_or_default() } else { self.name.clone() };
        match &self.project_display_name {
            Some(p) => format!("{p} / {name}"),
            None => name,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ProjectRecord {
    pub project_id: String,
    pub root_path: String,
    pub name: String,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Choice {
    pub id: String,
    pub label: String,
    pub is_selectable: Option<bool>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ProviderInfo {
    pub id: String,
    pub label: String,
    pub modes: Vec<Choice>,
    pub models: Vec<Choice>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct DiffLine {
    #[serde(rename = "type")]
    pub kind: String,
    pub content: String,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DiffHunk {
    pub old_start: u64,
    pub old_count: u64,
    pub new_start: u64,
    pub new_count: u64,
    pub lines: Vec<DiffLine>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DiffFile {
    pub path: String,
    pub old_path: Option<String>,
    pub is_new: bool,
    pub is_deleted: bool,
    pub additions: u64,
    pub deletions: u64,
    pub hunks: Vec<DiffHunk>,
    pub status: String,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DiffResult {
    pub files: Vec<DiffFile>,
    pub diff_too_large: bool,
    pub error: Option<String>,
}

/// One row of a diff on screen: `lineNumbers`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffRow {
    pub kind: String,
    pub n: String,
    pub content: String,
}

pub fn diff_rows(f: &DiffFile) -> Vec<DiffRow> {
    let mut out = Vec::new();
    for h in &f.hunks {
        out.push(DiffRow { kind: "hunk".into(), n: String::new(), content: format!("@@ -{},{} +{},{} @@", h.old_start, h.old_count, h.new_start, h.new_count) });
        let (mut o, mut n) = (h.old_start, h.new_start);
        for l in &h.lines {
            match l.kind.as_str() {
                "add" => {
                    out.push(DiffRow { kind: "add".into(), n: n.to_string(), content: l.content.clone() });
                    n += 1;
                }
                "remove" => {
                    out.push(DiffRow { kind: "remove".into(), n: o.to_string(), content: l.content.clone() });
                    o += 1;
                }
                "context" => {
                    out.push(DiffRow { kind: "context".into(), n: n.to_string(), content: l.content.clone() });
                    o += 1;
                    n += 1;
                }
                _ => out.push(DiffRow { kind: "header".into(), n: String::new(), content: l.content.clone() }),
            }
        }
    }
    out
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FileEntry {
    pub name: String,
    pub path: String,
    pub kind: String,
    pub size: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FileContent {
    pub path: String,
    pub encoding: String,
    pub content: Option<String>,
    pub size: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Terminal {
    pub id: String,
    pub name: String,
    pub title: Option<String>,
    pub workspace_id: String,
    pub status: String,
    pub exit_code: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Script {
    pub script_name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub port: Option<u32>,
    pub lifecycle: String,
    pub health: Option<String>,
    pub exit_code: Option<i64>,
    pub command: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Service {
    pub name: String,
    pub port: u32,
    pub health: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SearchMatch {
    pub snippet: String,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SearchResult {
    pub agent_id: String,
    pub title: Option<String>,
    pub provider: String,
    pub status: String,
    pub cwd: String,
    pub updated_at: String,
    pub archived_at: Option<String>,
    pub matches: Vec<SearchMatch>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PromptEntry {
    pub seq: u64,
    pub preview: String,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct UsageProvider {
    pub provider: String,
    pub agents: u64,
    pub running: u64,
    pub input_tokens: f64,
    pub cached_input_tokens: f64,
    pub output_tokens: f64,
    pub total_cost_usd: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct ScheduleCounts {
    pub active: u64,
    pub paused: u64,
    pub completed: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct UsageReport {
    pub providers: Option<Vec<UsageProvider>>,
    pub schedules: Option<ScheduleCounts>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct BoardTask {
    pub title: Option<String>,
    pub session: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct BoardWorkspace {
    pub workspace_id: String,
    pub project: Option<String>,
    pub branch: Option<String>,
    pub name: Option<String>,
    pub cwd: String,
    pub state: String,
    pub ahead: Option<i64>,
    pub behind: Option<i64>,
    pub dirty: bool,
    pub idle_days: Option<f64>,
    pub procs: Option<u64>,
    pub rss_mb: Option<f64>,
    pub task: Option<BoardTask>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct JobOutcome {
    pub message: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct BoardJob {
    pub job_id: String,
    pub kind: String,
    pub state: String,
    pub repo: Option<String>,
    pub branch: Option<String>,
    pub runtime: Option<String>,
    pub created_at: String,
    pub finished_at: Option<String>,
    pub outcome: Option<JobOutcome>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct BoardLease {
    pub holder: Option<String>,
    pub purpose: Option<String>,
    pub until: Option<String>,
    pub expires_at: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct BoardDevice {
    pub device: String,
    pub about: Option<String>,
    pub lease: Option<BoardLease>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct BoardDeploy {
    pub runtime: String,
    pub commit: String,
    pub recorded: String,
    pub by: Option<String>,
    pub note: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MainCheckout {
    pub path: String,
    pub dirty_files: Option<u64>,
    pub procs: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PipelineBoard {
    pub workspaces: Vec<BoardWorkspace>,
    pub queue: Vec<BoardJob>,
    pub recent_jobs: Vec<BoardJob>,
    pub leases: Vec<BoardDevice>,
    pub deploys: Vec<BoardDeploy>,
    pub main_checkouts: Vec<MainCheckout>,
    pub unmanaged: Vec<Value>,
}

// ---- Rules ----

/// `baseName`.
pub fn base_name(path: &str) -> &str {
    path.rsplit('/').find(|p| !p.is_empty()).unwrap_or(path)
}

/// `agentTitle`.
pub fn agent_title(title: Option<&str>, id: &str, cwd: &str) -> String {
    if let Some(t) = title.filter(|t| !t.trim().is_empty()) {
        return t.to_owned();
    }
    let dir = base_name(cwd);
    if dir.is_empty() { id.chars().take(7).collect() } else { dir.to_owned() }
}

/// `statusLabel`.
pub fn status_label(bucket: Bucket, status: AgentStatus) -> &'static str {
    match bucket {
        Bucket::NeedsInput => "Needs permission",
        Bucket::Failed => "Error",
        Bucket::Running => "Running",
        Bucket::Attention => "Finished",
        Bucket::Done => match status {
            AgentStatus::Closed => "Closed",
            AgentStatus::Initializing => "Starting",
            _ => "Idle",
        },
    }
}

/// `ago`, in English: "just now" style for the past, "in 5 min" for the future.
pub fn ago(iso: &str, now: i64) -> String {
    let Some(t) = parse_rfc3339_ms(iso) else { return String::new() };
    let sec = (t - now) / 1000;
    let abs = sec.abs();
    let (n, unit) = if abs < 60 {
        (abs, "s")
    } else if abs < 3600 {
        (abs / 60, "min")
    } else if abs < 86_400 {
        (abs / 3600, "hr")
    } else {
        (abs / 86_400, "d")
    };
    if sec > 0 { format!("in {n} {unit}") } else { format!("{n} {unit} ago") }
}

/// `n`: "1.2K", "3.4M", as `Intl.NumberFormat`'s compact notation.
pub fn compact(v: f64) -> String {
    let (x, suffix) = if v >= 1e9 {
        (v / 1e9, "B")
    } else if v >= 1e6 {
        (v / 1e6, "M")
    } else if v >= 1e3 {
        (v / 1e3, "K")
    } else {
        (v, "")
    };
    let s = format!("{x:.1}");
    format!("{}{suffix}", s.strip_suffix(".0").unwrap_or(&s))
}

/// `usd`.
pub fn usd(v: f64) -> String {
    format!("${v:.2}")
}

/// `sizeText`.
pub fn size_text(n: u64) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{:.1} MB", n as f64 / 1024.0 / 1024.0)
    }
}

/// `parentOf`.
pub fn parent_of(path: &str) -> String {
    path.rfind('/').map_or_else(String::new, |i| path[..i].to_owned())
}

/// `clock` in `ApprovalCard`: "2:05".
pub fn clock(seconds: i64) -> String {
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// `permissionText`: the command for a shell request, else the input as JSON.
pub fn permission_text(details: Option<&Map<String, Value>>, request_command: Option<&str>) -> String {
    if let Some(c) = request_command.filter(|c| !c.is_empty()) {
        return c.to_owned();
    }
    let Some(d) = details else { return String::new() };
    if let Some(c) = d.get("command").and_then(Value::as_str).filter(|c| !c.is_empty()) {
        return c.to_owned();
    }
    serde_json::to_string_pretty(d).unwrap_or_default()
}

/// `ansiToText`: the escape sequences are dropped, `\r` returns to the line start, `\b` erases.
pub fn ansi_to_text(input: &str) -> String {
    let mut lines: Vec<String> = vec![String::new()];
    let mut it = input.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\u{1b}' => match it.peek().copied() {
                // CSI: parameters, intermediates, one final byte.
                Some('[') => {
                    it.next();
                    for x in it.by_ref() {
                        if ('@'..='~').contains(&x) {
                            break;
                        }
                    }
                }
                // OSC: up to BEL or ESC backslash.
                Some(']') => {
                    it.next();
                    while let Some(x) = it.next() {
                        if x == '\u{7}' {
                            break;
                        }
                        if x == '\u{1b}' {
                            if it.peek() == Some(&'\\') {
                                it.next();
                            }
                            break;
                        }
                    }
                }
                Some('(' | ')') => {
                    it.next();
                    it.next();
                }
                Some('=' | '>' | '7' | '8') => {
                    it.next();
                }
                _ => {}
            },
            '\r' if it.peek() == Some(&'\n') => {}
            '\n' => lines.push(String::new()),
            '\r' => lines.last_mut().expect("never empty").clear(),
            '\u{8}' => {
                lines.last_mut().expect("never empty").pop();
            }
            c if c < ' ' && c != '\t' => {}
            c => lines.last_mut().expect("never empty").push(c),
        }
    }
    lines.join("\n")
}

/// Standard base64, for a terminal's `output` events. Stops at anything that is not base64.
pub fn base64(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut bits, mut n) = (0u32, 0u32);
    for b in s.bytes() {
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => break,
        };
        bits = (bits << 6) | u32::from(v);
        n += 6;
        if n >= 8 {
            n -= 8;
            out.push((bits >> n) as u8);
        }
    }
    out
}

/// `matches`: every word somewhere in the title, provider, model, cwd or short id.
pub fn matches(hay: &str, q: &str) -> bool {
    let hay = hay.to_lowercase();
    q.to_lowercase().split_whitespace().all(|w| hay.contains(w))
}

/// How a job or workspace state reads: `plState*`.
pub fn workspace_state(s: &str) -> &str {
    match s {
        "open" => "Open",
        "merge_queued" => "Merge queued",
        "merging" => "Merging",
        "merged" => "Merged",
        "stale" => "Stale",
        x => x,
    }
}

pub fn job_state(s: &str) -> &str {
    match s {
        "queued" => "Queued",
        "running" => "Running",
        "succeeded" => "Done",
        "failed" => "Failed",
        "cancelled" => "Cancelled",
        x => x,
    }
}

/// Labels on an agent the list hides: `lucina.internal`.
pub fn is_internal(labels: &BTreeMap<String, String>) -> bool {
    labels.get("lucina.internal").is_some_and(|v| !v.is_empty())
}
