//! The memory-pressure ladder is driven by an event, not by a poll (design `docs/design/juno-window-manager.md` section 5):
//! Juno's kernel posts `mem.pressure` to lucina-eventd at the edge where available memory crosses a threshold, with the
//! fields `level`, `available_kib`, `total_kib` and subject `mem`, and a thread here waits on the hub for the `mem.*` kinds
//! and writes the new level into a pipe the shell's loop polls. It is the shape of [`crate::power_watch`] over the memory
//! kinds: one waiting thread, one pipe, no timer of its own.
//!
//! The event itself is not in this tree yet — the kernel rule that posts it is the `lucina-mempressure` crate, on branch
//! `mempressure3`, and nothing posts `mem.pressure` today (section 8b). So `start` gives up when this machine has no hub to
//! wait on at all, and the shell keeps reading `system::memory()` on its own refresh; where the hub is there, the thread
//! retries it every 5 s exactly as `power_watch` does.
//!
//! Only a level change reaches the pipe: an edge-triggered event whose `level` field cannot be read says nothing the next
//! edge will not, and a lost byte is a level the loop takes from its own reading of `/proc/meminfo`. The current level is
//! not fetched at connect either, as in `power_watch`: the first thing the ladder sees is the first edge after the shell
//! started, and the loop's own `Level::next` on the memory figure covers the moment before it.

#![allow(dead_code)] // slice W: no loop wires the watch in yet, so nothing calls `start`, `fd` or `drain`.

use std::{
    ffi::OsString,
    fs::File,
    io::{Read, Write},
    os::fd::{AsRawFd, FromRawFd, RawFd},
    path::Path,
    time::Duration,
};

use lucina_app::events::{
    Reader,
    wire::{Event, Filter, READ_SOCKET},
};

use crate::backdrop::Level;

/// How long one wait on the hub lasts before it is asked again (a wait the hub answers at the first matching event).
const WAIT_MS: u64 = 60_000;
/// Between two tries to reach the hub.
const RETRY: Duration = Duration::from_secs(5);

/// Whether an event kind belongs to what the ladder acts on. `mem.pressure` is the one that carries a level today; the
/// subject's other kinds (a future `mem.reclaim`) are waited for with the same filter, as section 5's one mechanism asks.
pub fn wants(kind: &str) -> bool {
    kind.starts_with("mem.")
}

/// The level its `level` field names, as the kernel posts it: `normal`, `low` or `critical`. Anything else is not a level,
/// which is what keeps a hub from another generation from moving the ladder.
pub fn level_named(s: &str) -> Option<Level> {
    match s {
        "normal" => Some(Level::Normal),
        "low" => Some(Level::Low),
        "critical" => Some(Level::Critical),
        _ => None,
    }
}

/// The level an event carries: its `level` field, when the kind is one the ladder waits for.
fn level_of(e: &Event) -> Option<Level> {
    if !wants(&e.kind) {
        return None;
    }
    level_named(e.fields.get("level")?.as_str()?)
}

/// The byte for the pipe, and its reading back. A byte that is none of these three comes from a pipe that was never ours.
fn byte_of(level: Level) -> u8 {
    match level {
        Level::Normal => 0,
        Level::Low => 1,
        Level::Critical => 2,
    }
}

fn level_of_byte(b: u8) -> Option<Level> {
    match b {
        0 => Some(Level::Normal),
        1 => Some(Level::Low),
        2 => Some(Level::Critical),
        _ => None,
    }
}

/// The pipe's bytes, in order: the last level it held, `None` when it held no level at all.
fn fold(bytes: &[u8]) -> Option<Level> {
    bytes.iter().rev().find_map(|&b| level_of_byte(b))
}

pub struct PressureWatch {
    read: File,
}

impl PressureWatch {
    /// Starts the waiting thread. `None` when there is nothing to wait on — no hub on this machine, or no pipe or thread to
    /// be had — which leaves the shell's own memory refresh in charge and costs no thread.
    pub fn start() -> Option<PressureWatch> {
        start_at(Path::new(READ_SOCKET))
    }

    /// The descriptor to wake on, besides input.
    pub fn fd(&self) -> RawFd {
        self.read.as_raw_fd()
    }

    /// Empties the pipe; the level it ended on, `None` when nothing came.
    pub fn drain(&mut self) -> Option<Level> {
        let mut buf = [0u8; 64];
        let mut last = None;
        while let Ok(n) = self.read.read(&mut buf) {
            if n == 0 {
                break;
            }
            last = fold(&buf[..n]).or(last);
        }
        last
    }
}

/// The body of `start` over a hub path, so the giving-up is testable without a hub and without touching another path.
fn start_at(hub: &Path) -> Option<PressureWatch> {
    if !hub.exists() {
        return None;
    }
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: `fds` holds room for the two descriptors pipe2 writes.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) } != 0 {
        return None;
    }
    // SAFETY: pipe2 succeeded, so both descriptors are open and owned by nobody else.
    let (read, write) = unsafe { (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) };
    let hub: OsString = hub.as_os_str().to_os_string();
    std::thread::Builder::new()
        .name("pressure-watch".into())
        .spawn(move || run(write, Path::new(&hub)))
        .ok()?;
    Some(PressureWatch { read })
}

