//! What a boot reads in, once, early: the saved crash log (`/sys/fs/pstore`, read only: this never erases a record or writes
//! the region) and this boot's PMIC reasons. The saved log keeps 8 records and one boot's console; the event log keeps them
//! for years. Reading the same records again adds nothing, so `ingest` runs at every boot and on demand.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use lucina_watchd::Status;

use crate::analyze::{self, Context, Saved};
use crate::epoch::{self, State};
use crate::event::Event;
use crate::keys;
use crate::pon::{self, Pon};
use crate::providers::{self, id};
use crate::record;
use crate::record_text;
use crate::store::{self, Added, Store};

pub const PSTORE: &str = "/sys/fs/pstore";

/// Everything the ingest reads, so tests can hand it a made-up machine.
#[derive(Clone, Debug)]
pub struct Sources {
    pub pstore: PathBuf,
    /// This boot's kernel log so far (from /dev/kmsg), for the PON line.
    pub kmsg: String,
    pub uptime_ms: u64,
    pub now_ms: i64,
    /// `/proc/version`'s build, e.g. `5.13.0-LucinaJuno juno-9672e3f5ecc1`.
    pub build: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub boot: Option<u32>,
    /// The event log's epoch of `boot` (patch 1370).
    pub epoch: u32,
    pub added: Vec<u64>,
    pub known: usize,
    pub pon_found: bool,
}

/// The build in `/proc/version`: its release and the word after it that names the build.
pub fn build_of(version: &str) -> Option<String> {
    let mut words = version.split_ascii_whitespace();
    let release = words.find(|w| w.contains("Lucina"))?;
    let extra = version
        .split_ascii_whitespace()
        .find(|w| w.starts_with("juno-"));
    Some(match extra {
        Some(e) => format!("{release} {e}"),
        None => release.to_string(),
    })
}

/// The last boot-started event of `boot` in `epoch`, if the log has one.
fn boot_event(events: &[Event], epoch: u32, boot: u32) -> Option<&Event> {
    events.iter().rev().find(|e| {
        e.provider == providers::KERNEL_POWER
            && e.id == id::BOOT_STARTED
            && e.boot == Some(boot)
            && e.epoch == epoch
    })
}

/// Where the epoch rule's state is kept, in the event log's directory.
pub const EPOCH_FILE: &str = "epoch.json";

/// The persisted epoch state, if the file is there and reads.
pub fn load_epoch_state(dir: &Path) -> Option<State> {
    let v: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.join(EPOCH_FILE)).ok()?).ok()?;
    let known = v["known"]
        .as_array()?
        .iter()
        .map(|p| {
            let id = u64::from_str_radix(p[0].as_str()?, 16).ok()?;
            Some((id, u32::try_from(p[1].as_u64()?).ok()?))
        })
        .collect::<Option<Vec<_>>>()?;
    Some(State {
        epoch: u32::try_from(v["epoch"].as_u64()?).ok()?,
        last_boot: v["last_boot"].as_u64().and_then(|b| u32::try_from(b).ok()),
        known,
    })
}

/// Writes the state to a temporary file and renames it over the old one.
pub fn save_epoch_state(dir: &Path, state: &State) -> io::Result<()> {
    let known: Vec<_> = state
        .known
        .iter()
        .map(|(id, e)| serde_json::json!([format!("{id:016x}"), e]))
        .collect();
    let v = serde_json::json!({"epoch": state.epoch, "last_boot": state.last_boot, "known": known});
    store::write_atomic(
        &dir.join(EPOCH_FILE),
        &serde_json::to_vec(&v).map_err(io::Error::other)?,
    )
}

/// The state that a log from before this code implies: its newest epoch and the last boot counter in it. `None` for a log
/// with no boot in it.
fn legacy_state(events: &[Event]) -> Option<State> {
    let starts = || {
        events.iter().filter(|e| {
            e.provider == providers::KERNEL_POWER && e.id == id::BOOT_STARTED && e.boot.is_some()
        })
    };
    let epoch = starts().map(|e| e.epoch).max()?;
    let last_boot = starts()
        .rev()
        .find(|e| e.epoch == epoch)
        .and_then(|e| e.boot);
    Some(State {
        epoch,
        last_boot,
        known: Vec::new(),
    })
}

