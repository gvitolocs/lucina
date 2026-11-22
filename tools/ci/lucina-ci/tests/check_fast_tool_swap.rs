//! `check-fast` keeps the tool it started with, and leaves the directory it started in (and
//! that directory).
//!
//! Two things replace the binary a run is made of while the run is going on: another `check-fast` in the same worktree, and
//! a branch landing at the merge. Both build into `$CARGO_TARGET_DIR/release/lucina-ci`, which is the name the run and every
//! one of its steps re-starts, so the run hits `Failed to find executable .../release/lucina-ci (deleted)`.
//! `scripts/check-fast` has answered this since 2026-11-11 by copying the tool to a private name and running the check from
//! the copy. These tests pin that the copy is what runs, and pin the same guarantee for `lucina-ci check-fast` started
//! straight off the built binary — which is how this crate's own gate tests start it, and how anyone typing the command
//! starts it. The tool now makes a copy of its own in the directory of the run (`RunDir` and `self_copy` in `src/main.rs`,
//! `install_step_exe` in `src/runner.rs`), and that directory goes with the run however the run ended. A copy is used rather
//! than keying the installed binary by its source id, because an id separates only *different* sources: a merge and its own
//! lane build the same source and would still replace each other, which `srcid` catches when a run starts and not while it
//! runs.
//!
//! The swap is made to fall inside the run rather than around it: the first step parks (bounded, well under the step's own
//! limit) until the test has replaced and removed the shared binary, and every stub records whether the replacement was
//! already in place when it ran. So the test cannot pass by being too slow or too fast: the marker lines say the run's later
//! steps started from a copy of a tool that no longer exists at the name it was built into.
//!
//! `LUCINA_CI_NO_SCOPE=1` is set for every run here, the switch `runner_kill_scope.rs` uses for the same reason: the branch
//! under test is a step starting at all, and a machine — or a lane worker — that cannot have a `systemd-run` scope
//! would fail the run for a reason that has nothing to do with this finding.

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

mod common;

/// The memory cap the spawned `check-fast` asks for, as in `merge_gate_timeout.rs`: these tests judge which binary a step
/// starts from, not the machine's RAM, and the default 8 GiB cap makes the binary refuse to start on a small runner.
const MEM_CAP_GIB: &str = "1";
/// The steps a run over this synthetic tree executes, in the order it executes them. Each of them is this program, or a
/// tool it starts, so each of them is a place where the finding could take the run's tool away.
const STUB_STEPS: &str = "diffgate\nfmt\nclippy\nunit-tests\nsecrets";
/// The step that parks until the test has swapped the shared binary.
const SWAP_STEP: &str = "diffgate";
/// What a step prints if it is ever started from the shared name again during a run, which is the proof that it is not.
const SENTINEL: &str = "THE SHARED TOOL WAS STARTED AGAIN";

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

/// A script written under a temp name and renamed onto its own. Writing a name that something may already be running from
/// is the thing this finding is about, and it is also what keeps a spawn from starting half a file.
fn write_exec(p: &Path, body: &str) {
    let tmp = p.with_file_name(format!("{}.new", p.file_name().unwrap().to_string_lossy()));
    // Through a child: this thread never holds the file open for writing while another thread forks and a fork execs it
    // (ETXTBSY, "Text file busy"), the race the same helper of SecretaryIOS explains.
    let mut child = Command::new("sh")
        .args(["-c", "cat > \"$1\" && chmod 0755 \"$1\"", "sh"])
        .arg(&tmp)
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    let mut w = child.stdin.take().unwrap();
    w.write_all(body.as_bytes()).unwrap();
    drop(w);
    assert!(
        child.wait().unwrap().success(),
        "sh cannot write {}",
        tmp.display()
    );
    fs::rename(&tmp, p).unwrap();
}

/// The same for a binary: put it at `p` by rename, from a copy of the tool this test was built with.
fn install_real(to: &Path) {
    let tmp = to.with_file_name(format!("{}.new", to.file_name().unwrap().to_string_lossy()));
    fs::copy(env!("CARGO_BIN_EXE_lucina-ci"), &tmp).unwrap();
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755)).unwrap();
    fs::rename(&tmp, to).unwrap();
}

fn read(p: &Path) -> String {
    fs::read_to_string(p).unwrap_or_default()
}

