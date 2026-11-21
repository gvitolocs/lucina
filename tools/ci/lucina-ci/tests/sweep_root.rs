//! `lucina-ci sweep-target` root mode and `--single` as a child process (PF-C040): an unset or empty HOME is a refusal, a `--dir`
//! outside the canonical `$HOME/.cache` is a refusal, and every removal is announced with its canonical path first. HOME and the
//! rest of the environment are set on the child only, never on this test process. The tree each test makes is under the temp dir and
//! removed on drop, so a panic leaves nothing; no child outlives its call.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A canonical temp folder, removed on drop.
struct Tmp(PathBuf);
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn tmp(name: &str) -> Tmp {
    let p = std::env::temp_dir().join(format!("lucina-sweep-root-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&p);
    fs::create_dir_all(&p).unwrap();
    Tmp(fs::canonicalize(&p).unwrap())
}

/// `lucina-ci sweep-target args...` with HOME as given (`None`: removed from the child's environment). Exit code and stdout plus
/// stderr.
fn cli(home: Option<&str>, args: &[&str]) -> (Option<i32>, String) {
    let mut c = Command::new(env!("CARGO_BIN_EXE_lucina-ci"));
    c.arg("sweep-target").args(args);
    match home {
        Some(h) => c.env("HOME", h),
        None => c.env_remove("HOME"),
    };
    let o = c.output().unwrap();
    let msg = format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    (o.status.code(), msg)
}

/// A folder that looks like a cache root with one target in it, so a wrongly accepted run would have something to remove.
fn cache_with_canary(root: &Path) -> PathBuf {
    let canary = root.join("wt-canary/debug/deps/x.rlib");
    fs::create_dir_all(canary.parent().unwrap()).unwrap();
    fs::write(&canary, vec![0u8; 1024]).unwrap();
    canary
}

#[test]
fn an_unset_home_is_a_refusal_not_a_fallback() {
    let t = tmp("nohome");
    let canary = cache_with_canary(&t.0.join("cache"));
    let dir = t.0.join("cache");
    let (code, msg) = cli(None, &["--dir", dir.to_str().unwrap(), "--limit-gib", "1"]);
    assert_eq!(code, Some(2), "{msg}");
    assert!(msg.contains("HOME is unset or empty"), "{msg}");
    assert!(canary.exists(), "nothing removed: {msg}");
}

#[test]
fn an_empty_home_is_a_refusal_not_a_fallback() {
    let t = tmp("emptyhome");
    let canary = cache_with_canary(&t.0.join("cache"));
    let dir = t.0.join("cache");
    let (code, msg) = cli(
        Some(""),
        &["--dir", dir.to_str().unwrap(), "--limit-gib", "1"],
    );
    assert_eq!(code, Some(2), "{msg}");
    assert!(msg.contains("HOME is unset or empty"), "{msg}");
    assert!(canary.exists(), "nothing removed: {msg}");
}

#[test]
fn a_dir_outside_the_canonical_cache_root_is_refused_with_a_good_home() {
    let t = tmp("outside");
    let home = t.0.join("home");
    fs::create_dir_all(home.join(".cache")).unwrap();
    let elsewhere = t.0.join("elsewhere");
    let canary = cache_with_canary(&elsewhere);
    let (code, msg) = cli(
        Some(home.to_str().unwrap()),
        &["--dir", elsewhere.to_str().unwrap(), "--limit-gib", "1"],
    );
    assert_eq!(code, Some(2), "{msg}");
    assert!(msg.contains("not strictly under"), "{msg}");
    assert!(canary.exists(), "nothing removed: {msg}");
    // A zero limit is refused too, whatever the dir.
    let inside = home.join(".cache/x");
    let canary2 = cache_with_canary(&inside);
    let (code, msg) = cli(
        Some(home.to_str().unwrap()),
        &["--dir", inside.to_str().unwrap(), "--limit-gib", "0"],
    );
    assert_eq!(code, Some(2), "{msg}");
    assert!(canary2.exists(), "{msg}");
}

#[test]
fn a_single_sweep_logs_the_canonical_path_before_each_removal() {
    let t = tmp("single");
    let home = t.0.join("home");
    let target = home.join(".cache/x/target");
    let old = target.join("debug/deps/old.rlib");
    fs::create_dir_all(old.parent().unwrap()).unwrap();
    fs::write(&old, vec![1u8; 40 * 1024]).unwrap();
    let f = fs::OpenOptions::new().write(true).open(&old).unwrap();
    let when = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(100);
    f.set_times(fs::FileTimes::new().set_modified(when).set_accessed(when))
        .unwrap();
    drop(f);
    // The dir is given in a non-canonical form; the line names the canonical path.
    let given = format!("{}/.cache/./x/target", home.display());
    let (code, msg) = cli(
        Some(home.to_str().unwrap()),
        &[
            "--dir",
            &given,
            "--single",
            "--limit-kib",
            "20",
            "--to-kib",
            "10",
        ],
    );
    assert_eq!(code, Some(0), "{msg}");
    assert!(!old.exists(), "{msg}");
    let line = format!("sweep-target: removing {}", old.display());
    let at = msg
        .find(&line)
        .unwrap_or_else(|| panic!("no `{line}` in: {msg}"));
    assert!(!msg[at..].starts_with(&format!("{line}/")), "{msg}");
    assert!(!msg.contains("/./"), "{msg}");
}

#[test]
fn single_mode_has_the_same_home_and_cache_root_checks() {
    let t = tmp("single-guard");
    let home = t.0.join("home");
    fs::create_dir_all(home.join(".cache")).unwrap();
    let elsewhere = t.0.join("elsewhere/target");
    let old = elsewhere.join("debug/deps/old.rlib");
    fs::create_dir_all(old.parent().unwrap()).unwrap();
    fs::write(&old, vec![1u8; 40 * 1024]).unwrap();
    let args = |d: &Path| {
        vec![
            "--dir".to_string(),
            d.display().to_string(),
            "--single".to_string(),
            "--limit-kib".to_string(),
            "20".to_string(),
            "--to-kib".to_string(),
            "10".to_string(),
        ]
    };
    let run = |home: Option<&str>, d: &Path| {
        let a = args(d);
        let a: Vec<&str> = a.iter().map(String::as_str).collect();
        cli(home, &a)
    };
    // No HOME, an empty HOME: refused. A good HOME but a dir outside its cache: refused. Nothing is removed in any of them.
    for (h, why) in [
        (None, "HOME is unset or empty"),
        (Some(""), "HOME is unset or empty"),
        (Some(home.to_str().unwrap()), "not strictly under"),
    ] {
        let (code, msg) = run(h, &elsewhere);
        assert_eq!(code, Some(2), "{msg}");
        assert!(msg.contains(why), "{msg}");
        assert!(old.exists(), "{msg}");
    }
    // A cache that is a link to `/` leaves no legal root at all.
    let slash_home = t.0.join("slashhome");
    fs::create_dir_all(&slash_home).unwrap();
    std::os::unix::fs::symlink("/", slash_home.join(".cache")).unwrap();
    let (code, msg) = run(Some(slash_home.to_str().unwrap()), &elsewhere);
    assert_eq!(code, Some(2), "{msg}");
    assert!(msg.contains("allowed root is `/`"), "{msg}");
    assert!(old.exists(), "{msg}");
    // A cap of 0 KiB is refused like --limit-gib 0, even for a good dir.
    let good = home.join(".cache/x/target");
    let gold = good.join("debug/deps/old.rlib");
    fs::create_dir_all(gold.parent().unwrap()).unwrap();
    fs::write(&gold, vec![1u8; 40 * 1024]).unwrap();
    let (code, msg) = cli(
        Some(home.to_str().unwrap()),
        &[
            "--dir",
            good.to_str().unwrap(),
            "--single",
            "--limit-kib",
            "0",
        ],
    );
    assert_eq!(code, Some(2), "{msg}");
    assert!(msg.contains("cap of 0"), "{msg}");
    assert!(gold.exists(), "{msg}");
}

/// Stamps a file or folder `secs` seconds after the epoch (mtime and atime): old enough to be outside the in-use window.
fn stamp_old(p: &Path) {
    let f = fs::File::open(p).unwrap();
    let when = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(100);
    f.set_times(fs::FileTimes::new().set_modified(when).set_accessed(when))
        .unwrap();
}

/// A check-cache-shaped root `home/.cache/lucina-check-target` over 1 GiB (a sparse file) with one idle worktree target `wt-old`.
fn big_cache(home: &Path) -> (PathBuf, PathBuf) {
    let root = home.join(".cache/lucina-check-target");
    let wt = root.join("wt-old");
    let big = wt.join("debug/deps/big");
    fs::create_dir_all(big.parent().unwrap()).unwrap();
    fs::File::create(&big).unwrap().set_len(2 << 30).unwrap();
    // Bottom up, so stamping a folder is not undone by a later entry in it.
    for p in [&big, &wt.join("debug/deps"), &wt.join("debug"), &wt] {
        stamp_old(p);
    }
    (root, wt)
}

#[test]
fn root_mode_refuses_a_folder_without_the_check_cache_marker_and_sweeps_one_with_it() {
    let t = tmp("marker");
    let home = t.0.join("home");
    let (root, wt) = big_cache(&home);
    let args = [
        "--dir",
        root.to_str().unwrap(),
        "--limit-gib",
        "1",
        "--to-gib",
        "0",
    ];
    // Another cache pointed at (say `LUCINA_CHECK_CACHE=$HOME/.cache/cargo-target`): no marker, exit 2, nothing removed.
    let (code, msg) = cli(Some(home.to_str().unwrap()), &args);
    assert_eq!(code, Some(2), "{msg}");
    assert!(
        msg.contains("not a check cache") && msg.contains("nothing was removed"),
        "{msg}"
    );
    assert!(wt.exists(), "{msg}");
    // Cargo's own tag is not the marker.
    fs::write(
        root.join("CACHEDIR.TAG"),
        "Signature: 8a477f597d28d172789f06886806bc55\n# This file is a cache directory tag created by cargo.\n",
    )
    .unwrap();
    let (code, msg) = cli(Some(home.to_str().unwrap()), &args);
    assert_eq!(code, Some(2), "{msg}");
    assert!(wt.exists(), "{msg}");
    // A marker that is a link is refused.
    let real = t.0.join("real-marker");
    fs::write(&real, "lucina-check-target\n").unwrap();
    std::os::unix::fs::symlink(&real, root.join(".lucina-check-target")).unwrap();
    let (code, msg) = cli(Some(home.to_str().unwrap()), &args);
    assert_eq!(code, Some(2), "{msg}");
    assert!(wt.exists(), "{msg}");
    fs::remove_file(root.join(".lucina-check-target")).unwrap();
    // The marker file `scripts/check-fast` writes: the folder is swept.
    fs::write(root.join(".lucina-check-target"), "lucina-check-target\n").unwrap();
    let (code, msg) = cli(Some(home.to_str().unwrap()), &args);
    assert_eq!(code, Some(0), "{msg}");
    assert!(!wt.exists(), "a marked cache is swept: {msg}");
    assert!(msg.contains("removed"), "{msg}");
}
