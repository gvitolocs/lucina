//! scripts/sweep-caches.sh with a fake HOME: a few KiB of fake target tree, the cap moved to KiB by the environment, and a lock held
//! by this test process as the fake build. The sweep is the binary cargo just built, so the script and the sweep are checked
//! together. Nothing of a real disk is read or removed, and the log lands in the fake HOME. The idle pass at the bottom of this
//! file is the same script over the same kind of tree, with the removals it is allowed to make and the ones it must never see.

use std::fs::{self, File};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lucina-sweep-script-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

/// A file of `kib` KiB in `dir`, last used `days` days ago, so the order I write them in is the least-recently-used order.
fn artifact(dir: &Path, name: &str, kib: usize, days: u64) {
    fs::create_dir_all(dir).unwrap();
    let p = dir.join(name);
    File::create(&p)
        .unwrap()
        .write_all(&vec![7u8; kib << 10])
        .unwrap();
    let when = SystemTime::now() - Duration::from_secs(days * 86_400);
    let f = fs::OpenOptions::new().write(true).open(&p).unwrap();
    f.set_times(fs::FileTimes::new().set_modified(when).set_accessed(when))
        .unwrap();
}

/// The one target the script caps: 39 KiB of artifacts against the 20 KiB cap given below. `debug/tool` is a binary, never swept.
fn fake_target(home: &Path) -> PathBuf {
    let t = home.join(".cache/cargo-target/secretaryios");
    artifact(&t.join("debug/deps"), "old.rlib", 30, 3);
    artifact(&t.join("debug/deps"), "mid.rlib", 2, 2);
    artifact(&t.join("debug/deps"), "new.rlib", 2, 1);
    artifact(&t.join("debug"), "tool", 5, 0);
    t
}

/// The folder table `scripts/sweep-caches.sh` reads, written into the fake HOME: the same entries the script carried itself
/// before the table was a file (the app target at 40 GiB, every `*-landing` at 8, the largest of them at 14), so the folders it
/// sweeps here are the ones it always swept. The caps do not reach the sweep, which `run_conf` moves to KiB for the whole run.
fn fake_conf(home: &Path) -> PathBuf {
    let p = home.join("sweep-caches.conf");
    fs::write(
        &p,
        "secretaryios 40\n*-landing 8\nLucinaNet-js-landing 14\n",
    )
    .unwrap();
    p
}

/// `scripts/sweep-caches.sh` with `HOME` at `home` and the table of `fake_conf`. An empty fake process table, so only a real lock
/// says a build is running.
fn run_script(home: &Path, dry: bool) -> (i32, String) {
    let conf = fake_conf(home);
    run_conf(home, dry, &conf)
}

/// The same, with the folder table at `conf`: a path of a test's choosing, so a run without one is a run like any other.
fn run_conf(home: &Path, dry: bool, conf: &Path) -> (i32, String) {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../scripts/sweep-caches.sh");
    let mut cmd = Command::new(&script);
    cmd.env("HOME", home)
        .env("LUCINA_SWEEP_CONF", conf)
        .env("LUCINA_CI_SWEEP", env!("CARGO_BIN_EXE_lucina-ci"))
        .env("LUCINA_SWEEP_LIMIT_KIB", "20")
        .env("LUCINA_SWEEP_TO_KIB", "16")
        .env_remove("LUCINA_CACHE_SWEEP_LOG");
    if dry {
        cmd.arg("--dry-run");
    }
    let o = cmd.output().unwrap();
    let mut msg = String::from_utf8_lossy(&o.stderr).into_owned();
    msg.push_str(&String::from_utf8_lossy(&o.stdout));
    (o.status.code().unwrap_or(-1), msg)
}

/// The log the script wrote in the fake HOME, one line per folder.
fn log_lines(home: &Path) -> Vec<String> {
    let p = home.join(".local/state/lucina-cache-sweep.log");
    match fs::read_to_string(&p) {
        Ok(s) => s.lines().map(String::from).collect(),
        Err(e) => panic!("no {}: {e}", p.display()),
    }
}