/// The epoch of this status, and the state to keep. The first run with this code, on a log that already has boots in it, has no
/// state: the log's last counter stands in for it, so that a status whose counter is lower starts a new epoch, and the
/// store's id is bound to the epoch chosen.
fn choose_epoch(dir: &Path, status: &Status) -> (u32, State) {
    if let Some(state) = load_epoch_state(dir) {
        return epoch::next(&state, status.epoch, status.boot);
    }
    match legacy_state(&store::read_dir_events(dir)) {
        Some(legacy) => {
            let (e, after) = epoch::next(&legacy, None, status.boot);
            let after = match status.epoch {
                Some(id) => epoch::bind(&after, id, e),
                None => after,
            };
            (e, after)
        }
        None => epoch::next(&State::default(), status.epoch, status.boot),
    }
}

/// Says on the boot-started event that this boot moved the saved crash log from format `from` to 4.
fn note_format_upgrade(e: &mut Event, from: u32) {
    e.fields
        .insert("saved_log_format_from".into(), serde_json::json!(from));
    e.fields
        .insert("saved_log_format".into(), serde_json::json!(4));
    e.summary.push_str(&format!(
        " The saved crash log moved from format {from} to 4 on this boot; a kernel from before patch 1213 that boots later lays it out again and loses the saved records."
    ));
}

/// Reads what the machine kept and writes what is new.
pub fn ingest(store: &mut Store, src: &Sources) -> io::Result<Report> {
    let mut report = Report::default();
    let pon: Option<Pon> = src
        .kmsg
        .lines()
        .find_map(pon::parse_line)
        .and_then(|r| r.pon());
    report.pon_found = pon.is_some();
    let start_ms = src.now_ms - src.uptime_ms as i64;
    let status = fs::read_to_string(src.pstore.join("status"))
        .ok()
        .map(|t| Status::parse(&t));
    let add = |store: &mut Store, report: &mut Report, e: Event| -> io::Result<()> {
        match store.append(e, src.now_ms)? {
            Added::New(r) => report.added.push(r),
            Added::Known(_) => report.known += 1,
        }
        Ok(())
    };

    let Some(status) = status else {
        // No saved log on this boot: only the boot itself, keyed by its start minute.
        let mut e = analyze::boot_started(
            0,
            src.now_ms,
            src.uptime_ms,
            src.build.clone(),
            pon,
            "unknown",
        );
        e.boot = None;
        e.key = Some(format!("boot:t{}", start_ms / 60_000));
        e.summary = e.summary.replace(", boot 0", "");
        add(store, &mut report, e)?;
        return Ok(report);
    };
    let boot = status.boot;
    report.boot = Some(boot);
    // Saved before anything is added: reading the same status again gives the same epoch.
    let (epoch, state) = choose_epoch(store.dir(), &status);
    save_epoch_state(store.dir(), &state)?;
    report.epoch = epoch;
    let mut started = analyze::boot_started_in(
        epoch,
        boot,
        src.now_ms,
        src.uptime_ms,
        src.build.clone(),
        pon,
        &status.last_boot,
    );
    if let Some(from) = status.format_upgraded_from {
        note_format_upgrade(&mut started, from);
    }
    add(store, &mut report, started)?;

    // The log's own events, for the boots' clocks and PON readings of earlier boots.
    let known = store::read_dir_events(store.dir());
    if status.last_boot == "clean" && boot > 0 {
        let console = fs::read(src.pstore.join("console-lucina-0")).unwrap_or_default();
        let lines: Vec<_> = String::from_utf8_lossy(&console)
            .lines()
            .map(record::parse_log_line)
            .collect();
        let tail = &lines[lines.len().saturating_sub(200)..];
        let mut e = analyze::clean_end_in(epoch, boot - 1, tail, Some(start_ms));
        if let Some(b) = boot_event(&known, epoch, boot - 1)
            && let (Some(t), Some(up)) = (b.time_ms, e.uptime_ms)
        {
            e.time_ms = Some(t + up as i64);
        }
        add(store, &mut report, e)?;
    }
    for s in &status.saved {
        let key = keys::saved_key(epoch, s.boot, s.seq);
        if store.has_key(&key).is_some() {
            report.known += 1;
            continue;
        }
        let Ok(text) = fs::read(src.pstore.join(format!("dmesg-lucina-{}", s.slot))) else {
            continue;
        };
        let mut ctx = Context {
            epoch,
            ..Context::default()
        };
        if s.boot + 1 == boot {
            ctx.next_pon = pon;
            ctx.next_pon_source = Some(format!("boot {boot}'s kernel log"));
            ctx.next_boot_start_ms = Some(start_ms);
        } else if let Some(next) = boot_event(&known, epoch, s.boot + 1) {
            ctx.next_pon = analyze::pon_of(next);
            ctx.next_pon_source = Some(format!("boot {}'s kernel log", s.boot + 1));
            ctx.next_boot_start_ms = next.time_ms;
        }
        ctx.boot_start_ms = boot_event(&known, epoch, s.boot).and_then(|e| e.time_ms);
        let mut e = analyze::saved_event(&Saved::from(s), &text, &ctx);
        e.source = Some(
            src.pstore
                .join(format!("dmesg-lucina-{}", s.slot))
                .display()
                .to_string(),
        );
        // The whole record, which the region will lose (patch 1370's follow-up); the event's summary and 60 log lines stay either way.
        e.text_file = record_text::keep(store.dir(), &key, &text).ok();
        add(store, &mut report, e)?;
    }
    Ok(report)
}