/// The lane's overrides are not for a throwaway repository, and a waivers file or a Verus the test did not choose would
/// change what the run does.
fn drop_out_of_scope(c: &mut Command) {
    for k in [
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_KEY_0",
        "GIT_CONFIG_VALUE_0",
        "LAGENT_ALLOW",
        "LAGENT_WAIVERS_FILE",
        "LUCINA_CHECK_FAST_MERGE_CAP_S",
        "VERUS",
        "SWAP_HOLD",
        "MARKERS",
        "TMPDIR",
        "CARGO_CLIPPY_RC",
        "DIFFGATE_SLEEP",
        "BUILD_TORN",
        "BUILD_RC",
    ] {
        c.env_remove(k);
    }
}

/// The stub tools on `PATH`. Each records `begin <step>` so the test can see it start, parks if it is the named step, does
/// its nothing, and records `done <step> swapped=<yes|no>`: whether the swapped-in tool was already in the shared name when
/// this step ran. `cargo build` is the launcher's build: it installs whatever `TOOL_SRC` names, or half a file when
/// `BUILD_TORN` is set (what a failing build leaves behind), and exits `BUILD_RC`.
fn stubs(bin: &Path) {
    fs::create_dir_all(bin).unwrap();
    let prelude = r#"MARKERS=${MARKERS:?}
begin() { printf 'begin %s\n' "$1" >>"$MARKERS"; }
swapped() { [ -e "${SWAPPED:-}" ] && printf yes || printf no; }
finished() { printf 'done %s swapped=%s\n' "$1" "$(swapped)" >>"$MARKERS"; }
hold() {
    [ "${SWAP_HOLD:-}" = "$1" ] || return 0
    i=0
    while [ ! -e "${SWAPPED:-}" ] && [ "$i" -lt 200 ]; do sleep 0.1; i=$((i + 1)); done
}
"#;
    write_exec(
        &bin.join("diffgate"),
        &format!(
            "{}\nbegin diffgate\nhold diffgate\nsleep ${{DIFFGATE_SLEEP:-0}}\nfinished diffgate\nexit ${{DIFFGATE_RC:-0}}\n",
            prelude
        ),
    );
    write_exec(
        &bin.join("gitleaks"),
        &format!(
            "{}\nbegin secrets\nhold secrets\nfinished secrets\nexit 0\n",
            prelude
        ),
    );
    write_exec(
        &bin.join("cargo"),
        &format!(
            r#"{}
sub=${{1:-}}
case "$sub" in
    build)
        mkdir -p "$CARGO_TARGET_DIR/release"
        if [ -n "${{BUILD_TORN:-}}" ]; then
            printf 'HALF-A-BINARY\n' >"$CARGO_TARGET_DIR/release/lucina-ci"
            exit "${{BUILD_RC:-101}}"
        fi
        cp "${{TOOL_SRC:?}}" "$CARGO_TARGET_DIR/release/lucina-ci"
        chmod 755 "$CARGO_TARGET_DIR/release/lucina-ci"
        exit "${{BUILD_RC:-0}}"
        ;;
    fmt) begin fmt; hold fmt; finished fmt; exit 0 ;;
    clippy) begin clippy; hold clippy; finished clippy; exit "${{CARGO_CLIPPY_RC:-0}}" ;;
    test) begin unit-tests; hold unit-tests; finished unit-tests; exit 0 ;;
    *) exit 0 ;;
esac
"#,
            prelude
        ),
    );
}

