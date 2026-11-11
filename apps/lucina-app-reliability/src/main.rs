//! lucina-app-reliability: Windows' Reliability Monitor for Juno, and more: the stability timeline of the event log
//! (`lucina-evlog`, read from /data/lucina/events), each event in plain words with its likely cause, the evidence behind it and the
//! kernel log lines it points at. `LUCINA_APP_SAMPLE=1` runs on a made-up log, and the title says so.
//! `--render <file.ppm> [timeline|details|details-log|problems|formula]` (and `details-tech`) draws a screen at 1920x1080 on the sample.

// Juno with no C library: Eyra is the libc and the program start, in Rust (docs/research/juno-pure-rust.md).
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
extern crate eyra;

mod app;
mod chart;
mod details;
mod look;
mod model;
#[cfg(test)]
mod tests;

use std::ffi::CString;
use std::os::fd::RawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use app::{Msg, ReliabilityApp};
use lucina_app::{Harness, Sender};
use lucina_evlog::store::{self, DEFAULT_DIR};

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// The log's directory; `LUCINA_EVLOG_DIR` points the app elsewhere (a copy of a log, a test).
fn dir() -> PathBuf {
    std::env::var_os("LUCINA_EVLOG_DIR").map_or_else(|| PathBuf::from(DEFAULT_DIR), PathBuf::from)
}

/// The newest change time of any file in the log's directory: a segment growing or `meta.json` changing.
fn newest_change(dir: &Path) -> Option<SystemTime> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter_map(|e| e.metadata().ok()?.modified().ok())
        .max()
}

/// Reads the log when its files change, and at least every ten seconds, and sends what it read. Never takes the writer's lock.
/// inotify is the wake-up: the thread sleeps in `poll` and only rises for a change or once per ten seconds. Where the directory
/// cannot be watched (not there yet, or a kernel without inotify) it keeps the one-second loop and retries the watch every ten.
fn start_worker(tx: Sender<Msg>, dir: PathBuf) {
    std::thread::spawn(move || {
        let mut seen: Option<SystemTime> = None;
        let mut last = None::<Instant>;
        // The inotify queue watching `dir`; none while the watch could not be made.
        let mut watch: Option<RawFd> = None;
        let path = CString::new(dir.as_os_str().as_bytes()).ok();
        let mut retry = Instant::now();
        let mut said = false;
        loop {
            if watch.is_none() && Instant::now() >= retry {
                retry += Duration::from_secs(10);
                if let Some(p) = path.as_ref() {
                    // SAFETY: inotify_init1 only reads its flags integer and returns a new queue descriptor or -1;
                    // IN_NONBLOCK is what lets the drain below end on EAGAIN, IN_CLOEXEC keeps the queue from children.
                    let fd = unsafe { libc::inotify_init1(libc::IN_CLOEXEC | libc::IN_NONBLOCK) };
                    if fd >= 0 {
                        // SAFETY: `fd` is the queue made just above and owned by nobody else, `p` is a NUL-terminated path
                        // that outlives the call, and the mask is five inotify flag constants.
                        let wd = unsafe {
                            libc::inotify_add_watch(
                                fd,
                                p.as_ptr(),
                                libc::IN_CREATE
                                    | libc::IN_MODIFY
                                    | libc::IN_CLOSE_WRITE
                                    | libc::IN_MOVED_TO
                                    | libc::IN_DELETE,
                            )
                        };
                        if wd >= 0 {
                            watch = Some(fd);
                            eprintln!("lucina-app-reliability: the log is watched by inotify");
                        } else {
                            // SAFETY: closes the queue made above, which the failed watch leaves unused.
                            unsafe { libc::close(fd) };
                        }
                    }
                }
            }
            if watch.is_none() && !said {
                said = true;
                eprintln!(
                    "lucina-app-reliability: no inotify watch on {}: the log is read every second",
                    dir.display()
                );
            }
            let changed = newest_change(&dir);
            if last.is_none_or(|t| t.elapsed() >= Duration::from_secs(10)) || changed != seen {
                seen = changed;
                last = Some(Instant::now());
                tx.send(Msg::Loaded(Box::new((
                    store::read_dir_events(&dir),
                    store::read_meta(&dir),
                ))));
            }
            match watch {
                Some(fd) => {
                    let mut pfd = libc::pollfd {
                        fd,
                        events: libc::POLLIN,
                        revents: 0,
                    };
                    // SAFETY: `pfd` is one initialised pollfd naming the queue we hold; poll writes its revents and no memory.
                    unsafe { libc::poll(&mut pfd, 1, 10_000) };
                    let mut buf = [0u8; 4096];
                    // SAFETY: reads into a live local buffer from the queue we hold, which is non-blocking, so it returns -1
                    // (EAGAIN) as soon as the queued events are drained.
                    while unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) } > 0 {}
                }
                None => std::thread::sleep(Duration::from_secs(1)),
            }
        }
    });
}

/// `--render <file> [timeline|details|details-log|problems|formula]`: the screen as a picture, with the sample log.
fn render(path: &str, what: &str) {
    let now = now_ms();
    let (_, zone) = lucina_app::time::load();
    let mut a = ReliabilityApp::new(zone, DEFAULT_DIR).sample();
    a.fixed_now = Some(now);
    a.load(
        lucina_evlog::sample::events(now),
        store::Meta {
            dropped_routine: 0,
            ..store::Meta::default()
        },
    );
    let mut h = Harness::new(a, 1920, 1080);
    let settle = |h: &mut Harness<ReliabilityApp>| {
        for _ in 0..3 {
            h.core.input(lucina_app::Input::Wheel(0));
            h.core.paint();
        }
    };
    settle(&mut h);
    match what {
        "details" | "details-log" | "details-tech" => {
            let record = h
                .app()
                .newest_critical_today()
                .expect("the sample has a critical event today");
            h.app().open_details(record);
            settle(&mut h);
            if what == "details-tech" {
                h.key(lucina_app::keys::KEY_T);
                settle(&mut h);
                let last = h.app().detail.lines.len() - 1;
                h.app().detail.list.select(last);
            }
            if what == "details-log" {
                if let Some(row) = h.app().first_linked_row() {
                    h.app().select_row(row);
                }
                settle(&mut h);
            }
        }
        "problems" => {
            h.key(lucina_app::keys::KEY_2);
        }
        "formula" => {
            h.app().formula = true;
        }
        _ => {}
    }
    settle(&mut h);
    std::fs::write(path, h.ppm()).expect("write");
}

fn main() {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some("--render") {
        let path = args.next().unwrap_or_else(|| "reliability.ppm".into());
        render(&path, &args.next().unwrap_or_default());
        return;
    }
    lucina_app::run("lucina-app-reliability", |tx| {
        let (_, zone) = lucina_app::time::load();
        if std::env::var_os("LUCINA_APP_SAMPLE").is_some() {
            let (events, meta) = (
                lucina_evlog::sample::events(now_ms()),
                store::Meta {
                    dropped_routine: 0,
                    ..store::Meta::default()
                },
            );
            tx.send(Msg::Loaded(Box::new((events, meta))));
            return ReliabilityApp::new(zone, DEFAULT_DIR).sample();
        }
        let d = dir();
        let shown = d.to_string_lossy().into_owned();
        start_worker(tx, d);
        ReliabilityApp::new(zone, &shown)
    });
}