#[test]
fn the_script_sweeps_the_folder_over_the_cap_and_logs_it() {
    let home = tmp("swept");
    let target = fake_target(&home);
    let (code, msg) = run_script(&home, false);
    assert_eq!(code, 0, "{msg}");
    assert!(
        !target.join("debug/deps/old.rlib").exists(),
        "the oldest artifact goes: {msg}"
    );
    for keep in ["mid.rlib", "new.rlib"] {
        assert!(
            target.join("debug/deps").join(keep).exists(),
            "{keep} is newer and the cap is met: {msg}"
        );
    }
    assert!(
        target.join("debug/tool").exists(),
        "a binary is never swept: {msg}"
    );
    let log = log_lines(&home);
    assert_eq!(log.len(), 1, "{log:?}");
    // The line is the run time, the folder, both sizes, what happened, and the cap the sweep really got.
    let line = &log[0];
    let stamp = line.split(' ').next().unwrap();
    assert!(
        stamp.starts_with("20") && stamp.ends_with('Z') && stamp.matches(':').count() == 2,
        "UTC to the second first: {stamp}"
    );
    assert!(
        line.contains(&target.display().to_string()),
        "the folder by name: {line}"
    );
    assert!(
        line.contains(": swept (cap --limit-kib 20 --to-kib 16)"),
        "the KiB cap from the environment reached the sweep: {line}"
    );
    assert!(msg.contains("sweep-caches:"), "on stdout too: {msg}");
}

#[test]
fn a_folder_a_build_holds_is_logged_as_in_use_and_stays_whole() {
    let home = tmp("inuse");
    let target = fake_target(&home);
    let lock = target.join("debug/.cargo-lock");
    fs::write(&lock, "").unwrap();
    // The fake build: this process holds the profile lock, so the sweep must leave the whole target alone.
    let held = fs::OpenOptions::new().write(true).open(&lock).unwrap();
    held.lock().unwrap();
    let (code, msg) = run_script(&home, false);
    drop(held);
    assert_eq!(
        code, 0,
        "a target it will not touch is no failure of the run: {msg}"
    );
    for name in ["old.rlib", "mid.rlib", "new.rlib"] {
        assert!(
            target.join("debug/deps").join(name).exists(),
            "an in-use target stays whole: {msg}"
        );
    }
    let log = log_lines(&home);
    assert_eq!(log.len(), 1, "{log:?}");
    assert!(
        log[0].contains(": in use: not swept (cap --limit-kib 20"),
        "the log says what to wait for: {}",
        log[0]
    );
    assert!(
        log[0].contains(" -> "),
        "the sizes before and after are still there: {}",
        log[0]
    );
}

#[test]
fn a_dry_run_names_the_folder_and_removes_nothing() {
    let home = tmp("dry");
    let target = fake_target(&home);
    let (code, msg) = run_script(&home, true);
    assert_eq!(code, 0, "{msg}");
    for name in ["old.rlib", "mid.rlib", "new.rlib"] {
        assert!(
            target.join("debug/deps").join(name).exists(),
            "a dry run touches nothing: {msg}"
        );
    }
    let log = log_lines(&home);
    assert_eq!(log.len(), 1, "{log:?}");
    assert!(
        log[0].contains(": dry run (cap --limit-kib 20"),
        "a dry run says so: {}",
        log[0]
    );
}

#[test]
fn a_landing_folder_is_capped_too_and_a_small_one_is_left_alone() {
    let home = tmp("landing");
    fake_target(&home);
    let landing = home.join(".cache/cargo-target/Foo-landing");
    artifact(&landing.join("debug/deps"), "tiny.rlib", 1, 5);
    let (code, msg) = run_script(&home, false);
    assert_eq!(code, 0, "{msg}");
    assert!(
        landing.join("debug/deps/tiny.rlib").exists(),
        "2 KiB is under the 20 KiB cap: {msg}"
    );
    let log = log_lines(&home);
    assert_eq!(log.len(), 2, "one line per folder: {log:?}");
    assert!(log[0].contains("secretaryios"), "{log:?}");
    assert!(
        log[1].contains("Foo-landing") && log[1].contains(": nothing removed ("),
        "the folder under its cap is reported and left: {}",
        log[1]
    );
}

#[test]
fn no_folder_table_means_no_sweep_and_no_failure() {
    let home = tmp("noconf");
    let target = fake_target(&home);
    let missing = home.join("no-such-folder-table.conf");
    let (code, msg) = run_conf(&home, false, &missing);
    assert_eq!(
        code, 0,
        "a host with no table is a host with nothing to cap: {msg}"
    );
    assert!(msg.contains("no folder table"), "it says which: {msg}");
    assert!(msg.contains("nothing to sweep"), "and what it did: {msg}");
    for name in ["old.rlib", "mid.rlib", "new.rlib"] {
        assert!(
            target.join("debug/deps").join(name).exists(),
            "with no table nothing is swept: {msg}"
        );
    }
    assert!(
        target.join("debug/tool").exists(),
        "and no binary goes: {msg}"
    );
    assert!(
        !home.join(".local/state/lucina-cache-sweep.log").exists(),
        "no folder swept, so no line logged: {msg}"
    );
}

