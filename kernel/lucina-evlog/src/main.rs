//! lucina-evlog: Juno's event log, from a shell, over SSH and for agents (`--json` everywhere).
//!
//! ```text
//! lucina-evlog ingest                 read the saved crash log and this boot's PMIC reasons in (at every boot; idempotent)
//! lucina-evlog follow                 ingest, then record app crashes, links going down, low battery and kernel errors
//! lucina-evlog list [filters]         the events, newest first: --level critical|error|warning|info, --boot N,
//!                                     --since 7d|12h|<unix ms>, --provider P, --id N, --bucket ID, --text WORDS, --max N
//! lucina-evlog show <record>          one event in full: summary, likely cause, evidence, kernel log lines, details
//! lucina-evlog last-reset             the newest crash or unexpected reset in full
//! lucina-evlog timeline [--days N]    the days with their stability index and counts (Reliability Monitor)
//! lucina-evlog problems               the crash buckets: what keeps happening, how often, since when
//! lucina-evlog report [--days N]      everything an agent needs in one JSON: today's index, days, problems, recent failures
//! lucina-evlog backfill-boot <boot> <unix ms> [--source S] [--pon LINE]   a boot's start the log missed
//! lucina-evlog import <status> <dump> a pstore status file and a `=== record <slot>` dump taken elsewhere
//! lucina-evlog clear [--by WHO]       forgets every event, and says so in the log
//! ```
//! `--dir <path>` uses another log than /data/lucina/events; `--pstore <path>` another saved crash log (tests).

// Juno with no C library: Eyra is the libc and the program start, in Rust (docs/research/juno-pure-rust.md).
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
extern crate eyra;

use std::fs;
use std::io::{self, BufRead, Read};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use lucina_evlog::event::{Event, Level};
use lucina_evlog::export::{self, clock, title, when};
use lucina_evlog::follow::Follower;
use lucina_evlog::ingest::{self, Sources};
use lucina_evlog::store::{self, Filter, Retention, Store};
use lucina_evlog::{bucket, timeline};
use serde_json::json;

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

fn uptime_ms() -> u64 {
    fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|t| {
            t.split_whitespace()
                .next()
                .and_then(|s| s.parse::<f64>().ok())
        })
        .map_or(0, |s| (s * 1000.0) as u64)
}

/// This boot's kernel log so far, without waiting: /dev/kmsg opened non-blocking, records read until none is left.
fn kmsg_so_far() -> String {
    use std::os::unix::fs::OpenOptionsExt;
    let Ok(mut f) = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open("/dev/kmsg")
    else {
        return String::new();
    };
    let mut out = String::new();
    let mut buf = vec![0u8; 8192];
    loop {
        match f.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                out.push_str(&String::from_utf8_lossy(&buf[..n]));
                if !out.ends_with('\n') {
                    out.push('\n');
                }
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            // EAGAIN at the end; EPIPE when the ring overwrote what we were about to read: go on.
            Err(e) if e.raw_os_error() == Some(libc::EPIPE) => continue,
            Err(_) => break,
        }
        if out.len() > 4 << 20 {
            break;
        }
    }
    out
}

