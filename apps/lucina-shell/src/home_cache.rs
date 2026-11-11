//! The home's last known values: what each polled source last answered, kept on /data so that the cards are on screen at the
//! first frame of the next boot, before the network is up (6 s on Ethernet, 25-36 s on Wi-Fi).
//!
//! Only values a real source once answered are written (docs/design/rules.md, rule 8: no sample text). The calendar, the
//! decisions, the agents and the tasks are cached. The event hub, the player and the uploads are not: they are local, answer in
//! milliseconds, and their words ("Just now") would only be wrong when read back.
//!
//! * Format: `/data/lucina/home-cache/<source>.json` = `{"v":1,"saved_ms":<unix ms>,"kind":"<source>","update":{...}}`, where
//!   `update` holds exactly the fields of the source's [`Update`]. The conversion is explicit (no derive), both ways.
//! * A file older than 24 h is not shown. Before the clock is set (Juno starts in 1970) a file's age cannot be told, so it is
//!   shown, and the first fresh answer or the 60 s rule below replaces it; nothing is written while the clock is not set.
//! * [`Shared`] decides what a live update does to a cached card: a fresh answer replaces it, and a `Gone` does not remove it
//!   during the first 60 s after the feed started (the network is likely still coming up).

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use lucina_glass::home::{Build, Day, Deadline, Dot, Note, Week};
use serde_json::{Value, json};

use crate::home_feed::{Source, Update};

/// Where the values are kept.
pub const DIR: &str = "/data/lucina/home-cache";
/// A cached value older than this is not shown.
pub const MAX_AGE_MS: i64 = 24 * 60 * 60 * 1000;
/// After the feed started, a `Gone` does not remove a card that came from the cache for this long.
pub const GONE_GRACE: Duration = Duration::from_secs(60);
const VERSION: u64 = 1;

/// The cache key of a source: also the file's name.
pub fn key_of_source(s: Source) -> &'static str {
    match s {
        Source::Calendar => "calendar",
        Source::Decisions => "decisions",
        Source::Agents => "agents",
        Source::Tasks => "tasks",
    }
}

/// The cache key of an update, `None` for the ones that are never cached.
pub fn key_of(u: &Update) -> Option<&'static str> {
    match u {
        Update::Calendar { .. } => Some("calendar"),
        Update::Decisions { .. } => Some("decisions"),
        Update::Agents { .. } => Some("agents"),
        Update::Tasks { .. } => Some("tasks"),
        Update::Gone(s) => Some(key_of_source(*s)),
        Update::Events { .. } | Update::Media(_) | Update::Uploads(_) => None,
    }
}

// ---- Update <-> JSON, field by field ----

fn dot_name(d: Dot) -> &'static str {
    match d {
        Dot::Own => "own",
        Dot::Other => "other",
        Dot::Now => "now",
    }
}

fn dot_of(s: &str) -> Option<Dot> {
    Some(match s {
        "own" => Dot::Own,
        "other" => Dot::Other,
        "now" => Dot::Now,
        _ => return None,
    })
}

fn str_of(v: &Value, k: &str) -> Option<String> {
    v.get(k)?.as_str().map(String::from)
}

fn u32_of(v: &Value, k: &str) -> Option<u32> {
    u32::try_from(v.get(k)?.as_u64()?).ok()
}

fn bool_of(v: &Value, k: &str) -> Option<bool> {
    v.get(k)?.as_bool()
}

/// A field that is a string or null: the outer `None` is "malformed", the inner one "null".
fn opt_str_of(v: &Value, k: &str) -> Option<Option<String>> {
    match v.get(k)? {
        Value::Null => Some(None),
        Value::String(s) => Some(Some(s.clone())),
        _ => None,
    }
}

fn note_json(n: &Note) -> Value {
    json!({"id": n.id, "fact": n.fact, "line": n.line, "urgent": n.urgent, "app": n.app, "at": n.at})
}

fn note_of(v: &Value) -> Option<Note> {
    Some(Note {
        id: str_of(v, "id")?,
        fact: str_of(v, "fact")?,
        line: str_of(v, "line")?,
        urgent: bool_of(v, "urgent")?,
        app: opt_str_of(v, "app")?,
        at: match v.get("at")? {
            Value::Null => None,
            n => Some(n.as_i64()?),
        },
    })
}

