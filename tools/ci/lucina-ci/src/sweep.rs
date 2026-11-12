//! Keeps the fast-check cache under a size limit (docs/ci/SHIFT-LEFT.md). The cache root (`~/.cache/lucina-check-target`) holds one Cargo
//! target per worktree (`wt-<hash>/`): Cargo gives a workspace member the same artifact hash in every worktree, so a target shared by
//! worktrees on different commits let one overwrite the others' rlibs and test binaries (seen twice on 2026-11-11, a false pass
//! waiting to happen).
//! Over the limit, the least recently used worktree targets go whole, oldest first, down to a lower mark. Old layouts (a shared
//! `debug/` and `release/` straight under the root) count as never used and go first. A target whose cargo lock is held (a build is
//! running in it) is skipped. A removed target costs a rebuild, never correctness.
//! `--single` caps ONE Cargo target dir instead of a root of them (the app and landing caches under `~/.cache/cargo-target`,
//! docs/ci/SHIFT-LEFT.md "Other cargo targets"): over the limit the least recently used build artifacts of that one target go, oldest
//! first, down to the lower mark, and nothing else - never the binaries in the profile folder. Here a build running in the target means
//! the whole sweep is skipped, because half a target cleaned under a live build is worse than a dirty one.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: PathBuf,
    pub bytes: u64,
    /// Seconds since the epoch of the last use (0: an old layout, never used by the per-worktree check).
    pub used: u64,
}

/// Which entries to remove, oldest first, so that `total - removed <= to_bytes`. Nothing when the total is already within `limit_bytes`.
pub fn plan(entries: &[Entry], total: u64, limit_bytes: u64, to_bytes: u64) -> Vec<usize> {
    if total <= limit_bytes {
        return Vec::new();
    }
    let mut order: Vec<usize> = (0..entries.len()).collect();
    order.sort_by_key(|&i| (entries[i].used, entries[i].path.clone()));
    let mut left = total;
    let mut out = Vec::new();
    for i in order {
        if left <= to_bytes {
            break;
        }
        left = left.saturating_sub(entries[i].bytes);
        out.push(i);
    }
    out
}

