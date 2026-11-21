//! A deadline must STOP a step, not only flag it. `runner::inner` gives every step its own cgroup scope (a step that cannot get one fails the check, and is
//! never run uncapped); at the deadline the step is SIGTERMed, and rounds of SIGKILL follow
//! until nothing of the step is left or a hard bound passes, and only then is the step reported.
//!
//! These tests drive the real binary over a throwaway repository whose `diffgate` step is a shell script that ignores
//! SIGTERM and hides its work where one kill of one pid list cannot reach it: in a grandchild, in a process the step
//! hands to a nested `systemd-run --scope` (which lands in a SIBLING cgroup, so no walk of this step's tree sees it
//! again), and in a process that leaves the step's process group with `setsid` (so no group signal sees it). Each judges
//! two things: the time the check records for the step, and what is still in the process table afterwards.
//!
//! The bound is `limit + KILL_GRACE_S + KILL_SLACK_S` (5 s and 2 s, one constant each in `runner.rs`) plus the room a
//! scope's own start takes. An integration test cannot name the constants — this crate is a binary — so they are written
//! here, and `runner::tests` pins them where they live.
//!
//! The processes a test starts are `sleep` calls whose duration is this test process's pid followed by the index of the
//! test that started them, which no other process on the machine carries and which no other test of this one matches:
//! each test searches the table for its own sleepers only, so the five running side by side cannot see or take each
//! other's work, and two `Drop` guards sweep what one test owns even when an assertion fails half way — the group of the
//! `check-fast` it started, and its own sleepers by name. (A copy of `/bin/sleep` under a unique name cannot be the
//! marker: coreutils dispatches on `argv[0]` and refuses an unknown program name, which is how that shape was ruled out
//! here. And a sweep of every sleeper of this pid is not a per-test sweep: it takes the sleepers of the tests still
//! running, which is how a step here came back `ok 1.0s` with its sleeps killed by a thread that had finished.)

use std::fs;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::Duration;

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

/// `runner::KILL_GRACE_S`: the time a step gets to leave on its own after the deadline's SIGTERM.
const GRACE_S: f64 = 5.0;
/// `runner::KILL_SLACK_S`: how long the rounds of SIGKILL keep coming before the step is reported anyway.
const SLACK_S: f64 = 2.0;
/// A scope's own start (systemd answers before the step runs) and a tick of the wait loop.
const OVERHEAD_S: f64 = 4.0;
/// The limit the step under test is given, and the wall it is therefore never allowed to record. It is not smaller
/// because the step's own script has to finish counting what it started before the deadline arrives: those retakes
/// (`counted`) spend up to a second and a fifth on a runner that is forking for dear life, and a step killed mid-count
/// echoes nothing and the test says the step started nothing.
const LIMIT: f64 = 6.0;
const LIMIT_S: &str = "6";
/// The memory cap the spawned `check-fast` asks for, as in `merge_gate_timeout.rs`: these tests judge the clock, not the
/// machine's RAM, so they ask for a cap every runner holds.
const MEM_CAP_GIB: &str = "1";

/// One sleeper of one test: `sleep <pid><k>`, a duration of days, so a step that outlives its deadline is still
/// sleeping when the test looks and the time recorded for it is the step's own length, never the bound.
fn sleeper(k: u32) -> String {
    format!("sleep {}{k}", std::process::id())
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

/// `bq` scrubs a secret by removing `GIT_CONFIG_KEY_0` and leaves `GIT_CONFIG_COUNT` pointing at it, after which every
/// `git` in the job dies on "missing config key GIT_CONFIG_KEY_0". Dropping the count too makes the family simply
/// unread, which is what a job of this test wants: the repositories it builds carry no configuration of their own.
/// Reported as a finding of its own; nothing that shells out to `git` runs under `bq` without it.
fn scrub_git_config(c: &mut Command) -> &mut Command {
    c.env_remove("GIT_CONFIG_COUNT")
}

fn git(d: &Path, args: &[&str]) {
    let o = scrub_git_config(
        Command::new("git")
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(d),
    )
    .output()
    .unwrap();
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
}

/// What one test drives: a repository holding this crate's sources (so the source-id check passes) and a `diffgate` stub
/// whose script is `body`.
struct Case {
    repo: PathBuf,
    bin: PathBuf,
}

impl Drop for Case {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(self.repo.parent().unwrap());
    }
}