/// `--single` with the GiB marks a person and the timer give, over a fake target of a few KiB and a real (not dry) run. The cap
/// must reach the sweep as 40 GiB of bytes, not as 40 MiB: the unit bug that would have emptied the app cache every 15 minutes.
#[test]
fn a_single_target_with_a_gib_cap_is_within_it_and_reports_the_gib() {
    let home = tmp("gibcap");
    // Under the fake HOME's cache, which is where the binary now requires every target to be.
    let target = home.join(".cache/cargo-target/secretaryios");
    artifact(&target.join("debug/deps"), "old.rlib", 30, 3);
    artifact(&target.join("debug/deps"), "new.rlib", 2, 1);
    let o = Command::new(env!("CARGO_BIN_EXE_lucina-ci"))
        .arg("sweep-target")
        .arg("--dir")
        .arg(&target)
        .args(["--single", "--limit-gib", "40"])
        .env("HOME", &home)
        .output()
        .unwrap();
    let msg = format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    assert_eq!(o.status.code(), Some(0), "{msg}");
    assert!(
        msg.contains("limit 40 GiB, sweep down to 32 GiB"),
        "the cap as given, and the default mark of 80% of it: {msg}"
    );
    for name in ["old.rlib", "new.rlib"] {
        assert!(
            target.join("debug/deps").join(name).exists(),
            "32 KiB is within the cap, so a real run removes nothing: {msg}"
        );
    }
    fs::remove_dir_all(&home).ok();
}

// ---- the idle pass of the same script ---------------------------------------------------------------------------
//
// A cargo target the table does not name, in one of the roots the pass is allowed to reach, is removed WHOLE when its newest file
// is older than the cutoff and no running process is using it. Here the trees are a few bytes, the cutoff is the
// default 24 h, and the only thing the pass really reads is `/proc`: which is why a holder is a real `sleep` this test starts
// and kills again, and never something it does not own.

/// The age of a whole tree, directories included: the pass asks the newest entry of the target, and `fs::FileTimes` reaches a
/// directory only through a handle opened for writing, which a directory cannot give. `touch -d @<epoch>` is the one tool here.
fn age_tree(dir: &Path, hours: u64) {
    let epoch = SystemTime::now() - Duration::from_secs(hours * 3600);
    let epoch = epoch
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .to_string();
    let mut paths = vec![dir.to_path_buf()];
    let mut i = 0;
    while i < paths.len() {
        if let Ok(rd) = fs::read_dir(&paths[i]) {
            for e in rd.flatten() {
                paths.push(e.path());
            }
        }
        i += 1;
    }
    let o = Command::new("touch")
        .arg("-d")
        .arg(format!("@{epoch}"))
        .args(&paths)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "touch: {}",
        String::from_utf8_lossy(&o.stderr)
    );
}

/// A small cargo target, `hours` hours past its newest file: CACHEDIR.TAG and a profile with a `.fingerprint`, the two marks the
/// pass recognises. 0 hours leaves it as young as the moment it was written.
fn idle_target(dir: &Path, hours: u64) {
    fs::create_dir_all(dir.join("debug/.fingerprint")).unwrap();
    fs::create_dir_all(dir.join("debug/deps")).unwrap();
    cachedir_tag(dir);
    fs::write(dir.join("debug/.fingerprint/pkg-1"), "").unwrap();
    fs::write(dir.join("debug/deps/libpkg.rlib"), "not really an rlib").unwrap();
    if hours > 0 {
        age_tree(dir, hours);
    }
}

/// A folder that is not a cargo target at all: named `target...` by the scratch rule, but no tag and no fingerprint in it.
fn not_a_target(dir: &Path) {
    fs::create_dir_all(dir.join("inner")).unwrap();
    fs::write(dir.join("inner/readme.md"), "a checkout of something else").unwrap();
    age_tree(dir, 72);
}

/// The mark cargo leaves at the root of a target, so a folder is recognised as one: what the shared check cache carries at its
/// root, where its profiles sit beside it instead of under a target folder of their own.
fn cachedir_tag(dir: &Path) {
    fs::create_dir_all(dir).unwrap();
    fs::write(
        dir.join("CACHEDIR.TAG"),
        "Signature: 8a477f597d28d1786c96eae039bdb42b\n",
    )
    .unwrap();
}

