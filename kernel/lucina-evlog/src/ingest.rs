//! What a boot reads in, once, early: the saved crash log (`/sys/fs/pstore`, read only: this never erases a record or writes
//! the region) and this boot's PMIC reasons. The saved log keeps 8 records and one boot's console; the event log keeps them
//! for years. Reading the same records again adds nothing, so `ingest` runs at every boot and on demand.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use lucina_watchd::Status;

use crate::analyze::{self, Context, Saved};
use crate::event::Event;
use crate::pon::{self, Pon};
use crate::providers::{self, id};
use crate::record;
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

/// The last boot-started event of `boot`, if the log has one.
fn boot_event<'a>(events: &'a [Event], boot: u32) -> Option<&'a Event> {
    events.iter().rev().find(|e| {
        e.provider == providers::KERNEL_POWER && e.id == id::BOOT_STARTED && e.boot == Some(boot)
    })
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
    add(
        store,
        &mut report,
        analyze::boot_started(
            boot,
            src.now_ms,
            src.uptime_ms,
            src.build.clone(),
            pon,
            &status.last_boot,
        ),
    )?;

    // The log's own events, for the boots' clocks and PON readings of earlier boots.
    let known = store::read_dir_events(store.dir());
    if status.last_boot == "clean" && boot > 0 {
        let console = fs::read(src.pstore.join("console-lucina-0")).unwrap_or_default();
        let lines: Vec<_> = String::from_utf8_lossy(&console)
            .lines()
            .map(record::parse_log_line)
            .collect();
        let tail = &lines[lines.len().saturating_sub(200)..];
        let mut e = analyze::clean_end(boot - 1, tail, Some(start_ms));
        if let Some(b) = boot_event(&known, boot - 1) {
            if let (Some(t), Some(up)) = (b.time_ms, e.uptime_ms) {
                e.time_ms = Some(t + up as i64);
            }
        }
        add(store, &mut report, e)?;
    }
    for s in &status.saved {
        let key = analyze::saved_key(s.boot, s.seq);
        if store.has_key(&key).is_some() {
            report.known += 1;
            continue;
        }
        let Ok(text) = fs::read(src.pstore.join(format!("dmesg-lucina-{}", s.slot))) else {
            continue;
        };
        let mut ctx = Context::default();
        if s.boot + 1 == boot {
            ctx.next_pon = pon;
            ctx.next_pon_source = Some(format!("boot {boot}'s kernel log"));
            ctx.next_boot_start_ms = Some(start_ms);
        } else if let Some(next) = boot_event(&known, s.boot + 1) {
            ctx.next_pon = analyze::pon_of(next);
            ctx.next_pon_source = Some(format!("boot {}'s kernel log", s.boot + 1));
            ctx.next_boot_start_ms = next.time_ms;
        }
        ctx.boot_start_ms = boot_event(&known, s.boot).and_then(|e| e.time_ms);
        let mut e = analyze::saved_event(&Saved::from(s), &text, &ctx);
        e.source = Some(
            src.pstore
                .join(format!("dmesg-lucina-{}", s.slot))
                .display()
                .to_string(),
        );
        add(store, &mut report, e)?;
    }
    Ok(report)
}

/// Records a boot's start that the log missed (a boot from before the log existed), from a time found elsewhere: makes the
/// resets of earlier boots readable on the timeline. `source` says where the time came from.
pub fn backfill_boot(
    store: &mut Store,
    boot: u32,
    start_ms: i64,
    pon_line: Option<&str>,
    source: &str,
    now_ms: i64,
) -> io::Result<Added> {
    let pon = pon_line.and_then(pon::parse_line).and_then(|r| r.pon());
    let mut e = analyze::boot_started(boot, start_ms, 0, None, pon, "unknown");
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
        if let Some(next) = boot_event(&known, s.boot + 1) {
            ctx.next_pon = analyze::pon_of(next);
            ctx.next_pon_source = Some(format!("boot {}'s kernel log", s.boot + 1));
            ctx.next_boot_start_ms = next.time_ms;
        }
        ctx.boot_start_ms = boot_event(&known, s.boot).and_then(|e| e.time_ms);
        let mut e = analyze::saved_event(&Saved::from(s), text.as_bytes(), &ctx);
        e.source = Some("imported dump".into());
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
    fn a_backfilled_boot_gives_old_records_a_time() {
        let (dir, ps) = machine("backfill");
        let mut s = Store::open(&dir, Retention::default()).unwrap();
        backfill_boot(&mut s, 11, 500_000_000, None, "test", 0).unwrap();
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
}
