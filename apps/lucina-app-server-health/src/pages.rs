//! What each page says, as a flat list of lines: `Overview` and its cards, `ServicesTab`, `TimelineTab`, `AttentionDetail`,
//! `HostDetail` and `ComponentDetail` in `web/src/components/server/`. On a keyboard every one of them is one list: the arrows
//! walk it, Enter does what the line offers. Pure, so all of it is tested without a screen.

use std::collections::HashSet;

use lucina_app::time::Zone;

use crate::model::{
    Attention, Component, Disk, Event, History, Host, Kind, Overview, State, ago, chart_scale, clock, day_label, disk_level, duration, filter_services, gb, group_by_day,
    healthy_by_group, hhmm, num, series_stats,
};

pub const HOT_C: f64 = 90.0;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Act {
    None,
    OpenItem(String),
    OpenHost(String),
    OpenComponent(String),
    /// A group or project in the tree, by key.
    Toggle(String),
    ToggleRestarted,
    ToggleHealthy,
    /// A timeline row's long detail.
    ToggleDetail(String),
    /// Puts text on the clipboard: nothing here runs a command.
    Copy { what: String, text: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Text,
    Dim,
    Mono,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Metric {
    Tctl,
    Load,
    Mem,
}

impl Metric {
    pub fn label(self) -> &'static str {
        match self {
            Metric::Tctl => "CPU temperature",
            Metric::Load => "Load (1 min)",
            Metric::Mem => "Memory",
        }
    }

    pub fn short(self) -> &'static str {
        match self {
            Metric::Tctl => "CPU temp",
            Metric::Load => "Load",
            Metric::Mem => "Memory",
        }
    }

    pub fn fmt(self, v: f64) -> String {
        match self {
            Metric::Tctl => format!("{} °C", num(v, 0)),
            Metric::Load => num(v, 2),
            Metric::Mem => format!("{}%", num(v, 0)),
        }
    }

    pub fn fixed(self) -> Option<(f64, f64)> {
        match self {
            Metric::Tctl => Some((30.0, 100.0)),
            Metric::Load => None,
            Metric::Mem => Some((0.0, 100.0)),
        }
    }

    pub fn floor(self) -> Option<f64> {
        (self == Metric::Load).then_some(0.0)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Spark {
    pub metric: Metric,
    pub values: Vec<Option<f64>>,
    pub current: Option<f64>,
}

impl Spark {
    /// The value at the right of the sparkline; a dash without data.
    pub fn text(&self) -> String {
        self.current.or(series_stats(&self.values).map(|s| s.0)).map_or_else(|| "—".to_owned(), |v| self.metric.fmt(v))
    }

    pub fn scale(&self) -> (f64, f64) {
        chart_scale(&self.values, self.metric.fixed(), self.metric.floor())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Line {
    Heading(String),
    Para(Tone, String),
    Row { state: Option<State>, title: String, badge: Option<String>, sub: String, meta: String, chips: Vec<String>, act: Act, indent: u8 },
    Host { state: State, name: String, role: String, uptime: String, os: String, note: Option<String>, sparks: Vec<Spark>, disk: Option<(Disk, usize)>, act: Act },
    Disk { disk: Disk, more: usize },
    Chart { spark: Spark, stats: String, ticks: Vec<(f32, String)> },
    Button { label: String, act: Act },
}

impl Line {
    pub fn act(&self) -> Option<&Act> {
        match self {
            Line::Row { act, .. } | Line::Host { act, .. } | Line::Button { act, .. } => Some(act).filter(|a| **a != Act::None),
            _ => None,
        }
    }
}

/// What the owner opened or closed by hand. The tree's defaults depend on the state, so a toggle is a difference from them.
#[derive(Default)]
pub struct Opened {
    pub toggled: HashSet<String>,
    pub restarted: bool,
    pub healthy: bool,
    pub details: HashSet<String>,
    pub query: String,
    pub show_repos: bool,
}

pub struct Ctx<'a> {
    pub o: &'a Overview,
    pub zone: &'a Zone,
    pub now: i64,
}

fn dot_row(state: State, title: &str, sub: String, act: Act) -> Line {
    Line::Row { state: Some(state), title: title.to_owned(), badge: None, sub, meta: String::new(), chips: Vec::new(), act, indent: 0 }
}

fn para(tone: Tone, s: impl Into<String>) -> Line {
    Line::Para(tone, s.into())
}

/// `forTime`.
fn for_time(iso: &str, now: i64) -> String {
    format!("for {}", duration(iso, now))
}

/// A component opens its attention item when it has one, else its own page: `componentHref`.
fn component_act(o: &Overview, id: &str) -> Act {
    match o.attention_for(id) {
        Some(a) => Act::OpenItem(a.key.clone()),
        None => Act::OpenComponent(id.to_owned()),
    }
}

/// `AttentionRows`.
fn attention_rows(c: &Ctx, items: &[&Attention], hide_host: bool, out: &mut Vec<Line>) {
    for a in items {
        let warn = a.severity == State::Warn;
        let affected = a.impact.as_ref().map_or(0, |i| i.broken.len() + if warn { i.degraded.len() } else { 0 });
        let mut meta = for_time(&a.since, c.now);
        if affected > 0 {
            meta += &format!(" · {} {affected}", if warn { "affects" } else { "Breaks" });
        }
        let mut chips = Vec::new();
        if let (false, Some(h)) = (hide_host, &a.host) {
            chips.push(h.clone());
        }
        if let Some(p) = &a.project {
            chips.push(p.clone());
        }
        out.push(Line::Row {
            state: Some(a.severity),
            title: a.title.clone(),
            badge: Some(a.severity.label().to_owned()),
            sub: a.summary.clone(),
            meta,
            chips,
            act: Act::OpenItem(a.key.clone()),
            indent: 0,
        });
    }
}

fn os_line(h: &Host) -> String {
    [h.os.as_deref(), h.kernel.as_deref()].into_iter().flatten().collect::<Vec<_>>().join(" · ")
}

/// `hostNote`: why a host shows no charts.
fn host_note(h: &Host, c: &Ctx) -> Option<String> {
    if !h.reachable {
        return Some(h.last_report.as_ref().map_or_else(|| "Unreachable".to_owned(), |r| format!("Unreachable since {}", clock(r, c.zone, c.now))));
    }
    h.limited.then(|| "Reachable · limited data".to_owned())
}

fn spark_of(h: &Host, m: Metric) -> Spark {
    let empty = Vec::new();
    let hist = h.history.as_ref();
    let (values, current) = match m {
        Metric::Tctl => (hist.map_or(&empty, |x| &x.tctl), h.tctl),
        Metric::Load => (hist.map_or(&empty, |x| &x.load1), h.load.as_ref().and_then(|l| l.first().copied())),
        Metric::Mem => (hist.map_or(&empty, |x| &x.mem), h.mem_used_pct),
    };
    Spark { metric: m, values: values.clone(), current }
}

/// `HostCard`.
fn host_card(h: &Host, c: &Ctx) -> Line {
    let mut disks = h.disks.clone();
    disks.sort_by(|a, b| b.used_pct.total_cmp(&a.used_pct));
    let more = disks.len().saturating_sub(1);
    let note = host_note(h, c);
    Line::Host {
        state: h.state,
        name: h.name.clone(),
        role: h.role.clone().unwrap_or_default(),
        uptime: h.uptime.as_ref().map_or_else(String::new, |u| format!("up {u}")),
        os: os_line(h),
        sparks: if note.is_some() { Vec::new() } else { [Metric::Tctl, Metric::Load, Metric::Mem].map(|m| spark_of(h, m)).to_vec() },
        disk: if note.is_some() { None } else { disks.into_iter().next().map(|d| (d, more)) },
        note,
        act: Act::OpenHost(h.id.clone()),
    }
}

/// `TimelineRow`, with its long detail one Enter away.
fn event_lines(e: &Event, key: String, c: &Ctx, with_day: bool, compact: bool, opened: &Opened, out: &mut Vec<Line>) {
    let when = if with_day { clock(&e.at, c.zone, c.now) } else { hhmm(&e.at, c.zone) };
    let host = e.host.as_ref().filter(|h| e.project.as_ref() != Some(*h));
    let mut sub: Vec<String> = [e.project.clone(), host.cloned()].into_iter().flatten().collect();
    sub.push(when);
    let mut chips = vec![e.kind.label().to_owned()];
    if !compact {
        if let Some(s) = e.severity {
            chips.push(s.label().to_owned());
        }
        if let (Some(f), Some(t)) = (e.from, e.to) {
            chips.push(format!("{} → {}", f.label(), t.label()));
        }
    }
    let detail = if compact { None } else { e.detail.as_ref().filter(|d| !d.is_empty()) };
    let long = detail.is_some_and(|d| d.chars().count() > 140);
    out.push(Line::Row {
        state: None,
        title: e.title.clone(),
        badge: None,
        sub: sub.join(" · "),
        meta: String::new(),
        chips,
        act: if long { Act::ToggleDetail(key.clone()) } else { Act::None },
        indent: 0,
    });
    if let Some(d) = detail {
        if long && !opened.details.contains(&key) {
            let cut: String = d.chars().take(137).collect();
            out.push(para(Tone::Dim, format!("{cut}… (Enter: more)")));
        } else {
            out.push(para(Tone::Dim, d.clone()));
        }
    }
}

/// `Overview`: what needs attention, the hosts, what restarted, the healthy checks, the latest events.
pub fn overview(c: &Ctx, opened: &Opened) -> Vec<Line> {
    let o = c.o;
    let mut out = Vec::new();
    let needing: Vec<&Attention> = o.attention.iter().filter(|a| !a.flapping).collect();
    out.push(Line::Heading("Needs attention".into()));
    if needing.is_empty() {
        out.push(para(Tone::Text, "Nothing needs attention"));
        out.push(para(Tone::Dim, "Every check is passing."));
    } else {
        attention_rows(c, &needing, false, &mut out);
    }
    out.push(Line::Heading("Hosts".into()));
    if o.hosts.is_empty() {
        out.push(para(Tone::Dim, "No host has reported yet."));
    }
    out.extend(o.hosts.iter().map(|h| host_card(h, c)));
    let flapping: Vec<&Attention> = o.attention.iter().filter(|a| a.flapping).collect();
    if !flapping.is_empty() {
        let mark = if opened.restarted { "▾" } else { "▸" };
        out.push(Line::Row {
            state: None,
            title: format!("{mark} Restarted recently · {}", o.flapping_total()),
            badge: None,
            sub: String::new(),
            meta: String::new(),
            chips: Vec::new(),
            act: Act::ToggleRestarted,
            indent: 0,
        });
        if opened.restarted {
            for a in &flapping {
                let mut r = dot_row(a.severity, &a.title, format!("failed {} times in 24 h", a.failures_24h.unwrap_or(0)), Act::OpenItem(a.key.clone()));
                if let (Line::Row { chips, indent, .. }, Some(h)) = (&mut r, &a.host) {
                    chips.push(h.clone());
                    *indent = 1;
                }
                out.push(r);
            }
        }
    }
    if o.checks_ok > 0 {
        let mark = if opened.healthy { "▾" } else { "▸" };
        let word = if o.checks_ok == 1 { "check" } else { "checks" };
        out.push(Line::Row {
            state: Some(State::Ok),
            title: format!("{mark} All good · {} {word}", o.checks_ok),
            badge: None,
            sub: String::new(),
            meta: String::new(),
            chips: Vec::new(),
            act: Act::ToggleHealthy,
            indent: 0,
        });
        if opened.healthy {
            for (group, comps) in healthy_by_group(&o.services) {
                out.push(Line::Heading(group));
                for comp in comps {
                    out.push(component_row(o, &comp, 1, false));
                }
            }
        }
    }
    out.push(Line::Heading("Recent".into()));
    let mut latest: Vec<&Event> = o.timeline.iter().collect();
    latest.sort_by_key(|e| std::cmp::Reverse(lucina_account::timeline::parse_rfc3339_ms(&e.at).unwrap_or(0)));
    if latest.is_empty() {
        out.push(para(Tone::Dim, "Nothing happened lately."));
    }
    for (i, e) in latest.into_iter().take(5).enumerate() {
        event_lines(e, format!("recent-{i}"), c, true, true, opened, &mut out);
    }
    out
}

fn component_row(o: &Overview, comp: &Component, indent: u8, with_tags: bool) -> Line {
    let mut chips = Vec::new();
    if with_tags {
        if let Some(h) = &comp.host {
            chips.push(h.clone());
        }
        if let Some(cr) = comp.criticality.filter(|x| matches!(x.label(), "High" | "Critical")) {
            chips.push(cr.label().to_owned());
        }
    }
    Line::Row {
        state: Some(comp.state),
        title: comp.name.clone(),
        badge: comp.state.unhealthy().then(|| comp.state.label().to_owned()),
        sub: String::new(),
        meta: String::new(),
        chips,
        act: component_act(o, &comp.id),
        indent,
    }
}

fn service_count(n: usize) -> String {
    format!("{n} services")
}

/// `repoCount` for the "Show repos" button.
pub fn repo_count(o: &Overview) -> usize {
    o.all_components().iter().filter(|(c, _, _)| c.kind == "repo").count()
}

/// `ServicesTab`: group, project, components. Unhealthy branches start open; while searching everything that matches is open.
pub fn services(c: &Ctx, opened: &Opened) -> Vec<Line> {
    let o = c.o;
    let trimmed: Vec<_> = o
        .services
        .iter()
        .map(|g| {
            let mut g = g.clone();
            for p in &mut g.projects {
                p.components.retain(|x| opened.show_repos || x.kind != "repo");
            }
            g.projects.retain(|p| !p.components.is_empty());
            g
        })
        .filter(|g| !g.projects.is_empty())
        .collect();
    let groups = filter_services(&trimmed, &opened.query);
    let searching = !opened.query.trim().is_empty();
    let mut out = Vec::new();
    if groups.is_empty() {
        out.push(para(Tone::Dim, if searching { format!("No service matches “{}”.", opened.query.trim()) } else { "The catalog is empty.".to_owned() }));
    }
    for g in &groups {
        let key = format!("g/{}", g.name);
        let open = searching || (g.state.unhealthy() != opened.toggled.contains(&key));
        let count: usize = g.projects.iter().map(|p| p.components.len()).sum();
        out.push(Line::Row {
            state: Some(g.state),
            title: format!("{} {}", if open { "▾" } else { "▸" }, g.name),
            badge: g.state.unhealthy().then(|| g.state.label().to_owned()),
            sub: String::new(),
            meta: service_count(count),
            chips: Vec::new(),
            act: if searching { Act::None } else { Act::Toggle(key) },
            indent: 0,
        });
        if !open {
            continue;
        }
        for p in &g.projects {
            let key = format!("p/{}/{}", g.name, p.id);
            let open = searching || (p.state.unhealthy() != opened.toggled.contains(&key));
            out.push(Line::Row {
                state: Some(p.state),
                title: format!("{} {}", if open { "▾" } else { "▸" }, p.id),
                badge: p.state.unhealthy().then(|| p.state.label().to_owned()),
                sub: String::new(),
                meta: service_count(p.components.len()),
                chips: Vec::new(),
                act: if searching { Act::None } else { Act::Toggle(key) },
                indent: 1,
            });
            if open {
                out.extend(p.components.iter().map(|x| component_row(o, x, 2, true)));
            }
        }
    }
    out
}

/// `TimelineTab`.
pub fn timeline(c: &Ctx, opened: &Opened, kinds: &[Kind], empty: &str) -> Vec<Line> {
    let days = group_by_day(&c.o.timeline, kinds, c.zone);
    let mut out = Vec::new();
    if days.is_empty() {
        out.push(para(Tone::Dim, empty));
    }
    for (day, events) in days {
        out.push(Line::Heading(day_label(day, c.zone, c.now)));
        for (i, e) in events.iter().enumerate() {
            event_lines(e, format!("{day}-{i}"), c, false, false, opened, &mut out);
        }
    }
    out
}

/// `askText`: the short context Ask Lucina puts in the composer.
pub fn ask_text(a: &Attention, c: &Ctx) -> String {
    let mut l = vec!["Something on my server needs attention.".to_owned(), format!("Problem: {}", a.title)];
    if let Some(h) = &a.host {
        l.push(format!("Host: {h}"));
    }
    l.push(format!("What's wrong: {}", a.summary));
    l.push(format!("Since: {} ({})", clock(&a.since, c.zone, c.now), duration(&a.since, c.now)));
    if let Some(i) = a.impact.as_ref().filter(|i| !i.broken.is_empty()) {
        l.push(format!("It breaks: {}", i.broken.iter().map(|b| b.name.as_str()).collect::<Vec<_>>().join(", ")));
    }
    if let Some(logs) = &a.logs {
        l.push(format!("Logs: {logs}"));
    }
    l.push("What should I check first?".to_owned());
    l.join("\n")
}

/// A link to another component, inside a detail page.
fn ref_row(o: &Overview, id: &str, name: &str) -> Line {
    Line::Row { state: None, title: name.to_owned(), badge: None, sub: String::new(), meta: String::new(), chips: Vec::new(), act: component_act(o, id), indent: 1 }
}

/// `AttentionDetail`.
pub fn item(c: &Ctx, a: &Attention) -> Vec<Line> {
    let o = c.o;
    let mut out = Vec::new();
    let mut chips: Vec<String> = [a.host.clone(), a.project.clone(), a.criticality.map(|x| x.label().to_owned())].into_iter().flatten().collect();
    chips.insert(0, a.severity.label().to_owned());
    out.push(Line::Row { state: Some(a.severity), title: chips.join(" · "), badge: None, sub: String::new(), meta: String::new(), chips: Vec::new(), act: Act::None, indent: 0 });
    out.push(para(Tone::Text, a.summary.clone()));
    let mut since = format!("Since {} · {}", clock(&a.since, c.zone, c.now), duration(&a.since, c.now));
    if let Some(f) = a.failures_24h {
        since += &format!(" · {f} {} in 24 h", if f == 1 { "failure" } else { "failures" });
    }
    out.push(para(Tone::Dim, since));
    out.push(Line::Button { label: "Ask Lucina (copies it for Chat)".into(), act: Act::Copy { what: "The question".into(), text: ask_text(a, c) } });
    if let Some(cmd) = &a.logs {
        out.push(Line::Heading("Logs command".into()));
        out.push(para(Tone::Mono, cmd.clone()));
        out.push(Line::Button { label: "Copy logs command".into(), act: Act::Copy { what: "The logs command".into(), text: cmd.clone() } });
    }
    if let Some(cmd) = &a.restart {
        out.push(Line::Heading("Restart command".into()));
        out.push(para(Tone::Mono, cmd.clone()));
        out.push(Line::Button { label: "Copy restart command".into(), act: Act::Copy { what: "The restart command".into(), text: cmd.clone() } });
    }
    if !a.details.is_empty() {
        out.push(Line::Heading("Details".into()));
        out.extend(a.details.iter().map(|d| para(Tone::Text, format!("• {d}"))));
    }
    if let Some(i) = a.impact.as_ref().filter(|i| !i.broken.is_empty() || !i.degraded.is_empty() || !i.public_hostnames_broken.is_empty()) {
        let warn = a.severity == State::Warn;
        out.push(Line::Heading(if warn { "If it goes down" } else { "Impact" }.into()));
        if !i.broken.is_empty() {
            out.push(para(Tone::Dim, if warn { "Would break" } else { "Breaking" }));
            out.extend(i.broken.iter().map(|r| ref_row(o, &r.id, &r.name)));
        }
        if !i.degraded.is_empty() {
            out.push(para(Tone::Dim, if warn { "Would degrade" } else { "Degrades" }));
            out.extend(i.degraded.iter().map(|r| ref_row(o, &r.id, &r.name)));
        }
        if !i.public_hostnames_broken.is_empty() {
            out.push(para(Tone::Dim, "Public hostnames"));
            out.extend(i.public_hostnames_broken.iter().map(|h| para(Tone::Text, h.clone())));
        }
    }
    if !a.caused.is_empty() {
        out.push(Line::Heading("Also down because of this".into()));
        out.extend(a.caused.iter().map(|id| ref_row(o, id, &o.name_of(id))));
    }
    if let Some(md) = &a.runbook {
        out.push(Line::Heading("Runbook".into()));
        // The text as written: this toolkit has no Markdown, and nothing in it is executed.
        out.extend(md.lines().filter(|l| !l.trim().is_empty()).map(|l| para(Tone::Text, l.trim_start_matches(['#', ' ']).to_owned())));
    }
    out
}

/// Hour ticks every six hours on the local clock: `hourTicks`.
fn hour_ticks(h: &History, count: usize, c: &Ctx) -> Vec<(f32, String)> {
    let Some(end) = lucina_account::timeline::parse_rfc3339_ms(&h.end) else { return Vec::new() };
    let span = count as i64 * h.step_s as i64 * 1000;
    if span <= 0 {
        return Vec::new();
    }
    let start = end - span;
    let mut out = Vec::new();
    let mut t = (start + 3_599_999).div_euclid(3_600_000) * 3_600_000;
    while t <= end {
        let utc = t.div_euclid(1000);
        let local = (utc + c.zone.at(utc).0).rem_euclid(86_400);
        if (local / 3600) % 6 == 0 && local % 3600 == 0 {
            out.push(((t - start) as f32 / span as f32, format!("{:02}:00", local / 3600)));
        }
        t += 3_600_000;
    }
    out
}

fn big_chart(h: &Host, m: Metric, c: &Ctx) -> Line {
    let spark = spark_of(h, m);
    let mut all = spark.values.clone();
    all.push(spark.current);
    let fmt = |v: Option<f64>| v.map_or_else(|| "—".to_owned(), |x| m.fmt(x));
    let s = series_stats(&all);
    let stats = format!("{}: now {}, low {}, high {}", m.label(), fmt(spark.current.or(s.map(|x| x.0))), fmt(s.map(|x| x.1)), fmt(s.map(|x| x.2)));
    let ticks = h.history.as_ref().map_or_else(Vec::new, |x| hour_ticks(x, spark.values.len(), c));
    Line::Chart { spark, stats, ticks }
}

/// `HostDetail`.
pub fn host(c: &Ctx, h: &Host) -> Vec<Line> {
    let o = c.o;
    let mut out = Vec::new();
    let note = host_note(h, c);
    let mut head = note.clone().unwrap_or_else(|| h.state.label().to_owned());
    if let Some(r) = &h.role {
        head += &format!(" · {r}");
    }
    out.push(Line::Row { state: Some(h.state), title: head, badge: None, sub: String::new(), meta: String::new(), chips: Vec::new(), act: Act::None, indent: 0 });
    let mut meta = Vec::new();
    if let Some(u) = &h.uptime {
        meta.push(format!("up {u}"));
    }
    meta.push(h.last_report.as_ref().map_or_else(|| "no report yet".to_owned(), |r| format!("reported {}", ago(r, c.zone, c.now))));
    out.push(para(Tone::Dim, meta.join(" · ")));
    let mut os = os_line(h);
    if let Some(n) = h.cpus {
        if !os.is_empty() {
            os += " · ";
        }
        os += &format!("{n} {}", if n == 1 { "CPU" } else { "CPUs" });
    }
    if !os.is_empty() {
        out.push(para(Tone::Dim, os));
    }
    if h.history.is_some() && note.is_none() {
        for m in [Metric::Tctl, Metric::Load, Metric::Mem] {
            out.push(big_chart(h, m, c));
        }
    }
    if !h.disks.is_empty() {
        out.push(Line::Heading("Disks".into()));
        out.extend(h.disks.iter().map(|d| Line::Disk { disk: d.clone(), more: 0 }));
    }
    if !h.findings.is_empty() {
        out.push(Line::Heading("Findings".into()));
        out.extend(h.findings.iter().map(|f| para(Tone::Text, format!("• {f}"))));
    }
    let here: Vec<&Attention> = o.attention.iter().filter(|a| a.host.as_deref() == Some(&h.id) && !a.flapping).collect();
    if !here.is_empty() {
        out.push(Line::Heading("Needs attention on this host".into()));
        attention_rows(c, &here, true, &mut out);
    }
    out.push(Line::Heading("Components".into()));
    let comps: Vec<_> = o.all_components().into_iter().filter(|(x, _, _)| x.host.as_deref() == Some(&h.id) && x.kind != "repo").collect();
    if comps.is_empty() {
        out.push(para(Tone::Dim, "No catalog components on this host."));
    }
    for (x, _, project) in comps {
        let mut r = component_row(o, x, 0, false);
        if let Line::Row { chips, .. } = &mut r {
            chips.push(project.to_owned());
        }
        out.push(r);
    }
    out
}

/// `ComponentDetail`.
pub fn component(x: &Component, group: &str, project: &str) -> Vec<Line> {
    let mut out = Vec::new();
    let mut head: Vec<String> = vec![x.state.label().to_owned()];
    head.extend(x.host.clone());
    head.push(project.to_owned());
    out.push(Line::Row { state: Some(x.state), title: head.join(" · "), badge: None, sub: String::new(), meta: String::new(), chips: Vec::new(), act: Act::None, indent: 0 });
    if let Some(d) = &x.description {
        out.push(para(Tone::Text, d.clone()));
    }
    out.push(para(Tone::Dim, format!("Kind: {}", x.kind)));
    if let Some(h) = &x.host {
        out.push(para(Tone::Dim, format!("Host: {h}")));
    }
    out.push(para(Tone::Dim, format!("Group: {group}")));
    if let Some(cr) = x.criticality {
        out.push(para(Tone::Dim, format!("Criticality: {}", cr.label())));
    }
    out
}

/// The colour word for a disk's bar.
pub fn disk_state(d: &Disk) -> State {
    disk_level(d.used_pct)
}

pub fn disk_text(d: &Disk, more: usize) -> String {
    let mut s = format!("{}% · {} free", num(d.used_pct, 0), gb(d.free_gb));
    if more > 0 {
        s += &format!(" · +{more} more");
    }
    s
}
