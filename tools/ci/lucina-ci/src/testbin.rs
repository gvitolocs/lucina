//! `lucina-ci test-binaries`: `cargo test` with a clock on every test binary.
//!
//! A hung test binary under a plain `cargo test` shows nothing until the whole job times out, and the job's log then names
//! no test. This command builds the test binaries once (`cargo test --no-run --message-format=json`), runs each one as its
//! own process group under its own time limit, prints `== <binary> start` before it and `== <binary> <n> s` after it, and
//! kills a binary that runs over its limit with `HUNG: <binary> after <n> s`. The whole step has a budget as well: when it
//! is used up the binary that was running is named and the step stops. Every `skipped here:` line a test printed is listed at
//! the end, so a test that skipped itself on this machine is seen in the log and not read as a pass.
//!
//! Exit 0 when every binary passed in time, 1 when one failed, hung or ran out of budget, 2 when nothing could be run.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// The line a test prints when it cannot run on this machine and returns instead.
pub const SKIP_MARK: &str = "skipped here:";

/// What one run of one binary came to.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    Passed,
    Failed(i32),
    /// Killed after its own time limit.
    Hung,
    /// Killed because the budget of the whole step ran out while it ran.
    OutOfBudget,
}

pub struct Settings {
    pub limit: Duration,
    pub budget: Duration,
    /// Time the step has already used (the build), counted against the budget.
    pub used: Duration,
}

/// The name a binary goes by in the log: its file name without the hash cargo appends (`merge_gate_timeout-1f2e…`).
pub fn display_name(exe: &Path) -> String {
    let stem = exe
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    match stem.rsplit_once('-') {
        Some((head, tail))
            if !head.is_empty()
                && tail.len() == 16
                && tail.chars().all(|c| c.is_ascii_hexdigit()) =>
        {
            head.to_string()
        }
        _ => stem,
    }
}

/// A stable sort by (display_name, path) so the run order does not depend on cargo's compile-completion order.
pub fn in_name_order(exes: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut v = exes;
    v.sort_by(|a, b| display_name(a).cmp(&display_name(b)).then_with(|| a.cmp(b)));
    v
}

/// The test executables named by `cargo test --no-run --message-format=json` output: every `compiler-artifact` message
/// whose profile is a test profile and that has an executable.
pub fn test_executables(json_lines: &str) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for line in json_lines.lines() {
        if !line.contains("\"reason\":\"compiler-artifact\"") {
            continue;
        }
        // The profile's own `test` flag: the target carries a `test` field too (a bin target says `true` there).
        let profile = line
            .split_once("\"profile\":{")
            .and_then(|(_, r)| r.split_once('}'))
            .map_or("", |(p, _)| p);
        if !profile.contains("\"test\":true") {
            continue;
        }
        if let Some(exe) = crate::runner::report_field(line, "executable") {
            let p = PathBuf::from(exe);
            if !out.contains(&p) {
                out.push(p);
            }
        }
    }
    out
}

fn kill_group_and_descendants(child: &Child) {
    let pid = child.id() as i32;
    // Anything the binary started in a group of its own (the tests' own steps do) is found by the parent chain first.
    let mut doomed: Vec<i32> = Vec::new();
    if let Ok(rd) = std::fs::read_dir("/proc") {
        let mut parent_of: Vec<(i32, i32)> = Vec::new();
        for e in rd.flatten() {
            let Ok(p) = e.file_name().to_string_lossy().parse::<i32>() else {
                continue;
            };
            let Ok(stat) = std::fs::read_to_string(format!("/proc/{p}/stat")) else {
                continue;
            };
            // `pid (comm) state ppid ...`: comm may hold spaces and parentheses, so read from the last `)`.
            let Some(rest) = stat.rsplit_once(')').map(|x| x.1) else {
                continue;
            };
            if let Some(pp) = rest.split_whitespace().nth(1).and_then(|s| s.parse().ok()) {
                parent_of.push((p, pp));
            }
        }
        let mut frontier = vec![pid];
        while let Some(f) = frontier.pop() {
            for (p, pp) in &parent_of {
                if *pp == f && !doomed.contains(p) {
                    doomed.push(*p);
                    frontier.push(*p);
                }
            }
        }
    }
    // SAFETY: kill(2) with a pid or a negated process group id this program started or found as a descendant of it.
    unsafe {
        libc::kill(-pid, libc::SIGKILL);
        libc::kill(pid, libc::SIGKILL);
        for d in doomed {
            libc::kill(d, libc::SIGKILL);
        }
    }
}