/// The queue the launcher finds in the fake home: it prints our usage and writes the slot record bq would (the launcher believes its own marker only against it) and runs what follows the `--` in place.
const QUEUE_STUB: &str = "#!/usr/bin/env bash
if [ \"$#\" -eq 0 ]; then
    echo 'usage: bq run --class short|long|proof [--lock PATH] [--who NAME] [--priority gate|normal] [--max-wait SECONDS] [--keep-env NAME] -- cmd args...'
    exit 2
fi
while [ \"$1\" != \"--\" ]; do shift; done
shift
h=$(cd \"$(dirname \"$0\")/../..\" && pwd)
mkdir -p \"$h/.cache/bq\"
printf 'pid=%s\\nclass=long\\n' \"$$\" > \"$h/.cache/bq/long-0.meta\"
export BQ_SLOT=long-0 BQ_SLOT_PID=$$
exec \"$@\"
";

/// `repo/scripts/check-fast`: the launcher with its `qhome` assignment replaced by `home`, plus `home/.local/bin/bq`.
fn install_launcher(real: &Path, repo: &Path, home: &Path) {
    let text = fs::read_to_string(real).unwrap_or_else(|e| panic!("{}: {e}", real.display()));
    let (mut out, mut skip, mut replaced) = (String::new(), false, false);
    for line in text.lines() {
        if line == "qhome=$(" {
            out.push_str(&format!("qhome=\"{}\"\n", home.display()));
            (skip, replaced) = (true, true);
            continue;
        }
        if skip {
            skip = line != ") || qhome=\"\"";
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    assert!(
        replaced && !skip,
        "the home assignment is not in the launcher"
    );
    let to = repo.join("scripts/check-fast");
    write_exec(&to, &out);
    let bq = home.join(".local/bin/bq");
    fs::create_dir_all(bq.parent().unwrap()).unwrap();
    write_exec(&bq, QUEUE_STUB);
}

/// A throwaway repository holding this crate's own sources (so the source-id check of the run passes), one workspace
/// member with a change on `work` away from `main`, and a copy of `scripts/check-fast` whose root is this repository.
fn make_repo(tag: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("cfts-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    let repo = d.join("repo");
    fs::create_dir_all(&repo).unwrap();
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let dst = repo.join("tools/ci/lucina-ci");
    copy_dir(&crate_dir.join("src"), &dst.join("src"));
    for f in ["Cargo.toml", "Cargo.lock"] {
        fs::copy(crate_dir.join(f), dst.join(f)).unwrap();
    }
    fs::create_dir_all(repo.join("scripts")).unwrap();
    // A copy of the launcher in which the one assignment of `qhome` (the passwd lookup of this uid's home, which a test cannot
    // change) is the folder above the repo, which holds a stand-in `bq`: the launcher looks its queue up there and nowhere
    // else, and these tests judge the tool swap and not the queue. Nothing else in the copy differs.
    install_launcher(&crate_dir.join("../../../scripts/check-fast"), &repo, &d);
    fs::create_dir_all(repo.join("crates/demo/src")).unwrap();
    fs::write(
        repo.join("Cargo.toml"),
        "[workspace]\nmembers = [\"crates/demo\"]\nresolver = \"2\"\n",
    )
    .unwrap();
    fs::write(
        repo.join("crates/demo/Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(
        repo.join("crates/demo/src/lib.rs"),
        "pub fn one() -> u32 { 1 }\n",
    )
    .unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "base"]);
    git(&repo, &["checkout", "-q", "-b", "work"]);
    fs::write(
        repo.join("crates/demo/src/lib.rs"),
        "pub fn one() -> u32 { 2 }\n",
    )
    .unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "change"]);
    repo
}

/// One run: its scratch directory, its stub `PATH`, the shared install it starts from, and its own `TMPDIR`, which is where
/// the run's directory appears. Everything goes when the value goes, including a run that is still alive.
struct Scratch {
    repo: PathBuf,
    dir: PathBuf,
    shared: PathBuf,
    tmp: PathBuf,
    markers: PathBuf,
    swapped: PathBuf,
    log: PathBuf,
    run: Option<Child>,
}

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let repo = make_repo(tag);
        let dir = repo.parent().unwrap().join("scratch");
        let s = Scratch {
            shared: dir.join("shared/release"),
            tmp: dir.join("tmp"),
            markers: dir.join("MARKERS"),
            swapped: dir.join("SWAPPED"),
            log: dir.join("run.log"),
            repo,
            dir,
            run: None,
        };
        fs::create_dir_all(&s.shared).unwrap();
        fs::create_dir_all(&s.tmp).unwrap();
        fs::set_permissions(&s.tmp, fs::Permissions::from_mode(0o755)).unwrap();
        stubs(&s.dir.join("bin"));
        s
    }

    /// Puts the sentinel at `p` by rename and says so to the steps, then removes the name: both ways a rebuild under a
    /// running check takes the tool away from it.
    fn swap_away(&self, p: &Path) {
        write_exec(
            p,
            &format!("#!/usr/bin/env bash\necho {SENTINEL} >&2\nexit 3\n"),
        );
        fs::write(&self.swapped, "").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(200));
        let _ = fs::remove_file(p);
    }

    /// The environment every run here shares: the stubs on `PATH`, this scratch's `TMPDIR`, the marker file, no scope, and
    /// the first step parked. `extra` is per-run, so nothing a test chooses leaks into a test running beside it.
    fn command(&self, mut c: Command, extra: &[(&str, &str)]) -> Command {
        let path = format!(
            "{}:{}",
            self.dir.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        drop_out_of_scope(&mut c);
        c.current_dir(&self.repo)
            .env("PATH", path)
            .env("TMPDIR", &self.tmp)
            .env("MARKERS", &self.markers)
            .env("SWAPPED", &self.swapped)
            .env("LUCINA_CI_NO_SCOPE", "1")
            .env("SWAP_HOLD", SWAP_STEP);
        for (k, v) in extra {
            c.env(k, v);
        }
        c
    }

    /// Starts `argv` with its output going to a file the test can read while it runs.
    fn spawn(&mut self, argv: &[&str], extra: &[(&str, &str)]) {
        let mut c = self.command(Command::new(argv[0]), extra);
        c.args(&argv[1..]);
        let f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log)
            .unwrap();
        c.stdout(Stdio::from(f.try_clone().unwrap()));
        c.stderr(Stdio::from(f));
        // A thread of another test that forks while this one holds a file open for writing leaves that file busy to exec for
        // the moment until the fork's child has exec'd (ETXTBSY, 'Text file busy'); the start is tried again, as the helper that
        // writes the stand-ins avoids the same window by writing from a child it reaps first.
        let mut tries = 0;
        let child = loop {
            match c.spawn() {
                Ok(child) => break child,
                Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy && tries < 100 => {
                    tries += 1;
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Err(e) => panic!("the check starts: {e:?}"),
            }
        };
        self.run = Some(child);
    }

    /// Waits for the parked step to have started, so the swap falls inside the run and not around it.
    fn wait_for_swap_point(&self) {
        for _ in 0..600 {
            if read(&self.markers)
                .lines()
                .any(|l| l == format!("begin {SWAP_STEP}"))
            {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        panic!(
            "{SWAP_STEP} never started; the run says:\n{}",
            read(&self.log)
        );
    }

    fn release_swap(&self) {
        fs::write(&self.swapped, "").ok();
    }

    /// The directory the run lives in while it runs, named by its own process id under `TMPDIR`.
    fn run_dirs(&self) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = fs::read_dir(&self.tmp)
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.path())
                    .filter(|p| {
                        p.file_name()
                            .map(|n| n.to_string_lossy().starts_with("check-fast-"))
                            .unwrap_or(false)
                    })
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v
    }

    fn leftovers(&self) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(&self.tmp)
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|n| n.starts_with("lucina-ci.") || n.starts_with("check-fast-"))
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v
    }

    /// What the steps recorded, in the order they finished.
    fn steps(&self) -> Vec<String> {
        read(&self.markers)
            .lines()
            .filter_map(|l| l.strip_prefix("done "))
            .map(str::to_string)
            .collect()
    }

    fn wait(&mut self) -> (i32, String) {
        let mut child = self.run.take().expect("a run was started");
        let st = child.wait().expect("the check ends");
        // The run's output went to the log file, not to a pipe: reading it back is what a caller of a live run can do.
        (st.code().unwrap_or(-1), read(&self.log))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if let Some(mut child) = self.run.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = fs::remove_dir_all(self.repo.parent().unwrap());
    }
}

