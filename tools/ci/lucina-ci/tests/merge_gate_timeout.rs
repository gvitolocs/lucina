//! `check-fast --merge-gate`: a step that runs past its limit FAILS the merge, it is never moved to "skipped"
//! (2026-11-12: unit-tests skipped at the merge gate under load on SecretaryIOS). The real binary runs over a throwaway
//! repository that holds a copy of its own sources (so the source-id check passes), a stub `diffgate` first on PATH, and a limit
//! injected with LUCINA_CHECK_FAST_MERGE_CAP_S (never a real 180 s sleep). The wait for the queue slot is outside the binary:
//! scripts/check-fast takes the slot first, so a step's limit counts its own run time only.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

mod common;

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

/// The memory cap the spawned `check-fast` asks for. The binary refuses to start on a runner that cannot hold its cap plus a reserve
/// (the default 8 GiB cap needs 10 GiB; a private repository's 8 GB runner has 7.7), and these tests judge the time limit, not the
/// machine's RAM, so they ask for a cap every runner holds (`runner::budget`).
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

/// A repository on `main` holding this crate's sources, a branch with one added file, and a stub `diffgate` that sleeps `sleep_s`.
fn setup(tag: &str, sleep_s: u32) -> (PathBuf, PathBuf) {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("mgt-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let dst = d.join("repo/tools/ci/lucina-ci");
    fs::create_dir_all(&dst).unwrap();
    copy_dir(&crate_dir.join("src"), &dst.join("src"));
    for f in ["Cargo.toml", "Cargo.lock"] {
        fs::copy(crate_dir.join(f), dst.join(f)).unwrap();
    }
    let repo = d.join("repo");
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "base"]);
    git(&repo, &["checkout", "-q", "-b", "work"]);
    fs::write(repo.join("NOTES.txt"), "a change\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "change"]);
    let bin = d.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let stub = bin.join("diffgate");
    write_exec(&stub, &format!("#!/bin/sh\nsleep {sleep_s}\nexit 0\n"));
    (repo, bin)
}

fn run(repo: &Path, bin: &Path, cap: &str) -> (i32, String) {
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let o = Command::new(env!("CARGO_BIN_EXE_lucina-ci"))
        .args([
            "check-fast",
            "--base",
            "main",
            "--merge-gate",
            "--no-build",
            "--mem-cap-gib",
            MEM_CAP_GIB,
        ])
        .current_dir(repo)
        .env("PATH", path)
        .env("LUCINA_CHECK_FAST_MERGE_CAP_S", cap)
        .output()
        .unwrap();
    (
        o.status.code().unwrap_or(-1),
        format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        ),
    )
}

#[test]
fn merge_gate_step_past_its_limit_fails_the_merge_and_is_never_a_skip() {
    if common::skip_without_scope(
        "merge_gate_step_past_its_limit_fails_the_merge_and_is_never_a_skip",
    ) {
        return;
    }
    let (repo, bin) = setup("late", 30);
    let (code, out) = run(&repo, &bin, "1");
    assert_ne!(code, 0, "a timed-out step fails the merge: {out}");
    assert!(
        out.contains("diffgate: past its 1 s limit at merge: the machine is loaded; retry when it is quieter, or run scripts/check-fast in the worktree"),
        "{out}"
    );
    assert!(
        !out.lines()
            .any(|l| l.starts_with("skipped") && l.contains("past its")),
        "a timeout is never moved to skipped: {out}"
    );
    assert!(out.contains(" failed, "), "{out}");
    let _ = fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn merge_gate_step_within_its_limit_passes_without_the_late_message() {
    if common::skip_without_scope(
        "merge_gate_step_within_its_limit_passes_without_the_late_message",
    ) {
        return;
    }
    // the step ran for 2 s of its own, under a 20 s limit; any time spent waiting for a queue slot is not on this clock
    let (repo, bin) = setup("ontime", 2);
    let (_code, out) = run(&repo, &bin, "20");
    assert!(out.lines().any(|l| l.starts_with("ok   diffgate")), "{out}");
    assert!(!out.contains("past its"), "{out}");
    let _ = fs::remove_dir_all(repo.parent().unwrap());
}