/// The fake build, as the idle pass can actually see it: a real `sleep` standing in the target with its cwd, or pointed at it by
/// `CARGO_TARGET_DIR`. `Sleeps` kills exactly these, and only these.
fn hold(dir: &Path, by_env: bool) -> std::process::Child {
    let mut c = Command::new("sleep");
    c.arg("600");
    if by_env {
        c.env("CARGO_TARGET_DIR", dir);
    } else {
        c.current_dir(dir);
    }
    let kid = c.spawn().unwrap();
    // The exec has to be done before the pass asks /proc, or the child is still this test's fork and names nothing.
    std::thread::sleep(Duration::from_millis(400));
    kid
}

/// An idle target that a build has used: `.cargo-lock` in its profile folder, the file cargo locks while it builds. Written after
/// the tree and aged with it, since a young file would keep the target for the age rule and the lock would never be asked.
fn locked_target(dir: &Path, hours: u64) {
    idle_target(dir, 0);
    fs::write(dir.join("debug/.cargo-lock"), "").unwrap();
    age_tree(dir, hours);
}

/// A build the process table cannot see, which is all the cargo lock is for: the holder opens `.cargo-lock`, moves its working
/// directory away and becomes a `sleep` with the descriptor inherited, so no argument, no environment entry the pass reads and no
/// cwd of a running process names the target. The path reaches it only through `D`, which is not one of the variables.
fn hold_lock(lock: &Path) -> std::process::Child {
    let dir = lock.parent().unwrap();
    let kid = Command::new("bash")
        .env("D", dir)
        .args([
            "-c",
            "cd \"$D\" && exec 9< .cargo-lock && cd / && flock -n 9 && exec sleep 600",
        ])
        .spawn()
        .unwrap();
    // The lock has to be taken before the pass asks, or the target would go for the right reason by the wrong road.
    std::thread::sleep(Duration::from_millis(400));
    kid
}

struct Sleeps(Vec<std::process::Child>);

impl Drop for Sleeps {
    fn drop(&mut self) {
        for k in &mut self.0 {
            let _ = k.kill();
            let _ = k.wait();
        }
    }
}

/// The folder table of the idle tests: the same file the size pass reads, so an exclude line has to live with the caps.
fn conf_path(home: &Path) -> PathBuf {
    home.join("sweep-table.conf")
}

/// The script over a fixture HOME, with `table` for its folder table so nothing the run says can come from the size pass. The idle
/// pass follows `--apply` alone: without it, it reports. `over` adds what one test needs to point somewhere of its own: the loop
/// script an exclude keyword reads, or a PATH with a stub `systemctl` at its head.
fn run_idle_env(home: &Path, table: &str, apply: bool, over: &[(&str, PathBuf)]) -> (i32, String) {
    let conf = conf_path(home);
    fs::write(&conf, table).unwrap();
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../scripts/sweep-caches.sh");
    let mut cmd = Command::new(&script);
    cmd.env("HOME", home)
        .env("LUCINA_SWEEP_CONF", &conf)
        .env("LUCINA_CI_SWEEP", env!("CARGO_BIN_EXE_lucina-ci"));
    for (k, v) in over {
        cmd.env(k, v);
    }
    if apply {
        cmd.arg("--apply");
    }
    let o = cmd.output().unwrap();
    let mut msg = String::from_utf8_lossy(&o.stderr).into_owned();
    msg.push_str(&String::from_utf8_lossy(&o.stdout));
    (o.status.code().unwrap_or(-1), msg)
}

fn run_idle(home: &Path, apply: bool) -> (i32, String) {
    run_idle_env(home, "", apply, &[])
}

/// The line the run gave for one folder.
fn idle_line(msg: &str, dir: &Path) -> String {
    let want = format!("idle-sweep: {} ", dir.display());
    msg.lines()
        .find(|l| l.starts_with(&want))
        .unwrap_or_else(|| panic!("no idle-sweep line for {} in:\n{msg}", dir.display()))
        .to_string()
}

