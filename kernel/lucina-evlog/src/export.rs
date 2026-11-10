//! The log in words and in one JSON, for people and agents alike: the CLI's `show` and `report`, lucina-devd's `reliability`
//! tool and the crash-recovery runners all say the same thing. Sessions read this instead of raw `/sys/fs/pstore` records.

use std::fmt::Write;

use serde_json::json;

use crate::event::{Event, Level};
use crate::store::Meta;
use crate::{bucket, providers, timeline};

pub fn clock(ms: i64, off: i32) -> String {
    let local = ms + i64::from(off) * 1000;
    let day = local.div_euclid(86_400_000);
    let (y, m, d) = timeline::civil(day);
    let s = local.rem_euclid(86_400_000) / 1000;
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
        s / 3600,
        s / 60 % 60,
        s % 60
    )
}

pub fn when(e: &Event, off: i32) -> String {
    match e.time_ms {
        Some(t) => format!(
            "{}{}",
            if e.time_estimated { "~" } else { "" },
            clock(t, off)
        ),
        None => "time unknown       ".into(),
    }
}

pub fn title(e: &Event) -> String {
    providers::known(&e.provider, e.id).map_or_else(
        || format!("{} {}", e.provider, e.id),
        |d| d.name.to_string(),
    )
}

/// One event in full, as `lucina-evlog show` prints it.
pub fn event_text(e: &Event, off: i32) -> String {
    let mut o = String::new();
    let _ = writeln!(
        o,
        "#{}  {}  {}  ({} {}, {})",
        e.record,
        when(e, off),
        title(e),
        e.provider,
        e.id,
        e.level.name()
    );
    let mut where_ = Vec::new();
    if let Some(b) = e.boot {
        where_.push(format!("boot {b}"));
    }
    if let Some(u) = e.uptime_ms {
        where_.push(format!("uptime {}.{:03} s", u / 1000, u % 1000));
    }
    if let Some(b) = &e.build {
        where_.push(format!("build {b}"));
    }
    if !where_.is_empty() {
        let _ = writeln!(o, "    {}", where_.join(", "));
    }
    let _ = writeln!(o);
    let _ = writeln!(o, "{}", e.summary);
    if let Some(c) = &e.cause {
        let _ = writeln!(o, "\nLikely cause ({}): {}", c.confidence.name(), c.text);
    }
    if !e.evidence.is_empty() {
        let _ = writeln!(o, "\nEvidence:");
        for (i, ev) in e.evidence.iter().enumerate() {
            let line = ev
                .line
                .map(|l| format!(" -> log line {}", l + 1))
                .unwrap_or_default();
            let _ = writeln!(
                o,
                "  {}. [{}] {}{line}",
                i + 1,
                if ev.observed { "seen" } else { "reasoned" },
                ev.fact
            );
            let _ = writeln!(o, "     from {}", ev.source);
        }
    }
    if let Some(n) = &e.next_step {
        let _ = writeln!(o, "\nWhat to do: {n}");
    }
    if let Some(b) = &e.bucket {
        let _ = writeln!(o, "\nProblem bucket {}: {}", b.id, b.label);
    }
    if !e.log.is_empty() {
        let _ = writeln!(o, "\nKernel log (last {} lines kept):", e.log.len());
        for (i, l) in e.log.iter().enumerate() {
            let t = l
                .uptime_ms
                .map(|u| format!("[{:>5}.{:03}]", u / 1000, u % 1000))
                .unwrap_or_else(|| " ".repeat(11));
            let text: String = l.text.chars().take(150).collect();
            match &l.mark {
                Some(m) => {
                    let _ = writeln!(o, "{:>3} > {t} {text}    <- {m}", i + 1);
                }
                None => {
                    let _ = writeln!(o, "{:>3}   {t} {text}", i + 1);
                }
            }
        }
    }
    if !e.fields.is_empty() {
        let _ = writeln!(
            o,
            "\nDetails: {}",
            serde_json::to_string(&e.fields).unwrap_or_default()
        );
    }
    if let Some(s) = &e.source {
        let _ = writeln!(o, "Source: {s}");
    }
    o
}

/// Everything an agent needs in one value: today's index, the days, the problem buckets, recent failures in full.
pub fn report(events: &[Event], meta: &Meta, now: i64, days: usize, off: i32) -> serde_json::Value {
    let tl = timeline::days(events, now, days, off);
    let today = tl.last().cloned();
    let recent: Vec<&Event> = events
        .iter()
        .rev()
        .filter(|e| e.level <= Level::Error)
        .take(20)
        .collect();
    let undated: Vec<u64> = events
        .iter()
        .filter(|e| e.time_ms.is_none() && e.level <= Level::Error)
        .map(|e| e.record)
        .collect();
    json!({
        "now_ms": now,
        "stability_today": today.as_ref().and_then(|d| d.index),
        "formula": timeline::FORMULA,
        "days": tl,
        "problems": bucket::groups(events),
        "recent_failures": recent,
        "undated_failures": undated,
        "events": events.len(),
        "dropped": meta,
    })
}

/// The report in a few plain lines: what an agent says back, or a person reads first.
pub fn report_text(events: &[Event], now: i64, days: usize, off: i32) -> String {
    let tl = timeline::days(events, now, days, off);
    let mut o = String::new();
    if let Some(today) = tl.last() {
        let idx = today
            .index
            .map_or("no data yet".to_string(), |i| format!("{i:.1} / 10"));
        let _ = writeln!(
            o,
            "Stability today ({}): {idx}. Today: {} crashes or resets, {} app crashes, {} warnings.",
            today.date, today.juno_failures, today.app_failures, today.warnings
        );
    }
    let bad: u32 = tl.iter().map(|d| d.juno_failures + d.app_failures).sum();
    let _ = writeln!(o, "Last {days} days: {bad} failures.");
    let recent: Vec<&Event> = events
        .iter()
        .rev()
        .filter(|e| e.level <= Level::Error)
        .take(8)
        .collect();
    if !recent.is_empty() {
        let _ = writeln!(o, "Recent failures (lucina-evlog show <n> for each):");
    }
    for e in recent {
        let _ = writeln!(
            o,
            "  #{} {} {}: {}",
            e.record,
            when(e, off),
            title(e),
            e.summary
        );
        if let Some(c) = &e.cause {
            let _ = writeln!(
                o,
                "      likely cause ({}): {}",
                c.confidence.name(),
                c.text
            );
        }
    }
    for g in bucket::groups(events)
        .iter()
        .filter(|g| g.count > 1)
        .take(5)
    {
        let boots: Vec<String> = g.boots.iter().map(u32::to_string).collect();
        let _ = writeln!(
            o,
            "Repeats: {} x{} (boots {})",
            g.bucket.label,
            g.count,
            boots.join(", ")
        );
    }
    o
}