fn run(mut write: File, hub: &Path) {
    let filter = Filter {
        kind: vec!["mem.*".into()],
        ..Filter::default()
    };
    loop {
        if let Ok(mut reader) = Reader::connect(hub) {
            // From now on: what is queued already is what the ladder would have acted on.
            let mut since = reader
                .read(0, &filter, Some(0))
                .map(|b| b.next)
                .unwrap_or(0);
            while let Ok(waited) = reader.wait(since, &filter, WAIT_MS) {
                since = waited.next;
                if let Some(level) = waited.event.as_ref().and_then(level_of) {
                    // A full pipe already says "the last edge wins": a failed write loses nothing the next edge repeats.
                    let _ = write.write(&[byte_of(level)]);
                }
            }
        }
        std::thread::sleep(RETRY);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lucina_app::events::wire::Source;
    use serde_json::{Map, Value};

    fn event(kind: &str, level: Option<&str>) -> Event {
        let mut fields = Map::new();
        if let Some(l) = level {
            fields.insert("level".into(), Value::String(l.into()));
        }
        Event {
            seq: 1,
            src: Source::Kernel,
            src_seq: 3,
            t_ms: 0,
            kind: kind.into(),
            app: None,
            subject: Some("mem".into()),
            fields,
        }
    }

    #[test]
    fn only_the_memory_subject_is_wanted() {
        assert!(wants("mem.pressure"));
        assert!(wants("mem."));
        assert!(!wants("mem"));
        assert!(!wants("memory"));
        assert!(!wants("power.charger"));
        assert!(!wants("proc.oom_killed"));
    }

    #[test]
    fn a_hub_that_is_not_there_gives_no_watcher() {
        // The path does not exist, so nothing is spawned and no pipe is taken: the shell's timed refresh stays.
        assert!(start_at(Path::new("/nonexistent/lucina-events-read.sock")).is_none());
        // On a machine without eventd at all, `start` is the same call over the real path.
        if !Path::new(READ_SOCKET).exists() {
            assert!(PressureWatch::start().is_none());
        }
    }

    #[test]
    fn the_level_of_an_event_is_its_level_field() {
        assert_eq!(level_named("normal"), Some(Level::Normal));
        assert_eq!(level_named("low"), Some(Level::Low));
        assert_eq!(level_named("critical"), Some(Level::Critical));
        // Not a level, not a case the kernel uses, not a number: none of them moves the ladder.
        assert_eq!(level_named("Normal"), None);
        assert_eq!(level_named(""), None);
        assert_eq!(level_named("high"), None);
        assert_eq!(
            level_of(&event("mem.pressure", Some("low"))),
            Some(Level::Low)
        );
        assert_eq!(level_of(&event("mem.pressure", None)), None);
        assert_eq!(level_of(&event("power.battery", Some("critical"))), None);
        // A number where the kernel puts a word (an event of another generation) is not a level either.
        let mut fields = Map::new();
        fields.insert("level".into(), Value::from(1));
        let e = Event {
            fields,
            ..event("mem.pressure", None)
        };
        assert_eq!(level_of(&e), None);
    }

    #[test]
    fn the_pipe_carries_the_last_edge_and_nothing_else() {
        for level in [Level::Normal, Level::Low, Level::Critical] {
            assert_eq!(fold(&[byte_of(level)]), Some(level));
            assert_eq!(level_of_byte(byte_of(level)), Some(level));
        }
        // The last level wins; a byte that is no level is skipped, and an empty or junk pipe says nothing.
        assert_eq!(
            fold(&[byte_of(Level::Low), byte_of(Level::Critical)]),
            Some(Level::Critical)
        );
        assert_eq!(
            fold(&[byte_of(Level::Critical), byte_of(Level::Normal)]),
            Some(Level::Normal)
        );
        assert_eq!(fold(&[byte_of(Level::Low), 7]), Some(Level::Low));
        assert_eq!(fold(&[7, 9]), None);
        assert_eq!(fold(&[]), None);
    }

    #[test]
    fn drain_takes_the_pipe_and_ends_on_the_last_edge() {
        // The same read end the loop polls, with no hub behind it: what the pipe says between two edges is all `drain` has.
        let Some((mut watch, mut write)) = pipe_watch() else {
            return; // no pipe to be had here: then there is no watcher either, as `start` says.
        };
        assert_eq!(watch.drain(), None, "a quiet pipe says nothing");
        write.write_all(&[byte_of(Level::Low)]).unwrap();
        write.write_all(&[byte_of(Level::Critical)]).unwrap();
        assert_eq!(watch.drain(), Some(Level::Critical), "the last edge wins");
        assert_eq!(watch.drain(), None, "and it is taken once, not again");
        write.write_all(&[9, byte_of(Level::Normal)]).unwrap();
        assert_eq!(
            watch.drain(),
            Some(Level::Normal),
            "a byte that is no level is passed over"
        );
    }

    /// A watch over a pipe the test feeds itself: `start` needs a hub, and this box has none.
    fn pipe_watch() -> Option<(PressureWatch, File)> {
        let mut fds = [0 as libc::c_int; 2];
        // SAFETY: `fds` holds room for the two descriptors pipe2 writes.
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) } != 0 {
            return None;
        }
        // SAFETY: pipe2 succeeded, so both descriptors are open and owned by nobody else. The write end stays open, so the
        // non-blocking read end gives `WouldBlock` on an empty pipe rather than an end-of-file the loop would treat as a closed watch.
        let read = unsafe { File::from_raw_fd(fds[0]) };
        let write = unsafe { File::from_raw_fd(fds[1]) };
        Some((PressureWatch { read }, write))
    }
}
