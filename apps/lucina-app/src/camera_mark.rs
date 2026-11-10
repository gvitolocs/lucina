//! The "camera on" mark: who captures from the webcam, from the kernel's `/proc/lucina-camera`.
//!
//! The kernel is the truth: while any program has the camera open, the last line of that file says
//! `capturing: yes, pid <pid> (<program>)`, and `capturing: no` when nobody does. On a machine without
//! the file (older kernels, other machines) the camera counts as off. Reading it is cheap, so apps
//! read it through [`current`]: a cached value that one background thread keeps fresh and, when it
//! changes, wakes the main loop for a full redraw, so the mark in the title bar appears and disappears
//! without the app doing anything.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// The kernel's word for the webcam's state.
const PROC: &str = "/proc/lucina-camera";

/// How often the watcher re-reads the file, and how long [`current`] trusts its cache.
const REFRESH: Duration = Duration::from_millis(500);

/// Who captures, from the text of `/proc/lucina-camera`: the line starting `capturing: yes, pid `
/// gives `Some((pid, program))`; anything else, including no such line, is `None`. Never panics.
pub fn parse(text: &str) -> Option<(u32, String)> {
    let line = text.lines().find(|l| l.starts_with("capturing: yes, pid "))?;
    let rest = line.strip_prefix("capturing: yes, pid ")?;
    // The pid is the first word; the program runs from the first `(` after it to the line's last `)`,
    // so a name with spaces or parentheses of its own comes through whole.
    let pid = rest.split_whitespace().next()?.parse::<u32>().ok()?;
    let open = rest.find('(')?;
    let close = rest.rfind(')')?;
    if close < open {
        return None;
    }
    Some((pid, rest[open + 1..close].to_string()))
}

/// Who captures now, read from `/proc/lucina-camera`. Any error (no file, no such line) means off.
pub fn read() -> Option<(u32, String)> {
    parse(&std::fs::read_to_string(PROC).ok()?)
}

/// The mark's text: "Camera on: <program>".
pub fn label(program: &str) -> String {
    format!("Camera on: {program}")
}

/// The cached answer of [`current`], with when it was read.
struct Cache {
    value: Option<(u32, String)>,
    at: Instant,
}

static CACHE: Mutex<Option<Cache>> = Mutex::new(None);
/// Set once the watcher thread has been started.
static WATCHING: OnceLock<()> = OnceLock::new();
/// Set by the watcher when the camera state changed: the main loop checks it, clears it, and draws
/// everything again, so the mark in the title bar appears or disappears.
pub(crate) static CHANGED: AtomicBool = AtomicBool::new(false);

/// Who captures now, for the title bar: the cached value, re-read at most every [`REFRESH`]. The
/// first call starts one background thread that re-reads the file every [`REFRESH`] and, when the
/// value changes, sets [`CHANGED`] and wakes the main loop. In unit tests no thread is started.
pub fn current() -> Option<(u32, String)> {
    // `cfg!(test)` is the constant false under `cargo test`, so the branch is never taken there.
    if !cfg!(test) {
        start_watcher();
    }
    let now = Instant::now();
    let mut guard = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(c) = guard.as_ref()
        && now.duration_since(c.at) < REFRESH
    {
        return c.value.clone();
    }
    let value = read();
    *guard = Some(Cache {
        value: value.clone(),
        at: now,
    });
    value
}

/// Starts the watcher thread once. It never panics (`read` cannot), and being a plain std thread it
/// does not keep the process alive at exit.
fn start_watcher() {
    if WATCHING.set(()).is_err() {
        return;
    }
    std::thread::spawn(|| {
        let mut last = read();
        loop {
            std::thread::sleep(REFRESH);
            let now = read();
            if now != last {
                last = now;
                CHANGED.store(true, Ordering::SeqCst);
                crate::app::wake_fd(crate::app::signal_fd());
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_capturing_line_is_parsed() {
        assert_eq!(
            parse("capturing: yes, pid 412 (lucina-app-came)\n"),
            Some((412, "lucina-app-came".to_string()))
        );
        // A whole sample file, whose last line is the capturing one.
        let file = "camera: Integrated Camera (04f2:b67b)\n\
                    state: streaming\n\
                    format 0 YUY2 640x480@30 1920x1080@10 (colour 1/1/1)\n\
                    power: TLMM 13 high\n\
                    capturing: yes, pid 412 (lucina-app-came)\n";
        assert_eq!(parse(file), Some((412, "lucina-app-came".to_string())));
        assert_eq!(parse("capturing: no"), None);
        assert_eq!(parse(""), None);
        assert_eq!(parse("garbage\nnot a proc file\ncapturing: maybe"), None);
        // A program name with spaces and parentheses of its own: from the first `(` after the pid
        // to the last `)` of the line.
        assert_eq!(
            parse("capturing: yes, pid 7 (a (b) c)"),
            Some((7, "a (b) c".to_string()))
        );
    }

    #[test]
    fn the_label_names_the_program() {
        assert_eq!(label("lucina-app-came"), "Camera on: lucina-app-came");
    }

    #[test]
    fn current_agrees_with_a_fresh_read_and_starts_no_thread() {
        // Under cfg(test) the watcher is never started; the answer is the file's, or None without it.
        assert_eq!(current(), read());
    }
}
