//! Shared by the tests that run a step of `check-fast` through the launcher. Since the launcher gives every step a memory-capped
//! systemd scope and fails a step that cannot get one (it never runs one uncapped, and it never runs `sudo`), these tests need a
//! systemd user manager that can make such a scope. A machine without one (a CI runner with no user session) cannot run them:
//! the test says so on stderr, in the form `skipped here: <reason>: <test name>`, and returns. It is never `#[ignore]`, never a
//! hang and never a silent pass. `lucina-ci test-binaries` lists every such line at the end of its run.

#![allow(dead_code)]

use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// The longest a probe may take. A probe that waits on a bus that never answers must not hold the test with it.
const PROBE_LIMIT: Duration = Duration::from_secs(5);

/// Runs `cmd` and says whether it succeeded within the limit; a probe still running at the limit is killed and is a "no".
fn succeeds_within_limit(cmd: &mut Command) -> bool {
    let Ok(mut child) = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let end = Instant::now() + PROBE_LIMIT;
    loop {
        match child.try_wait() {
            Ok(Some(s)) => return s.success(),
            Ok(None) if Instant::now() < end => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

fn is_root() -> bool {
    Command::new("id")
        .arg("-u")
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "0")
}

/// Why this machine cannot give a memory-capped scope, or `None` when it can. Asked once per test binary.
fn why_no_scope() -> Option<&'static str> {
    static ANSWER: OnceLock<Option<&'static str>> = OnceLock::new();
    *ANSWER.get_or_init(|| {
        let root = is_root();
        if !root
            && !succeeds_within_limit(
                Command::new("systemctl").args(["--user", "show-environment"]),
            )
        {
            return Some("no user manager");
        }
        let mut c = Command::new("systemd-run");
        if !root {
            c.arg("--user");
        }
        c.args([
            "--scope",
            "--quiet",
            "--collect",
            "--property=MemoryMax=256M",
            "true",
        ]);
        if succeeds_within_limit(&mut c) {
            None
        } else {
            Some("no scope")
        }
    })
}

/// True when this machine cannot give a capped scope: the reason is printed as `skipped here: <reason>: <test>` and the caller
/// returns. False when it can, and the test runs for real.
pub fn skip_without_scope(test: &str) -> bool {
    match why_no_scope() {
        Some(why) => {
            eprintln!("skipped here: {why}: {test}");
            true
        }
        None => false,
    }
}
