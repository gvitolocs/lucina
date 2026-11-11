//! Keeps the fast-check cache under a size limit (docs/ci/SHIFT-LEFT.md). The cache root (`~/.cache/lucina-check-target`) holds one Cargo
//! target per worktree (`wt-<hash>/`): Cargo gives a workspace member the same artifact hash in every worktree, so a target shared by
//! worktrees on different commits let one overwrite the others' rlibs and test binaries (seen twice on 2026-11-11, a false pass
//! waiting to happen).
//! Over the limit, the least recently used worktree targets go whole, oldest first, down to a lower mark. Old layouts (a shared
//! `debug/` and `release/` straight under the root) count as never used and go first. A target whose cargo lock is held (a build is
//! running in it) is skipped. A removed target costs a rebuild, never correctness.

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
        assert!(lock_target(&t.join("wt-busy")).is_some());
        fs::remove_dir_all(&t).ok();
    }
}