fn notes_of(v: &Value, k: &str) -> Option<Vec<Note>> {
    v.get(k)?.as_array()?.iter().map(note_of).collect()
}

fn pairs_json(p: &[(String, String)]) -> Value {
    Value::Array(p.iter().map(|(a, b)| json!([a, b])).collect())
}

fn pairs_of(v: &Value, k: &str) -> Option<Vec<(String, String)>> {
    v.get(k)?
        .as_array()?
        .iter()
        .map(|p| {
            let p = p.as_array()?;
            Some((
                p.first()?.as_str()?.to_string(),
                p.get(1)?.as_str()?.to_string(),
            ))
        })
        .collect()
}

fn day_json(d: &Day) -> Value {
    json!({
        "n": d.n, "name": d.name, "dots": d.dots.iter().map(|x| dot_name(*x)).collect::<Vec<_>>(),
        "today": d.today, "hours": d.hours, "ranges": d.ranges,
    })
}

fn day_of(v: &Value) -> Option<Day> {
    Some(Day {
        n: u32_of(v, "n")?,
        name: str_of(v, "name")?,
        dots: v
            .get("dots")?
            .as_array()?
            .iter()
            .map(|d| dot_of(d.as_str()?))
            .collect::<Option<_>>()?,
        today: bool_of(v, "today")?,
        hours: v
            .get("hours")?
            .as_array()?
            .iter()
            .map(|h| u32::try_from(h.as_u64()?).ok())
            .collect::<Option<_>>()?,
        ranges: str_of(v, "ranges")?,
    })
}

fn week_json(w: &Week) -> Value {
    json!({
        "days": w.days.iter().map(day_json).collect::<Vec<_>>(),
        "selected": w.selected, "first_hour": w.first_hour, "last_hour": w.last_hour,
    })
}

fn week_of(v: &Value) -> Option<Week> {
    Some(Week {
        days: v
            .get("days")?
            .as_array()?
            .iter()
            .map(day_of)
            .collect::<Option<_>>()?,
        selected: usize::try_from(v.get("selected")?.as_u64()?).ok()?,
        first_hour: u32_of(v, "first_hour")?,
        last_hour: u32_of(v, "last_hour")?,
    })
}

fn deadline_json(d: &Deadline) -> Value {
    json!({"fact": d.fact, "line": d.line, "soon": d.soon})
}

fn deadline_of(v: &Value) -> Option<Deadline> {
    Some(Deadline {
        fact: str_of(v, "fact")?,
        line: str_of(v, "line")?,
        soon: bool_of(v, "soon")?,
    })
}

fn build_json(b: &Build) -> Value {
    json!({
        "title": b.title, "phase": b.phase, "phases": b.phases, "percent": b.percent,
        "live": b.live, "line": b.line, "app": b.app,
    })
}

fn build_of(v: &Value) -> Option<Build> {
    Some(Build {
        title: str_of(v, "title")?,
        phase: u32_of(v, "phase")?,
        phases: u32_of(v, "phases")?,
        percent: u32_of(v, "percent")?,
        live: bool_of(v, "live")?,
        line: str_of(v, "line")?,
        app: opt_str_of(v, "app")?,
    })
}

/// `null` stays `null`; anything else goes through `f`. The outer `None` is "malformed".
fn optional<T>(v: &Value, k: &str, f: impl Fn(&Value) -> Option<T>) -> Option<Option<T>> {
    match v.get(k)? {
        Value::Null => Some(None),
        x => f(x).map(Some),
    }
}

fn ms_of(t: SystemTime) -> Option<i64> {
    i64::try_from(t.duration_since(UNIX_EPOCH).ok()?.as_millis()).ok()
}

/// The file's text for a cacheable update, `None` for one that is never cached (and for a `Gone`).
pub fn encode(u: &Update, saved_ms: i64) -> Option<String> {
    let kind = key_of(u)?;
    let update = match u {
        Update::Calendar {
            deadline,
            week,
            next_start,
        } => json!({
            "deadline": deadline.as_ref().map(deadline_json),
            "week": week.as_ref().map(week_json),
            "next_start_ms": next_start.and_then(ms_of),
        }),
        Update::Decisions { pending, notes } => {
            json!({"pending": pending, "notes": notes.iter().map(note_json).collect::<Vec<_>>()})
        }
        Update::Agents { running, build } => {
            json!({"running": running, "build": build.as_ref().map(build_json)})
        }
        Update::Tasks {
            due_today,
            overdue,
            today,
            overdue_tasks,
        } => json!({
            "due_today": due_today, "overdue": overdue,
            "today": pairs_json(today), "overdue_tasks": pairs_json(overdue_tasks),
        }),
        _ => return None,
    };
    Some(json!({"v": VERSION, "saved_ms": saved_ms, "kind": kind, "update": update}).to_string())
}