fn wanted(name: &str) -> String {
    format!("{name} swapped=yes")
}

fn expected_steps() -> Vec<String> {
    STUB_STEPS.lines().map(wanted).collect()
}

#[test]
/// (a) Replace the shared tool while a run is in flight — by rename onto the name, then by removing the name — and the run
/// still finishes every step it had to run. This is the finding itself.
fn a_check_fast_run_keeps_the_tool_it_started_with_when_the_shared_one_is_replaced() {
    if common::skip_without_scope(
        "a_check_fast_run_keeps_the_tool_it_started_with_when_the_shared_one_is_replaced",
    ) {
        return;
    }
    let mut s = Scratch::new("swap");
    let exe = s.shared.join("lucina-ci");
    install_real(&exe);
    s.spawn(
        &[
            exe.to_str().unwrap(),
            "check-fast",
            "--base",
            "main",
            "--mem-cap-gib",
            MEM_CAP_GIB,
        ],
        &[],
    );
    s.wait_for_swap_point();
    let dirs = s.run_dirs();
    assert_eq!(
        dirs.len(),
        1,
        "the run made a directory of its own: {dirs:?}"
    );
    let copy = dirs[0].join("bin").join("lucina-ci");
    assert!(
        copy.exists(),
        "the run's copy of the tool is in that directory"
    );
    assert_eq!(
        fs::metadata(&copy).unwrap().len(),
        fs::metadata(env!("CARGO_BIN_EXE_lucina-ci")).unwrap().len(),
        "the copy is this program, whole, not what the stub left in the shared name"
    );
    s.swap_away(&exe);
    let (code, out) = s.wait();
    assert_eq!(
        code, 0,
        "the run finished clean after the tool it started from was taken away:\n{out}"
    );
    assert!(out.contains(", 0 failed, "), "nothing failed:\n{out}");
    assert_eq!(
        s.steps(),
        expected_steps(),
        "every step of the run ran, from the copy, after the swap:\n{}",
        read(&s.markers)
    );
    for name in STUB_STEPS.lines() {
        assert!(
            out.lines()
                .any(|l| l.starts_with("ok  ") && l.split_whitespace().nth(1) == Some(name)),
            "the run lists {name} as a step it ran:\n{out}"
        );
    }
    assert!(
        !out.contains(SENTINEL),
        "no step was started from the shared name again:\n{out}"
    );
    assert!(
        !out.contains("Failed to find executable") && !out.contains("cannot start"),
        "no step lost its tool:\n{out}"
    );
    assert!(
        s.leftovers().is_empty(),
        "and it left nothing: {:?}",
        s.leftovers()
    );
}

