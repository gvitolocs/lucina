//! scripts/sweep-caches.sh with a fake HOME: a few KiB of fake target tree, the cap moved to KiB by the environment, and a lock held
//! by this test process as the fake build. The sweep is the binary cargo just built, so the script and the sweep are checked
//! together. Nothing of a real disk is read or removed, and the log lands in the fake HOME.

use std::fs::{self, File};
use std::io::Write;
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
    fs::create_dir_all(home.join("fake-proc")).unwrap();
    let mut cmd = Command::new(&script);
    cmd.env("HOME", home)
        .env("LUCINA_SWEEP_CONF", conf)
        .env("LUCINA_CI_SWEEP", env!("CARGO_BIN_EXE_lucina-ci"))
        .env("LUCINA_PROC_ROOT", home.join("fake-proc"))
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
    let target = home.join("cargo-target/secretaryios");
    artifact(&target.join("debug/deps"), "old.rlib", 30, 3);
    artifact(&target.join("debug/deps"), "new.rlib", 2, 1);
    // An empty fake process table, as the script tests use: nothing here pretends to be a build.
    let procs = home.join("fake-proc");
    fs::create_dir_all(&procs).unwrap();
    let o = Command::new(env!("CARGO_BIN_EXE_lucina-ci"))
        .arg("sweep-target")
        .arg("--dir")
        .arg(&target)
        .args(["--single", "--limit-gib", "40"])
        .env("LUCINA_PROC_ROOT", &procs)
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