/// The update a file holds, if it is well formed, is `key`'s and is young enough. `clock_set` false (the 1970 clock): the age
/// cannot be told, so the value is shown and a stale calendar countdown is dropped.
pub fn decode(text: &str, key: &str, now_ms: i64, clock_set: bool) -> Option<Update> {
    let f: Value = serde_json::from_str(text).ok()?;
    if f.get("v")?.as_u64()? != VERSION || f.get("kind")?.as_str()? != key {
        return None;
    }
    let saved = f.get("saved_ms")?.as_i64()?;
    if clock_set {
        let age = now_ms - saved;
        // A file from the future (the clock was wrong when it was written) is not trusted either.
        if !(-60_000..=MAX_AGE_MS).contains(&age) {
            return None;
        }
    }
    let u = f.get("update")?;
    Some(match key {
        "calendar" => Update::Calendar {
            deadline: optional(u, "deadline", deadline_of)?,
            week: optional(u, "week", week_of)?,
            next_start: optional(u, "next_start_ms", |v| v.as_i64())?
                .filter(|&ms| clock_set && ms > now_ms)
                .map(|ms| UNIX_EPOCH + Duration::from_millis(ms as u64)),
        },
        "decisions" => Update::Decisions {
            pending: u32_of(u, "pending")?,
            notes: notes_of(u, "notes")?,
        },
        "agents" => Update::Agents {
            running: u32_of(u, "running")?,
            build: optional(u, "build", build_of)?,
        },
        "tasks" => Update::Tasks {
            due_today: u32_of(u, "due_today")?,
            overdue: u32_of(u, "overdue")?,
            today: pairs_of(u, "today")?,
            overdue_tasks: pairs_of(u, "overdue_tasks")?,
        },
        _ => return None,
    })
}

/// Every cached update in `dir` that can be shown now.
pub fn load_all(dir: &Path, now_ms: i64, clock_set: bool) -> Vec<Update> {
    ["calendar", "decisions", "agents", "tasks"]
        .iter()
        .filter_map(|key| {
            let text = std::fs::read_to_string(dir.join(format!("{key}.json"))).ok()?;
            decode(&text, key, now_ms, clock_set)
        })
        .collect()
}

/// Writes `text` as `dir/<key>.json`: a temporary file, fsync, rename. Errors are the caller's to ignore.
pub fn write_atomic(dir: &Path, key: &str, text: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!("{key}.json.tmp"));
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(text.as_bytes())?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, dir.join(format!("{key}.json")))
}

/// What the live and the cached updates do to each other, shared by every source thread.
pub struct Shared {
    started: Instant,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// Sources that have answered for real since the feed started.
    fresh: HashSet<&'static str>,
    /// Sources whose card on screen came from the cache.
    cached_shown: HashSet<&'static str>,
    /// The text last written for each source, so that an unchanged answer does not write again.
    written: HashMap<&'static str, String>,
}