#[test]
/// (a) again over the real launcher, which builds the tool into its per-worktree target, copies it to a private name and
/// runs the check from that copy. Replacing and removing the artifact mid-run takes nothing away, and the private copy and
/// the raw log go with the run.
fn the_launcher_runs_the_check_from_its_private_copy() {
    if common::skip_without_scope("the_launcher_runs_the_check_from_its_private_copy") {
        return;
    }
    let mut s = Scratch::new("launcher");
    let cache = s.dir.join("cache");
    let target = cache.join("wt-test");
    fs::create_dir_all(target.join("release")).unwrap();
    let whole = s.dir.join("TOOL-ONE");
    let real = env!("CARGO_BIN_EXE_lucina-ci");
    fs::write(&whole, format!("#!/usr/bin/env bash\nexec {real} \"$@\"\n")).unwrap();
    fs::set_permissions(&whole, fs::Permissions::from_mode(0o755)).unwrap();
    let held: Vec<(&str, String)> = vec![
        ("LUCINA_CHECK_CACHE", cache.display().to_string()),
        ("CARGO_TARGET_DIR", target.display().to_string()),
        ("LUCINA_CHECK_TMP", s.tmp.display().to_string()),
        ("LUCINA_CHECK_FREE_FLOOR_GIB", "0".into()),
        ("TOOL_SRC", whole.display().to_string()),
    ];
    let pairs: Vec<(&str, &str)> = held.iter().map(|(k, v)| (*k, v.as_str())).collect();
    s.spawn(
        &[
            "bash",
            s.repo.join("scripts/check-fast").to_str().unwrap(),
            "--base",
            "main",
            "--mem-cap-gib",
            MEM_CAP_GIB,
        ],
        &pairs,
    );
    s.wait_for_swap_point();
    s.swap_away(&target.join("release/lucina-ci"));
    let (code, out) = s.wait();
    assert_eq!(code, 0, "the launcher's run finished clean:\n{out}");
    assert!(out.contains(", 0 failed, "), "nothing failed:\n{out}");
    assert_eq!(
        s.steps(),
        expected_steps(),
        "every step ran, after the artifact they were built from was replaced and removed:\n{}",
        read(&s.markers)
    );
    assert!(
        !out.contains(SENTINEL),
        "no step was started from the artifact name:\n{out}"
    );
    assert!(
        !out.contains("Failed to find executable"),
        "no step lost its tool:\n{out}"
    );
    assert!(
        s.leftovers().is_empty(),
        "the launcher's private copy and the run's own directory both went with the run: {:?}",
        s.leftovers()
    );
}

