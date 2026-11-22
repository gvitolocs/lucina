//! `check-fast --merge-gate` with a limit PER step (2026-11-17: `unit-tests-tools-ci-lucina-ci` and `mutants-text` failed
//! merges at the one 180 s limit on a loaded buildhost). The real binary runs over a throwaway repository holding a copy of its
//! own sources — every file added on the work branch, so the crate's fast steps are `fmt-`, `clippy-` and
//! `unit-tests-tools-ci-lucina-ci`, and the source-id check passes because the files are byte for byte the ones this binary
//! was built from. `cargo`, `diffgate` and `gitleaks` are stubs first on PATH that sleep a stated amount, so no step is ever
//! really built and no test sleeps a real 180 s.
//!
//! Two ways a step gets its limit, and both are exercised: the table in `main.rs` (`MERGE_STEP_LIMITS`, and the 180 default)
//! with the override absent, and `LUCINA_CHECK_FAST_MERGE_CAP_S` with a `step=SECONDS` entry — the only way to see one step
//! pass at a limit the default would have cut, since the table's numbers are minutes apart.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

mod common;

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap().flatten() {
        let (p, q) = (e.path(), to.join(e.file_name()));
        if p.is_dir() {
            copy_dir(&p, &q);
        } else {
            fs::copy(&p, &q).unwrap();
        }
    }
}

/// The memory cap the spawned `check-fast` asks for, as in `merge_gate_timeout.rs`: these tests judge the time limit, not
/// the machine's RAM, so they ask for a cap every runner holds (`runner::budget`).
const MEM_CAP_GIB: &str = "1";

fn git(d: &Path, args: &[&str]) {
    let o = Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(d)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
}

/// A stub goes in through a child, never from this thread: it is exec'd by the step, and a file this thread still holds open
/// for writing makes that exec answer "Text file busy" (ETXTBSY) when another thread forks at the same moment, after which
/// the shell goes on to the next `diffgate` on PATH, the real one. The same helper in SecretaryIOS explains the measurement.
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
        "sh cannot write {}: the stand-in is never exec'd",
        path.display()
    );
}

fn stub(dir: &Path, name: &str, body: &str) {
    write_exec(&dir.join(name), &format!("#!/bin/sh\n{body}\n"));
}

/// A repository whose `main` is empty and whose `work` branch adds this crate: the whole crate is then "changed", which is
/// what makes its `unit-tests-tools-ci-lucina-ci` step run. The stub `cargo` sleeps `test_s` for `cargo test`, `clippy_s`
/// for `cargo clippy` and nothing for `cargo fmt`; the stub `diffgate` sleeps `diffgate_s`.
fn setup(tag: &str, test_s: &str, clippy_s: &str, diffgate_s: &str) -> (PathBuf, PathBuf) {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("mgsl-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let repo = d.join("repo");
    let dst = repo.join("tools/ci/lucina-ci");
    fs::create_dir_all(&dst).unwrap();
    copy_dir(&crate_dir.join("src"), &dst.join("src"));
    for f in ["Cargo.toml", "Cargo.lock"] {
        fs::copy(crate_dir.join(f), dst.join(f)).unwrap();
    }
    git(&repo, &["init", "-q", "-b", "main"]);
    git(
        &repo,
        &["commit", "-q", "--allow-empty", "-m", "empty base"],
    );
    git(&repo, &["checkout", "-q", "-b", "work"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "the crate"]);
    let bin = d.join("bin");
    fs::create_dir_all(&bin).unwrap();
    stub(
        &bin,
        "cargo",
        &format!(
            "for a in \"$@\"; do case \"$a\" in test) sleep {test_s};; clippy) sleep {clippy_s};; \
             fmt) ;; esac; done; exit 0"
        ),
    );
    stub(&bin, "diffgate", &format!("sleep {diffgate_s}; exit 0"));
    stub(&bin, "gitleaks", "exit 0");
    (repo, bin)
}

/// `cap` is `LUCINA_CHECK_FAST_MERGE_CAP_S`; `None` leaves the table and the 180 default in charge, and takes the variable
/// off the child's environment so whatever the caller's shell holds cannot decide the test.
fn run(repo: &Path, bin: &Path, cap: Option<&str>) -> (i32, String) {
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut c = Command::new(env!("CARGO_BIN_EXE_lucina-ci"));
    c.args([
        "check-fast",
        "--base",
        "main",
        "--merge-gate",
        "--mem-cap-gib",
        MEM_CAP_GIB,
    ])
    .current_dir(repo)
    .env("PATH", path);
    match cap {
        Some(v) => c.env("LUCINA_CHECK_FAST_MERGE_CAP_S", v),
        None => c.env_remove("LUCINA_CHECK_FAST_MERGE_CAP_S"),
    };
    let o = c.output().unwrap();
    (
        o.status.code().unwrap_or(-1),
        format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        ),
    )
}

