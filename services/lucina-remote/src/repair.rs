//! Copies of what e2fsck moved or cut after an unclean end, so that a repair can be seen and undone.
//!
//! e2fsck puts files it cannot place into /data/lost+found as `#<inode>` and clears block numbers it cannot trust. `lucina-data`
//! cannot know what that will be before the check, so afterwards this copies, into `<data>/lucina/repaired/<boot timestamp>/`:
//! - every entry of lost+found that is new since the last copy (`seen-lost-found` lists the ones already handled), and
//! - every file e2fsck reported truncated or cleared that is still on the disk (found by the path e2fsck gave, or by inode number).
//!
//! `MANIFEST` has one tab-separated line per copy: kind, original path (`-` if unknown), inode, size, sha256, name of the copy, and
//! what e2fsck said. Each file is written under a new name, fsynced, renamed, and the folder fsynced; the manifest goes last in
//! the same way. The total is capped at [`CAP_BYTES`]; what does not fit is listed in the manifest as `skipped`.

use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    time::Instant,
};

use sha2::{Digest, Sha256};

use crate::fsck::{Finding, Kind, Report};

pub const CAP_BYTES: u64 = 512 << 20;
/// The most directory entries looked at when a file is searched for by inode number.
const WALK_LIMIT: usize = 400_000;
const WALK_SECS: u64 = 20;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    /// The folder, if anything was copied or listed.
    pub dir: Option<PathBuf>,
    pub copied: usize,
    pub bytes: u64,
    pub skipped: Vec<String>,
}

impl Outcome {
    pub fn describe(&self) -> String {
        match &self.dir {
            None => "nothing to copy".to_string(),
            Some(d) => format!(
                "{} copied ({} bytes), {} skipped, in {}",
                self.copied,
                self.bytes,
                self.skipped.len(),
                d.display()
            ),
        }
    }
}

struct Entry {
    kind: &'static str,
    original: Option<String>,
    ino: u64,
    size: u64,
    sha: String,
    copy: String,
    said: String,
}

struct Run<'a> {
    dir: &'a Path,
    budget: u64,
    entries: Vec<Entry>,
    skipped: Vec<String>,
    bytes: u64,
}

/// `YYYYMMDDTHHMMSSZ` for a Unix time; with the clock still unset (before 2020) the seconds since boot are not known, so it says so.
pub fn timestamp(unix_s: u64) -> String {
    let days = (unix_s / 86_400) as i64;
    let rem = unix_s % 86_400;
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    let stamp = format!(
        "{y:04}{m:02}{d:02}T{:02}{:02}{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    );
    if unix_s < 1_577_836_800 {
        format!("{stamp}-clock-unset")
    } else {
        stamp
    }
}

fn fsync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

/// New name, fsync, rename, fsync of the folder.
fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let tmp = path.with_file_name(format!(".{name}.new"));
    let mut f = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    drop(f);
    fs::rename(&tmp, path)?;
    fsync_dir(path.parent().unwrap_or(Path::new("/")))
}