#[test]
fn an_idle_target_is_listed_by_dry_run_and_removed_by_apply() {
    let home = tmp("idle-apply");
    let old = home.join("scratch/one/target");
    let young = home.join("scratch/two/target");
    let plain = home.join("scratch/three/target-files");
    idle_target(&old, 72);
    idle_target(&young, 1);
    not_a_target(&plain);

    let (code, msg) = run_idle(&home, false);
    assert_eq!(code, 0, "{msg}");
    let want = format!("{} 0.0 GiB newest 72 h: would remove", old.display());
    assert!(
        idle_line(&msg, &old).contains(&want),
        "a report carries the size it would free and the age that earned it: {}",
        idle_line(&msg, &old)
    );
    assert!(
        idle_line(&msg, &young).contains("kept: recent"),
        "a file an hour old is not idle: {}",
        idle_line(&msg, &young)
    );
    assert!(
        idle_line(&msg, &plain).contains("skipped: not a cargo target"),
        "no tag, no fingerprint: {}",
        idle_line(&msg, &plain)
    );
    assert!(
        !msg.contains("freed"),
        "a report keeps its total for a run that removes: {msg}"
    );
    for keep in [&old, &young, &plain] {
        assert!(keep.exists(), "no --apply, nothing removed: {keep:?}");
    }

    let (code, msg) = run_idle(&home, true);
    assert_eq!(code, 0, "{msg}");
    assert!(
        idle_line(&msg, &old).contains(": removed"),
        "{}",
        idle_line(&msg, &old)
    );
    assert!(!old.exists(), "the idle target is gone: {msg}");
    assert!(
        idle_line(&msg, &young).contains("kept: recent") && young.exists(),
        "the young one stays: {}",
        idle_line(&msg, &young)
    );
    assert!(
        idle_line(&msg, &plain).contains("not a cargo target") && plain.exists(),
        "and so does the folder that is not a target at all: {}",
        idle_line(&msg, &plain)
    );
    assert!(
        msg.contains("idle-sweep: freed 0.0 GiB from 1 target"),
        "what it freed, in GiB over the targets it took: {msg}"
    );
    let log = log_lines(&home);
    let want = format!("{} 0.0 GiB newest 72 h: removed", old.display());
    assert!(
        log.iter().any(|l| l.ends_with(&want)),
        "the removal is in the state log too: {log:?}"
    );
    fs::remove_dir_all(&home).ok();
}

#[test]
fn a_target_a_live_process_is_using_is_kept_and_named_by_pid() {
    let home = tmp("idle-held");
    let by_cwd = home.join("scratch/cwdhold/target");
    let by_env = home.join("scratch/envhold/target");
    idle_target(&by_cwd, 72);
    idle_target(&by_env, 72);
    let _kids = Sleeps(vec![hold(&by_cwd, false), hold(&by_env, true)]);

    // With --apply, because a build in progress is exactly the case an applied run must not get wrong.
    let (code, msg) = run_idle(&home, true);
    assert_eq!(code, 0, "{msg}");
    for (dir, kid) in [(&by_cwd, _kids.0[0].id()), (&by_env, _kids.0[1].id())] {
        let line = idle_line(&msg, dir);
        assert!(
            line.contains(&format!("kept: in use by pid {kid}")),
            "{line}"
        );
        assert!(dir.exists(), "a held target is left whole: {dir:?}");
    }
    assert!(
        msg.contains("freed 0.0 GiB from 0 targets"),
        "two idle targets, both held, so nothing freed: {msg}"
    );
    fs::remove_dir_all(&home).ok();
}

#[test]
fn a_holder_whose_cwd_path_holds_a_newline_and_spaces_keeps_the_target_under_it() {
    // The fixture has to go even when an assert below does not, because a run that got this path wrong leaves the folder behind.
    struct Gone<'a>(&'a Path);
    impl Drop for Gone<'_> {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(self.0);
        }
    }
    let home = tmp("idle-cwd-newline");
    let _home = Gone(&home);
    // A folder of the LucinaNet cache is a candidate by sitting in it, whatever it is named, so this name is really reached: a
    // leading space, a newline, a trailing space. Records framed by lines cut this cwd at the newline, and what is left of it names
    // no target at all - so the keep is gone and an applied run removes the folder the build stands in.
    let odd = home.join(".cache/lucinanet-target/ we\nird ");
    idle_target(&odd, 72);
    let _kids = Sleeps(vec![hold(&odd, false)]);

    // With --apply, because a path the pass carries wrong must not end in a removal.
    let (code, msg) = run_idle(&home, true);
    assert_eq!(code, 0, "{msg}");
    assert!(
        msg.contains(&format!("idle-sweep: {}", odd.display())),
        "the candidate is named by its whole path, newline and spaces and all: {msg}"
    );
    assert!(
        msg.contains(&format!("kept: in use by pid {}", _kids.0[0].id())),
        "the process standing in it is the reason it stays: {msg}"
    );
    assert!(
        odd.exists(),
        "a target a live process stands in is left whole: {msg}"
    );
    assert!(
        msg.contains("freed 0.0 GiB from 0 targets"),
        "one idle target and it is held, so nothing is freed: {msg}"
    );
}