fn step_line<'a>(out: &'a str, name: &str) -> &'a str {
    out.lines()
        .find(|l| l.split_whitespace().nth(1) == Some(name))
        .unwrap_or_else(|| panic!("no line for step {name}:\n{out}"))
}

/// With no override the limits are the table's and the default's, and the line prints the one actually enforced —
/// never above the step's own timeout.
#[test]
fn merge_gate_prints_each_step_s_own_limit_and_keeps_180_for_the_rest() {
    if common::skip_without_scope(
        "merge_gate_prints_each_step_s_own_limit_and_keeps_180_for_the_rest",
    ) {
        return;
    }
    let (repo, bin) = setup("limits", "0.2", "0.2", "0.2");
    let (_code, out) = run(&repo, &bin, None);
    assert!(
        step_line(&out, "unit-tests-tools-ci-lucina-ci").starts_with("ok  "),
        "the table's step passed: {out}"
    );
    assert!(
        step_line(&out, "unit-tests-tools-ci-lucina-ci").contains("/ 300s"),
        "unit-tests is at the table's 300: {}",
        step_line(&out, "unit-tests-tools-ci-lucina-ci")
    );
    assert!(
        step_line(&out, "clippy-tools-ci-lucina-ci").contains("/ 180s"),
        "a step no table names is at the default 180: {}",
        step_line(&out, "clippy-tools-ci-lucina-ci")
    );
    assert!(
        step_line(&out, "diffgate").contains("/ 60s"),
        "a limit never lifts a step above its own 60 s timeout: {}",
        step_line(&out, "diffgate")
    );
    assert!(
        !out.contains("near-limit"),
        "nothing came near a limit: {out}"
    );
    let _ = fs::remove_dir_all(repo.parent().unwrap());
}

/// The step whose limit is larger passes at a time the default cuts: the same sleep, two limits, two verdicts. And a
/// timeout is still a failure that names its own limit, never a skip.
#[test]
fn merge_gate_a_step_with_a_larger_limit_passes_where_the_default_cuts_it() {
    if common::skip_without_scope(
        "merge_gate_a_step_with_a_larger_limit_passes_where_the_default_cuts_it",
    ) {
        return;
    }
    let (repo, bin) = setup("bigger", "1.5", "1.5", "1.5");
    let (code, out) = run(&repo, &bin, Some("1,unit-tests-tools-ci-lucina-ci=3"));
    assert_ne!(code, 0, "two steps timed out: {out}");
    let passed = step_line(&out, "unit-tests-tools-ci-lucina-ci");
    assert!(
        passed.starts_with("ok  ") && passed.contains("/ 3s"),
        "{passed}"
    );
    for name in ["clippy-tools-ci-lucina-ci", "diffgate"] {
        let late = step_line(&out, name);
        assert!(
            late.starts_with("FAIL") && late.contains("/ 1s"),
            "{name} is at the default 1: {late}"
        );
        assert!(
            out.contains(&format!(
                "{name}: past its 1 s limit at merge: the machine is loaded; retry when it is quieter, or run scripts/check-fast in the worktree"
            )),
            "the message names the step's own limit: {out}"
        );
    }
    assert!(
        !out.lines()
            .any(|l| l.starts_with("skipped") && l.contains("past its")),
        "a timeout is never moved to skipped: {out}"
    );
    let _ = fs::remove_dir_all(repo.parent().unwrap());
}

/// A step that used more than 80% of its limit says so on its own line, so the next raise of a limit comes from a log
/// line rather than from a merge that failed.
#[test]
fn merge_gate_a_step_past_eighty_percent_of_its_limit_prints_near_limit() {
    if common::skip_without_scope(
        "merge_gate_a_step_past_eighty_percent_of_its_limit_prints_near_limit",
    ) {
        return;
    }
    let (repo, bin) = setup("near", "5.0", "0", "0");
    let (_code, out) = run(&repo, &bin, Some("10,unit-tests-tools-ci-lucina-ci=6"));
    let line = step_line(&out, "unit-tests-tools-ci-lucina-ci");
    assert!(
        line.starts_with("ok  ") && line.contains("/ 6s") && line.contains("near-limit"),
        "5.0 s of 6 s is past 80%: {line}\n{out}"
    );
    assert!(
        !step_line(&out, "diffgate").contains("near-limit"),
        "a step that ran 0 s of its 10 s limit is not near it: {}",
        step_line(&out, "diffgate")
    );
    let _ = fs::remove_dir_all(repo.parent().unwrap());
}