/// Records a boot's start that the log missed (a boot from before the log existed), from a time found elsewhere: makes the
/// resets of earlier boots readable on the timeline. `source` says where the time came from.
pub fn backfill_boot(
    store: &mut Store,
    epoch: u32,
    boot: u32,
    start_ms: i64,
    pon_line: Option<&str>,
    source: &str,
    now_ms: i64,
) -> io::Result<Added> {
    let pon = pon_line.and_then(pon::parse_line).and_then(|r| r.pon());
    let mut e = analyze::boot_started_in(epoch, boot, start_ms, 0, None, pon, "unknown");
    e.time_estimated = true;
    e.fields.insert("time_from".into(), source.into());
    store.append(e, now_ms)
}

/// The pstore directory's records, with their text, for `lucina-evlog import` of a dump made elsewhere: `=== record <slot>`
/// sections and a `status` file's lines.
pub fn import_dump(
    store: &mut Store,
    status_text: &str,
    dump: &str,
    now_ms: i64,
) -> io::Result<Report> {
    let status = Status::parse(status_text);
    let mut report = Report {
        boot: Some(status.boot),
        ..Default::default()
    };
    let known = store::read_dir_events(store.dir());
    let sections = record::split_dump(dump);
    for s in &status.saved {
        let Some((_, text)) = sections.iter().find(|(slot, _)| *slot == s.slot) else {
            continue;
        };
        let mut ctx = Context::default();
        if let Some(next) = boot_event(&known, 0, s.boot + 1) {
            ctx.next_pon = analyze::pon_of(next);
            ctx.next_pon_source = Some(format!("boot {}'s kernel log", s.boot + 1));
            ctx.next_boot_start_ms = next.time_ms;
        }
        ctx.boot_start_ms = boot_event(&known, 0, s.boot).and_then(|e| e.time_ms);
        let mut e = analyze::saved_event(&Saved::from(s), text.as_bytes(), &ctx);
        e.source = Some("imported dump".into());
        if let Some(key) = &e.key {
            e.text_file = record_text::keep(store.dir(), key, text.as_bytes()).ok();
        }
        match store.append(e, now_ms)? {
            Added::New(r) => report.added.push(r),
            Added::Known(_) => report.known += 1,
        }
    }
    Ok(report)
}

