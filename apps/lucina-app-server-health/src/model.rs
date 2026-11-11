//! `GET /v1/system/overview` and the helpers of `web/src/api/systemOverview.ts`: one document with the hosts (latest metrics and
//! 24 h history), what needs attention, the catalog tree and the timeline. A failing source shows up in `sources`; the rest
//! still renders. Every field defaults, so a server newer than this app still draws.

use lucina_account::timeline::parse_rfc3339_ms;
use lucina_app::time::Zone;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Ok,
    Warn,
    Crit,
    Idle,
    #[default]
    #[serde(other)]
    Unknown,
}

impl State {
    /// `stateLabel`.
    pub fn label(self) -> &'static str {
        match self {
            State::Ok => "OK",
            State::Warn => "Warning",
            State::Crit => "Critical",
            State::Idle => "Idle",
            State::Unknown => "Unknown",
        }
    }

    /// `isUnhealthy`.
    pub fn unhealthy(self) -> bool {
        matches!(self, State::Warn | State::Crit)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Criticality {
    #[default]
    Low,
    Medium,
    High,
    Critical,
}

impl Criticality {
    /// `levelLabel`.
    pub fn label(self) -> &'static str {
        match self {
            Criticality::Low => "Low",
            Criticality::Medium => "Medium",
            Criticality::High => "High",
            Criticality::Critical => "Critical",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Incident,
    #[default]
    Change,
    Deploy,
    Decision,
}

impl Kind {
    /// `kindLabel`.
    pub fn label(self) -> &'static str {
        match self {
            Kind::Incident => "Incident",
            Kind::Deploy => "Deploy",
            Kind::Decision => "Decision",
            Kind::Change => "State change",
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SourceStatus {
    pub ok: bool,
    pub at: Option<String>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Sources {
    pub catalog: Option<SourceStatus>,
    pub sentinel: Option<SourceStatus>,
    pub ops: Option<SourceStatus>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Disk {
    pub mount: String,
    pub used_pct: f64,
    pub free_gb: f64,
    pub size_gb: f64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct History {
    pub step_s: u64,
    /// End of the last bucket.
    pub end: String,
    pub tctl: Vec<Option<f64>>,
    pub load1: Vec<Option<f64>>,
    pub mem: Vec<Option<f64>>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Host {
    pub id: String,
    pub name: String,
    pub role: Option<String>,
    pub state: State,
    pub reachable: bool,
    pub limited: bool,
    pub last_report: Option<String>,
    pub uptime: Option<String>,
    pub os: Option<String>,
    pub kernel: Option<String>,
    pub cpus: Option<u32>,
    pub load: Option<Vec<f64>>,
    pub mem_used_pct: Option<f64>,
    pub tctl: Option<f64>,
    pub disks: Vec<Disk>,
    pub history: Option<History>,
    pub findings: Vec<String>,
    pub component_id: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Reference {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Impact {
    pub broken: Vec<Reference>,
    pub degraded: Vec<Reference>,
    pub public_hostnames_broken: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Attention {
    pub key: String,
    pub severity: State,
    pub title: String,
    pub component_id: Option<String>,
    pub host: Option<String>,
    pub group: Option<String>,
    pub project: Option<String>,
    pub kind: Option<String>,
    pub criticality: Option<Criticality>,
    pub summary: String,
    pub details: Vec<String>,
    pub since: String,
    pub failures_24h: Option<u64>,
    pub flapping: bool,
    pub impact: Option<Impact>,
    pub caused: Vec<String>,
    pub logs: Option<String>,
    pub restart: Option<String>,
    pub runbook: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Component {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub host: Option<String>,
    pub state: State,
    pub criticality: Option<Criticality>,
    pub description: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ServiceProject {
    pub id: String,
    pub state: State,
    pub components: Vec<Component>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ServiceGroup {
    pub name: String,
    pub state: State,
    pub projects: Vec<ServiceProject>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Event {
    pub at: String,
    pub kind: Kind,
    pub title: String,
    pub project: Option<String>,
    pub host: Option<String>,
    pub severity: Option<Criticality>,
    pub from: Option<State>,
    pub to: Option<State>,
    pub detail: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Overview {
    pub generated_at: String,
    pub overall: State,
    pub attention_count: u64,
    pub flapping_count: Option<u64>,
    pub checks_ok: u64,
    pub checks_total: u64,
    pub sources: Sources,
    pub hosts: Vec<Host>,
    pub attention: Vec<Attention>,
    pub services: Vec<ServiceGroup>,
    /// Newest first.
    pub timeline: Vec<Event>,
}

impl Overview {
    /// `flappingTotal`.
    pub fn flapping_total(&self) -> u64 {
        self.flapping_count.unwrap_or_else(|| self.attention.iter().filter(|a| a.flapping).count() as u64)
    }

    /// `brokenSources`, in the page's words.
    pub fn broken_sources(&self) -> Vec<&'static str> {
        let s = &self.sources;
        let mut out = Vec::new();
        if s.catalog.as_ref().is_some_and(|x| !x.ok) {
            out.push("Catalog unavailable — showing the last host reports");
        }
        if s.sentinel.as_ref().is_some_and(|x| !x.ok) {
            out.push("Host reports unavailable — showing the catalog only");
        }
        if s.ops.as_ref().is_some_and(|x| !x.ok) {
            out.push("Ops records unavailable — the timeline may be incomplete");
        }
        out
    }

    /// `allComponents`: with the group's name and the project's id.
    pub fn all_components(&self) -> Vec<(&Component, &str, &str)> {
        let mut out = Vec::new();
        for g in &self.services {
            for p in &g.projects {
                for c in &p.components {
                    out.push((c, g.name.as_str(), p.id.as_str()));
                }
            }
        }
        out
    }

    pub fn name_of(&self, id: &str) -> String {
        self.all_components().iter().find(|(c, _, _)| c.id == id).map_or_else(|| id.to_owned(), |(c, _, _)| c.name.clone())
    }

    /// `attentionFor`: `catalog:<id>` or any item naming the component.
    pub fn attention_for(&self, component: &str) -> Option<&Attention> {
        let key = format!("catalog:{component}");
        self.attention.iter().find(|a| a.key == key).or_else(|| self.attention.iter().find(|a| a.component_id.as_deref() == Some(component)))
    }
}

/// `filterServices`: the tree trimmed to components matching `query` (id, name or host, any case). A project whose id matches
/// keeps all its components. An empty query returns the tree.
pub fn filter_services(groups: &[ServiceGroup], query: &str) -> Vec<ServiceGroup> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return groups.to_vec();
    }
    let has = |s: &str| s.to_lowercase().contains(&q);
    let mut out = Vec::new();
    for g in groups {
        let mut projects = Vec::new();
        for p in &g.projects {
            let components: Vec<Component> = if has(&p.id) {
                p.components.clone()
            } else {
                p.components.iter().filter(|c| has(&c.id) || has(&c.name) || c.host.as_deref().is_some_and(has)).cloned().collect()
            };
            if !components.is_empty() {
                projects.push(ServiceProject { components, ..p.clone() });
            }
        }
        if !projects.is_empty() {
            out.push(ServiceGroup { projects, ..g.clone() });
        }
    }
    out
}

/// `healthyByGroup`: the ok and idle components by group, groups in tree order, names sorted.
pub fn healthy_by_group(groups: &[ServiceGroup]) -> Vec<(String, Vec<Component>)> {
    let mut out = Vec::new();
    for g in groups {
        let mut c: Vec<Component> = g.projects.iter().flat_map(|p| p.components.iter()).filter(|c| matches!(c.state, State::Ok | State::Idle)).cloned().collect();
        c.sort_by_key(|x| x.name.to_lowercase());
        if !c.is_empty() {
            out.push((g.name.clone(), c));
        }
    }
    out
}

/// `diskLevel`: at least 90 % used is critical, at least 80 % a warning.
pub fn disk_level(used_pct: f64) -> State {
    if used_pct >= 90.0 {
        State::Crit
    } else if used_pct >= 80.0 {
        State::Warn
    } else {
        State::Ok
    }
}

/// `num`: at most `digits` decimals, without trailing zeros.
pub fn num(n: f64, digits: usize) -> String {
    let s = format!("{n:.digits$}");
    if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_owned() } else { s }
}

/// `gb`: "422 GB", "1.8 TB".
pub fn gb(n: f64) -> String {
    if n >= 1000.0 { format!("{} TB", num(n / 1000.0, 1)) } else { format!("{} GB", num(n, 0)) }
}

fn ms(iso: &str) -> Option<i64> {
    parse_rfc3339_ms(iso)
}

/// `duration`: "12 min", "3 hr", "2 days": how long since `iso`.
pub fn duration(iso: &str, now: i64) -> String {
    let Some(t) = ms(iso) else { return String::new() };
    let min = ((now - t) / 60_000).max(0);
    if min < 60 {
        format!("{min} min")
    } else if min < 48 * 60 {
        format!("{} hr", min / 60)
    } else {
        format!("{} days", min / 1440)
    }
}

/// Year, month and day of a day count since 1970, and the hour and minute: the local fields of a time.
fn civil(local_secs: i64) -> (i64, u32, u32, u32, u32) {
    let days = local_secs.div_euclid(86_400);
    let rem = local_secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d, (rem / 3600) as u32, (rem % 3600 / 60) as u32)
}

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const WEEKDAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];

fn local(zone: &Zone, at_ms: i64) -> i64 {
    let utc = at_ms.div_euclid(1000);
    utc + zone.at(utc).0
}

fn today_key(zone: &Zone, now: i64) -> i64 {
    local(zone, now).div_euclid(86_400)
}

/// `clock`: "10:30" today, "27 Oct, 10:30" otherwise.
pub fn clock(iso: &str, zone: &Zone, now: i64) -> String {
    let Some(t) = ms(iso) else { return String::new() };
    let l = local(zone, t);
    let (_, m, d, h, mi) = civil(l);
    if l.div_euclid(86_400) == today_key(zone, now) {
        format!("{h:02}:{mi:02}")
    } else {
        format!("{d} {}, {h:02}:{mi:02}", MONTHS[m as usize - 1])
    }
}

/// `hhmm`: the time alone.
pub fn hhmm(iso: &str, zone: &Zone) -> String {
    let Some(t) = ms(iso) else { return String::new() };
    let (_, _, _, h, mi) = civil(local(zone, t));
    format!("{h:02}:{mi:02}")
}

/// `relativeTime`: "just now", "2 min ago", "3 hr ago", after a day the date and time.
pub fn ago(iso: &str, zone: &Zone, now: i64) -> String {
    let Some(t) = ms(iso) else { return String::new() };
    let min = ((now - t).max(0)) / 60_000;
    if min < 1 {
        "just now".to_owned()
    } else if min < 60 {
        format!("{min} min ago")
    } else if min / 60 < 24 {
        format!("{} hr ago", min / 60)
    } else {
        let (_, m, d, h, mi) = civil(local(zone, t));
        format!("{d} {}, {h:02}:{mi:02}", MONTHS[m as usize - 1])
    }
}

/// `dayLabel`: "Today", "Yesterday", else the day with the year when it is not this one.
pub fn day_label(day: i64, zone: &Zone, now: i64) -> String {
    let today = today_key(zone, now);
    if day == today {
        return "Today".to_owned();
    }
    if day == today - 1 {
        return "Yesterday".to_owned();
    }
    let (y, m, d, _, _) = civil(day * 86_400);
    let (this, ..) = civil(today * 86_400);
    let wd = WEEKDAYS[day.rem_euclid(7) as usize];
    let mon = MONTHS[m as usize - 1];
    if y == this { format!("{wd}, {d} {mon}") } else { format!("{wd}, {d} {mon} {y}") }
}

/// `groupByDay`: events of the given kinds by local day, newest day first.
pub fn group_by_day(events: &[Event], kinds: &[Kind], zone: &Zone) -> Vec<(i64, Vec<Event>)> {
    let mut sorted: Vec<&Event> = events.iter().filter(|e| kinds.contains(&e.kind)).collect();
    sorted.sort_by_key(|e| std::cmp::Reverse(ms(&e.at).unwrap_or(0)));
    let mut out: Vec<(i64, Vec<Event>)> = Vec::new();
    for e in sorted {
        let day = local(zone, ms(&e.at).unwrap_or(0)).div_euclid(86_400);
        match out.last_mut() {
            Some((d, list)) if *d == day => list.push(e.clone()),
            _ => out.push((day, vec![e.clone()])),
        }
    }
    out
}

/// `seriesStats`: the last value, the lowest and the highest.
pub fn series_stats(values: &[Option<f64>]) -> Option<(f64, f64, f64)> {
    let real: Vec<f64> = values.iter().flatten().copied().filter(|v| v.is_finite()).collect();
    let last = *real.last()?;
    let (lo, hi) = real.iter().fold((f64::MAX, f64::MIN), |(l, h), v| (l.min(*v), h.max(*v)));
    Some((last, lo, hi))
}

/// `chartScale`: `fixed` when given, else the series' range padded by 10 % (0.5 when flat), never below `floor`.
pub fn chart_scale(values: &[Option<f64>], fixed: Option<(f64, f64)>, floor: Option<f64>) -> (f64, f64) {
    if let Some(f) = fixed {
        return f;
    }
    let Some((_, lo, hi)) = series_stats(values) else { return (0.0, 1.0) };
    let pad = if hi > lo { (hi - lo) * 0.1 } else { 0.5 };
    let low = floor.map_or(lo - pad, |f| f.max(lo - pad));
    (low, hi + pad)
}
