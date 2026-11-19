//! The full text of each saved record, kept beside the event that summarises it. The saved log holds 8 records and the event log
//! keeps its events for years; the event has a summary and 60 log lines, and the rest (about 43 KiB: registers, the whole kernel
//! log tail, the register trace) was lost with the region on 2026-11-11. One file per record under `records/` in the event log's
//! directory, named by the event's key. The files together stay within [`CAP_BYTES`]: the oldest whole file goes first, and the
//! event's `text_file` then points at nothing (readers say so).

use std::fs;
use std::io;
use std::path::Path;

use crate::store::{fsync_dir, write_atomic};

/// The directory, under the event log's.
pub const DIR: &str = "records";
/// All the files together.
pub const CAP_BYTES: u64 = 4 * 1024 * 1024;

/// A file name for a key: `pstore:e1:boot2:seq4` is `pstore_e1_boot2_seq4.txt`. Anything but letters, digits, `-` and `_` becomes `_`.
pub fn file_name(key: &str) -> String {
    let stem: String = key
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{stem}.txt")
}

/// Writes the record's text (to a temporary file, then renamed), drops the oldest files beyond [`CAP_BYTES`] (never this one),
/// and returns the path to put in the event, relative to the event log's directory.
pub fn keep(dir: &Path, key: &str, text: &[u8]) -> io::Result<String> {
    keep_within(dir, key, text, CAP_BYTES)
}

pub fn keep_within(dir: &Path, key: &str, text: &[u8], cap: u64) -> io::Result<String> {
    let records = dir.join(DIR);
    fs::create_dir_all(&records)?;
    let name = file_name(key);
    write_atomic(&records.join(&name), text)?;
    fsync_dir(&records)?;
    drop_oldest(&records, &name, cap)?;
    Ok(format!("{DIR}/{name}"))
}

/// Removes whole files, oldest first (by modification time, then name), until the total is within `cap`. `keep` stays.
fn drop_oldest(records: &Path, keep: &str, cap: u64) -> io::Result<()> {
    let mut files = Vec::new();
    for entry in fs::read_dir(records)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".txt") {
            continue;
        }
        let meta = entry.metadata()?;
        files.push((meta.modified().ok(), name, meta.len()));
    }
    let mut total: u64 = files.iter().map(|f| f.2).sum();
    files.sort();
    let mut removed = false;
    for (_, name, len) in &files {
        if total <= cap {
            break;
        }
        if name == keep {
            continue;
        }
        fs::remove_file(records.join(name))?;
        total -= len;
        removed = true;
    }
    if removed {
        fsync_dir(records)?;
    }
    Ok(())
}

/// The text an event's `text_file` names, if the file is still there. Only names this module made are read.
pub fn read(dir: &Path, text_file: &str) -> Option<Vec<u8>> {
    let name = text_file.strip_prefix(DIR)?.strip_prefix('/')?;
    if name.is_empty() || name.contains('/') || name.contains("..") {
        return None;
    }
    fs::read(dir.join(DIR).join(name)).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    struct Dir {
        path: std::path::PathBuf,
    }

    impl std::ops::Deref for Dir {
        type Target = Path;

        fn deref(&self) -> &Self::Target {
            &self.path
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn dir(name: &str) -> Dir {
        let path = std::env::temp_dir().join(format!("evlog-rt-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Dir { path }
    }

    fn age(dir: &Path, rel: &str, secs_ago: u64) {
        let f = fs::OpenOptions::new()
            .write(true)
            .open(dir.join(rel))
            .unwrap();
        f.set_modified(SystemTime::now() - Duration::from_secs(secs_ago))
            .unwrap();
    }

    #[test]
    fn a_record_is_kept_under_its_key() {
        let d = dir("keep");
        let rel = keep(&d, "pstore:e1:boot2:seq4", b"full text").unwrap();
        assert_eq!(rel, "records/pstore_e1_boot2_seq4.txt");
        assert_eq!(read(&d, &rel).unwrap(), b"full text");
        // Keys of different epochs name different files.
        assert_ne!(
            file_name("pstore:boot2:seq4"),
            file_name("pstore:e1:boot2:seq4")
        );
    }

    #[test]
    fn only_our_names_are_read() {
        let d = dir("names");
        fs::write(d.join("secret.txt"), b"x").unwrap();
        for bad in [
            "../secret.txt",
            "records/../secret.txt",
            "records/",
            "secret.txt",
            "records/a/b.txt",
        ] {
            assert_eq!(read(&d, bad), None, "{bad}");
        }
    }

    #[test]
    fn the_oldest_whole_files_go_first_beyond_the_cap() {
        let d = dir("cap");
        let text = vec![b'x'; 1000];
        for (i, key) in ["a", "b", "c"].iter().enumerate() {
            let rel = keep_within(&d, key, &text, 3500).unwrap();
            age(&d, &rel, 100 - i as u64 * 10);
        }
        assert!(read(&d, "records/a.txt").is_some());
        // The fourth makes 4000 bytes: "a", the oldest, goes whole; the rest stay whole.
        keep_within(&d, "d", &text, 3500).unwrap();
        assert!(read(&d, "records/a.txt").is_none());
        for n in ["b", "c", "d"] {
            assert_eq!(
                read(&d, &format!("records/{n}.txt")).unwrap().len(),
                1000,
                "{n}"
            );
        }
        // One bigger than the cap stays alone: the newest is never the one dropped.
        keep_within(&d, "big", &vec![b'y'; 5000], 3500).unwrap();
        assert_eq!(read(&d, "records/big.txt").unwrap().len(), 5000);
        assert!(read(&d, "records/d.txt").is_none());
    }

    #[test]
    fn the_real_cap_is_4_mib() {
        assert_eq!(CAP_BYTES, 4 * 1024 * 1024);
    }
}
