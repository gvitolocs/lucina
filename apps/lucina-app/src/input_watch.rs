//! Input devices by event (patch 1192, now for every app): the kernel posts `input.added` and `input.removed` when an
//! evdev node appears or goes, and an app opens the new device at once instead of scanning `/dev/input` on a timer (the
//! owner's rule for Juno: hardware state by event, no polling). As the shell's `power_watch` does for the plug, a thread
//! waits on lucina-eventd for those kinds and writes a byte into a pipe the app's loop polls: `c` once it reaches the hub,
//! `i` for an input event, `d` when it loses the hub. An app stops its timed rescans only while the hub is there
//! (`Inputs::set_event_driven`), so without the hub it scans as before; a lost hub is tried again every 5 s, doubling up
//! to a minute, so a hubless machine does not wake forever.
use std::{
    fs::File,
    io::{Read, Write},
    os::fd::{AsRawFd, RawFd},
    path::Path,
    time::{Duration, Instant},
};
// Only the Linux `start` turns pipe2's descriptors into files.
#[cfg(any(target_os = "linux", target_os = "android"))]
use std::os::fd::FromRawFd;

use crate::events::{
    Reader,
    wire::{Filter, READ_SOCKET},
};
use crate::input::Inputs;

/// How long one wait on the hub lasts before it is asked again (the hub answers a wait at the first matching event).
const WAIT_MS: u64 = 600_000;
/// The first wait between two tries to reach the hub; `next_retry` doubles it after every try that found no hub.
const RETRY_MIN: Duration = Duration::from_secs(5);
/// The longest wait between two tries to reach the hub.
const RETRY_MAX: Duration = Duration::from_secs(60);

const CONNECTED: u8 = b'c';
const INPUT: u8 = b'i';
const LOST: u8 = b'd';

/// The next wait to reach the hub again: twice the last, never more than a minute.
fn next_retry(prev: Duration) -> Duration {
    (prev * 2).min(RETRY_MAX)
}

/// Whether an event kind says an input device came or went.
pub fn wants(kind: &str) -> bool {
    kind == "input.added" || kind == "input.removed"
}

pub struct InputWatch {
    read: File,
}

/// What the pipe said since the last look.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Seen {
    /// An input device came or went: read `/dev/input` now.
    pub devices_changed: bool,
    /// The hub's state as of the last byte: `Some(true)` reached, `Some(false)` lost, `None` unchanged.
    pub hub: Option<bool>,
}

impl InputWatch {
    /// Starts the waiting thread; `None` when no pipe or thread could be had (the timed rescans stay).
    #[cfg(any(target_os = "linux", target_os = "android"))]
    pub fn start() -> Option<InputWatch> {
        let mut fds = [0 as libc::c_int; 2];
        // SAFETY: `fds` holds room for the two descriptors pipe2 writes.
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) } != 0 {
            return None;
        }
        // SAFETY: pipe2 succeeded, so both descriptors are open and owned by nobody else.
        let (read, write) = unsafe { (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) };
        std::thread::Builder::new()
            .name("input-watch".into())
            .spawn(move || run(write))
            .ok()?;
        Some(InputWatch { read })
    }

    /// Starts the waiting thread; `None` when no pipe or thread could be had (the timed rescans stay).
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    pub fn start() -> Option<InputWatch> {
        // `/dev/input` is the Linux input ABI and exists nowhere else: there is no device to be told of, so
        // no watcher runs here and every app keeps its timed rescans, as it does while the hub is away.
        None
    }

    pub fn fd(&self) -> RawFd {
        self.read.as_raw_fd()
    }

    /// Empties the pipe and says what it held.
    pub fn drain(&mut self) -> Seen {
        let mut buf = [0u8; 64];
        let mut seen = Seen::default();
        while let Ok(n) = self.read.read(&mut buf) {
            if n == 0 {
                break;
            }
            seen = fold(seen, &buf[..n]);
        }
        seen
    }
}