#[test]
fn a_build_only_the_cargo_lock_shows_keeps_its_target_and_a_free_lock_keeps_nothing() {
    let home = tmp("idle-lock");
    let held = home.join("scratch/builds/target-held");
    let free = home.join("scratch/builds/target-free");
    locked_target(&held, 72);
    locked_target(&free, 72);
    let _kid = Sleeps(vec![hold_lock(&held.join("debug/.cargo-lock"))]);

    let (code, msg) = run_idle(&home, true);
    assert_eq!(code, 0, "{msg}");
    let line = idle_line(&msg, &held);
    assert!(line.contains("kept: cargo build lock held"), "{line}");
    assert!(held.exists(), "the lock is the only evidence: {line}");
    let line = idle_line(&msg, &free);
    assert!(
        line.contains("removed"),
        "a lock nobody holds is no reason to keep: {line}"
    );
    assert!(!free.exists(), "{line}");
    fs::remove_dir_all(&home).ok();
}

#[test]
fn only_the_candidate_roots_are_candidates_and_nothing_else_under_home() {
    let home = tmp("idle-roots");
    // In: the target of a worktree, one folder of each of the two check caches, any other cargo target sitting directly in the
    // second cache, and a scratch build at the deepest level the rule reaches.
    let worktree = home.join("Projects/.worktrees/LucinaOS/br/target");
    let cache_wt = home.join(".cache/lucina-check-target/wt-ffff000001");
    let net_wt = home.join(".cache/lucinanet-target/wt-ffff000002");
    let scratch_deep = home.join("scratch/a/b/c/target-branch");
    // Out: anything else under ~/Projects, a scratch folder one level deeper still, the table's own target (which public-sync
    // needs between syncs), and a checkout that merely has a `target` folder in it.
    let outside = home.join("Projects/LucinaOS/target");
    let too_deep = home.join("scratch/a/b/c/d/target");
    let table = home.join(".cache/cargo-target/secretaryios");
    let stray = home.join("Projects/.worktrees/LucinaOS/br/inner/target");
    // In as well, since the LucinaNet cache is taken by the folders in it and not by what the loop called them: a target there
    // named after the run that made it. Out of that same cache: a folder that is no cargo target (listed, left alone) and the root
    // itself, which is never a source of candidates and has no unit rule either.
    let net_other = home.join(".cache/lucinanet-target/before-htmldda");
    let net_plain = home.join(".cache/lucinanet-target/notes");
    let net_root = home.join(".cache/lucinanet-target");
    for d in [&worktree, &cache_wt, &net_wt, &scratch_deep, &net_other] {
        idle_target(d, 72);
    }
    for d in [&outside, &too_deep, &table, &stray] {
        idle_target(d, 72);
    }
    fs::create_dir_all(&net_plain).unwrap();
    fs::write(net_plain.join("readme.md"), "not a build at all").unwrap();
    age_tree(&net_plain, 72);
    let (code, msg) = run_idle(&home, false);
    assert_eq!(code, 0, "{msg}");
    for d in [&worktree, &cache_wt, &net_wt, &scratch_deep, &net_other] {
        assert!(
            idle_line(&msg, d).contains("would remove"),
            "{d:?} is one of the roots the pass reaches: {}",
            idle_line(&msg, d)
        );
    }
    for d in [&outside, &too_deep, &table, &stray] {
        assert!(
            !msg.contains(&d.display().to_string()),
            "{d:?} is not a candidate, so it is not even named: {msg}"
        );
        assert!(d.exists(), "{d:?} is never touched");
    }
    assert!(
        idle_line(&msg, &net_plain).contains("not a cargo target"),
        "a folder of that cache with no tag and no fingerprint: {}",
        idle_line(&msg, &net_plain)
    );
    assert!(net_plain.exists(), "{net_plain:?} is left alone");
    assert!(
        !msg.contains(&format!("idle-sweep: {} ", net_root.display())),
        "the cache itself is not a candidate, only the folders directly in it: {msg}"
    );
    assert!(net_root.exists(), "{net_root:?} is never touched");
    fs::remove_dir_all(&home).ok();
}