/// The step's script: it ignores SIGTERM, says which cgroup it stands in, then runs
/// `body` and waits for what it started. The counts of `body` name this test's own markers: a pattern that covers every
/// sleeper of this process would count the sleepers of the tests running next door.
fn setup(tag: &str, body: &str) -> Case {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("gate03-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
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
    let script = format!(
        "#!/bin/sh\n\
         trap '' TERM\n\
         echo \"gate03-cgroup: $(cut -d: -f3 /proc/self/cgroup)\"\n\
         {body}\n\
         wait\n"
    );
    let stub = bin.join("diffgate");
    write_exec(&stub, &script);
    Case { repo, bin }
}

/// How many of the sleepers matching `pat` the step counted while it ran: its own echo, which the check prints with the
/// log of a step that failed. The count is retaken until `want` of them are there or a little over a second has passed:
/// a grandchild is three forks away, and on a loaded runner two of them were up when this was first read once, so the
/// step echoed 1 where it had started 2 and the test said the step started nothing.
fn counted(pat: &str, want: usize) -> String {
    format!(
        "c=0\n\
         for i in 1 2 3 4 5 6; do c=$(pgrep -fc '{pat}$'); [ \"$c\" -ge {want} ] && break; sleep 0.2; done\n\
         echo \"gate03-count {pat}$: $c\"\n"
    )
}

/// The step script of case (f). It hands work to a nested scope, counts it while forking is still possible, and then
/// takes every task slot of its own scope. From the first fill on the script is shell builtins only: `$(pgrep …)` would
/// not be a measurement but a fork that a full scope cannot grant, and a fork that dash cannot make kills the step there
/// and then (measured: the script died on one, and the deadline never arrived). So how many processes the step holds comes
/// from the scope's own `cgroup.procs`, read line by line with the `read` builtin, and the cap and the charge from
/// `pids.max` and `pids.current`. No fork is attempted after the fill: the step and the launcher that waits for it share
/// this cgroup, so a forked kill at the deadline would already have nowhere to go.
fn fill_body(nested: &str, plain: &str) -> String {
    format!(
        "systemd-run --user --scope --quiet --collect --unit=gate03-f-$$-{}.scope {nested} &\n\
         {plain} &\n\
         {}\n{}\n{}\n\
         read -r cl < /proc/self/cgroup; cg=/sys/fs/cgroup${{cl#0::}}\n\
         read -r lim < \"$cg/pids.max\"\n\
         i=0; n=0\n\
         while [ $i -lt 400 ]; do\n\
         \ti=$((i+1))\n\
         \t{plain} &\n\
         \tn=0; while read -r p; do n=$((n+1)); done < \"$cg/cgroup.procs\"\n\
         \t[ \"$n\" -ge \"$lim\" ] && break\n\
         done\n\
         read -r cur < \"$cg/pids.current\"\n\
         echo \"gate03-fill: $cur $lim $i\"\n",
        std::process::id(),
        counted(nested, 1),
        counted(plain, 1),
        outside_cgroup(nested),
    )
}

/// A count of the step's sleepers named by `one` that are NOT in the step's own cgroup — which is what a nested scope
/// makes, and what no walk of the step's tree can find again.
fn outside_cgroup(one: &str) -> String {
    format!(
        "me=$(cut -d: -f3 /proc/self/cgroup); n=0\n\
         for p in $(pgrep -f '{one}$'); do\n\
         \t[ \"$(cut -d: -f3 /proc/$p/cgroup 2>/dev/null)\" != \"$me\" ] && n=$((n+1))\n\
         done\n\
         echo \"gate03-outside: $n\"\n"
    )
}

/// The same for a process group, which is what `setsid` leaves. `setsid --fork` takes a moment to leave the group (the
/// fork, the `setsid(2)`, the exec), and a count taken inside that moment sees the sleeper still in the step's own
/// group (GATE-11). So this observes before it judges: it polls, for up to 5 s, until exactly one process matches `one`
/// and that one stands in a group other than the step's (an empty read of `stat`, from a process that exited meanwhile, is
/// no group at all), and says whether that ever happened in `gate03-setsid-wait`.
fn outside_group(one: &str) -> String {
    format!(
        "me=$(ps -o pgid= -p $$ | tr -d ' '); n=0; w=timeout; i=0\n\
         while [ $i -lt 50 ]; do\n\
         \tn=0; all=0\n\
         \tfor p in $(pgrep -f '{one}$'); do\n\
         \t\tall=$((all+1))\n\
         \t\tg=$(awk '{{print $5}}' /proc/$p/stat 2>/dev/null)\n\
         \t\t[ -n \"$g\" ] && [ \"$g\" != \"$me\" ] && n=$((n+1))\n\
         \tdone\n\
         \tif [ $n -ge 1 ] && [ $all -eq 1 ]; then w=ok; break; fi\n\
         \tsleep 0.1; i=$((i+1))\n\
         done\n\
         echo \"gate03-setsid-wait: $w\"\n\
         echo \"gate03-othergroup: $n\"\n"
    )
}

/// The group of one `check-fast`: what a test leaves no process behind with. Only the check and what inherits its group;
/// a step's sleepers stand in a group of their own and are taken by `Markers`, because the step is started outside it.
struct Sweep(u32);

impl Drop for Sweep {
    fn drop(&mut self) {
        let _ = Command::new("kill")
            .args(["-KILL", &format!("-{}", self.0)])
            .stderr(Stdio::null())
            .status();
    }
}

/// The sleepers of ONE test, named by their exact marker. A panic half way unwinds this guard, and the marker is unique to
/// the test that built it: the tests run side by side in one process, so a sweep of every sleeper of this pid would take
/// the sleepers of the tests still running — which is how a step came back `ok 1.0s`, its sleeps killed by a thread that
/// had already finished.
struct Markers(Vec<String>);

impl Drop for Markers {
    fn drop(&mut self) {
        for m in self.0.iter() {
            let _ = Command::new("pkill")
                .args(["-KILL", "-f", &format!("{m}$")])
                .stderr(Stdio::null())
                .status();
        }
    }
}

/// Runs the merge gate over `case`.
fn run(case: &Case, cap: &str) -> (i32, String) {
    run_env(case, cap, None, &[])
}

/// `quota` is `LUCINA_CI_CPU_QUOTA`, the one switch the runner has for the properties of a step's scope: with it set,
/// `run_unit` asks systemd for `CPUQuota=<it>,TasksMax=256` (`runner.rs`). `None` passes on what this process already
/// carries, which is the check's own value (`scripts/check-fast` exports it) — so the ordinary cases here run with a
/// task-capped scope, exactly as a real check does.
fn run_env(case: &Case, cap: &str, quota: Option<&str>, envs: &[(&str, &str)]) -> (i32, String) {
    let path = format!(
        "{}:{}",
        case.bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut c = Command::new(env!("CARGO_BIN_EXE_lucina-ci"));
    c.args([
        "check-fast",
        "--base",
        "main",
        "--merge-gate",
        "--no-build",
        "--mem-cap-gib",
        MEM_CAP_GIB,
    ])
    .current_dir(&case.repo)
    .env("PATH", path)
    .env("LUCINA_CHECK_FAST_MERGE_CAP_S", cap)
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    // The check and everything it starts live in this group and no other, so `Sweep` takes them all without touching the
    // test process, which stands in a group of its own.
    .process_group(0);
    c.envs(envs.iter().copied());
    if let Some(q) = quota {
        c.env("LUCINA_CI_CPU_QUOTA", q);
    }
    // The check finds the changed files with `git`, and the report names the commit with it.
    scrub_git_config(&mut c);
    let child = c.spawn().expect("check-fast starts");
    let _sweep = Sweep(child.id());
    let o = child.wait_with_output().expect("check-fast ends");
    let mut text = String::from_utf8_lossy(&o.stdout).to_string();
    text.push_str(&String::from_utf8_lossy(&o.stderr));
    (o.status.code().unwrap_or(-1), text)
}

/// A step line as `fast_step` prints it (`FAIL diffgate   9.2s / 4s`): whether it passed, and the time recorded for it.
fn step_line(out: &str, name: &str) -> Option<(bool, f64)> {
    for l in out.lines() {
        let mut w = l.split_whitespace();
        if !matches!(w.next()?, "ok" | "FAIL") || w.next()? != name {
            continue;
        }
        let secs = l
            .split_whitespace()
            .find(|t| t.ends_with('s') && t.trim_end_matches('s').parse::<f64>().is_ok())?
            .trim_end_matches('s')
            .parse::<f64>()
            .ok()?;
        return Some((l.trim_start().starts_with("ok"), secs));
    }
    None
}

/// A line the step's script echoed (its log reaches this output indented, when the step failed).
fn echoed(out: &str, label: &str) -> Option<String> {
    out.lines()
        .map(str::trim_start)
        .find_map(|l| l.strip_prefix(label))
        .map(|v| v.trim().to_string())
}

fn echoed_count(out: &str, label: &str) -> usize {
    echoed(out, label)
        .and_then(|v| v.parse().ok())
        .unwrap_or_default()
}

fn echo_count(out: &str, sleeper: &str) -> usize {
    echoed_count(out, &format!("gate03-count {sleeper}$: "))
}

fn cgroup_of(out: &str) -> Option<String> {
    echoed(out, "gate03-cgroup: ")
}

/// The cgroup this test stands in, which is what a step that got no scope is left in.
fn own_cgroup() -> String {
    fs::read_to_string("/proc/self/cgroup")
        .unwrap_or_default()
        .lines()
        .find_map(|l| l.strip_prefix("0::"))
        .unwrap_or_default()
        .to_string()
}

/// Did the step stand in a cgroup of its own rather than the one it was handed? Comparing the two is what tells the
/// scope `run_unit` made for this step from a scope the whole test suite already runs inside.
fn scoped(out: &str) -> bool {
    cgroup_of(out).is_some_and(|c| c != own_cgroup())
}

/// The processes matching `pat` that are still running. One killed but not yet reaped is a zombie whose command line is
/// gone: neither it nor its corpse can do anything, so neither counts as a survivor of the kill.
fn survivors(pat: &str) -> Vec<u32> {
    let o = Command::new("pgrep")
        .args(["-f", pat])
        .output()
        .expect("pgrep runs");
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .filter_map(|l| l.trim().parse::<u32>().ok())
        .filter(|p| {
            fs::read_to_string(format!("/proc/{p}/status"))
                .unwrap_or_default()
                .lines()
                .any(|l| l.starts_with("State:") && !l.contains('Z'))
        })
        .collect()
}

/// Waits out the moment a SIGKILL takes to show in the process table, then says what is left. Bounded on purpose: a
/// survivor is reported, never waited on.
fn survivors_after_a_moment(pat: &str) -> Vec<u32> {
    for _ in 0..20 {
        let s = survivors(pat);
        if s.is_empty() {
            return s;
        }
        sleep(Duration::from_millis(250));
    }
    survivors(pat)
}

/// What the host answered when it was asked, in the test's own name, whether it can hand work to a nested
/// `systemd-run --scope`. Three answers, because two of them mean opposite things and the old `bool` read them the same
/// way (PF-R027 §1).
enum Probe {
    /// The scope was made. The sibling assertions are owed, and a run without them is a failure.
    Takes,
    /// The host has no user manager, said in the words only that condition produces. The sibling case may pass without
    /// a sibling, and says so out loud.
    NoUserManager(String),
    /// The probe did not answer the question: `systemd-run` would not start, the unit name was already loaded, the bus
    /// refused in a way this host does not otherwise refuse. Nothing about the host follows from that, so the test
    /// FAILS rather than skipping — libtest has no dynamic `ignored`, and a skip that counts as a pass is the hole this
    /// enum exists to close.
    DidNotAnswer(String),
}

/// Can this host hand work to a nested `systemd-run --scope` at all? Asked of the host, in the test's own name, and not
/// read out of the step's output: a run where the sibling never appeared must be a failure, and only a host that says it
/// cannot make a scope in the first place may let that case pass without it.
fn nested_scopes_possible(k: u32) -> Probe {
    // One unit name per caller: `k` is that test's own sleeper index, the number `sleeper(k)` puts in its command line.
    // Two tests sharing one probe name is how a lost probe came to skip the assertions of both.
    let unit = format!("gate03-probe-{}-{}.scope", std::process::id(), k);
    let ask = format!("--unit={unit}");
    let o = match Command::new("systemd-run")
        .args([
            "--user",
            "--scope",
            "--quiet",
            "--collect",
            &ask,
            "/bin/true",
        ])
        .stdin(Stdio::null())
        .output()
    {
        Ok(o) => o,
        Err(e) => return Probe::DidNotAnswer(format!("`systemd-run` could not be started: {e}")),
    };
    let why = String::from_utf8_lossy(&o.stderr).trim().to_string();
    if o.status.success() {
        return Probe::Takes;
    }
    // Only the phrases a machine with no user `systemd` answers with may be read as "this host cannot". Measured on this
    // host: with `$DBUS_SESSION_BUS_ADDRESS` and `$XDG_RUNTIME_DIR` removed `systemd-run --user` says "Failed to connect
    // to user scope bus via local transport: $DBUS_SESSION_BUS_ADDRESS and $XDG_RUNTIME_DIR not defined", and with the
    // name already in use it says "Failed to start transient scope unit: Unit ….scope was already loaded or has a
    // fragment file" — which is not in this list, and so is `DidNotAnswer`, exactly as PF-R027 §1 asks.
    const NO_USER_MANAGER: [&str; 4] = [
        "not been booted with systemd",
        "Failed to connect to user scope bus",
        "Failed to connect to bus",
        "Cannot connect to bus",
    ];
    if NO_USER_MANAGER.iter().any(|p| why.contains(p)) {
        Probe::NoUserManager(why)
    } else {
        Probe::DidNotAnswer(format!("{unit} exited with {:?}: {why}", o.status.code()))
    }
}

/// What every deadline test says about a step that ran past its limit: it failed, it was reported as past its limit, it
/// did not run past the bound the deadline sets, and none of its sleepers is left.
fn assert_stopped(out: &str, sleepers: &[String], what: &str) {
    let (ok, secs) = step_line(out, "diffgate").expect("the diffgate step reported a time");
    assert!(
        !ok,
        "{what}: a step past its limit is never reported as passing: {out}"
    );
    let bound = LIMIT + GRACE_S + SLACK_S + OVERHEAD_S;
    assert!(
        secs <= bound,
        "{what}: the step recorded {secs}s against a {LIMIT_S}s limit. A limit that does not stop the step is not a \
         limit (GATE-03): the bound here is {bound}s (limit + grace + slack + a scope's start), and the step's own \
         sleep is measured in days: {out}"
    );
    assert!(
        out.contains(&format!("diffgate: past its {LIMIT_S} s limit at merge")),
        "{what}: a step killed at its deadline is reported as past its limit: {out}"
    );
    for s in sleepers {
        let left = survivors_after_a_moment(&format!("{s}$"));
        assert!(
            left.is_empty(),
            "{what}: {left:?} survived the deadline kill of their step (searched for {s})"
        );
    }
}

/// (a) A step that ignores SIGTERM, with a child and a grandchild that ignore it too: the grace ends, the SIGKILL rounds
/// start, and all of it is gone inside the bound.
#[test]
fn a_step_that_ignores_sigterm_is_gone_at_its_deadline() {
    if common::skip_without_scope("a_step_that_ignores_sigterm_is_gone_at_its_deadline") {
        return;
    }
    let s = sleeper(1);
    let _markers = Markers(vec![s.clone()]);
    let body = format!("{s} &\n( {s} & ) &\n{}", counted(&s, 2));
    let case = setup("term", &body);
    let (_code, out) = run(&case, LIMIT_S);
    assert!(
        scoped(&out),
        "a step is meant to get a scope of its own; it stood in {} instead, which is no scope at all. A runner \
         that cannot give a scope caps nothing, and the check fails instead.",
        cgroup_of(&out).unwrap_or_default()
    );
    assert_eq!(
        echo_count(&out, &s),
        2,
        "the step did not start the child and the grandchild that ignore SIGTERM: {out}"
    );
    assert_stopped(&out, &[s], "sigterm ignored by child and grandchild");
}

/// (b) A step that cannot get its scope is NOT run uncapped. The machine here has no systemd user manager
/// (`systemctl --user show-environment` fails, as it does where there is no user bus), a `sudo` that would say yes if it
/// were asked, and a `systemd-run` that refuses. The check must fail with one message that names the step and the reason;
/// the step's script must never have started; and `sudo` must never have been called. There is no environment switch
/// that turns this off: setting the retired `LUCINA_CI_NO_SCOPE` changes nothing.
#[test]
fn a_step_that_cannot_get_a_scope_fails_and_never_runs_uncapped() {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("gate03-noscope-{}", std::process::id()));
    let ran = d.join("ran");
    let log = d.join("calls.log");
    let case = setup("noscope", &format!("touch '{}'", ran.display()));
    let stub = |name: &str, body: &str| {
        write_exec(&case.bin.join(name), &format!("#!/bin/sh\n{body}\n"));
    };
    stub(
        "id",
        "case \"$1\" in -u|-g) echo 1000 ;; *) exec /usr/bin/id \"$@\" ;; esac",
    );
    stub(
        "systemctl",
        &format!("echo \"systemctl $*\" >> '{}'\nexit 1", log.display()),
    );
    stub(
        "systemd-run",
        &format!("echo \"systemd-run $*\" >> '{}'\nexit 1", log.display()),
    );
    stub(
        "sudo",
        &format!("echo \"sudo $*\" >> '{}'\nexit 0", log.display()),
    );
    let (code, out) = run_env(&case, "20", None, &[("LUCINA_CI_NO_SCOPE", "1")]);
    assert_ne!(code, 0, "a check with no scope for a step must fail: {out}");
    assert!(
        out.contains("cannot get a memory-capped scope") && out.contains("no systemd user manager"),
        "the failure names the reason: {out}"
    );
    assert!(
        out.contains("step fast/diffgate:"),
        "the failure names the step: {out}"
    );
    assert!(
        !ran.exists(),
        "the step ran although it had no scope, i.e. uncapped: {out}"
    );
    let calls = fs::read_to_string(&log).unwrap_or_default();
    assert!(
        !calls.lines().any(|l| l.starts_with("sudo")),
        "lucina-ci called sudo: {calls}"
    );
    assert!(
        !calls.lines().any(|l| l.starts_with("systemd-run")),
        "the step was handed to systemd-run although no user manager answered: {calls}"
    );
}

/// lucina-ci never builds an argv with `sudo` in it. The behavioural half is the test above (a `sudo` stub that
/// is never called); this half reads the sources, where a command line is built, and refuses the word as a program name
/// or an argument, in code (comments may say it).
#[test]
fn no_source_of_lucina_ci_builds_a_sudo_argv() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut seen = 0;
    for e in fs::read_dir(&src).unwrap().flatten() {
        let p = e.path();
        if p.extension().is_none_or(|x| x != "rs") {
            continue;
        }
        seen += 1;
        for (i, l) in fs::read_to_string(&p).unwrap().lines().enumerate() {
            let code = l.split("//").next().unwrap_or("");
            assert!(
                !code.contains("\"sudo\"") && !code.contains("sudo -"),
                "{}:{}: a sudo argv in code: {l}",
                p.display(),
                i + 1
            );
        }
    }
    assert!(seen > 0, "no sources read under {}", src.display());
}