/// Drains the watch (if any), turns timed rescans off while the hub is there, and reads `/dev/input` now when a device
/// came or went, else the timed rescan. Returns what the rescan returned (true = the device list changed).
pub fn rescan(watch: Option<&mut InputWatch>, inputs: &mut Inputs, now: Instant) -> bool {
    let Some(watch) = watch else {
        return inputs.rescan(now);
    };
    let seen = watch.drain();
    if let Some(hub) = seen.hub {
        // While the hub is there a new device comes as an event; without it the timed rescans come back.
        inputs.set_event_driven(hub);
    }
    if seen.devices_changed {
        inputs.rescan_now(now)
    } else {
        inputs.rescan(now)
    }
}

/// The pipe's bytes, in order, onto what was seen already: an input event marks the devices changed, and the last hub byte wins.
fn fold(mut seen: Seen, bytes: &[u8]) -> Seen {
    for &b in bytes {
        match b {
            INPUT => seen.devices_changed = true,
            CONNECTED => {
                seen.hub = Some(true);
                // Devices may have come while the hub was away: read the directory once.
                seen.devices_changed = true;
            }
            LOST => seen.hub = Some(false),
            _ => {}
        }
    }
    seen
}

/// The hub thread. Only the Linux `start` above spawns it; off Linux it is compiled, so the rest of the file stays the
/// same everywhere, but nothing ever starts it.
#[cfg_attr(not(any(target_os = "linux", target_os = "android")), allow(dead_code))]
fn run(mut write: File) {
    let filter = Filter {
        kind: vec!["input.*".into()],
        ..Filter::default()
    };
    let mut retry = RETRY_MIN;
    loop {
        let Ok(mut reader) = Reader::connect(Path::new(READ_SOCKET)) else {
            std::thread::sleep(retry);
            retry = next_retry(retry);
            continue;
        };
        retry = RETRY_MIN;
        let mut since = reader
            .read(0, &filter, Some(0))
            .map(|b| b.next)
            .unwrap_or(0);
        // A full pipe already says enough: a failed write loses nothing the next byte does not repeat.
        let _ = write.write(&[CONNECTED]);
        while let Ok(waited) = reader.wait(since, &filter, WAIT_MS) {
            since = waited.next;
            if waited.event.as_ref().is_some_and(|e| wants(&e.kind)) {
                let _ = write.write(&[INPUT]);
            }
        }
        let _ = write.write(&[LOST]);
        std::thread::sleep(retry);
        retry = next_retry(retry);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_input_events_count() {
        assert!(wants("input.added"));
        assert!(wants("input.removed"));
        assert!(!wants("input.addedx"));
        assert!(!wants("power.charger"));
    }

    #[test]
    fn the_pipe_folds_in_order() {
        assert_eq!(fold(Seen::default(), b""), Seen::default());
        assert_eq!(
            fold(Seen::default(), b"i"),
            Seen {
                devices_changed: true,
                hub: None
            }
        );
        // Reaching the hub reads the directory once; losing it keeps the change already seen.
        assert_eq!(
            fold(Seen::default(), b"c"),
            Seen {
                devices_changed: true,
                hub: Some(true)
            }
        );
        assert_eq!(
            fold(Seen::default(), b"cid"),
            Seen {
                devices_changed: true,
                hub: Some(false)
            }
        );
        assert_eq!(
            fold(Seen::default(), b"dc"),
            Seen {
                devices_changed: true,
                hub: Some(true)
            }
        );
    }

    #[test]
    fn the_retry_backs_off_to_a_minute() {
        let mut retry = RETRY_MIN;
        let waits: Vec<Duration> = std::iter::from_fn(|| {
            let w = retry;
            retry = next_retry(retry);
            Some(w)
        })
        .take(6)
        .collect();
        assert_eq!(
            waits,
            vec![
                Duration::from_secs(5),
                Duration::from_secs(10),
                Duration::from_secs(20),
                Duration::from_secs(40),
                Duration::from_secs(60),
                Duration::from_secs(60),
            ]
        );
    }
}