fn secs(t: SystemTime) -> u64 {
    t.duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn tree_bytes(p: &Path) -> u64 {
    let Ok(md) = fs::symlink_metadata(p) else {
        return 0;
    };
    if md.is_dir() {
        fs::read_dir(p)
            .map(|rd| rd.flatten().map(|e| tree_bytes(&e.path())).sum())
            .unwrap_or(0)
    } else {
        md.len()
    }
}

/// The cache entries that may go: each folder under the root except `keep` (the target in use now).
pub fn candidates(root: &Path, keep: Option<&Path>) -> Vec<Entry> {
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(root) else {
        return out;
    };
    for e in rd.flatten() {
        let p = e.path();
        if !p.is_dir() || keep.is_some_and(|k| k == p) {
            continue;
        }
        let name = e.file_name().to_string_lossy().into_owned();
        let used = if name.starts_with("wt-") {
            fs::metadata(p.join(".last-used"))
                .or_else(|_| fs::metadata(&p))
                .and_then(|m| m.modified())
                .map_or(0, secs)
        } else {
            0
        };
        out.push(Entry {
            bytes: tree_bytes(&p),
            used,
            path: p,
        });
    }
    out
}

/// Holds the cargo locks of a target (`.cargo-lock` in it, or in its `debug/` and `release/`), without waiting. `None`: a build holds one.
fn lock_target(dir: &Path) -> Option<Vec<fs::File>> {
    let mut held = Vec::new();
    for sub in ["", "debug", "release"] {
        let lock = dir.join(sub).join(".cargo-lock");
        if !lock.exists() {
            continue;
        }
        let f = fs::OpenOptions::new().write(true).open(&lock).ok()?;
        f.try_lock().ok()?;
        held.push(f);
    }
    Some(held)
}

/// `sweep-target --dir ROOT --limit-gib N [--to-gib M] [--keep TARGET] [--dry-run]`. Always exit 0 except a usage error.
pub fn run(
    root: &Path,
    limit_gib: u64,
    to_gib: u64,
    keep: Option<&Path>,
    dry: bool,
) -> Result<i32, String> {
    const GIB: u64 = 1 << 30;
    let total = tree_bytes(root);
    println!(
        "sweep-target: {} is {:.1} GiB (limit {limit_gib}, sweep down to {to_gib})",
        root.display(),
        total as f64 / GIB as f64
    );
    if total <= limit_gib * GIB {
        return Ok(0);
    }
    let entries = candidates(root, keep);
    let doomed = plan(&entries, total, limit_gib * GIB, to_gib * GIB);
    let (mut freed, mut n, mut busy) = (0u64, 0, 0);
    for i in &doomed {
        let e = &entries[*i];
        let Some(_locks) = lock_target(&e.path) else {
            busy += 1;
            continue;
        };
        if !dry && fs::remove_dir_all(&e.path).is_err() {
            continue;
        }
        freed += e.bytes;
        n += 1;
    }
    println!(
        "sweep-target: {} {n} cache folders, {:.1} GiB, least recently used first; {busy} skipped because a build is running in them{}",
        if dry { "would remove" } else { "removed" },
        freed as f64 / GIB as f64,
        if dry { " (dry run)" } else { "" }
    );
    Ok(0)
}

/// The later of mtime and atime: a link, a test run or a rebuild reads an artifact without writing it.
fn last_use(md: &fs::Metadata) -> u64 {
    md.modified()
        .map_or(0, secs)
        .max(md.accessed().map_or(0, secs))
}

/// The paths a folder holds; none when it cannot be read.
fn children(dir: &Path) -> Vec<PathBuf> {
    fs::read_dir(dir).map_or_else(
        |_| Vec::new(),
        |rd| rd.flatten().map(|e| e.path()).collect(),
    )
}

/// A profile folder of a Cargo target, where its artifacts live: `debug` or `release`, directly under the target or under a
/// target-triple folder, or any folder that holds a `deps/`.
fn is_profile(p: &Path) -> bool {
    if !p.is_dir() {
        return false;
    }
    let name = p
        .file_name()
        .map_or(String::new(), |n| n.to_string_lossy().into_owned());
    name == "debug" || name == "release" || p.join("deps").is_dir()
}

/// The profile folders of ONE target: those directly under it and those one level deeper under a target triple.
pub fn profiles(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for c in children(dir) {
        if is_profile(&c) {
            out.push(c);
        } else {
            out.extend(children(&c).into_iter().filter(|g| is_profile(g)));
        }
    }
    out.sort();
    out
}

/// What may go from ONE target: the files of each profile's `deps/` and the folders of its `incremental/` and `build/`. Nothing
/// directly in the profile folder (those are the binaries) and no fingerprint. Cargo rebuilds every one of them.
pub fn single_artifacts(dir: &Path) -> Vec<Entry> {
    let mut out = Vec::new();
    for prof in profiles(dir) {
        for p in children(&prof.join("deps")) {
            let Ok(md) = fs::symlink_metadata(&p) else {
                continue;
            };
            if md.is_dir() {
                continue;
            }
            out.push(Entry {
                bytes: md.len(),
                used: last_use(&md),
                path: p,
            });
        }
        for sub in ["incremental", "build"] {
            for p in children(&prof.join(sub)) {
                let Ok(md) = fs::symlink_metadata(&p) else {
                    continue;
                };
                if !md.is_dir() {
                    continue;
                }
                out.push(Entry {
                    bytes: tree_bytes(&p),
                    used: last_use(&md),
                    path: p,
                });
            }
        }
    }
    out
}

/// The process table to ask whether a build is running: `/proc`, or `LUCINA_PROC_ROOT` when a test brings its own. A process is a
/// folder named by its pid with `comm`, NUL-separated `environ`, a `cwd` link and an `fd/` folder of links.
fn proc_root() -> PathBuf {
    std::env::var_os("LUCINA_PROC_ROOT").map_or_else(|| PathBuf::from("/proc"), PathBuf::from)
}

/// A program name that is part of a Cargo build. A build script or test binary has an arbitrary name, so those are recognised by
/// what they hold open instead.
fn is_builder(comm: &str) -> bool {
    let n = comm.split_whitespace().next().unwrap_or("");
    n == "cargo"
        || n == "rustc"
        || n == "rustdoc"
        || n.starts_with("build-script")
        || n.starts_with("cargo-")
}

/// `cand`, or the path it resolves to, lies inside `real`.
fn inside(real: &Path, cand: &Path) -> bool {
    fs::canonicalize(cand)
        .unwrap_or_else(|_| cand.to_path_buf())
        .starts_with(real)
}

/// Is a process building into this exact target right now? Two ways, and only two:
///
/// - it names this target in `CARGO_TARGET_DIR` and is a build program (cargo, rustc, rustdoc, a build script): that is a build
///   of this target, running now, whatever it has open;
/// - it has its cwd or a file open inside the target, and either names this target (which is how a test binary or a build script,
///   whose name says nothing, is recognised: Cargo's variable is in its environment) or is a build program (a hand-run rustc,
///   whose parent took no lock at all).
///
/// An idle shell that only exports the variable is neither, so it cannot hold the cap open forever. A `CARGO_TARGET_DIR` that is
/// an ancestor or a sibling of this target writes elsewhere, so it is not a reason to skip. Cargo's package lock covers the same
/// ground while a build runs; this also catches a build between two locks. Our own pid does not count: we hold those locks.
fn target_in_use(dir: &Path, procs: &Path) -> Option<String> {
    let real = fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    for p in children(procs) {
        let pid = p
            .file_name()
            .map_or(String::new(), |n| n.to_string_lossy().into_owned());
        if pid == std::process::id().to_string() {
            continue;
        }
        let comm = fs::read_to_string(p.join("comm")).unwrap_or_default();
        let builder = is_builder(&comm);
        let names_us = fs::read(p.join("environ")).is_ok_and(|env| {
            env.split(|b| *b == 0).any(|kv| {
                let kv = String::from_utf8_lossy(kv);
                ["CARGO_TARGET_DIR", "CARGO_BUILD_TARGET_DIR"]
                    .iter()
                    .filter_map(|k| kv.strip_prefix(&format!("{k}=")))
                    .any(|v| inside(&real, Path::new(v)))
            })
        });
        if !builder && !names_us {
            continue;
        }
        if builder && names_us {
            return Some(format!(
                "pid {pid} ({}) builds into it, CARGO_TARGET_DIR names it",
                comm.trim_end()
            ));
        }
        let open = children(&p.join("fd")).into_iter().chain([p.join("cwd")]);
        for f in open {
            let Ok(target) = fs::read_link(&f) else {
                continue;
            };
            if inside(&real, &target) {
                return Some(format!(
                    "pid {pid} ({}) has {} open in it",
                    comm.trim_end(),
                    target.display()
                ));
            }
        }
    }
    None
}

/// Exclusive non-blocking locks on `.cargo-lock` of EVERY profile folder of one target, held while its artifacts go. `None`: a build
/// is running in it (or a lock cannot be had), so nothing must be removed. A missing lock is created first, so a build starting now
/// waits for us; a dry run creates nothing and only checks the locks that exist.
fn lock_profiles(dir: &Path, dry: bool) -> Option<Vec<fs::File>> {
    let mut held = Vec::new();
    for prof in profiles(dir) {
        let lock = prof.join(".cargo-lock");
        if dry && !lock.exists() {
            continue;
        }
        let f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock)
            .ok()?;
        f.try_lock().ok()?;
        held.push(f);
    }
    Some(held)
}