/// (c) A step that finishes inside its limit is not touched by any of this: it passes, the time recorded for it is its
/// own work and not the kill machinery, and the straggler it leaves behind is taken when the step ends.
#[test]
fn a_step_that_ends_in_time_is_untouched() {
    if common::skip_without_scope("a_step_that_ends_in_time_is_untouched") {
        return;
    }
    let s = sleeper(3);
    let _markers = Markers(vec![s.clone()]);
    // `exit 0` before the script's `wait`: the step is meant to end at once and leave a straggler standing, which is what
    // the runner's kill at the end of a step is for.
    let case = setup(
        "ontime",
        &format!("{s} &\necho gate03-ontime: yes\nexit 0\n"),
    );
    let (_code, out) = run(&case, "20");
    let (ok, secs) = step_line(&out, "diffgate").expect("the diffgate step reported a time");
    assert!(
        ok,
        "a step that finished at once under a 20 s limit passes: {out}"
    );
    assert!(secs < 12.0, "a finished step recorded {secs}s: {out}");
    assert!(
        !out.contains("diffgate: past its"),
        "a step in time is never past its limit: {out}"
    );
    let left = survivors_after_a_moment(&s);
    assert!(
        left.is_empty(),
        "the straggler of a step that ended in time outlived it: {left:?}"
    );
}

