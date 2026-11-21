//! `lucina-ci test-binaries` (the "Launcher tests" step of the public workflow): the stand-in test binaries are shell
//! scripts, so what is under test is the runner itself. A binary that sleeps is killed at its limit and named; a fast one
//! gets its seconds line; every `skipped here:` line a binary printed is listed at the end; a failing binary fails the run
//! and the ones after it still run; the budget of the whole step stops the run and names the binary that was running.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

/// A stub goes in through a child that is reaped before anything execs it: a file this thread still holds open for writing
/// makes the exec answer "Text file busy" when another thread forks at the same moment.
fn write_exec(path: &Path, script: &str) {
    let mut child = Command::new("sh")
        .args(["-c", "cat > \"$1\" && chmod 0755 \"$1\"", "sh"])
        .arg(path)
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    let mut w = child.stdin.take().unwrap();
    w.write_all(script.as_bytes()).unwrap();
    drop(w);
    assert!(
        child.wait().unwrap().success(),
        "cannot write {}",
        path.display()
    );
}

struct Dir(PathBuf);

impl Dir {
    fn new(tag: &str) -> Dir {
        let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("tb-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        Dir(d)
    }
    fn bin(&self, name: &str, body: &str) -> String {
        let p = self.0.join(name);
        write_exec(&p, &format!("#!/bin/sh\n{body}\n"));
        p.display().to_string()
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn run(args: &[&str], dir: &Dir) -> (i32, String, f64) {
    let t = Instant::now();
    let o = Command::new(env!("CARGO_BIN_EXE_lucina-ci"))
        .arg("test-binaries")
        .args(args)
        .current_dir(&dir.0)
        .stdin(Stdio::null())
        .output()
        .expect("lucina-ci starts");
    let mut text = String::from_utf8_lossy(&o.stdout).to_string();
    text.push_str(&String::from_utf8_lossy(&o.stderr));
    (
        o.status.code().unwrap_or(-1),
        text,
        t.elapsed().as_secs_f64(),
    )
}

fn survivors(marker: &str) -> usize {
    let o = Command::new("pgrep")
        .args(["-f", marker])
        .output()
        .expect("pgrep");
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count()
}

#[test]
fn a_fast_binary_gets_its_seconds_line_and_no_skips_is_said() {
    let d = Dir::new("fast");
    let a = d.bin("quick_one", "echo 'running 1 test'\necho 'test x ... ok'");
    let (code, out, _) = run(&["--binary", &a, "--cwd", "."], &d);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("== quick_one start"), "{out}");
    assert!(out.contains("== quick_one 0 s"), "{out}");
    assert!(
        out.contains("test x ... ok"),
        "the binary's own output is shown: {out}"
    );
    assert!(out.contains("== no skips"), "{out}");
}

#[test]
fn a_binary_that_sleeps_is_killed_at_its_limit_and_named() {
    let d = Dir::new("hung");
    // A marker only this test has, in the sleeper's command line, so that its survivors can be counted.
    let marker = format!("tb-hung-{}", std::process::id());
    let a = d.bin(
        "sleeper",
        &format!("sleep 4000 # {marker}\n(exec sleep 4001 && true) &\nsleep 4002 &\nwait"),
    );
    let (code, out, took) = run(&["--binary", &a, "--limit-s", "2", "--cwd", "."], &d);
    assert_eq!(code, 1, "a hung binary fails the step: {out}");
    assert!(out.contains("HUNG: sleeper after 2 s"), "{out}");
    assert!(
        took < 20.0,
        "the run took {took} s against a 2 s limit: {out}"
    );
    // The stand-in's `sleep 4000` was a child of the script: it is gone with the group.
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert_eq!(
        survivors("sleep 400[0-2]"),
        0,
        "the hung binary's children are left running"
    );
}

#[test]
fn skip_lines_are_collected_and_listed_at_the_end() {
    let d = Dir::new("skips");
    let a = d.bin(
        "needs_scope",
        "echo 'skipped here: no user manager: a_test' >&2\necho 'test a_test ... ok'",
    );
    let b = d.bin("needs_bq", "echo 'skipped here: no bq: b_test'");
    let (code, out, _) = run(&["--binary", &a, "--binary", &b, "--cwd", "."], &d);
    assert_eq!(code, 0, "a skip is not a failure: {out}");
    let tail = out
        .rsplit_once("== 2 skip line(s)")
        .map(|x| x.1)
        .unwrap_or_default();
    assert!(
        tail.contains("skipped here: no user manager: a_test"),
        "{out}"
    );
    assert!(tail.contains("skipped here: no bq: b_test"), "{out}");
    assert!(!out.contains("== no skips"), "{out}");
}

#[test]
fn a_failing_binary_fails_the_run_and_the_next_one_still_runs() {
    let d = Dir::new("fail");
    let a = d.bin("breaks", "echo 'test t ... FAILED'\nexit 3");
    let b = d.bin("after_it", "echo fine");
    let (code, out, _) = run(&["--binary", &a, "--binary", &b, "--cwd", "."], &d);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("FAILED: breaks exited with 3"), "{out}");
    assert!(
        out.contains("== after_it start"),
        "the next binary was not run: {out}"
    );
    assert!(out.contains("== FAILED: breaks (exit 3)"), "{out}");
}

#[test]
fn the_budget_of_the_step_stops_it_and_names_the_binary_that_was_running() {
    let d = Dir::new("budget");
    let a = d.bin("first_ok", "echo ok");
    let b = d.bin("slow_one", "sleep 4003");
    let c = d.bin("never_started", "echo should-not-run");
    let (code, out, took) = run(
        &[
            "--binary",
            &a,
            "--binary",
            &b,
            "--binary",
            &c,
            "--limit-s",
            "100",
            "--budget-s",
            "3",
            "--cwd",
            ".",
        ],
        &d,
    );
    assert_eq!(code, 1, "{out}");
    assert!(
        out.contains("HUNG: slow_one after 3 s (the step's budget of 3 s ran out while it ran)"),
        "{out}"
    );
    assert!(!out.contains("never_started start"), "{out}");
    assert!(took < 20.0, "{out}");
    assert_eq!(survivors("sleep 4003"), 0);
}
