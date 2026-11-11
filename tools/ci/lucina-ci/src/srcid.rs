//! A source id for the lucina-ci binary: a 64-bit FNV hash of its own sources (`src/**`, `Cargo.toml`, `Cargo.lock`), sorted by path.
//! `build.rs` embeds it as `LUCINA_CI_SRC_ID`; `check-fast` recomputes it from the worktree under test and refuses when the running
//! binary was built from something else (a stale binary, another worktree's target). Pure std; this file is shared by `build.rs`
//! (`#[path]`) and `main.rs`, so both compute it the same way.
#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else {
            out.push(p);
        }
    }
}

/// FNV-1a over `(relative path, 0, length, 0, bytes)` of every file, in path order.
pub fn hash_files(files: &[(String, Vec<u8>)]) -> String {
    let mut sorted: Vec<&(String, Vec<u8>)> = files.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |b: u8| {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    };
    for (p, bytes) in sorted {
        p.bytes().for_each(&mut eat);
        eat(0);
        bytes.len().to_string().bytes().for_each(&mut eat);
        eat(0);
        bytes.iter().copied().for_each(&mut eat);
    }
    format!("{h:016x}")
}

/// The source id of the lucina-ci crate rooted at `dir` (the folder with `Cargo.toml` and `src/`); `None` when it has no `src`.
pub fn compute(dir: &Path) -> Option<String> {
    let mut paths = Vec::new();
    walk(&dir.join("src"), &mut paths);
    if paths.is_empty() {
        return None;
    }
    paths.push(dir.join("Cargo.toml"));
    paths.push(dir.join("Cargo.lock"));
    let files: Vec<(String, Vec<u8>)> = paths
        .into_iter()
        .filter_map(|p| {
            let rel = p
                .strip_prefix(dir)
                .ok()?
                .to_string_lossy()
                .replace('\\', "/");
            Some((rel, fs::read(&p).ok()?))
        })
        .collect();
    Some(hash_files(&files))
}

/// `Some(message)` when the binary's embedded id differs from the worktree's.
pub fn mismatch(built: &str, tree: Option<&str>) -> Option<String> {
    match tree {
        None => Some(
            "the tree under test has no tools/ci/lucina-ci sources to compare this binary with"
                .into(),
        ),
        Some(t) if t == built => None,
        Some(t) => Some(format!(
            "this lucina-ci was built from sources {built}, the tree under test has {t}: a stale binary or another worktree's target; rebuild (cargo build --release --locked --manifest-path tools/ci/lucina-ci/Cargo.toml)"
        )),
    }
}