/// (d) Work the step hands to a nested `systemd-run --scope`: systemd puts it in a SIBLING cgroup, so it leaves this
/// step's scope for good and no walk of that tree sees it again. It stays in the step's process group, which is what the
/// deadline reaches it with — and nothing else here does.
#[test]
fn work_handed_to_a_nested_scope_is_gone_at_its_deadline() {
    if common::skip_without_scope("work_handed_to_a_nested_scope_is_gone_at_its_deadline") {
        return;
    }
    let (nested, plain) = (sleeper(4), sleeper(5));
    let _markers = Markers(vec![nested.clone(), plain.clone()]);
    let counts = format!("{}\n{}", counted(&nested, 1), counted(&plain, 1));
    let body = format!(
        "systemd-run --user --scope --quiet --collect --unit=gate03-n-$$-{}.scope {nested} &\n\
         {plain} &\n\
         {}\n{counts}\n",
        std::process::id(),
        outside_cgroup(&nested),
    );
    let case = setup("nested", &body);
    let (_code, out) = run(&case, LIMIT_S);
    assert_eq!(
        echo_count(&out, &plain),
        1,
        "the step's own sleeper did not start: {out}"
    );
    let saw_sibling = echo_count(&out, &nested);
    let outside = echoed_count(&out, "gate03-outside: ");
    match nested_scopes_possible(4) {
        // A probe that did not answer the question says nothing about the host, so it must not be read as "this host
        // cannot": that reading is what let a lost probe skip the assertions this test exists for.
        Probe::DidNotAnswer(why) => panic!(
            "the nested-scope probe for this test could not run, so nothing here says whether this host takes sibling \
             scopes: {why}"
        ),
        Probe::NoUserManager(why) => eprintln!(
            "note: this host makes no nested scope ({why}), so this run tested the plain tree only"
        ),
        Probe::Takes => {
            // On a host that CAN make the sibling, this test is about nothing else: it is a failure when the sibling was
            // never there, because a deadline kill that had no sibling to reach proves the claim it is meant to prove.
            assert_eq!(
                saw_sibling, 1,
                "this host takes a nested scope, yet the step never got its sleeper into one: {out}"
            );
            assert!(
                outside >= 1,
                "the nested scope's sleeper was counted inside the step's own cgroup, so the kill had a tree to walk and \
                 the case this test exists for never happened: {out}"
            );
        }
    }
    assert_stopped(&out, &[nested, plain], "work handed to a nested scope");
}