/// A file name that is safe on its own: the original's last component with anything odd replaced.
fn safe_name(path: &str) -> String {
    let last = path.rsplit('/').find(|s| !s.is_empty()).unwrap_or("file");
    let name: String = last
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '#') {
                c
            } else {
                '_'
            }
        })
        .take(80)
        .collect();
    if name.starts_with('.') {
        format!("_{name}")
    } else {
        name
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

impl Run<'_> {
    fn skip(&mut self, what: &str, why: &str) {
        self.skipped.push(format!("{what}: {why}"));
    }

    /// Copies one regular file into the folder. Returns the entry's data on success.
    fn copy_file(
        &mut self,
        src: &Path,
        shown: &str,
        kind: &'static str,
        ino: u64,
        said: &str,
        original: Option<String>,
    ) {
        let meta = match fs::symlink_metadata(src) {
            Ok(m) => m,
            Err(e) => return self.skip(shown, &format!("cannot stat: {e}")),
        };
        if meta.is_dir() {
            let Ok(rd) = fs::read_dir(src) else {
                return self.skip(shown, "cannot read the directory");
            };
            // A directory: its files one by one, as `<name>/<file>`.
            let mut children: Vec<_> = rd.flatten().collect();
            children.sort_by_key(|c| c.file_name());
            for c in children {
                let name = c.file_name().to_string_lossy().into_owned();
                let child_shown = format!("{shown}/{name}");
                let child_original = original.as_ref().map(|o| format!("{o}/{name}"));
                let cino = c.metadata().map(|m| m.ino()).unwrap_or(0);
                self.copy_file(&c.path(), &child_shown, kind, cino, said, child_original);
            }
            return;
        }
        if !meta.is_file() {
            return self.skip(shown, "not a regular file");
        }
        let size = meta.len();
        if self.bytes + size > self.budget {
            return self.skip(
                shown,
                &format!("{size} bytes would pass the {} MiB cap", CAP_BYTES >> 20),
            );
        }
        let copy = self.unique_name(ino, shown);
        let target = self.dir.join(&copy);
        let result = (|| -> io::Result<String> {
            let mut input = File::open(src)?;
            let tmp = self.dir.join(format!(".{copy}.new"));
            let mut out = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)?;
            let mut sha = Sha256::new();
            let mut buf = vec![0u8; 1 << 16];
            loop {
                let n = input.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                sha.update(&buf[..n]);
                out.write_all(&buf[..n])?;
            }
            out.sync_all()?;
            drop(out);
            fs::rename(&tmp, &target)?;
            fsync_dir(self.dir)?;
            Ok(hex(&sha.finalize()))
        })();
        match result {
            Ok(sha) => {
                self.bytes += size;
                self.entries.push(Entry {
                    kind,
                    original,
                    ino,
                    size,
                    sha,
                    copy,
                    said: said.replace(['\t', '\n'], " "),
                });
            }
            Err(e) => {
                let _ = fs::remove_file(self.dir.join(format!(".{copy}.new")));
                self.skip(shown, &format!("copy failed: {e}"));
            }
        }
    }

    fn unique_name(&self, ino: u64, shown: &str) -> String {
        let base = format!("{ino}-{}", safe_name(shown));
        let mut name = base.clone();
        let mut n = 1;
        while self.entries.iter().any(|e| e.copy == name) || self.dir.join(&name).exists() {
            n += 1;
            name = format!("{base}.{n}");
        }
        name
    }
}

/// The file with this inode number somewhere under `root`, other than under `skip`. Depth first, bounded in entries and time.
fn find_inode(root: &Path, ino: u64, skip: &[PathBuf]) -> Option<PathBuf> {
    let started = Instant::now();
    let mut seen = 0usize;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if skip.contains(&dir) {
            continue;
        }
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            seen += 1;
            if seen > WALK_LIMIT || started.elapsed().as_secs() > WALK_SECS {
                return None;
            }
            let Ok(m) = e.metadata() else { continue };
            if m.ino() == ino {
                return Some(e.path());
            }
            if m.is_dir() {
                stack.push(e.path());
            }
        }
    }
    None
}

fn load_seen(path: &Path) -> HashSet<String> {
    fs::read_to_string(path)
        .map(|t| t.lines().map(str::to_owned).collect())
        .unwrap_or_default()
}