#[test]
fn the_shared_check_cache_goes_as_one_unit_only_when_no_worktree_folder_uses_it() {
    // With wt- folders beside it, the root cannot be a candidate: taking it would take those with it, and each is a cache of its
    // own that the idle rule judges on its own age.
    let with = tmp("idle-shared-with");
    let cache = with.join(".cache/lucina-check-target");
    idle_target(&cache.join("wt-eeee000001"), 72);
    idle_target(&cache.join("debug"), 0);
    cachedir_tag(&cache);
    let (code, msg) = run_idle(&with, false);
    assert_eq!(code, 0, "{msg}");
    assert!(
        idle_line(&msg, &cache.join("wt-eeee000001")).contains("would remove"),
        "the folder of one worktree is a target of its own: {}",
        idle_line(&msg, &cache.join("wt-eeee000001"))
    );
    assert!(
        !msg.contains(&format!("idle-sweep: {} ", cache.display())),
        "the shared root is not a candidate while a wt- folder lives in it: {msg}"
    );
    fs::remove_dir_all(&with).ok();

    // The old layout alone: the cache root itself is the candidate, and the whole cache goes with it.
    let alone = tmp("idle-shared-alone");
    let shared = alone.join(".cache/lucina-check-target");
    idle_target(&shared.join("debug"), 72);
    cachedir_tag(&shared);
    age_tree(&shared, 72);
    let (code, msg) = run_idle(&alone, false);
    assert_eq!(code, 0, "{msg}");
    assert!(
        idle_line(&msg, &shared).contains("would remove"),
        "the shared cache is one unit: {}",
        idle_line(&msg, &shared)
    );
    fs::remove_dir_all(&alone).ok();
}