/// (e) Work that leaves the step's process group with `setsid` but stays in its cgroup: no group signal reaches it, so
/// this is the case only the cgroup route takes. A run where that process never appeared is a failure, not a skip.
#[test]
fn work_handed_to_a_new_session_is_gone_at_its_deadline() {
    if common::skip_without_scope("work_handed_to_a_new_session_is_gone_at_its_deadline") {
        return;
    }
    let (detached, plain) = (sleeper(6), sleeper(7));
    let _markers = Markers(vec![detached.clone(), plain.clone()]);
    let counts = format!("{}\n{}", counted(&detached, 1), counted(&plain, 1));
    let body = format!(
        // `setsid --fork` (util-linux, on buildhost and on GitHub's ubuntu): the sleeper is a forked child that always has a
        // session of its own, whether or not the shell made the job a group leader.
        "setsid --fork {detached} &\n{plain} &\n{}\n{counts}\n",
        outside_group(&detached),
    );
    let case = setup("session", &body);
    let (_code, out) = run(&case, LIMIT_S);
    // Observe before judging (GATE-11): the step waited, up to 5 s, until the sleeper stood in a group other than the
    // step's. If that never happened (or the step was cut off before it said so), nothing was judged, and that is a
    // failure here — never a pass.
    assert_eq!(
        echoed(&out, "gate03-setsid-wait: ").as_deref(),
        Some("ok"),
        "setsid did not take effect within 5 s: nothing was judged: {out}"
    );
    assert!(
        scoped(&out),
        "the cgroup route needs a cgroup of its own to walk: {}",
        cgroup_of(&out).unwrap_or_default()
    );
    assert_eq!(
        echo_count(&out, &plain),
        1,
        "the step's own sleeper did not start: {out}"
    );
    // Nothing here depends on the host: `setsid` is a plain program, and the step's own cgroup is already required above.
    // So a run that never had a process outside the group is not a skip — it is this test failing to test anything.
    assert_eq!(
        echo_count(&out, &detached),
        1,
        "the step never got its `setsid` sleeper up, so there was nothing outside the process group for the deadline \
         to reach: {out}"
    );
    assert!(
        echoed_count(&out, "gate03-othergroup: ") >= 1,
        "the `setsid` sleeper was counted in the step's own process group, so the group signal is what took it and the \
         cgroup route this test exists for was never exercised: {out}"
    );
    assert_stopped(&out, &[detached, plain], "work handed to a new session");
}

