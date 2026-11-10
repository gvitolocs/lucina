//! Holding the screen on while something is watched: the shell's idle timer does not turn the light off while an app holds it.
//!
//! The originals:
//! - Windows: a playing app asks for `ES_DISPLAY_REQUIRED | ES_CONTINUOUS` (`SetThreadExecutionState`) or a
//!   `PowerRequestDisplayRequired` (`PowerSetRequest`); Media Foundation does it while it plays. The request also resets the display's
//!   idle timer, so the timeout counts again from when it is cleared, and it does not stop an explicit sleep (the power button).
//! - Linux desktops: Wayland's `idle-inhibit-unstable-v1` (`zwp_idle_inhibitor_v1`), destroyed with its surface or the client's
//!   connection, and `org.freedesktop.ScreenSaver.Inhibit`, dropped when the caller leaves the bus. mpv and browsers take one while
//!   playing.
//!
//! Here the shell gives every app it starts one end of a pipe as file descriptor [`FD`] (named in `LUCINA_HOLD_FD`). The app writes
//! [`ON`] or [`OFF`] lines to it; the hold lives on that pipe as an inhibitor lives on its client's connection: when the app ends,
//! crashes or is killed, the pipe closes and the shell drops the hold, with nothing for the app to remember. [`DisplayHold`] marks
//! the descriptor close-on-exec, so a program the app starts cannot keep a dead app's hold alive.

use std::fs::File;
use std::io::Write;
use std::os::fd::FromRawFd;

/// The descriptor the shell gives an app for its holds.
pub const FD: i32 = 3;
/// Set by the shell to [`FD`] when the descriptor is there.
pub const ENV: &str = "LUCINA_HOLD_FD";
/// A line asking for the screen to stay on, followed by a space and why (for Settings and the log).
pub const ON: &str = "display-required on";
/// A line letting it go.
pub const OFF: &str = "display-required off";
/// The longest reason kept; more is cut, so one app cannot fill the shell's screen.
pub const REASON_MAX: usize = 60;

/// What a line from an app says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message {
    /// Keep the screen on, with the app's reason.
    Hold(String),
    Release,
}

/// One line from an app's hold descriptor; `None` for anything else, which the shell ignores.
pub fn parse_line(line: &str) -> Option<Message> {
    let line = line.trim_end_matches(['\r', '\n']);
    if line == OFF {
        return Some(Message::Release);
    }
    let rest = line.strip_prefix(ON)?;
    if !(rest.is_empty() || rest.starts_with(' ')) {
        return None;
    }
    let reason: String = rest
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(REASON_MAX)
        .collect();
    Some(Message::Hold(reason))
}

/// The line for a state, ending in a newline.
pub fn line(held: bool, reason: &str) -> String {
    if held {
        let reason: String = reason
            .chars()
            .filter(|c| !c.is_control())
            .take(REASON_MAX)
            .collect();
        format!("{ON} {reason}\n")
    } else {
        format!("{OFF}\n")
    }
}

/// An app's side: the screen stays on while [`DisplayHold::set`] says so. Without a shell (no `LUCINA_HOLD_FD`, or a test) it does
/// nothing.
pub struct DisplayHold {
    fd: Option<File>,
    held: bool,
    reason: String,
}

impl DisplayHold {
    /// Takes the shell's descriptor, once per process: a second call finds the variable gone and does nothing.
    pub fn from_env() -> DisplayHold {
        let fd = std::env::var(ENV)
            .ok()
            .and_then(|v| v.parse::<i32>().ok())
            .filter(|&fd| fd == FD)
            .filter(|&fd| {
                // SAFETY: fcntl only reads the flags of a descriptor number; on a closed one it fails with EBADF.
                unsafe { libc::fcntl(fd, libc::F_GETFD) >= 0 }
            })
            .map(|fd| {
                // So that what this app starts does not inherit it: the hold must end with the app.
                // SAFETY: sets a flag on a descriptor that is open (checked above).
                unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
                // SAFETY: the shell made this descriptor for this process alone, and the variable is removed below, so no
                // second File takes it.
                unsafe { File::from_raw_fd(fd) }
            });
        // SAFETY: done before the app starts threads of its own, as the toolkit's setup runs first.
        unsafe { std::env::remove_var(ENV) };
        DisplayHold {
            fd,
            held: false,
            reason: String::new(),
        }
    }

    /// A hold that goes nowhere, for tests and programs without a shell.
    pub fn none() -> DisplayHold {
        DisplayHold {
            fd: None,
            held: false,
            reason: String::new(),
        }
    }

    /// Hold the screen on (`held`), saying why, or let it go. Only a change is written. A shell that has gone is not an error.
    pub fn set(&mut self, held: bool, reason: &str) {
        if held == self.held && (!held || reason == self.reason) {
            return;
        }
        self.held = held;
        self.reason = if held { reason.to_string() } else { String::new() };
        if let Some(f) = self.fd.as_mut()
            && f.write_all(line(held, reason).as_bytes()).is_err()
        {
            self.fd = None;
        }
    }

    pub fn is_held(&self) -> bool {
        self.held
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_round_trip() {
        assert_eq!(parse_line(&line(true, "playing video")), Some(Message::Hold("playing video".into())));
        assert_eq!(parse_line(&line(false, "ignored")), Some(Message::Release));
        assert_eq!(parse_line("display-required on"), Some(Message::Hold(String::new())));
    }

    #[test]
    fn other_lines_are_nothing() {
        assert_eq!(parse_line("display-required onwards"), None);
        assert_eq!(parse_line("hello"), None);
        assert_eq!(parse_line(""), None);
        assert_eq!(parse_line("display-required off now"), None);
    }

    #[test]
    fn reasons_are_short_and_plain() {
        let long = "x".repeat(500);
        match parse_line(&format!("{ON} {long}\u{7}")) {
            Some(Message::Hold(r)) => assert_eq!(r.len(), REASON_MAX),
            other => panic!("{other:?}"),
        }
        assert_eq!(line(true, "a\nb"), format!("{ON} ab\n"));
    }

    #[test]
    fn only_changes_are_written_and_none_writes_nothing() {
        let mut h = DisplayHold::none();
        h.set(true, "playing");
        assert!(h.is_held());
        h.set(true, "playing");
        h.set(false, "");
        assert!(!h.is_held());
    }

    #[test]
    fn only_changes_go_down_the_pipe() {
        use std::io::Read;
        let mut fds = [-1; 2];
        // SAFETY: pipe fills the array with two new descriptors.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        // SAFETY: both were just made and are owned here alone.
        let (mut read_end, write_end) = unsafe { (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) };
        let mut h = DisplayHold {
            fd: Some(write_end),
            held: false,
            reason: String::new(),
        };
        h.set(false, "");
        h.set(true, "playing");
        h.set(true, "playing");
        h.set(true, "full screen");
        h.set(false, "");
        h.set(false, "");
        drop(h);
        let mut out = String::new();
        read_end.read_to_string(&mut out).unwrap();
        assert_eq!(out, format!("{ON} playing\n{ON} full screen\n{OFF}\n"));
    }
}