#[test]
/// (b) A build that fails leaves the launcher with nothing to run: it stops where the build stopped, copies nothing, and
/// writes nothing itself at the artifact's name, so a half file is whatever the build left and not something a next reader
/// of an installed name will trip over.
fn a_failed_build_stops_the_launcher_before_it_copies_anything() {
    let s = Scratch::new("badbuild");
    let cache = s.dir.join("cache");
    let target = cache.join("wt-test");
    fs::create_dir_all(target.join("release")).unwrap();
    let whole = s.dir.join("WHOLE");
    fs::write(&whole, "WHOLE-FIRST-INSTALL\n").unwrap();
    let artifact = target.join("release/lucina-ci");
    fs::write(&artifact, "WHOLE-FIRST-INSTALL\n").unwrap();
    let mut c = Command::new("bash");
    c.args([
        s.repo.join("scripts/check-fast").to_str().unwrap(),
        "--base",
        "main",
        "--mem-cap-gib",
        MEM_CAP_GIB,
    ]);
    let mut c = s.command(c, &[]);
    c.env("LUCINA_CHECK_CACHE", &cache)
        .env("CARGO_TARGET_DIR", &target)
        .env("LUCINA_CHECK_TMP", &s.tmp)
        .env("LUCINA_CHECK_FREE_FLOOR_GIB", "0")
        .env("TOOL_SRC", &whole)
        .env("BUILD_TORN", "1")
        .env("SWAP_HOLD", "");
    let o = c.output().expect("the launcher ends");
    let out = format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    assert_ne!(
        o.status.code().unwrap_or(0),
        0,
        "a failed build fails the check instead of running a half tool:\n{out}"
    );
    assert!(
        read(&s.markers).is_empty(),
        "no step ran on a tool that was never built: {}",
        read(&s.markers)
    );
    assert!(
        !out.contains("check-fast: "),
        "the run never started, so it printed no summary:\n{out}"
    );
    assert!(
        s.leftovers().is_empty(),
        "the launcher copied nothing, so it had nothing to leave behind: {:?}",
        s.leftovers()
    );
}

/// One way a run can end: its name, the environment it ends under, the summary it must still print, and whether it is the
/// merge gate, where a step past its limit is killed instead of merely slow.
type Case = (
    &'static str,
    Vec<(&'static str, &'static str)>,
    &'static str,
    bool,
);

#[test]
/// (c) The run's directory holds the run while it runs and is gone after it, however it ended: a clean pass, a step that
/// failed, and a step killed past its limit at the merge gate. The removal used to be the last statement of a run that
/// reached its end.
fn the_per_run_directory_holds_the_run_and_leaves_nothing_behind() {
    if common::skip_without_scope("the_per_run_directory_holds_the_run_and_leaves_nothing_behind") {
        return;
    }
    let cases: Vec<Case> = vec![
        ("clean", vec![], ", 0 failed, ", false),
        (
            "failing",
            vec![("CARGO_CLIPPY_RC", "1")],
            ", 1 failed, ",
            false,
        ),
        (
            "late",
            vec![
                ("DIFFGATE_SLEEP", "30"),
                ("LUCINA_CHECK_FAST_MERGE_CAP_S", "1"),
            ],
            " failed, ",
            true,
        ),
    ];
    for (mode, pairs, want, merge_gate) in cases {
        let mut s = Scratch::new(&format!("rundir-{mode}"));
        let exe = s.shared.join("lucina-ci");
        install_real(&exe);
        let mut argv: Vec<&str> = vec![
            exe.to_str().unwrap(),
            "check-fast",
            "--base",
            "main",
            "--mem-cap-gib",
            MEM_CAP_GIB,
        ];
        if merge_gate {
            argv.push("--merge-gate");
        }
        s.spawn(&argv, &pairs);
        s.wait_for_swap_point();
        let dirs = s.run_dirs();
        assert_eq!(
            dirs.len(),
            1,
            "{mode}: the run made one directory: {dirs:?}"
        );
        assert!(
            dirs[0].join("bin").join("lucina-ci").exists(),
            "{mode}: the run's tool copy is in it while it runs"
        );
        s.release_swap();
        let (code, out) = s.wait();
        match mode {
            "clean" => assert_eq!(code, 0, "{mode}: a clean run exits 0:\n{out}"),
            "failing" => {
                assert_eq!(code, 1, "{mode}: a failed step fails the run:\n{out}");
                assert!(
                    out.lines()
                        .any(|l| l.starts_with("FAIL ") && l.contains("clippy")),
                    "{mode}: the failing step is the one named:\n{out}"
                );
            }
            _ => {
                assert_ne!(
                    code, 0,
                    "{mode}: a step killed at the merge gate fails the merge:\n{out}"
                );
                assert!(
                    out.contains("past its 1 s limit at merge"),
                    "{mode}: the killed step says what it was killed for:\n{out}"
                );
            }
        }
        assert!(out.contains(want), "{mode}: wanted {want} in:\n{out}");
        let left = s.run_dirs();
        assert!(
            left.is_empty(),
            "{mode}: the run left its directory behind: {left:?}\n{out}"
        );
        assert!(
            s.leftovers().is_empty(),
            "{mode}: the run left something in its temp directory: {:?}",
            s.leftovers()
        );
    }
}