pub fn pstore_dir() -> &'static Path {
    Path::new(PSTORE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sample;
    use crate::store::Retention;

    fn machine(name: &str) -> (PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!("evlog-ingest-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let ps = root.join("pstore");
        fs::create_dir_all(&ps).unwrap();
        fs::write(
            ps.join("status"),
            "boot 17\ncrashes_in_a_row 1\nlast_boot vanished\n\
             record 2 seq 40 boot 11 kind panic len 3000 reason Heap allocation error, layout = Layout {\n\
             record 3 seq 41 boot 16 kind unexplained reset len 9000 reason nothing recorded a crash\n",
        )
        .unwrap();
        fs::write(ps.join("dmesg-lucina-2"), sample::PANIC_HEAP).unwrap();
        fs::write(ps.join("dmesg-lucina-3"), sample::RESET_STARTUP).unwrap();
        (root.join("events"), ps)
    }

    const KEY: &str = "[    0.150000] power: PON reasons 08: 03 00 00 00 00 00 00 00 c0: c1 00 00 00 80 02 00 80 00 08 80 00 00 00 00 00";

    #[test]
    fn a_boot_after_a_reset() {
        let (dir, ps) = machine("reset");
        let src = Sources {
            pstore: ps.clone(),
            kmsg: format!("x\n{KEY}\n"),
            uptime_ms: 30_000,
            now_ms: 1_000_000_000,
            build: Some("b".into()),
        };
        let mut s = Store::open(&dir, Retention::default()).unwrap();
        let r = ingest(&mut s, &src).unwrap();
        assert_eq!(r.boot, Some(17));
        assert!(r.pon_found);
        assert_eq!(r.added.len(), 3, "boot started, the panic, the reset");
        let events = store::read_dir_events(&dir);
        let reset = events
            .iter()
            .find(|e| e.id == id::UNEXPECTED_RESET)
            .unwrap();
        assert_eq!(reset.boot, Some(16));
        assert_eq!(
            reset.time_ms,
            Some(1_000_000_000 - 30_000 - analyze::FIRMWARE_RESTART_MS)
        );
        assert!(
            reset.evidence.iter().any(|e| e.fact.contains("power key")),
            "{:#?}",
            reset.evidence
        );
        let panic = events.iter().find(|e| e.id == id::KERNEL_CRASH).unwrap();
        assert!(panic.summary.contains("256 KiB"), "{}", panic.summary);
        assert!(panic.time_ms.is_none(), "boot 11's start is unknown");
        // Again: nothing new.
        let r = ingest(&mut s, &src).unwrap();
        assert!(r.added.is_empty());
        assert_eq!(r.known, 3);
        fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }

    #[test]
    fn a_format_upgrade_is_said_on_the_boot_event() {
        let (dir, ps) = machine("upgrade");
        fs::write(
            ps.join("status"),
            "boot 18\ncrashes_in_a_row 0\nlast_boot clean\nformat_upgraded_from 3\n",
        )
        .unwrap();
        let src = Sources {
            pstore: ps,
            kmsg: String::new(),
            uptime_ms: 1000,
            now_ms: 2_000_000,
            build: None,
        };
        let mut s = Store::open(&dir, Retention::default()).unwrap();
        ingest(&mut s, &src).unwrap();
        let events = store::read_dir_events(&dir);
        let b = events.iter().find(|e| e.id == id::BOOT_STARTED).unwrap();
        assert_eq!(b.fields["saved_log_format_from"], 3);
        assert_eq!(b.fields["saved_log_format"], 4);
        assert!(b.summary.ends_with(" The saved crash log moved from format 3 to 4 on this boot; a kernel from before patch 1213 that boots later lays it out again and loses the saved records."), "{}", b.summary);
        fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }

    #[test]
    fn a_backfilled_boot_gives_old_records_a_time() {
        let (dir, ps) = machine("backfill");
        let mut s = Store::open(&dir, Retention::default()).unwrap();
        backfill_boot(&mut s, 0, 11, 500_000_000, None, "test", 0).unwrap();
        let src = Sources {
            pstore: ps,
            kmsg: String::new(),
            uptime_ms: 30_000,
            now_ms: 1_000_000_000,
            build: None,
        };
        ingest(&mut s, &src).unwrap();
        let events = store::read_dir_events(&dir);
        let panic = events.iter().find(|e| e.id == id::KERNEL_CRASH).unwrap();
        assert_eq!(panic.time_ms, Some(500_000_000 + 5_491_462));
        fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }

    #[test]
    fn a_clean_boot_says_who_ended_it() {
        let (dir, ps) = machine("clean");
        fs::write(
            ps.join("status"),
            "boot 18\ncrashes_in_a_row 0\nlast_boot clean\n",
        )
        .unwrap();
        fs::write(
            ps.join("console-lucina-0"),
            "[  455.885000] [kernel] lucina-shell (pid 751) asks for Restart\n",
        )
        .unwrap();
        let src = Sources {
            pstore: ps,
            kmsg: String::new(),
            uptime_ms: 1000,
            now_ms: 2_000_000,
            build: None,
        };
        let mut s = Store::open(&dir, Retention::default()).unwrap();
        ingest(&mut s, &src).unwrap();
        let events = store::read_dir_events(&dir);
        let end = events.iter().find(|e| e.id == id::SHUT_DOWN).unwrap();
        assert_eq!(
            end.summary,
            "Boot 17 ended cleanly: lucina-shell asked for restart."
        );
        assert_eq!(end.boot, Some(17));
        fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }

    #[test]
    fn builds() {
        assert_eq!(
            build_of("Linux version 5.13.0-LucinaJuno (nez@buildhost) juno-9672e3f5ecc1 #1")
                .as_deref(),
            Some("5.13.0-LucinaJuno juno-9672e3f5ecc1")
        );
        assert_eq!(build_of("nothing"), None);
    }

    /// One ingest of a made-up status at `now_ms`, 30 s after the boot began.
    fn boot_at(dir: &Path, ps: &Path, status: &str, now_ms: i64) -> Report {
        fs::write(ps.join("status"), status).unwrap();
        let src = Sources {
            pstore: ps.to_path_buf(),
            kmsg: String::new(),
            uptime_ms: 30_000,
            now_ms,
            build: None,
        };
        let mut s = Store::open(dir, Retention::default()).unwrap();
        ingest(&mut s, &src).unwrap()
    }

    const PANIC_RECORD: &str = "record 1 seq 4 boot 2 kind panic len 3000 reason Heap allocation error, layout = Layout {\n";

    #[test]
    fn a_counter_that_started_over_is_a_new_epoch_and_loses_nothing() {
        let (dir, ps) = machine("epoch");
        fs::write(ps.join("dmesg-lucina-1"), sample::PANIC_HEAP).unwrap();
        // Epoch 0 as the old code wrote it: boots 1 to 6, a panic in boot 2 (seen at boot 3), no epoch line, no state file.
        for b in 1..=6u32 {
            let status = if b >= 3 {
                format!("boot {b}\nlast_boot vanished\n{PANIC_RECORD}")
            } else {
                format!("boot {b}\nlast_boot clean\n")
            };
            boot_at(
                &dir,
                &ps,
                &status,
                1_000_000_000 + i64::from(b) * 100_000_000,
            );
        }
        fs::remove_file(dir.join(EPOCH_FILE)).unwrap();
        let before = store::read_dir_events(&dir);
        let old_panic = before
            .iter()
            .find(|e| e.id == id::KERNEL_CRASH)
            .unwrap()
            .clone();
        assert_eq!(old_panic.epoch, 0);
        assert_eq!(old_panic.key.as_deref(), Some("pstore:boot2:seq4"));

        // What the old keys would have dropped: the new boots 1, 2 and 3 of a region that started over.
        let dropped = (1..=3)
            .filter(|&b| {
                before
                    .iter()
                    .any(|e| e.key.as_deref() == Some(&keys::boot_key(0, b)))
            })
            .count();
        assert_eq!(dropped, 3, "boot-started events the old keys drop");

        // The saved log started over: its id is new, the counter is 3 and a record of boot 2 is in it.
        let status = format!("boot 3\nepoch 00000000000000aa\nlast_boot vanished\n{PANIC_RECORD}");
        let r = boot_at(&dir, &ps, &status, 2_000_000_000);
        assert_eq!(r.epoch, 1);
        assert_eq!(
            r.added.len(),
            2,
            "the boot and the record, both new under the new keys"
        );
        let events = store::read_dir_events(&dir);
        let start = events
            .iter()
            .find(|e| e.key.as_deref() == Some("boot:e1:3"))
            .unwrap();
        assert_eq!((start.epoch, start.boot), (1, Some(3)));
        assert_eq!(start.time_ms, Some(2_000_000_000 - 30_000));
        assert_eq!(start.activity.as_deref(), Some("boot:e1:3"));
        let panic = events
            .iter()
            .find(|e| e.key.as_deref() == Some("pstore:e1:boot2:seq4"))
            .unwrap();
        assert_eq!(panic.epoch, 1);
        // Its time comes from its own epoch, which has no boot 2: not from the old epoch's boot 2.
        assert_ne!(panic.time_ms, old_panic.time_ms);
        assert!(old_panic.time_ms.is_some());
        // The old entries are as they were.
        assert!(
            events
                .iter()
                .any(|e| e.key.as_deref() == Some("boot:3") && e.epoch == 0)
        );
        assert!(
            events
                .iter()
                .any(|e| e.key.as_deref() == Some("pstore:boot2:seq4") && e.epoch == 0)
        );

        // Reading the same status again adds nothing; the next boot and an old boot's id both stay in place.
        let again = boot_at(&dir, &ps, &status, 2_000_000_000);
        assert_eq!((again.epoch, again.added.len()), (1, 0));
        let next = boot_at(
            &dir,
            &ps,
            "boot 4\nepoch 00000000000000aa\nlast_boot clean\n",
            2_100_000_000,
        );
        assert_eq!((next.epoch, next.added.len()), (1, 2));
        fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }

    #[test]
    fn a_log_from_before_keeps_its_epoch_when_the_counter_goes_on() {
        let (dir, ps) = machine("epoch-same");
        for b in 1..=3u32 {
            boot_at(
                &dir,
                &ps,
                &format!("boot {b}\nlast_boot clean\n"),
                1_000_000_000 + i64::from(b) * 100_000_000,
            );
        }
        fs::remove_file(dir.join(EPOCH_FILE)).unwrap();
        // The first status with an id continues the counter: the same epoch, no copies of what is known.
        let r = boot_at(
            &dir,
            &ps,
            "boot 4\nepoch 00000000000000aa\nlast_boot clean\n",
            2_000_000_000,
        );
        assert_eq!(r.epoch, 0);
        let st = load_epoch_state(&dir).unwrap();
        assert_eq!(st.known, vec![(0xaa, 0)]);
        // A different id from here on is a new epoch.
        let r = boot_at(
            &dir,
            &ps,
            "boot 1\nepoch 00000000000000bb\nlast_boot unknown\n",
            2_100_000_000,
        );
        assert_eq!(r.epoch, 1);
        fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }

    #[test]
    fn a_boot_started_event_is_dated_by_its_own_epoch() {
        let mut a = analyze::boot_started_in(0, 3, 1_000, 0, None, None, "clean");
        let mut b = analyze::boot_started_in(1, 3, 9_000, 0, None, None, "clean");
        b.record = 2;
        a.record = 1;
        let mut late = analyze::boot_started_in(1, 3, 0, 0, None, None, "clean");
        late.key = Some("x".into());
        late.time_ms = None;
        late.uptime_ms = Some(500);
        let mut events = vec![a, b, late];
        store::fill_times(&mut events);
        assert_eq!(events[2].time_ms, Some(9_500));
    }

    #[test]
    fn the_full_record_is_kept_with_the_event_and_survives_a_restart() {
        let (dir, ps) = machine("fulltext");
        let src = Sources {
            pstore: ps.clone(),
            kmsg: String::new(),
            uptime_ms: 30_000,
            now_ms: 1_000_000_000,
            build: None,
        };
        let mut s = Store::open(&dir, Retention::default()).unwrap();
        ingest(&mut s, &src).unwrap();
        let events = store::read_dir_events(&dir);
        let panic = events.iter().find(|e| e.id == id::KERNEL_CRASH).unwrap();
        let name = panic.text_file.clone().expect("a text file");
        assert_eq!(name, "records/pstore_boot11_seq40.txt");
        assert_eq!(
            record_text::read(&dir, &name).unwrap(),
            sample::PANIC_HEAP.as_bytes()
        );
        // A new process (the store opened again) reads the same.
        drop(s);
        let events = store::read_dir_events(&dir);
        let again = events.iter().find(|e| e.id == id::KERNEL_CRASH).unwrap();
        assert_eq!(again.text_file.as_deref(), Some(name.as_str()));
        assert_eq!(
            record_text::read(&dir, &name).unwrap(),
            sample::PANIC_HEAP.as_bytes()
        );
        // Reading the same record again does not write it twice.
        let mut s = Store::open(&dir, Retention::default()).unwrap();
        ingest(&mut s, &src).unwrap();
        assert_eq!(fs::read_dir(dir.join(record_text::DIR)).unwrap().count(), 2);
        fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }

    #[test]
    fn a_region_restart_is_a_warning_with_its_cause() {
        let (dir, ps) = machine("restart");
        fs::write(
            ps.join("status"),
            "boot 1\ncrashes_in_a_row 0\nlast_boot unknown\nrecord 0 seq 0 boot 1 kind region restarted len 120 reason first use\n",
        )
        .unwrap();
        fs::write(
            ps.join("dmesg-lucina-0"),
            "The saved log's region was started over\nreason: first use\nthe records it held are lost\n",
        )
        .unwrap();
        let src = Sources {
            pstore: ps,
            kmsg: String::new(),
            uptime_ms: 5000,
            now_ms: 1_000_000_000,
            build: None,
        };
        let mut s = Store::open(&dir, Retention::default()).unwrap();
        ingest(&mut s, &src).unwrap();
        let events = store::read_dir_events(&dir);
        let e = events
            .iter()
            .find(|e| e.id == id::REGION_RESTARTED)
            .expect("an event");
        assert_eq!(
            (e.provider.as_str(), e.level),
            (providers::CRASH, crate::event::Level::Warning)
        );
        assert_eq!(
            e.summary,
            "The saved crash log was started over: first use; the records it held are lost."
        );
        assert_eq!(e.time_ms, Some(1_000_000_000 - 5000));
        assert!(providers::known(providers::CRASH, id::REGION_RESTARTED).is_some());
        assert!(e.text_file.is_some());
        fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }
}
