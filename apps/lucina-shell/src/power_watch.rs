//! The battery indicator follows the cable at once, as on Windows (its battery class driver calls BatteryClassStatusNotify on
//! every power-state change): Juno's kernel posts `power.charger` and `power.battery` to lucina-eventd at the plug's interrupt
//! (patches 1130-1132), and a thread here waits on the hub for them and writes a byte into a pipe the shell's loop polls. The
//! loop then refreshes the status at once. Without the hub (not started, or gone) it tries again every 5 s, and the status's own
//! 5 s refresh stays as it was.
use std::{
    fs::File,
    io::{Read, Write},
    os::fd::{AsRawFd, FromRawFd, RawFd},
    path::Path,
    time::Duration,
};

use lucina_app::events::{
    Reader,
    wire::{Filter, READ_SOCKET},
};

/// How long one wait on the hub lasts before it is asked again (a wait the hub answers at the first matching event).
const WAIT_MS: u64 = 60_000;
/// Between two tries to reach the hub.
const RETRY: Duration = Duration::from_secs(5);

/// Whether an event kind changes what the bar shows about power.
pub fn wants(kind: &str) -> bool {
    kind.starts_with("power.")
}

pub struct PowerWatch {
    read: File,
}

impl PowerWatch {
    /// Starts the waiting thread; `None` when no pipe or thread could be had (the 5 s refresh stays).
    pub fn start() -> Option<PowerWatch> {
        let mut fds = [0 as libc::c_int; 2];
        // SAFETY: `fds` holds room for the two descriptors pipe2 writes.
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) } != 0 {
            return None;
        }
        // SAFETY: pipe2 succeeded, so both descriptors are open and owned by nobody else.
        let (read, write) = unsafe { (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) };
        std::thread::Builder::new()
            .name("power-watch".into())
            .spawn(move || run(write))
            .ok()?;
        Some(PowerWatch { read })
    }

    pub fn fd(&self) -> RawFd {
        self.read.as_raw_fd()
    }

    /// Empties the pipe; whether anything was in it.
    pub fn drain(&mut self) -> bool {
        let mut buf = [0u8; 64];
        let mut any = false;
        while let Ok(n) = self.read.read(&mut buf) {
            if n == 0 {
                break;
            }
            any = true;
        }
        any
    }
}

fn run(mut write: File) {
    let filter = Filter {
        kind: vec!["power.*".into()],
        ..Filter::default()
    };
    loop {
        if let Ok(mut reader) = Reader::connect(Path::new(READ_SOCKET)) {
            // From now on: what is queued already is what the status shows.
            let mut since = reader
                .read(0, &filter, Some(0))
                .map(|b| b.next)
                .unwrap_or(0);
            while let Ok(waited) = reader.wait(since, &filter, WAIT_MS) {
                since = waited.next;
                if waited.event.as_ref().is_some_and(|e| wants(&e.kind)) {
                    // A full pipe already says "refresh": a failed write loses nothing.
                    let _ = write.write(&[1]);
                }
            }
        }
        std::thread::sleep(RETRY);
    }
}

#[cfg(test)]
mod tests {
    use super::wants;

    #[test]
    fn only_power_events_refresh_the_status() {
        assert!(wants("power.charger"));
        assert!(wants("power.battery"));
        assert!(!wants("net.down"));
        assert!(!wants("powerline"));
    }
}