/// Copies what `report` says e2fsck touched, from the mounted file system at `data`, into
/// `<data>/lucina/repaired/<stamp>/`. Nothing is created when there is nothing to copy.
pub fn copy_repairs(data: &Path, report: &Report, stamp: &str, cap: u64) -> io::Result<Outcome> {
    let repaired = data.join("lucina/repaired");
    let lost = data.join("lost+found");
    let seen_path = repaired.join("seen-lost-found");
    let mut seen = load_seen(&seen_path);
    // New entries of lost+found: key = name, inode, size, mtime.
    let mut fresh: Vec<(PathBuf, String, u64)> = Vec::new();
    if let Ok(rd) = fs::read_dir(&lost) {
        for e in rd.flatten() {
            let Ok(m) = e.metadata() else { continue };
            let key = format!(
                "{} {} {} {}",
                e.file_name().to_string_lossy(),
                m.ino(),
                m.len(),
                m.mtime()
            );
            if !seen.contains(&key) {
                fresh.push((e.path(), key, m.ino()));
            }
        }
    }
    fresh.sort();
    let others: Vec<&Finding> = report
        .findings
        .iter()
        .filter(|f| matches!(f.kind, Kind::Truncated | Kind::Cleared))
        .collect();
    if fresh.is_empty() && others.is_empty() {
        return Ok(Outcome::default());
    }
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&repaired)?;
    let mut dir = repaired.join(stamp);
    let mut n = 1;
    while dir.exists() {
        n += 1;
        dir = repaired.join(format!("{stamp}-{n}"));
    }
    fs::DirBuilder::new().mode(0o700).create(&dir)?;
    let mut run = Run {
        dir: &dir,
        budget: cap,
        entries: Vec::new(),
        skipped: Vec::new(),
        bytes: 0,
    };
    let said_for = |ino: u64| {
        report
            .findings
            .iter()
            .find(|f| f.ino == ino && f.kind == Kind::LostFound)
            .map(|f| f.said.clone())
    };
    for (path, _, ino) in &fresh {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let said = said_for(*ino)
            .unwrap_or_else(|| "in lost+found, not in e2fsck's output of this boot".to_string());
        run.copy_file(
            path,
            &format!("lost+found/{name}"),
            "lost+found",
            *ino,
            &said,
            None,
        );
    }
    let skip = [repaired.clone(), lost.clone()];
    for f in others {
        let found = match (&f.path, f.kind) {
            (_, Kind::Cleared) => None,
            (Some(p), _) => Some(data.join(p.trim_start_matches('/'))),
            (None, _) => find_inode(data, f.ino, &skip),
        };
        let original = f.path.clone();
        match found {
            Some(p) if p.exists() => run.copy_file(
                &p,
                &original.clone().unwrap_or_else(|| p.display().to_string()),
                f.kind.name(),
                f.ino,
                &f.said,
                original,
            ),
            _ if f.kind == Kind::Cleared => {
                // Nothing left to copy: the manifest keeps what was lost and what e2fsck said.
                run.entries.push(Entry {
                    kind: f.kind.name(),
                    original,
                    ino: f.ino,
                    size: 0,
                    sha: "-".into(),
                    copy: "-".into(),
                    said: f.said.replace('\t', " "),
                });
            }
            _ => run.skip(
                &format!("inode {}", f.ino),
                "not found on the disk after the check",
            ),
        }
    }
    let mut manifest = String::new();
    for e in &run.entries {
        manifest.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            e.kind,
            e.original.as_deref().unwrap_or("-"),
            e.ino,
            e.size,
            e.sha,
            e.copy,
            e.said
        ));
    }
    for s in &run.skipped {
        manifest.push_str(&format!(
            "skipped\t-\t-\t-\t-\t-\t{}\n",
            s.replace(['\t', '\n'], " ")
        ));
    }
    write_atomic(&dir.join("MANIFEST"), manifest.as_bytes())?;
    // The entries are handled whether they were copied or skipped: do not copy them again next boot.
    for (_, key, _) in &fresh {
        seen.insert(key.clone());
    }
    let mut keys: Vec<&String> = seen.iter().collect();
    keys.sort();
    let text: String = keys.iter().map(|k| format!("{k}\n")).collect();
    write_atomic(&seen_path, text.as_bytes())?;
    let copied = run.entries.iter().filter(|e| e.copy != "-").count();
    let (bytes, skipped) = (run.bytes, run.skipped);
    Ok(Outcome {
        dir: Some(dir),
        copied,
        bytes,
        skipped,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsck;

    fn scratch(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("lucina-repair-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(p.join("lost+found")).unwrap();
        fs::create_dir_all(p.join("lucina")).unwrap();
        p
    }

    #[test]
    fn the_stamp_is_a_utc_date_and_says_when_the_clock_was_unset() {
        assert_eq!(timestamp(1_794_000_000), "20261106T212000Z");
        assert_eq!(timestamp(86_400 * 3 + 5), "19700104T000005Z-clock-unset");
        assert_eq!(timestamp(1_709_164_800), "20240229T000000Z");
    }

    #[test]
    fn a_lost_found_entry_is_copied_with_its_hash_and_what_e2fsck_said() {
        let data = scratch("lf");
        fs::write(data.join("lost+found/#13"), b"hello repaired").unwrap();
        let ino = fs::metadata(data.join("lost+found/#13")).unwrap().ino();
        let text = format!(
            "Pass 4: Checking reference counts\nUnattached inode {ino}\nConnect to /lost+found? yes\n\n"
        );
        let report = fsck::parse(&text, 1);
        let out = copy_repairs(&data, &report, "20261111T120000Z", CAP_BYTES).unwrap();
        assert_eq!(out.copied, 1);
        let dir = out.dir.unwrap();
        assert_eq!(dir, data.join("lucina/repaired/20261111T120000Z"));
        let copy = fs::read(dir.join(format!("{ino}-#13"))).unwrap();
        assert_eq!(copy, b"hello repaired");
        let manifest = fs::read_to_string(dir.join("MANIFEST")).unwrap();
        let sha = hex(&Sha256::digest(b"hello repaired"));
        assert!(manifest.contains(&sha), "{manifest}");
        assert!(
            manifest.contains(&format!("lost+found\t-\t{ino}\t14\t")),
            "{manifest}"
        );
        assert!(
            manifest.contains("Connect to /lost+found? yes"),
            "{manifest}"
        );
        // The original stays where e2fsck put it.
        assert!(data.join("lost+found/#13").exists());
        // The same entry is not copied twice.
        let again = copy_repairs(&data, &report, "20261111T120100Z", CAP_BYTES).unwrap();
        assert_eq!(again, Outcome::default());
        fs::remove_dir_all(&data).unwrap();
    }

    #[test]
    fn a_truncated_file_is_found_by_the_path_or_by_its_inode_number() {
        let data = scratch("cut");
        fs::create_dir_all(data.join("files")).unwrap();
        fs::write(data.join("files/a.txt"), b"short").unwrap();
        fs::write(data.join("files/b.txt"), b"other").unwrap();
        let (ia, ib) = (
            fs::metadata(data.join("files/a.txt")).unwrap().ino(),
            fs::metadata(data.join("files/b.txt")).unwrap().ino(),
        );
        let text = format!(
            "Pass 1: Checking inodes, blocks, and sizes\nInode {ia} has illegal block(s).  Clear? yes\n\nIllegal block #0 (999) in inode {ia}.  CLEARED.\nInode {ib}, i_size is 1, should be 5.  Fix? yes\n\n"
        );
        let report = fsck::parse(&text, 1);
        let out = copy_repairs(&data, &report, "s", CAP_BYTES).unwrap();
        assert_eq!(out.copied, 2, "{:?}", out.skipped);
        let manifest = fs::read_to_string(out.dir.unwrap().join("MANIFEST")).unwrap();
        assert!(manifest.contains("truncated"), "{manifest}");
        assert!(manifest.contains("5\t"), "{manifest}");
        fs::remove_dir_all(&data).unwrap();
    }

    #[test]
    fn a_cleared_entry_is_listed_even_with_nothing_to_copy() {
        let data = scratch("gone");
        let report = fsck::parse(
            "Entry 'd' in / (2) has deleted/unused inode 12.  Clear? yes\n",
            1,
        );
        let out = copy_repairs(&data, &report, "s", CAP_BYTES).unwrap();
        assert_eq!(out.copied, 0);
        let manifest = fs::read_to_string(out.dir.unwrap().join("MANIFEST")).unwrap();
        assert!(
            manifest.starts_with("cleared\t/d\t12\t0\t-\t-\t"),
            "{manifest}"
        );
        fs::remove_dir_all(&data).unwrap();
    }

    #[test]
    fn what_passes_the_cap_is_skipped_and_logged() {
        let data = scratch("cap");
        fs::write(data.join("lost+found/#20"), vec![1u8; 100]).unwrap();
        fs::write(data.join("lost+found/#21"), vec![2u8; 100]).unwrap();
        let out = copy_repairs(&data, &fsck::parse("", 1), "s", 150).unwrap();
        assert_eq!(out.copied, 1);
        assert_eq!(out.skipped.len(), 1);
        assert!(out.skipped[0].contains("cap"), "{:?}", out.skipped);
        let manifest = fs::read_to_string(out.dir.unwrap().join("MANIFEST")).unwrap();
        assert!(manifest.contains("skipped\t"), "{manifest}");
        fs::remove_dir_all(&data).unwrap();
    }

    #[test]
    fn a_clean_lost_found_makes_no_folder() {
        let data = scratch("none");
        let out = copy_repairs(&data, &fsck::parse("", 0), "s", CAP_BYTES).unwrap();
        assert_eq!(out, Outcome::default());
        assert!(!data.join("lucina/repaired").exists());
        fs::remove_dir_all(&data).unwrap();
    }

    #[test]
    fn names_from_e2fsck_cannot_escape_the_folder() {
        assert_eq!(safe_name("/a/../../etc/passwd"), "passwd");
        assert_eq!(safe_name("/a/b c"), "b_c");
        assert_eq!(safe_name("/.hidden"), "_.hidden");
        assert_eq!(safe_name(".."), "_..");
    }
}