impl Shared {
    pub fn new(started: Instant) -> Shared {
        Shared {
            started,
            state: Mutex::new(State::default()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Whether a live update goes on to the shell. A fresh answer replaces the cached card; a `Gone` does not remove a cached
    /// card in the first [`GONE_GRACE`] (the network is probably still coming up).
    pub fn admit_live(&self, u: &Update, now: Instant) -> bool {
        let Some(key) = key_of(u) else {
            return true;
        };
        let mut st = self.lock();
        if matches!(u, Update::Gone(_)) {
            if st.cached_shown.contains(key)
                && now.saturating_duration_since(self.started) < GONE_GRACE
            {
                return false;
            }
        } else {
            st.fresh.insert(key);
        }
        st.cached_shown.remove(key);
        true
    }

    /// Whether a cached update may be shown: not when its source has already answered for real.
    pub fn admit_cached(&self, u: &Update) -> bool {
        let Some(key) = key_of(u) else {
            return false;
        };
        let mut st = self.lock();
        if st.fresh.contains(key) {
            return false;
        }
        st.cached_shown.insert(key);
        true
    }

    /// Writes a real answer to `dir`, unless it is the one written last. Ignores errors.
    pub fn persist(&self, dir: &Path, u: &Update, saved_ms: i64) {
        let Some(key) = key_of(u) else {
            return;
        };
        let Some(text) = encode(u, saved_ms) else {
            return;
        };
        // The time stamp changes every time; what matters for "unchanged" is the rest.
        let body = encode(u, 0).unwrap_or_default();
        {
            let mut st = self.lock();
            if st.written.get(key) == Some(&body) {
                return;
            }
            st.written.insert(key, body);
        }
        if write_atomic(dir, key, &text).is_err() {
            self.lock().written.remove(key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000_000;

    fn day(n: u32) -> Day {
        Day {
            n,
            name: "Wed".into(),
            dots: vec![Dot::Now, Dot::Own, Dot::Other],
            today: n == 4,
            hours: vec![0, 30, 60, 0, 0, 0, 15, 0, 0, 0],
            ranges: "9–10, 14–15".into(),
        }
    }

    fn note(id: &str, app: Option<&str>, at: Option<i64>) -> Note {
        Note {
            id: id.into(),
            fact: "Ship the build…".into(),
            line: "Lucina asks".into(),
            urgent: at.is_some(),
            app: app.map(String::from),
            at,
        }
    }

    fn samples() -> Vec<Update> {
        vec![
            Update::Calendar {
                deadline: Some(Deadline {
                    fact: "Exam".into(),
                    line: "Tomorrow 09:30".into(),
                    soon: true,
                }),
                week: Some(Week {
                    days: (1..=7).map(day).collect(),
                    selected: 2,
                    first_hour: 8,
                    last_hour: 18,
                }),
                next_start: Some(UNIX_EPOCH + Duration::from_millis(NOW as u64 + 600_000)),
            },
            Update::Calendar {
                deadline: None,
                week: None,
                next_start: None,
            },
            Update::Decisions {
                pending: 2,
                notes: vec![
                    note("decision:1", Some("decisions"), None),
                    note("decision:2", None, Some(5)),
                ],
            },
            Update::Decisions {
                pending: 0,
                notes: vec![],
            },
            Update::Agents {
                running: 3,
                build: Some(Build {
                    title: "Home cache".into(),
                    phase: 2,
                    phases: 5,
                    percent: 40,
                    live: true,
                    line: "Testing".into(),
                    app: Some("agents".into()),
                }),
            },
            Update::Agents {
                running: 0,
                build: None,
            },
            Update::Tasks {
                due_today: 2,
                overdue: 1,
                today: vec![
                    ("t1".into(), "Buy milk".into()),
                    ("t2".into(), "Call".into()),
                ],
                overdue_tasks: vec![("t0".into(), "Pay rent".into())],
            },
        ]
    }

    #[test]
    fn every_cached_update_round_trips() {
        for u in samples() {
            let key = key_of(&u).unwrap();
            let text = encode(&u, NOW - 1000).unwrap();
            assert_eq!(decode(&text, key, NOW, true), Some(u.clone()), "{key}");
        }
    }

    #[test]
    fn uncached_updates_and_gone_are_never_encoded() {
        assert!(encode(&Update::Events { notes: vec![] }, NOW).is_none());
        assert!(encode(&Update::Media(None), NOW).is_none());
        assert!(encode(&Update::Uploads(None), NOW).is_none());
        assert!(encode(&Update::Gone(Source::Tasks), NOW).is_none());
    }

    #[test]
    fn a_value_older_than_24_hours_is_not_shown() {
        let u = Update::Agents {
            running: 1,
            build: None,
        };
        let just_in = encode(&u, NOW - MAX_AGE_MS).unwrap();
        let too_old = encode(&u, NOW - MAX_AGE_MS - 1).unwrap();
        assert_eq!(decode(&just_in, "agents", NOW, true), Some(u));
        assert_eq!(decode(&too_old, "agents", NOW, true), None);
        // From the future: not trusted.
        let future = encode(
            &Update::Decisions {
                pending: 0,
                notes: vec![],
            },
            NOW + 3_600_000,
        )
        .unwrap();
        assert_eq!(decode(&future, "decisions", NOW, true), None);
    }

    #[test]
    fn without_a_clock_the_age_is_unknown_and_the_value_is_shown() {
        let u = samples().remove(0);
        let text = encode(&u, NOW - 10 * MAX_AGE_MS).unwrap();
        match decode(&text, "calendar", 0, false) {
            Some(Update::Calendar {
                week, next_start, ..
            }) => {
                assert!(week.is_some());
                assert_eq!(
                    next_start, None,
                    "a countdown cannot be told without a clock"
                );
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn wrong_kind_version_or_garbage_is_ignored() {
        let text = encode(
            &Update::Agents {
                running: 1,
                build: None,
            },
            NOW,
        )
        .unwrap();
        assert_eq!(decode(&text, "tasks", NOW, true), None);
        assert_eq!(
            decode(&text.replace("\"v\":1", "\"v\":2"), "agents", NOW, true),
            None
        );
        assert_eq!(decode("{", "agents", NOW, true), None);
        assert_eq!(
            decode(
                r#"{"v":1,"kind":"agents","saved_ms":0,"update":{}}"#,
                "agents",
                NOW,
                false
            ),
            None
        );
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("home-cache-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn files_are_written_atomically_and_read_back() {
        let dir = scratch("files");
        let shared = Shared::new(Instant::now());
        let all = samples();
        for u in [&all[0], &all[2], &all[4], &all[6]] {
            shared.persist(&dir, u, NOW - 5);
        }
        assert!(!dir.join("calendar.json.tmp").exists());
        let mut back = load_all(&dir, NOW, true);
        back.sort_by_key(key_of);
        assert_eq!(back.len(), 4);
        for u in [&all[0], &all[2], &all[4], &all[6]] {
            assert!(back.contains(u));
        }
        // Old files are skipped by the loader.
        assert!(load_all(&dir, NOW + 2 * MAX_AGE_MS, true).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unchanged_answer_is_not_written_again() {
        let dir = scratch("same");
        let shared = Shared::new(Instant::now());
        let u = Update::Agents {
            running: 1,
            build: None,
        };
        shared.persist(&dir, &u, 111);
        shared.persist(&dir, &u, 222);
        let text = std::fs::read_to_string(dir.join("agents.json")).unwrap();
        assert!(text.contains("\"saved_ms\":111"), "{text}");
        shared.persist(
            &dir,
            &Update::Agents {
                running: 2,
                build: None,
            },
            333,
        );
        assert!(
            std::fs::read_to_string(dir.join("agents.json"))
                .unwrap()
                .contains("333")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn gone_keeps_a_cached_card_for_the_first_60_seconds_only() {
        let t0 = Instant::now();
        let s = Shared::new(t0);
        let cached = Update::Tasks {
            due_today: 1,
            overdue: 0,
            today: vec![],
            overdue_tasks: vec![],
        };
        assert!(s.admit_cached(&cached));
        // Within the grace: held back. Another source's Gone is not.
        assert!(!s.admit_live(&Update::Gone(Source::Tasks), t0 + Duration::from_secs(59)));
        assert!(s.admit_live(&Update::Gone(Source::Agents), t0 + Duration::from_secs(10)));
        // After it: removes the card, as today.
        assert!(s.admit_live(&Update::Gone(Source::Tasks), t0 + Duration::from_secs(60)));
    }

    #[test]
    fn gone_removes_a_card_that_did_not_come_from_the_cache() {
        let t0 = Instant::now();
        let s = Shared::new(t0);
        assert!(s.admit_live(&Update::Gone(Source::Calendar), t0));
        // A fresh answer replaced the cached card; a later Gone is a real one even inside the 60 s.
        let cached = Update::Agents {
            running: 1,
            build: None,
        };
        assert!(s.admit_cached(&cached));
        assert!(s.admit_live(
            &Update::Agents {
                running: 2,
                build: None
            },
            t0 + Duration::from_secs(5)
        ));
        assert!(s.admit_live(&Update::Gone(Source::Agents), t0 + Duration::from_secs(6)));
    }

    #[test]
    fn a_cache_read_late_never_replaces_a_fresh_answer() {
        let t0 = Instant::now();
        let s = Shared::new(t0);
        assert!(s.admit_live(
            &Update::Agents {
                running: 2,
                build: None
            },
            t0
        ));
        assert!(!s.admit_cached(&Update::Agents {
            running: 1,
            build: None
        }));
        assert!(s.admit_cached(&Update::Tasks {
            due_today: 0,
            overdue: 0,
            today: vec![],
            overdue_tasks: vec![]
        }));
    }
}