/// Runs one executable. Its output is echoed as it comes; `skips` takes every skip line.
fn run_one(
    exe: &Path,
    cwd: &Path,
    extra: &[String],
    limit: Duration,
    budget_left: Duration,
    skips: &mut Vec<String>,
) -> Result<(Verdict, f64), String> {
    let mut cmd = Command::new(exe);
    cmd.arg("--nocapture")
        .args(extra)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", exe.display()))?;
    let (tx, rx) = mpsc::channel::<String>();
    for (is_err, pipe) in [
        (
            false,
            child
                .stdout
                .take()
                .map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
        ),
        (
            true,
            child
                .stderr
                .take()
                .map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
        ),
    ] {
        if let Some(p) = pipe {
            let tx = tx.clone();
            std::thread::spawn(move || {
                for l in BufReader::new(p).split(b'\n').map_while(Result::ok) {
                    let mut s = String::from_utf8_lossy(&l).to_string();
                    if is_err {
                        s.insert(0, '\u{1}');
                    }
                    if tx.send(s).is_err() {
                        break;
                    }
                }
            });
        }
    }
    drop(tx);
    let start = Instant::now();
    let (own_limit, over) = if budget_left < limit {
        (budget_left, Verdict::OutOfBudget)
    } else {
        (limit, Verdict::Hung)
    };
    let verdict;
    loop {
        let mut quiet = true;
        while let Ok(l) = rx.try_recv() {
            quiet = false;
            echo(&l, skips);
        }
        if let Some(st) = child.try_wait().map_err(|e| e.to_string())? {
            // The binary has ended; what it still holds in the pipes is read to the end, but not waited for: a straggler it
            // left behind keeps the pipes open and must not hold this step.
            let t = Instant::now();
            while t.elapsed() < Duration::from_millis(500) {
                match rx.recv_timeout(Duration::from_millis(100)) {
                    Ok(l) => echo(&l, skips),
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        break;
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
            }
            verdict = if st.success() {
                Verdict::Passed
            } else {
                Verdict::Failed(st.code().unwrap_or(137))
            };
            // A group the binary leaves behind is not left running.
            // SAFETY: kill(2) on the process group this program started for the binary.
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            break;
        }
        if start.elapsed() >= own_limit {
            kill_group_and_descendants(&child);
            let _ = child.wait();
            while let Ok(l) = rx.recv_timeout(Duration::from_millis(200)) {
                echo(&l, skips);
            }
            verdict = over;
            break;
        }
        if quiet {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    Ok((verdict, start.elapsed().as_secs_f64()))
}

fn echo(l: &str, skips: &mut Vec<String>) {
    let (is_err, text) = match l.strip_prefix('\u{1}') {
        Some(t) => (true, t),
        None => (false, l),
    };
    if let Some(i) = text.find(SKIP_MARK) {
        skips.push(text[i..].trim().to_string());
    }
    if is_err {
        eprintln!("{text}");
    } else {
        println!("{text}");
    }
    let _ = std::io::stdout().flush();
}

/// Runs `exes` one after the other. Returns the exit code.
pub fn run_all(
    exes: &[PathBuf],
    cwd: &Path,
    extra: &[String],
    s: &Settings,
) -> Result<i32, String> {
    let begin = Instant::now();
    println!(
        "== order: {}",
        exes.iter()
            .map(|e| display_name(e))
            .collect::<Vec<_>>()
            .join(" ")
    );
    let _ = std::io::stdout().flush();
    let mut skips: Vec<String> = Vec::new();
    let mut failed: Vec<String> = Vec::new();
    for exe in exes {
        let name = display_name(exe);
        let left = s.budget.saturating_sub(s.used + begin.elapsed());
        if left.is_zero() {
            println!(
                "HUNG: {name} was not started: the step's budget of {} s is used up",
                s.budget.as_secs()
            );
            failed.push(format!("{name} (not started)"));
            break;
        }
        println!("== {name} start");
        let _ = std::io::stdout().flush();
        let (v, secs) = run_one(exe, cwd, extra, s.limit, left, &mut skips)?;
        match v {
            Verdict::Passed => println!("== {name} {secs:.0} s"),
            Verdict::Failed(c) => {
                println!("FAILED: {name} exited with {c} after {secs:.0} s");
                println!("== {name} {secs:.0} s");
                failed.push(format!("{name} (exit {c})"));
            }
            Verdict::Hung => {
                println!(
                    "HUNG: {name} after {secs:.0} s (limit {} s)",
                    s.limit.as_secs()
                );
                println!("== {name} {secs:.0} s");
                failed.push(format!("{name} (hung)"));
            }
            Verdict::OutOfBudget => {
                println!(
                    "HUNG: {name} after {secs:.0} s (the step's budget of {} s ran out while it ran)",
                    s.budget.as_secs()
                );
                println!("== {name} {secs:.0} s");
                failed.push(format!("{name} (out of budget)"));
                break;
            }
        }
    }
    if skips.is_empty() {
        println!("== no skips");
    } else {
        println!("== {} skip line(s)", skips.len());
        for l in &skips {
            println!("{l}");
        }
    }
    if failed.is_empty() {
        println!(
            "== all {} test binaries passed in {:.0} s",
            exes.len(),
            begin.elapsed().as_secs_f64()
        );
        Ok(0)
    } else {
        println!("== FAILED: {}", failed.join(", "));
        Ok(1)
    }
}

/// Builds the test binaries of `manifest` and says where they are.
pub fn build(manifest: &Path, cargo_args: &[String]) -> Result<Vec<PathBuf>, String> {
    let o = Command::new("cargo")
        .args([
            "test",
            "--release",
            "--locked",
            "--no-run",
            "--message-format=json",
        ])
        .args(cargo_args)
        .arg("--manifest-path")
        .arg(manifest)
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| format!("cannot start cargo: {e}"))?;
    if !o.status.success() {
        return Err(format!(
            "cargo test --no-run failed ({})",
            o.status
                .code()
                .map_or("signal".to_string(), |c| c.to_string())
        ));
    }
    let exes = in_name_order(test_executables(&String::from_utf8_lossy(&o.stdout)));
    if exes.is_empty() {
        return Err("cargo built no test binary".into());
    }
    Ok(exes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hash_suffix_is_not_part_of_the_name() {
        assert_eq!(
            display_name(Path::new("/t/deps/merge_gate_timeout-0123456789abcdef")),
            "merge_gate_timeout"
        );
        assert_eq!(display_name(Path::new("/t/deps/plain")), "plain");
        assert_eq!(display_name(Path::new("/t/deps/a-b")), "a-b");
    }

    #[test]
    fn only_test_artifacts_with_an_executable_are_taken() {
        let json = concat!(
            "{\"reason\":\"compiler-artifact\",\"profile\":{\"test\":false},\"executable\":\"/t/bin\"}\n",
            "{\"reason\":\"compiler-artifact\",\"target\":{\"kind\":[\"bin\"],\"test\":true},\"profile\":{\"test\":false},\"executable\":\"/t/bin2\"}\n",
            "{\"reason\":\"compiler-artifact\",\"profile\":{\"test\":true},\"executable\":null}\n",
            "{\"reason\":\"compiler-artifact\",\"profile\":{\"test\":true},\"executable\":\"/t/a-0123456789abcdef\"}\n",
            "{\"reason\":\"compiler-artifact\",\"profile\":{\"test\":true},\"executable\":\"/t/a-0123456789abcdef\"}\n",
            "{\"reason\":\"build-finished\",\"success\":true}\n",
        );
        assert_eq!(
            test_executables(json),
            vec![PathBuf::from("/t/a-0123456789abcdef")]
        );
    }

    #[test]
    fn binaries_run_in_name_order_not_in_cargo_order() {
        let input = vec![
            PathBuf::from("/t/deps/z-0001"),
            PathBuf::from("/t/deps/a-0002"),
            PathBuf::from("/t/deps/m-0003"),
        ];
        let got = in_name_order(input);
        assert_eq!(
            got,
            vec![
                PathBuf::from("/t/deps/a-0002"),
                PathBuf::from("/t/deps/m-0003"),
                PathBuf::from("/t/deps/z-0001"),
            ]
        );
    }

    #[test]
    fn same_set_in_another_input_order_gives_the_same_result() {
        let a = vec![
            PathBuf::from("/t/deps/z-0001"),
            PathBuf::from("/t/deps/a-0002"),
            PathBuf::from("/t/deps/m-0003"),
        ];
        let b = vec![
            PathBuf::from("/t/deps/m-0003"),
            PathBuf::from("/t/deps/z-0001"),
            PathBuf::from("/t/deps/a-0002"),
        ];
        assert_eq!(in_name_order(a), in_name_order(b));
    }

    #[test]
    fn two_executables_with_the_same_display_name_are_ordered_by_path() {
        let input = vec![
            PathBuf::from("/t/deps/bbb-0123456789abcdef"),
            PathBuf::from("/t/deps/aaa-0123456789abcdef"),
        ];
        let got = in_name_order(input);
        assert_eq!(
            got,
            vec![
                PathBuf::from("/t/deps/aaa-0123456789abcdef"),
                PathBuf::from("/t/deps/bbb-0123456789abcdef"),
            ]
        );
    }
}