/// Whether `dir` may be written: a log under /data needs /data mounted (another device than /), or it would land on the
/// root file system and vanish. Waits up to `wait_s` for the mount, which comes a few seconds into a boot.
fn data_ready(dir: &std::path::Path, wait_s: u64) -> bool {
    use std::os::unix::fs::MetadataExt;
    if !dir.starts_with("/data") {
        return true;
    }
    for _ in 0..=wait_s {
        if let (Ok(d), Ok(r)) = (fs::metadata("/data"), fs::metadata("/"))
            && d.dev() != r.dev()
        {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    false
}

struct Args {
    rest: Vec<String>,
    dir: PathBuf,
    pstore: PathBuf,
    json: bool,
}

impl Args {
    fn parse() -> Args {
        let mut rest = Vec::new();
        let mut dir = PathBuf::from(store::DEFAULT_DIR);
        let mut pstore = PathBuf::from(ingest::PSTORE);
        let mut json = false;
        let mut it = std::env::args().skip(1);
        while let Some(a) = it.next() {
            match a.as_str() {
                "--dir" => dir = it.next().map(PathBuf::from).unwrap_or(dir),
                "--pstore" => pstore = it.next().map(PathBuf::from).unwrap_or(pstore),
                "--json" => json = true,
                _ => rest.push(a),
            }
        }
        Args {
            rest,
            dir,
            pstore,
            json,
        }
    }

    fn opt(&self, name: &str) -> Option<&str> {
        self.rest
            .iter()
            .position(|a| a == name)
            .and_then(|i| self.rest.get(i + 1))
            .map(String::as_str)
    }
}

fn since(arg: &str, now: i64) -> Option<i64> {
    let (n, unit) = arg.split_at(
        arg.trim_end_matches(|c: char| c.is_ascii_alphabetic())
            .len(),
    );
    let n: i64 = n.parse().ok()?;
    Some(match unit {
        "d" => now - n * 86_400_000,
        "h" => now - n * 3_600_000,
        "m" => now - n * 60_000,
        "" => n,
        _ => return None,
    })
}

fn filter_of(a: &Args, now: i64) -> Result<Filter, String> {
    let mut f = Filter::default();
    if let Some(l) = a.opt("--level") {
        f.max_level = Some(Level::parse(l).ok_or(format!("unknown level {l}"))?);
    }
    if let Some(b) = a.opt("--boot") {
        f.boot = Some(b.parse().map_err(|_| format!("bad boot {b}"))?);
    }
    if let Some(s) = a.opt("--since") {
        f.since_ms = Some(since(s, now).ok_or(format!("bad --since {s}"))?);
    }
    if let Some(p) = a.opt("--provider") {
        f.providers.push(p.to_string());
    }
    if let Some(i) = a.opt("--id") {
        f.ids.push(i.parse().map_err(|_| format!("bad id {i}"))?);
    }
    if let Some(b) = a.opt("--bucket") {
        f.bucket = Some(b.to_string());
    }
    if let Some(t) = a.opt("--text") {
        f.text = Some(t.to_string());
    }
    if let Some(r) = a.opt("--after") {
        f.after_record = Some(r.parse().map_err(|_| format!("bad record {r}"))?);
    }
    Ok(f)
}

/// Local offset east of UTC in seconds, from the TZ that libc knows (Juno's /etc/localtime), 0 if none.
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

fn main() -> ExitCode {
    let a = Args::parse();
    let now = now_ms();
    let off = offset_s(now);
    let cmd = a.rest.first().map(String::as_str).unwrap_or("list");
    let fail = |msg: String| {
        eprintln!("lucina-evlog: {msg}");
        ExitCode::FAILURE
    };
    match cmd {
        "ingest" | "follow" => {
            if !data_ready(&a.dir, 120) {
                return fail("/data is not mounted: nothing written".into());
            }
            let mut s = match Store::open(&a.dir, Retention::default()) {
                Ok(s) => s,
                Err(e) => return fail(format!("{}: {e}", a.dir.display())),
            };
            let version = fs::read_to_string("/proc/version").unwrap_or_default();
            let src = Sources { pstore: a.pstore.clone(), kmsg: kmsg_so_far(), uptime_ms: uptime_ms(), now_ms: now, build: ingest::build_of(&version) };
            let r = match ingest::ingest(&mut s, &src) {
                Ok(r) => r,
                Err(e) => return fail(format!("ingest: {e}")),
            };
            let events = store::read_dir_events(&a.dir);
            for rec in &r.added {
                if let Some(e) = events.iter().find(|e| e.record == *rec) {
                    // Only how the boot before this one ended: older records read in late are history, not news for the screen.
                    if e.level <= Level::Error && r.boot.is_some_and(|b| e.boot == Some(b.wrapping_sub(1))) {
                        // The kernel log, so the saved log and the console carry it too.
                        let line = format!("lucina-evlog: #{} {}\n", e.record, e.summary);
                        let _ = fs::OpenOptions::new().write(true).open("/dev/kmsg").and_then(|mut k| io::Write::write_all(&mut k, line.as_bytes()));
                        let _ = fs::OpenOptions::new().append(true).create(true).open("/run/lucina/notice").and_then(|mut n| {
                            io::Write::write_all(&mut n, format!("{} (lucina-evlog show {})\n", e.summary, e.record).as_bytes())
                        });
                    }
                }
            }
            if a.json {
                println!("{}", json!({"boot": r.boot, "added": r.added, "known": r.known, "pon_found": r.pon_found}));
            } else {
                println!("boot {}: {} new events, {} already in the log, PMIC reasons {}", r.boot.map_or("?".into(), |b| b.to_string()), r.added.len(), r.known, if r.pon_found { "read" } else { "not in the log" });
            }
            if cmd == "follow" {
                return follow(s, r.boot);
            }
            ExitCode::SUCCESS
        }
        "list" => {
            let f = match filter_of(&a, now) {
                Ok(f) => f,
                Err(e) => return fail(e),
            };
            let max: usize = a.opt("--max").and_then(|m| m.parse().ok()).unwrap_or(50);
            let events = store::read_dir_events(&a.dir);
            let shown: Vec<&Event> = events.iter().rev().filter(|e| f.matches(e)).take(max).collect();
            if a.json {
                println!("{}", serde_json::to_string(&shown).unwrap_or_default());
            } else {
                if shown.is_empty() {
                    println!("no events");
                }
                for e in shown {
                    println!("{:>5}  {}  {:<8} {:<24} {}", e.record, when(e, off), e.level.name(), title(e), e.summary);
                }
            }
            ExitCode::SUCCESS
        }
        "show" | "last-reset" => {
            let events = store::read_dir_events(&a.dir);
            let found = if cmd == "show" {
                let Some(r) = a.rest.get(1).and_then(|r| r.trim_start_matches('#').parse::<u64>().ok()) else {
                    return fail("usage: lucina-evlog show <record>".into());
                };
                events.iter().find(|e| e.record == r)
            } else {
                events.iter().rev().find(|e| e.level == Level::Critical)
            };
            let Some(e) = found else { return fail("no such event".into()) };
            if a.json {
                println!("{}", serde_json::to_string(e).unwrap_or_default());
            } else {
                print!("{}", export::event_text(e, off));
            }
            ExitCode::SUCCESS
        }
        "timeline" => {
            let days: usize = a.opt("--days").and_then(|d| d.parse().ok()).unwrap_or(14);
            let events = store::read_dir_events(&a.dir);
            let tl = timeline::days(&events, now, days, off);
            if a.json {
                println!("{}", serde_json::to_string(&tl).unwrap_or_default());
                return ExitCode::SUCCESS;
            }
            println!("date        index  Juno  apps  warn  info  costs most");
            for d in &tl {
                let idx = d.index.map_or("  -  ".into(), |i| format!("{i:>5.1}"));
                let why = d.why.first().map(|w| format!("#{} {}", w.record, w.summary.chars().take(60).collect::<String>())).unwrap_or_default();
                println!("{}  {idx}  {:>4}  {:>4}  {:>4}  {:>4}  {why}", d.date, d.juno_failures, d.app_failures, d.warnings, d.info);
            }
            println!("\n{}", timeline::FORMULA);
            ExitCode::SUCCESS
        }
        "problems" => {
            let events = store::read_dir_events(&a.dir);
            let groups = bucket::groups(&events);
            if a.json {
                println!("{}", serde_json::to_string(&groups).unwrap_or_default());
                return ExitCode::SUCCESS;
            }
            if groups.is_empty() {
                println!("no problems recorded");
            }
            for g in groups {
                let first = g.first_ms.map_or("?".into(), |t| clock(t, off));
                let last = g.last_ms.map_or("?".into(), |t| clock(t, off));
                let boots: Vec<String> = g.boots.iter().map(u32::to_string).collect();
                println!("{}  x{}  {}\n    first {first}, last {last}, boots {}", g.bucket.id, g.count, g.bucket.label, boots.join(", "));
            }
            ExitCode::SUCCESS
        }
        "report" => {
            let days: usize = a.opt("--days").and_then(|d| d.parse().ok()).unwrap_or(14);
            let events = store::read_dir_events(&a.dir);
            let r = export::report(&events, &store::read_meta(&a.dir), now, days, off);
            if a.json {
                println!("{r}");
            } else {
                println!("{}", serde_json::to_string_pretty(&r).unwrap_or_default());
            }
            ExitCode::SUCCESS
        }
        "backfill-boot" if !data_ready(&a.dir, 0) => fail("/data is not mounted".into()),
        "import" if !data_ready(&a.dir, 0) => fail("/data is not mounted".into()),
        "backfill-boot" => {
            let (Some(boot), Some(t)) = (a.rest.get(1).and_then(|b| b.parse().ok()), a.rest.get(2).and_then(|t| t.parse().ok())) else {
                return fail("usage: lucina-evlog backfill-boot <boot> <unix ms> [--source S] [--pon LINE]".into());
            };
            let mut s = match Store::open(&a.dir, Retention::default()) {
                Ok(s) => s,
                Err(e) => return fail(e.to_string()),
            };
            match ingest::backfill_boot(&mut s, boot, t, a.opt("--pon"), a.opt("--source").unwrap_or("given by hand"), now) {
                Ok(r) => {
                    println!("{r:?}");
                    ExitCode::SUCCESS
                }
                Err(e) => fail(e.to_string()),
            }
        }
        "import" => {
            let (Some(st), Some(dump)) = (a.rest.get(1), a.rest.get(2)) else {
                return fail("usage: lucina-evlog import <status file> <dump file>".into());
            };
            let (Ok(st), Ok(dump)) = (fs::read(st), fs::read(dump)) else { return fail("cannot read the files".into()) };
            let mut s = match Store::open(&a.dir, Retention::default()) {
                Ok(s) => s,
                Err(e) => return fail(e.to_string()),
            };
            match ingest::import_dump(&mut s, &String::from_utf8_lossy(&st), &String::from_utf8_lossy(&dump), now) {
                Ok(r) => {
                    println!("{} new events, {} already in the log", r.added.len(), r.known);
                    ExitCode::SUCCESS
                }
                Err(e) => fail(e.to_string()),
            }
        }
        "clear" => {
            if !data_ready(&a.dir, 0) {
                return fail("/data is not mounted".into());
            }
            let mut s = match Store::open(&a.dir, Retention::default()) {
                Ok(s) => s,
                Err(e) => return fail(e.to_string()),
            };
            match s.clear(now, a.opt("--by").unwrap_or("lucina-evlog clear")) {
                Ok(r) => {
                    println!("cleared; event #{r} says so");
                    ExitCode::SUCCESS
                }
                Err(e) => fail(e.to_string()),
            }
        }
        _ => fail("usage: lucina-evlog [ingest | follow | list | show <n> | last-reset | timeline | problems | report | backfill-boot | import | clear] [--json]".into()),
    }
}

/// Runs for ever: lucina-eventd's kernel probes on one thread, /dev/kmsg's new lines on another, one store between them.
fn follow(store: Store, boot: Option<u32>) -> ExitCode {
    let store = Arc::new(Mutex::new(store));
    let follower = Arc::new(Mutex::new(Follower::new(boot)));
    let write = {
        let store = store.clone();
        move |e: Event| {
            if let Ok(mut s) = store.lock()
                && let Err(err) = s.append(e, now_ms())
            {
                eprintln!("lucina-evlog: append: {err}");
            }
        }
    };
    let hub = {
        let follower = follower.clone();
        let write = write.clone();
        std::thread::spawn(move || {
            use lucina_events::client::Reader;
            use lucina_events::wire::Filter as HubFilter;
            let filter = HubFilter {
                kind: vec![
                    "proc.*".into(),
                    "net.down".into(),
                    "power.battery".into(),
                    "power.charger".into(),
                    "power.sleep".into(),
                    "power.wake".into(),
                ],
                ..Default::default()
            };
            let mut since = 0u64;
            loop {
                let Ok(mut r) =
                    Reader::connect(std::path::Path::new(lucina_events::wire::READ_SOCKET))
                else {
                    std::thread::sleep(std::time::Duration::from_secs(5));
                    continue;
                };
                // From the oldest event the hub kept: they are all this boot's, and none is in the log yet.
                while let Ok(b) = r.read(since, &filter, Some(64)) {
                    since = b.next;
                    for ev in b.events {
                        let made = follower
                            .lock()
                            .ok()
                            .and_then(|mut f| f.on_hub(&ev.kind, &ev.fields, ev.t_ms, now_ms()));
                        if let Some(e) = made {
                            write(e);
                        }
                    }
                    match r.wait(since, &filter, 30_000) {
                        Ok(w) if w.event.is_some() => {}
                        Ok(_) => {}
                        Err(_) => break,
                    }
                }
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
        })
    };
    // /dev/kmsg from its end: lines logged from now on.
    let kmsg = std::thread::spawn(move || {
        let Ok(f) = fs::File::open("/dev/kmsg") else {
            return;
        };
        // SAFETY: lseek on a descriptor we own; SEEK_END on /dev/kmsg moves the reader to the ring's end.
        unsafe {
            libc::lseek(std::os::unix::io::AsRawFd::as_raw_fd(&f), 0, libc::SEEK_END);
        }
        let mut r = io::BufReader::new(f);
        let mut line = String::new();
        loop {
            line.clear();
            match r.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    if line.contains("lucina-evlog:") {
                        continue;
                    }
                    let made = follower
                        .lock()
                        .ok()
                        .and_then(|mut f| f.on_kmsg(line.trim_end(), now_ms()));
                    if let Some(e) = made {
                        write(e);
                    }
                }
                Err(e) if e.raw_os_error() == Some(libc::EPIPE) => continue,
                Err(_) => std::thread::sleep(std::time::Duration::from_secs(1)),
            }
        }
    });
    let _ = hub.join();
    let _ = kmsg.join();
    ExitCode::SUCCESS
}
