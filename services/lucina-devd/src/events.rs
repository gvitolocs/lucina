//! `lucina-eventd` from devd's side: the `events` tool, `wait` on an event or on the toolkit's presents, and the forwarder that
//! posts events to the server.
//!
//! ```text
//! POST /v1/device/juno/events   {"events": [...], "lost": n}     at most 64 events a post, at most a post a second
//! ```
//!
//! The forwarder keeps its cursor in RAM: a restart of devd starts from what eventd keeps. Its filter leaves out `proc.*` (every
//! exec, our own `bash` included); the `events` tool can still ask for them.
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use lucina_account::{ApiError, Client};
use lucina_events::Reader;
use lucina_events::wire::{Event, Filter};
use serde_json::{Value, json};

use crate::tools::Machine;

pub const MAX_POST: u64 = 64;
pub const POST_EVERY: Duration = Duration::from_secs(1);

fn reader(socket: &Path) -> Result<Reader, String> {
    Reader::connect(socket).map_err(|e| {
        format!(
            "lucina-eventd does not answer: {}",
            crate::places::plain(&e)
        )
    })
}

fn filter(kinds: Vec<String>, app: Option<String>) -> Filter {
    Filter {
        kind: kinds,
        app,
        ..Default::default()
    }
}

fn line(e: &Event) -> String {
    let mut s = format!("#{} {}", e.seq, e.kind);
    if let Some(a) = &e.app {
        s += &format!(" app={a}");
    }
    if let Some(sub) = &e.subject {
        s += &format!(" {sub}");
    }
    if !e.fields.is_empty() {
        s += &format!(" {}", Value::Object(e.fields.clone()));
    }
    s
}

/// The `events` tool.
pub fn read(
    m: &Machine,
    since: u64,
    kinds: Vec<String>,
    app: Option<String>,
    max: u64,
) -> Result<Value, String> {
    let mut r = reader(&m.events_socket)?;
    let b = r
        .read(since, &filter(kinds, app), Some(max.min(MAX_POST)))
        .map_err(|e| format!("lucina-eventd: {e}"))?;
    let mut text = format!(
        "{} events; next cursor {}{}{}",
        b.events.len(),
        b.next,
        if b.lost > 0 {
            format!("; {} lost", b.lost)
        } else {
            String::new()
        },
        if b.reset {
            "; the queue restarted, so the cursor starts over"
        } else {
            ""
        }
    );
    for e in &b.events {
        text += &format!("\n{}", line(e));
    }
    Ok(json!({"text": text, "events": b.events, "next": b.next, "lost": b.lost, "reset": b.reset}))
}

/// `wait {for, app}`: the first such event after now, or the timeout.
pub fn wait_for(
    m: &Machine,
    kinds: Vec<String>,
    app: Option<String>,
    timeout_ms: u64,
) -> Result<Value, String> {
    let mut r = reader(&m.events_socket)?;
    let f = filter(kinds, app);
    // Where the queue stands now: only what comes after the call counts.
    let now = r.stats().map_err(|e| format!("lucina-eventd: {e}"))?["next"]
        .as_u64()
        .unwrap_or(0);
    let w = r
        .wait(now, &f, timeout_ms)
        .map_err(|e| format!("lucina-eventd: {e}"))?;
    let text = match &w.event {
        Some(e) => format!("Came: {}", line(e)),
        None => format!("No such event within {timeout_ms} ms"),
    };
    Ok(
        json!({"text": text, "event": w.event, "timedOut": w.timeout, "next": w.next, "lost": w.lost}),
    )
}

/// `wait` without `since`, on a toolkit app: eventd's `settle` on its presents. `None` (use the framebuffer) when eventd does not
/// answer or nothing a toolkit drew came in the time.
pub fn settle(m: &Machine, settle_ms: u64, timeout_ms: u64) -> Option<Value> {
    let t0 = Instant::now();
    let mut r = Reader::connect(&m.events_socket).ok()?;
    // The toolkit's presents after this one count.
    let now = r.stats().ok()?["present_seq"].as_u64()?;
    let s = r.settle(now, settle_ms, timeout_ms).ok()?;
    if !s.ok || s.app.is_none() {
        return None;
    }
    let ms = t0.elapsed().as_millis() as u64;
    let app = s.app.unwrap_or_default();
    let what = if s.settled {
        "drew and then settled"
    } else {
        "was still drawing at the time limit"
    };
    Some(json!({
        "text": format!("{app} {what} ({ms} ms; from its present reports, not the screen's pixels)."),
        "changed": true, "settled": s.settled, "elapsedMs": ms, "app": app, "rect": s.rect, "presentSeq": s.present_seq,
    }))
}

/// What the forwarder sends: everything but `proc.*`.
pub fn forwarded(e: &Event) -> bool {
    !(e.kind == "proc" || e.kind.starts_with("proc."))
}

/// Posts events to the server until `stop`, while signed in. Never fails: eventd away or the server away is tried again later.
pub fn forward(socket: &Path, client: &dyn Fn() -> Option<Client>, stop: &AtomicBool) {
    let mut cursor = 0u64;
    let mut r: Option<Reader> = None;
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(POST_EVERY);
        let Some(c) = client() else { continue };
        if r.is_none() {
            r = Reader::connect(socket).ok();
        }
        let Some(reader) = r.as_mut() else { continue };
        let b = match reader.read(cursor, &Filter::default(), Some(MAX_POST)) {
            Ok(b) => b,
            Err(_) => {
                r = None;
                continue;
            }
        };
        if b.reset {
            cursor = 0;
        }
        let events: Vec<&Event> = b.events.iter().filter(|e| forwarded(e)).collect();
        if events.is_empty() && b.lost == 0 {
            cursor = b.next;
            continue;
        }
        match c.post_json_within(
            "/v1/device/juno/events",
            &json!({"events": events, "lost": b.lost}),
            Duration::from_secs(20),
        ) {
            Ok(_) => cursor = b.next,
            // Signed out or refused: the poll loop reports it; these events wait for the next sign-in.
            Err(ApiError::Unauthorized(_)) => {}
            Err(e) => eprintln!("lucina-devd: events: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lucina_events::wire::Source;

    fn ev(kind: &str) -> Event {
        Event {
            seq: 1,
            src: Source::Kernel,
            src_seq: 1,
            t_ms: 0,
            kind: kind.into(),
            app: None,
            subject: None,
            fields: Default::default(),
        }
    }

    #[test]
    fn the_forwarder_leaves_out_every_exec() {
        assert!(!forwarded(&ev("proc.exec")));
        assert!(!forwarded(&ev("proc.exit")));
        assert!(forwarded(&ev("app.opened")));
        assert!(forwarded(&ev("process.thing")));
    }

    #[test]
    fn without_eventd_the_tools_say_so_and_wait_uses_the_screen() {
        let (root, m, _) = crate::tools::tests::test_machine("noeventd");
        assert!(
            read(&m, 0, vec![], None, 10)
                .unwrap_err()
                .contains("does not answer")
        );
        assert!(settle(&m, 100, 100).is_none());
        assert!(m.missing("events").unwrap().contains("not running"));
        drop(root);
    }
}