/// (f) The case the second review round (PF-R024 §1) put its finger on. The deadline kill used to be the `kill` *program*,
/// started as a child of the launcher, and its failure was thrown away. The launcher shares the step's cgroup, so a step
/// that has taken every task slot of its `TasksMax=256` scope leaves no slot for that child: nothing is signalled, the
/// report is bounded and short, and the work is still running. Only the group signal reaches the process the step handed
/// to a nested scope, because that one stands outside this cgroup tree — so this case fills the slots AND starts the
/// sibling, and judges both. The kill is `kill(2)` now: a system call takes no task slot.
#[test]
fn a_step_that_has_taken_every_task_slot_is_still_gone_at_its_deadline() {
    if common::skip_without_scope(
        "a_step_that_has_taken_every_task_slot_is_still_gone_at_its_deadline",
    ) {
        return;
    }
    let (nested, plain) = (sleeper(8), sleeper(9));
    let _markers = Markers(vec![nested.clone(), plain.clone()]);
    let case = setup("tasksfull", &fill_body(&nested, &plain));
    // `LUCINA_CI_CPU_QUOTA` is the switch the runner already has, and the only one: `run_unit` writes
    // `CPUQuota=<it>,TasksMax=256` on the scope whenever it is set. There is no switch for a smaller `TasksMax`, so the
    // step fills all 256 of them. The value is the production one (`scripts/check-fast` exports `400%`): the task cap,
    // not the CPU one, is what is under test.
    let (_code, out) = run_env(&case, LIMIT_S, Some("400%"), &[]);
    assert!(
        scoped(&out),
        "the task cap is a scope property, so this case needs a scope of its own: {}",
        cgroup_of(&out).unwrap_or_default()
    );
    let fill = echoed(&out, "gate03-fill: ").unwrap_or_default();
    let mut w = fill.split_whitespace();
    let charged: u32 = w.next().and_then(|v| v.parse().ok()).unwrap_or_default();
    let lim: u32 = w.next().and_then(|v| v.parse().ok()).unwrap_or_default();
    assert!(
        (32..=256).contains(&lim),
        "the step's scope reports `pids.max` = {fill}: this case needs a task cap the step can actually fill, which is \
         what `TasksMax=256` is for: {out}"
    );
    assert!(
        charged >= lim,
        "the step reached {charged} of the {lim} task slots charged to its scope, so a fork was still possible there \
         and this case measured nothing: {out}"
    );
    assert_eq!(
        echo_count(&out, &plain),
        1,
        "the step's own sleeper did not start: {out}"
    );
    match nested_scopes_possible(8) {
        // Its own probe, in its own name (`k` = 8, the index of the sleeper this case starts). A probe that did not
        // answer is this test failing to have asked, not a host that cannot: it fails, and does not skip.
        Probe::DidNotAnswer(why) => panic!(
            "the nested-scope probe for this test could not run, so nothing here says whether this host takes the \
             sibling this case is about: {why}"
        ),
        Probe::NoUserManager(why) => eprintln!(
            "note: this host makes no nested scope ({why}), so the sibling this case is about was not started"
        ),
        Probe::Takes => {
            assert_eq!(
                echo_count(&out, &nested),
                1,
                "this host takes a nested scope, yet the step never got its sleeper into one before it filled its \
                 slots: {out}"
            );
            assert!(
                echoed_count(&out, "gate03-outside: ") >= 1,
                "the nested scope's sleeper stood inside the step's own cgroup, where the fork-free `cgroup.kill` would \
                 have taken it and the sibling this case exists for never happened: {out}"
            );
        }
    }
    assert_stopped(
        &out,
        &[nested, plain],
        "a step that has taken every task slot of its scope",
    );
}
