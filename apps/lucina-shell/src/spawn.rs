//! The one place that forks and executes children.
//!
//! Every fork/exec of a child in lucina-shell goes through here. The lock is held across "copy the program, close its write
//! descriptor, exec", so no other thread can fork between the open of a copy and its exec: a child forked while a copy's write
//! descriptor was open would inherit it, and exec of a file that someone has open for writing fails with ETXTBSY
//! ("Text file busy").
//!
//! `locked` is reentrant on one thread (the owner is remembered per thread), so `spawn` and `output` may be called inside a
//! `locked` closure without deadlocking. `spawn_unlocked` is the plain `Command::spawn` for code that knows it is inside one.

use std::cell::Cell;
use std::io;
use std::process::{Child, Command, Output};
use std::sync::Mutex;

static LOCK: Mutex<()> = Mutex::new(());

thread_local! {
    /// True while this thread is inside `locked`.
    static HELD: Cell<bool> = const { Cell::new(false) };
}

/// Runs `f` with the process-wide spawn lock held. A panic inside does not wedge later spawns.
pub fn locked<R>(f: impl FnOnce() -> R) -> R {
    if HELD.with(Cell::get) {
        return f();
    }
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    struct Release;
    impl Drop for Release {
        fn drop(&mut self) {
            HELD.with(|h| h.set(false));
        }
    }
    HELD.with(|h| h.set(true));
    let _release = Release;
    f()
}

/// `Command::spawn` under the lock.
pub fn spawn(cmd: &mut Command) -> io::Result<Child> {
    locked(|| cmd.spawn())
}

/// `Command::spawn` without taking the lock: only for code already inside a `locked` closure.
#[cfg(test)]
pub fn spawn_unlocked(cmd: &mut Command) -> io::Result<Child> {
    cmd.spawn()
}

/// `Command::output` under the lock.
pub fn output(cmd: &mut Command) -> io::Result<Output> {
    locked(|| cmd.output())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    const ETXTBSY: i32 = 26;

    fn temp_dir(tag: &str) -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "lucina-shell-spawn-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Opens `dst` for writing, holds it for 100 ms, closes it, then returns the exec result of `dst`.
    fn copy_and_exec(
        dst: &Path,
        exec: impl Fn(&mut Command) -> io::Result<Child>,
    ) -> io::Result<()> {
        let mut f = File::create(dst)?;
        f.write_all(b"#!/bin/sh\nexit 0\n")?;
        fs::set_permissions(dst, fs::Permissions::from_mode(0o755))?;
        std::thread::sleep(Duration::from_millis(100));
        drop(f);
        exec(&mut Command::new(dst))?.wait()?;
        Ok(())
    }

    #[test]
    fn overlap_does_not_break_exec() {
        let dir = temp_dir("locked");
        for round in 0..20 {
            let dst = dir.join(format!("copy{round}"));
            std::thread::scope(|s| {
                let a = s.spawn(|| locked(|| copy_and_exec(&dst, spawn_unlocked)));
                let b = s.spawn(|| {
                    for _ in 0..20 {
                        spawn(&mut Command::new("/bin/true"))
                            .unwrap()
                            .wait()
                            .unwrap();
                    }
                });
                if let Err(e) = a.join().unwrap() {
                    panic!("round {round}: {e} (raw {:?})", e.raw_os_error());
                }
                b.join().unwrap();
            });
        }
        let _ = fs::remove_dir_all(&dir);
    }

    /// Shows the race is real, but a platform may never fail, so it asserts nothing and runs only with LUCINA_SPAWN_RACE=1
    /// (the diff gate refuses `#[ignore]`): `LUCINA_SPAWN_RACE=1 cargo test -p lucina-shell without_the_lock -- --nocapture`.
    #[test]
    fn without_the_lock_the_race_is_real() {
        if std::env::var_os("LUCINA_SPAWN_RACE").is_none() {
            return;
        }
        let dir = temp_dir("unlocked");
        let mut busy = 0;
        for round in 0..200 {
            let dst = dir.join(format!("copy{round}"));
            std::thread::scope(|s| {
                let a = s.spawn(|| copy_and_exec(&dst, |c| c.spawn()));
                let b = s.spawn(|| {
                    for _ in 0..20 {
                        Command::new("/bin/true").spawn().unwrap().wait().unwrap();
                    }
                });
                if let Err(e) = a.join().unwrap()
                    && e.raw_os_error() == Some(ETXTBSY)
                {
                    busy += 1;
                }
                b.join().unwrap();
            });
        }
        eprintln!("ETXTBSY in {busy} of 200 attempts");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_panic_inside_does_not_wedge_the_lock() {
        let r = std::panic::catch_unwind(|| locked(|| panic!("boom")));
        assert!(r.is_err());
        assert!(
            spawn(&mut Command::new("/bin/true"))
                .unwrap()
                .wait()
                .unwrap()
                .success()
        );
    }

    /// Only this file may fork a child. Allowed elsewhere: thread spawns, spelled `thread::Builder::...spawn(`, `scope.spawn(`,
    /// `s.spawn(`, `std::thread::spawn(` or `thread::spawn(`; and a line whose `.spawn()`/`.output()`/`.status()` has no
    /// `Command` in the statement is not looked at beyond these patterns, so the check is textual: a hit in any other file fails.
    #[test]
    fn no_other_spawn_site() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut hits = Vec::new();
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|e| e != "rs") || path.file_name().unwrap() == "spawn.rs"
            {
                continue;
            }
            let text = fs::read_to_string(&path).unwrap();
            let lines: Vec<&str> = text.lines().collect();
            for (i, line) in lines.iter().enumerate() {
                let code = line.split("//").next().unwrap();
                for pat in [".spawn()", ".output()", ".status()"] {
                    if !code.contains(pat) {
                        continue;
                    }
                    // The statement: this line and up to five before it.
                    let from = i.saturating_sub(5);
                    let stmt = lines[from..=i].join("\n");
                    let thread = ["thread::Builder", "scope.spawn", "s.spawn", "thread::spawn"]
                        .iter()
                        .any(|t| stmt.contains(t));
                    if stmt.contains("Command") && !thread {
                        hits.push(format!("{}:{}: {}", path.display(), i + 1, line.trim()));
                    }
                }
            }
        }
        assert!(
            hits.is_empty(),
            "spawn outside spawn.rs:\n{}",
            hits.join("\n")
        );
    }
}