/// One artifact: a whole folder with what is in it, or a single file. False when it could not be removed.
fn remove_artifact(p: &Path) -> bool {
    match fs::symlink_metadata(p) {
        Ok(md) if md.is_dir() => fs::remove_dir_all(p).is_ok(),
        Ok(_) => fs::remove_file(p).is_ok(),
        Err(_) => false,
    }
}

/// One target dir, byte limits (so a test needs no GiB of files), byte marks and the process table to consult. Exit 0 as usual,
/// 3 when the target is in use and was left alone, so the caller can log that it skipped; the `Err` paths are the usage errors.
fn sweep_single(
    dir: &Path,
    limit_bytes: u64,
    to_bytes: u64,
    dry: bool,
    procs: &Path,
) -> Result<i32, String> {
    const GIB: f64 = (1u64 << 30) as f64;
    if !dir.is_dir() {
        return Err(format!(
            "sweep-target --single: {} is not a folder",
            dir.display()
        ));
    }
    let total = tree_bytes(dir);
    println!(
        "sweep-target --single: {} is {:.1} GiB (limit {:.0} GiB, sweep down to {:.0} GiB)",
        dir.display(),
        total as f64 / GIB,
        limit_bytes as f64 / GIB,
        to_bytes as f64 / GIB
    );
    if total <= limit_bytes {
        return Ok(0);
    }
    if let Some(why) = target_in_use(dir, procs) {
        println!(
            "sweep-target --single: {} in use: not swept ({why})",
            dir.display()
        );
        return Ok(3);
    }
    let Some(locks) = lock_profiles(dir, dry) else {
        println!(
            "sweep-target --single: {} in use: not swept (a build holds a cargo lock in it)",
            dir.display()
        );
        return Ok(3);
    };
    // A dry run removes nothing, so it does not hold a running build off the target while it counts; a real run keeps the locks
    // until the artifacts are gone, and Cargo waits for them.
    if dry {
        drop(locks);
    }
    let entries = single_artifacts(dir);
    let doomed = plan(&entries, total, limit_bytes, to_bytes);
    let (mut freed, mut n) = (0u64, 0);
    for i in &doomed {
        let e = &entries[*i];
        if !dry && !remove_artifact(&e.path) {
            continue;
        }
        freed += e.bytes;
        n += 1;
    }
    println!(
        "sweep-target --single: {} {n} build artifacts, {:.1} GiB, least recently used first; {:.1} GiB -> {:.1} GiB{}",
        if dry { "would remove" } else { "removed" },
        freed as f64 / GIB,
        total as f64 / GIB,
        (total - freed.min(total)) as f64 / GIB,
        if dry { " (dry run)" } else { "" }
    );
    Ok(0)
}

