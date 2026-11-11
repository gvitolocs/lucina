//! `reliability`: the laptop's event log (kernel/lucina-evlog) for Lucina: today's stability, what failed lately with its likely
//! cause and evidence, the problems that repeat, or one event in full with its kernel log lines. Read only.

use std::path::PathBuf;

use lucina_evlog::{export, store};
use serde_json::{Value, json};

/// The log's folder; `LUCINA_EVLOG_DIR` moves it for tests.
fn dir() -> PathBuf {
    std::env::var_os("LUCINA_EVLOG_DIR").map_or_else(|| PathBuf::from(store::DEFAULT_DIR), PathBuf::from)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

/// The zone's offset east of UTC, as the rest of the machine sees it.
// musl marks time_t deprecated (its 64-bit change); localtime_r still takes it.
#[allow(deprecated)]
fn offset_s(now: i64) -> i32 {
    let t = (now / 1000) as libc::time_t;
    // SAFETY: localtime_r writes into the tm we own; both pointers are valid for the call.
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            return 0;
        }
        tm.tm_gmtoff as i32
    }
}

pub fn run(record: Option<u64>, days: u64) -> Result<Value, String> {
    let dir = dir();
    let events = store::read_dir_events(&dir);
    let now = now_ms();
    let off = offset_s(now);
    if let Some(r) = record {
        let e = events.iter().find(|e| e.record == r).ok_or(format!("no event #{r} in {}", dir.display()))?;
        return Ok(json!({"text": export::event_text(e, off), "event": e}));
    }
    if events.is_empty() {
        return Ok(json!({"text": format!("The event log in {} is empty: lucina-evlog has not run on this boot yet.", dir.display())}));
    }
    let days = days as usize;
    let mut report = export::report(&events, &store::read_meta(&dir), now, days, off);
    // Kernel log lines stay with `record`: the overview would be too long for a turn.
    if let Some(list) = report.get_mut("recent_failures").and_then(Value::as_array_mut) {
        for e in list {
            if let Some(o) = e.as_object_mut() {
                o.remove("log");
            }
        }
    }
    Ok(json!({"text": export::report_text(&events, now, days, off), "report": report}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_overview_and_one_event() {
        let d = std::env::temp_dir().join(format!("devd-evlog-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let mut s = store::Store::open(&d, store::Retention::default()).unwrap();
        for e in lucina_evlog::sample::events(now_ms()) {
            s.append(e, now_ms()).unwrap();
        }
        drop(s);
        // SAFETY of set_var: this test is the only one in the crate that reads LUCINA_EVLOG_DIR.
        unsafe { std::env::set_var("LUCINA_EVLOG_DIR", &d) };
        let v = run(None, 7).unwrap();
        let text = v["text"].as_str().unwrap();
        assert!(text.starts_with("Stability today"), "{text}");
        assert!(text.contains("likely cause"), "{text}");
        assert!(v["report"]["recent_failures"][0].get("log").is_none());
        let first = v["report"]["recent_failures"][0]["record"].as_u64().unwrap();
        let one = run(Some(first), 7).unwrap();
        assert!(one["text"].as_str().unwrap().contains("Kernel log"));
        assert!(run(Some(99_999), 7).is_err());
        std::fs::remove_dir_all(&d).unwrap();
    }
}