/// A stub `systemctl` answering `--user show … -p Environment` with `answer`, or failing like a box with no such unit. Returns the
/// PATH to run with, so the script finds the stub first and every real tool after it.
fn path_with_systemctl_stub(home: &Path, answer: &str) -> PathBuf {
    let dir = home.join("fakebin");
    fs::create_dir_all(&dir).unwrap();
    let p = dir.join("systemctl");
    let body = if answer.is_empty() {
        "#!/bin/sh\nexit 127\n".to_string()
    } else {
        format!("#!/bin/sh\ncat <<'EOF'\n{answer}\nEOF\n")
    };
    fs::write(&p, body).unwrap();
    fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!(
        "{}:{}",
        dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    PathBuf::from(path)
}

/// The loop script the keyword falls back to, with the worktree its default builds into.
fn auto_loop(home: &Path, default_worktree: &Path) -> PathBuf {
    let p = home.join("juno-auto-install.sh");
    fs::write(
        &p,
        format!(
            "# Env: JUNO_AUTO_WORKTREE (the build tree, checked out detached)\nW=${{JUNO_AUTO_WORKTREE:-{}}}\n",
            default_worktree.display()
        ),
    )
    .unwrap();
    p
}

#[test]
fn an_excluded_path_is_never_a_candidate_and_names_the_line_that_excluded_it() {
    let home = tmp("idle-exclude");
    let kept = home.join("scratch/keep");
    let tilde = home.join("scratch/tilde");
    let one = kept.join("target");
    let nested = kept.join("work/target");
    let other = tilde.join("target");
    let gone = home.join("scratch/go/target");
    for d in [&one, &nested, &other, &gone] {
        idle_target(d, 72);
    }
    let table = format!(
        "# the owner's live checkout, warm on purpose\nexclude {}\n# ~ is the home the run works from\nexclude ~/scratch/tilde\n",
        kept.display()
    );
    // --apply, because the only thing worth proving about an exclude is that a run which removes does not remove it.
    let (code, msg) = run_idle_env(&home, &table, true, &[]);
    assert_eq!(code, 0, "{msg}");
    let conf = conf_path(&home);
    for (d, n) in [(&one, 2usize), (&nested, 2), (&other, 4)] {
        let line = idle_line(&msg, d);
        assert!(
            line.contains(&format!("kept: excluded ({}:{n})", conf.display())),
            "the exclude that reaches {d:?} says which line it was: {line}"
        );
        assert!(d.exists(), "an exclude survives --apply: {line}");
    }
    let line = idle_line(&msg, &gone);
    assert!(
        line.contains(": removed"),
        "a sibling is not excluded: {line}"
    );
    assert!(!gone.exists(), "{line}");
    assert!(
        msg.contains("idle-sweep: freed 0.0 GiB from 1 target"),
        "the three excluded ones are not in the total: {msg}"
    );
    assert_eq!(
        msg.lines()
            .filter(|l| l.contains(&one.display().to_string()))
            .count(),
        1,
        "an excluded target is listed once, not once per test: {msg}"
    );
    fs::remove_dir_all(&home).ok();
}

#[test]
fn the_exclude_keyword_takes_the_loop_build_tree_of_the_unit_or_the_script_or_says_it_know_best() {
    let home = tmp("idle-auto-exclude");
    let from_script = home.join("Projects/.worktrees/LucinaOS/gen-33-build");
    let from_unit = home.join("Projects/.worktrees/LucinaOS/gen-99-build");
    let other = home.join("Projects/.worktrees/LucinaOS/br");
    for d in [&from_script, &from_unit, &other] {
        idle_target(&d.join("target"), 72);
    }
    let table = "# G-builds must not start cold because the loop sat paused or held for a day\nexclude-auto-install-worktree\n";
    let over = [
        ("LUCINA_JUNO_AUTO_SCRIPT", auto_loop(&home, &from_script)),
        (
            "PATH",
            path_with_systemctl_stub(
                &home,
                &format!(
                    "Environment=DISPLAY=:0 JUNO_AUTO_WORKTREE={} OTHER=x",
                    from_unit.display()
                ),
            ),
        ),
    ];
    let conf = conf_path(&home);
    let want = format!("kept: excluded ({conf}:2)", conf = conf.display());

    // What the running unit was given wins over the script's default, and the target under it is the one kept warm.
    let (code, msg) = run_idle_env(&home, table, false, &over);
    assert_eq!(code, 0, "{msg}");
    assert!(
        idle_line(&msg, &from_unit.join("target")).contains(&want),
        "the unit's build tree is the one kept warm: {}",
        idle_line(&msg, &from_unit.join("target"))
    );
    assert!(
        idle_line(&msg, &from_script.join("target")).contains("would remove"),
        "the script default is only the fallback: {}",
        idle_line(&msg, &from_script.join("target"))
    );

    // Nothing set on the unit: the default the loop script itself uses.
    let over = [
        ("LUCINA_JUNO_AUTO_SCRIPT", auto_loop(&home, &from_script)),
        ("PATH", path_with_systemctl_stub(&home, "Environment=")),
    ];
    let (code, msg) = run_idle_env(&home, table, false, &over);
    assert_eq!(code, 0, "{msg}");
    assert!(
        idle_line(&msg, &from_script.join("target")).contains(&want),
        "the loop's own default, read from the loop script: {}",
        idle_line(&msg, &from_script.join("target"))
    );
    assert!(
        idle_line(&msg, &other.join("target")).contains("would remove"),
        "and its neighbours are judged like any other target: {}",
        idle_line(&msg, &other.join("target"))
    );

    // No unit, no script: one line that says so, nothing excluded, and the run goes on.
    let over = [
        ("LUCINA_JUNO_AUTO_SCRIPT", home.join("no-such-loop.sh")),
        ("PATH", path_with_systemctl_stub(&home, "")),
    ];
    let (code, msg) = run_idle_env(&home, table, false, &over);
    assert_eq!(code, 0, "an unresolvable keyword is never a failure: {msg}");
    assert!(
        msg.contains("not resolved, nothing excluded"),
        "it says so once: {msg}"
    );
    for d in [&from_script, &from_unit, &other] {
        assert!(
            idle_line(&msg, &d.join("target")).contains("would remove"),
            "nothing is excluded on a guess: {}",
            idle_line(&msg, &d.join("target"))
        );
    }
    fs::remove_dir_all(&home).ok();
}

#[test]
fn the_size_pass_reads_its_lines_and_a_broken_exclude_stops_the_run() {
    let home = tmp("idle-exclude-table");
    let single = fake_target(&home);
    let conf = home.join("sweep-caches.conf");
    fs::write(
        &conf,
        format!(
            "secretaryios 40\n# the loop's build tree\nexclude {}\n",
            home.join("scratch/juno-build").display()
        ),
    )
    .unwrap();
    let (code, msg) = run_conf(&home, false, &conf);
    assert_eq!(code, 0, "an exclude line is not a bad folder entry: {msg}");
    assert!(
        msg.contains("swept"),
        "the cap on the line above it still sweeps: {msg}"
    );
    assert!(single.exists());

    let bad = home.join("broken.conf");
    fs::write(&bad, "secretaryios 40\nexclude relative/path\n").unwrap();
    let (code, msg) = run_conf(&home, false, &bad);
    assert_eq!(code, 2, "a path that is not absolute is refused: {msg}");
    assert!(
        msg.contains("not an absolute path"),
        "and it says which line: {msg}"
    );
    assert!(
        !msg.contains("swept"),
        "the run ends before anything is swept: {msg}"
    );
    fs::remove_dir_all(&home).ok();
}