/// The GiB marks people and the timer give, in the KiB `run_single` takes: one GiB is 1024 KiB of 1024 bytes, so a cap of 40 GiB
/// is 41 943 040 KiB. The same scale as `run` uses for the cache root, and the one `run_single` multiplies back into bytes.
pub fn caps_kib(limit_gib: u64, to_gib: u64) -> (u64, u64) {
    const KIB_IN_GIB: u64 = 1 << 20;
    (limit_gib * KIB_IN_GIB, to_gib * KIB_IN_GIB)
}

/// `sweep-target --dir D --single --limit-gib N [--to-gib M] [--limit-kib K] [--to-kib T] [--dry-run]`: cap ONE Cargo target dir
/// (the app and landing caches). The KiB marks exist so a test can run the real thing on a folder of a few KiB.
pub fn run_single(dir: &Path, limit_kib: u64, to_kib: u64, dry: bool) -> Result<i32, String> {
    const KIB: u64 = 1 << 10;
    sweep_single(dir, limit_kib * KIB, to_kib * KIB, dry, &proc_root())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(name: &str, bytes: u64, used: u64) -> Entry {
        Entry {
            path: PathBuf::from(name),
            bytes,
            used,
        }
    }

    #[test]
    fn under_the_limit_nothing_goes() {
        let v = [e("a", 10, 1), e("b", 10, 2)];
        assert!(plan(&v, 20, 20, 10).is_empty());
        assert!(plan(&v, 5, 20, 10).is_empty());
    }

    #[test]
    fn the_oldest_go_first_down_to_the_lower_mark() {
        let v = [
            e("new", 10, 300),
            e("old", 10, 100),
            e("mid", 10, 200),
            e("tiny", 1, 50),
        ];
        let gone: Vec<&str> = plan(&v, 31, 25, 15)
            .into_iter()
            .map(|i| v[i].path.to_str().unwrap())
            .collect();
        assert_eq!(gone, ["tiny", "old", "mid"]);
    }

    #[test]
    fn ties_go_in_name_order_and_all_can_go() {
        let v = [e("b", 5, 1), e("a", 5, 1)];
        assert_eq!(plan(&v, 10, 4, 0), [1, 0]);
    }

    #[test]
    fn worktree_targets_go_least_recently_used_first_and_the_one_in_use_stays() {
        let t = std::env::temp_dir().join(format!("lucina-ci-sweep-{}", std::process::id()));
        let _ = fs::remove_dir_all(&t);
        for (name, age) in [("wt-old", 10_000u64), ("wt-new", 10), ("wt-mine", 5_000)] {
            let d = t.join(name);
            fs::create_dir_all(d.join("debug")).unwrap();
            fs::write(d.join("debug/blob"), vec![0u8; 1000]).unwrap();
            let f = fs::File::create(d.join(".last-used")).unwrap();
            let when = SystemTime::now() - std::time::Duration::from_secs(age);
            f.set_times(fs::FileTimes::new().set_modified(when))
                .unwrap();
        }
        // The old shared layout: never used, goes first.
        fs::create_dir_all(t.join("debug")).unwrap();
        fs::write(t.join("debug/blob"), vec![0u8; 1000]).unwrap();
        fs::write(t.join("CACHEDIR.TAG"), "x").unwrap();
        let mine = t.join("wt-mine");
        let entries = candidates(&t, Some(&mine));
        assert_eq!(entries.len(), 3, "{entries:?}");
        let names: Vec<String> = plan(&entries, tree_bytes(&t), 3000, 2200)
            .into_iter()
            .map(|i| {
                entries[i]
                    .path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(names, ["debug", "wt-old"]);
        fs::remove_dir_all(&t).ok();
    }

    #[test]
    fn a_target_with_a_build_running_is_skipped() {
        let t = std::env::temp_dir().join(format!("lucina-ci-sweep-lock-{}", std::process::id()));
        let _ = fs::remove_dir_all(&t);
        fs::create_dir_all(t.join("wt-busy/debug")).unwrap();
        fs::write(t.join("wt-busy/debug/.cargo-lock"), "").unwrap();
        let build = fs::OpenOptions::new()
            .write(true)
            .open(t.join("wt-busy/debug/.cargo-lock"))
            .unwrap();
        build.lock().unwrap();
        assert!(lock_target(&t.join("wt-busy")).is_none());
        drop(build);
        assert!(
            lock_frees_up(&mut || lock_target(&t.join("wt-busy")).is_some()),
            "the target lock we released is lockable again"
        );
        fs::remove_dir_all(&t).ok();
    }

    // ---- one target dir (--single) ----

    /// `--limit-gib N` reaches `run_single` as the KiB of N GiB, not of N MiB: one 1024 was the unit bug.
    #[test]
    fn a_gib_cap_is_the_kib_of_whole_gib() {
        assert_eq!(caps_kib(40, 32), (41_943_040, 33_554_432));
        // The default `--to-gib` (80% of the limit) converts exactly like a given one.
        let limit = 8;
        assert_eq!(caps_kib(limit, limit * 4 / 5), (8_388_608, 6_291_456));
        // The bytes `run_single` hands `sweep_single` are the cap the flag names, no more and no less.
        let (lim, to) = caps_kib(40, 32);
        assert_eq!((lim * 1024, to * 1024), (40u64 << 30, 32u64 << 30));
    }

    #[test]
    fn a_cap_of_a_thousand_gib_stays_inside_u64() {
        assert_eq!(caps_kib(1000, 800), (1_048_576_000, 838_860_800));
        assert_eq!(caps_kib(1000, 800).0 * 1024, 1000u64 << 30);
    }

    const KIB: usize = 1 << 10;
    /// One artifact of the fake target below: 32 KiB, so a KiB mark is exact.
    const ONE: usize = 32 * KIB;

    fn kib(n: usize) -> u64 {
        (n * KIB) as u64
    }

    /// A file of `bytes` bytes at the target-relative path `rel`, last used `ago` seconds after the epoch. Both its times are set,
    /// so the LRU order here does not depend on the mount options.
    fn art(t: &Path, rel: &str, bytes: usize, ago: u64) {
        let p = t.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, vec![7u8; bytes]).unwrap();
        let when = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(ago);
        let f = fs::OpenOptions::new().write(true).open(&p).unwrap();
        f.set_times(fs::FileTimes::new().set_modified(when).set_accessed(when))
            .unwrap();
    }

    fn has(t: &Path, rel: &str) -> bool {
        t.join(rel).exists()
    }

    /// Keeps trying `take` for up to one second. A lock we released keeps reading as held for a few milliseconds now and then:
    /// other tests in this same test binary fork children, and a child forked while we held the lock carries an inherited copy
    /// of that fd until it execs — the kernel counts the lock as held for the description, and reports it under our pid. The
    /// wait is bounded, so a lock that really stays held still fails the test.
    fn lock_frees_up(take: &mut dyn FnMut() -> bool) -> bool {
        for _ in 0..50 {
            if take() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        false
    }

    /// A process table with nobody in it: the sweep is then down to Cargo's locks alone.
    fn no_builds(name: &str) -> PathBuf {
        let p =
            std::env::temp_dir().join(format!("lucina-sweep-procs-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    /// One process of a fake process table: a folder of its pid with `comm`, its NUL-separated environment, and a link per open
    /// file under `fd/`. Where a link points need not exist: the sweep only asks where it points.
    fn fake_proc(procs: &Path, pid: &str, comm: &str, env: &[&str], open: &[PathBuf]) {
        let p = procs.join(pid);
        fs::create_dir_all(p.join("fd")).unwrap();
        fs::write(p.join("comm"), format!("{comm}\n")).unwrap();
        fs::write(p.join("environ"), env.join("\0")).unwrap();
        for (i, target) in open.iter().enumerate() {
            std::os::unix::fs::symlink(target, p.join("fd").join(i.to_string())).unwrap();
        }
    }

    /// A target as Cargo lays it out: its binaries and Cargo's own lock in each profile folder, the artifacts in `deps/`,
    /// `incremental/` and `build/`, and the same again under a target-triple folder. Eight 32 KiB files: 256 KiB.
    fn fake_target(name: &str) -> PathBuf {
        let t =
            std::env::temp_dir().join(format!("lucina-sweep-single-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&t);
        art(&t, "debug/mybinary", ONE, 500); // a binary, directly in the profile folder
        art(&t, "debug/deps/old.rlib", ONE, 100);
        art(&t, "debug/deps/mid.rlib", ONE, 200);
        art(&t, "debug/deps/new.rlib", ONE, 900);
        art(&t, "debug/deps/note.d", ONE, 300);
        art(&t, "release/build/some1-aaa/output", ONE, 400);
        art(&t, "release/incremental/some2-bbb/in.lock", ONE, 600);
        art(&t, "aarch64-fakeos-gnu/debug/deps/triple.rlib", ONE, 50);
        // Cargo's lock, one per profile folder; the triple folder has none yet, as on a target only built by hand.
        fs::write(t.join("debug/.cargo-lock"), "").unwrap();
        fs::write(t.join("release/.cargo-lock"), "").unwrap();
        t
    }

    #[test]
    fn one_target_sweeps_its_artifacts_least_recently_used_first_down_to_the_mark() {
        let t = fake_target("lru");
        // 256 KiB, over 224 sweep down to 192: exactly the two oldest artifacts go, one of them from the target triple.
        let rc = sweep_single(&t, kib(224), kib(192), false, &no_builds("lru")).unwrap();
        assert_eq!(rc, 0, "nothing was in use");
        for gone in [
            "aarch64-fakeos-gnu/debug/deps/triple.rlib",
            "debug/deps/old.rlib",
        ] {
            assert!(!has(&t, gone), "{gone} should be gone");
        }
        for keep in [
            "debug/deps/mid.rlib",
            "debug/deps/note.d",
            "debug/deps/new.rlib",
            "debug/mybinary",
            "release/build/some1-aaa",
            "release/incremental/some2-bbb",
        ] {
            assert!(has(&t, keep), "{keep} should stay");
        }
        fs::remove_dir_all(&t).ok();
    }

    #[test]
    fn one_target_sweep_keeps_the_binaries_and_touches_nothing_else_in_the_profile_folder() {
        let t = fake_target("bins");
        // Down to nothing: everything removable goes, the rest of the target stays.
        assert_eq!(
            sweep_single(&t, 0, 0, false, &no_builds("bins")).unwrap(),
            0,
            "nothing was in use"
        );
        assert!(has(&t, "debug/mybinary"), "a binary is never removed");
        assert!(
            has(&t, "debug/.cargo-lock"),
            "the lock stays in the profile folder"
        );
        assert!(has(&t, "release/.cargo-lock"));
        assert!(has(&t, "aarch64-fakeos-gnu/debug/deps"), "the folder stays");
        for gone in [
            "debug/deps/old.rlib",
            "debug/deps/new.rlib",
            "release/build/some1-aaa",
            "release/incremental/some2-bbb",
            "aarch64-fakeos-gnu/debug/deps/triple.rlib",
        ] {
            assert!(!has(&t, gone), "{gone} should be gone");
        }
        fs::remove_dir_all(&t).ok();
    }

    #[test]
    fn one_target_sweep_does_nothing_while_a_build_holds_a_lock() {
        let t = fake_target("lock");
        // A lock held in ONE profile folder is enough: half a target cleaned under a running build is worse than a dirty one.
        let held = fs::OpenOptions::new()
            .write(true)
            .open(t.join("release/.cargo-lock"))
            .unwrap();
        held.lock().unwrap();
        assert!(lock_profiles(&t, false).is_none());
        assert_eq!(
            sweep_single(&t, 0, 0, false, &no_builds("lock")).unwrap(),
            3,
            "in use: not swept"
        );
        for keep in [
            "debug/deps/old.rlib",
            "debug/mybinary",
            "release/build/some1-aaa",
        ] {
            assert!(has(&t, keep), "{keep} should be untouched");
        }
        drop(held);
        assert!(
            lock_frees_up(&mut || lock_profiles(&t, false).is_some()),
            "the profile locks we released are lockable again"
        );
        fs::remove_dir_all(&t).ok();
    }

    #[test]
    fn a_dry_run_of_one_target_removes_nothing_and_creates_no_lock() {
        let t = fake_target("dry");
        let missing = "aarch64-fakeos-gnu/debug/.cargo-lock";
        assert!(!has(&t, missing), "the fake target has no lock there yet");
        assert_eq!(sweep_single(&t, 0, 0, true, &no_builds("dry")).unwrap(), 0);
        for keep in [
            "debug/deps/old.rlib",
            "debug/deps/mid.rlib",
            "debug/mybinary",
            "release/build/some1-aaa/output",
            "release/incremental/some2-bbb/in.lock",
            "aarch64-fakeos-gnu/debug/deps/triple.rlib",
        ] {
            assert!(has(&t, keep), "{keep} should still be there");
        }
        assert!(!has(&t, missing), "a dry run creates nothing");
        fs::remove_dir_all(&t).ok();
    }

    // ---- a build that takes the target without holding its lock ----

    #[test]
    fn a_build_that_names_the_target_in_cargo_target_dir_stops_the_sweep() {
        let t = fake_target("procenv");
        let procs = no_builds("procenv");
        let real = fs::canonicalize(&t).unwrap();
        fake_proc(
            &procs,
            "4242",
            "cargo",
            &[
                "PATH=/usr/bin",
                &format!("CARGO_TARGET_DIR={}", real.display()),
            ],
            &[],
        );
        assert_eq!(
            sweep_single(&t, 0, 0, false, &procs).unwrap(),
            3,
            "in use: not swept"
        );
        assert!(
            has(&t, "debug/deps/old.rlib"),
            "a running build keeps its artifacts"
        );
        assert!(
            has(&t, "release/incremental/some2-bbb"),
            "and its incremental folder"
        );
        fs::remove_dir_all(&t).ok();
        fs::remove_dir_all(&procs).ok();
    }

    #[test]
    fn a_rustc_with_a_file_open_in_the_target_stops_the_sweep() {
        let t = fake_target("procfd");
        let procs = no_builds("procfd");
        let real = fs::canonicalize(&t).unwrap();
        // A hand-run rustc: no lock taken and nothing in its environment, but its output is open in the target.
        fake_proc(
            &procs,
            "4343",
            "rustc",
            &["PATH=/usr/bin"],
            &[real.join("debug/deps/mid.rlib")],
        );
        assert_eq!(
            sweep_single(&t, 0, 0, false, &procs).unwrap(),
            3,
            "in use: not swept"
        );
        assert!(
            has(&t, "debug/deps/mid.rlib"),
            "a running rustc keeps its objects"
        );
        fs::remove_dir_all(&t).ok();
        fs::remove_dir_all(&procs).ok();
    }

    #[test]
    fn a_shell_that_only_exports_the_variable_does_not_hold_the_cap_open() {
        let t = fake_target("procshell");
        let procs = no_builds("procshell");
        let real = fs::canonicalize(&t).unwrap();
        // Anyone can export CARGO_TARGET_DIR in a shell and leave it there; nothing of ours is open, so this is not a build.
        fake_proc(
            &procs,
            "4444",
            "bash",
            &[
                "PATH=/usr/bin",
                &format!("CARGO_TARGET_DIR={}", real.display()),
            ],
            &[real.parent().unwrap().to_path_buf()],
        );
        assert_eq!(sweep_single(&t, 0, 0, false, &procs).unwrap(), 0);
        assert!(!has(&t, "debug/deps/old.rlib"), "the sweep ran");
        fs::remove_dir_all(&t).ok();
        fs::remove_dir_all(&procs).ok();
    }

    #[test]
    fn a_build_of_another_target_does_not_stop_the_sweep() {
        let t = fake_target("procother");
        let procs = no_builds("procother");
        // The check cache of a sibling worktree: a busy build, but not in this folder.
        let other = no_builds("procother-target");
        fake_proc(
            &procs,
            "4545",
            "cargo",
            &[
                "PATH=/usr/bin",
                &format!("CARGO_TARGET_DIR={}", other.display()),
            ],
            &[other.join("debug/deps/x.rlib")],
        );
        assert_eq!(sweep_single(&t, 0, 0, false, &procs).unwrap(), 0);
        assert!(!has(&t, "debug/deps/old.rlib"), "the sweep ran");
        fs::remove_dir_all(&t).ok();
        fs::remove_dir_all(&procs).ok();
        fs::remove_dir_all(&other).ok();
    }
}
