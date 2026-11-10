//! The log on /data. Two chains of append-only JSON-line segments: `important-*` for critical events and errors, `routine-*`
//! for the rest, each with its own size and age limit, so that a chatty day of warnings never pushes the record of a crash
//! out (in Windows a full System log overwrites its oldest events, crashes included). Records are numbered from 1 across
//! both chains and never reused. A source's key (`pstore:boot17:seq42`) is written once, so reading the same saved crash at
//! every boot adds nothing.
//!
//! Durability (docs/research/juno-event-log.md, and memory juno-ext4-dir-fsync): each append is fsynced; a new segment's
//! directory is fsynced too, because Juno's ext4 keeps a new file's name only after its directory is synced; `meta.json` is
//! written to a temporary file, fsynced, renamed and its directory fsynced. Writers take an exclusive `flock` on `.lock`.

use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::event::{Channel, Event, Level};
use crate::providers;
use crate::rules;

pub const DEFAULT_DIR: &str = "/data/lucina/events";

/// Size and age limits of one chain.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub segment_bytes: u64,
    pub total_bytes: u64,
    /// Segments whose newest event is older than this go, whatever the size. 0 keeps them.
    pub max_age_days: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct Retention {
    pub important: Limits,
    pub routine: Limits,
}

impl Default for Retention {
    fn default() -> Self {
        Retention {
            // About 2000 crash reports with their log lines: years of them.
            important: Limits {
                segment_bytes: 512 * 1024,
                total_bytes: 16 * 1024 * 1024,
                max_age_days: 0,
            },
            routine: Limits {
                segment_bytes: 256 * 1024,
                total_bytes: 4 * 1024 * 1024,
                max_age_days: 90,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Chain {
    Important,
    Routine,
}

impl Chain {
    fn of(event: &Event) -> Chain {
        let cleared =
            event.provider == providers::EVENTLOG && event.id == providers::id::LOG_CLEARED;
        if event.level <= Level::Error || cleared {
            Chain::Important
        } else {
            Chain::Routine
        }
    }

    fn prefix(self) -> &'static str {
        match self {
            Chain::Important => "important-",
            Chain::Routine => "routine-",
        }
    }
}

/// What the log has dropped to stay within its limits, kept in `meta.json` and shown by readers ("812 routine events dropped").
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Meta {
    #[serde(default)]
    pub dropped_important: u64,
    #[serde(default)]
    pub dropped_routine: u64,
    /// When someone last cleared the log, unix ms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleared_ms: Option<i64>,
}

/// What an append did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Added {
    /// Written with this record number.
    New(u64),
    /// A source key already in the log: nothing written.
    Known(u64),
}

/// A held lock and the log's state, read once.
pub struct Store {
    dir: PathBuf,
    retention: Retention,
    _lock: File,
    next_record: u64,
    keys: BTreeMap<String, u64>,
    meta: Meta,
}

fn segments(dir: &Path, chain: Chain) -> io::Result<Vec<(u64, PathBuf)>> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(rest) = name.strip_prefix(chain.prefix()) else {
            continue;
        };
        let Some(num) = rest
            .strip_suffix(".jsonl")
            .and_then(|n| n.parse::<u64>().ok())
        else {
            continue;
        };
        out.push((num, entry.path()));
    }
    out.sort();
    Ok(out)
}

/// Every event of a segment; a line that does not parse (cut by a power loss mid-write) is skipped.
fn read_segment(path: &Path) -> Vec<Event> {
    let Ok(file) = File::open(path) else {
        return Vec::new();
    };
    BufReader::new(file)
        .split(b'\n')
        .filter_map(|line| line.ok())
        .filter_map(|line| serde_json::from_slice::<Event>(&line).ok())
        .collect()
}

pub fn fsync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

/// Creates `dir` and every missing parent, syncing each parent whose entry is new.
fn create_dirs(dir: &Path) -> io::Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    if let Some(parent) = dir.parent() {
        create_dirs(parent)?;
    }
    match fs::create_dir(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Ok(()),
        Err(e) => return Err(e),
    }
    if let Some(parent) = dir.parent() {
        fsync_dir(parent)?;
    }
    Ok(())
}

/// Writes `bytes` to `path` so that a power cut leaves either the old file or the new one.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let tmp = dir.join(format!(
        ".{}.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("file")
    ));
    {
        let mut f = File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    fsync_dir(dir)
}

impl Store {
    /// Opens (creating) the log in `dir` for writing, waiting for other writers.
    pub fn open(dir: &Path, retention: Retention) -> io::Result<Store> {
        create_dirs(dir)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join(".lock"))?;
        // SAFETY: flock on a descriptor this function owns; no memory is involved.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let meta = fs::read(dir.join("meta.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let mut store = Store {
            dir: dir.to_path_buf(),
            retention,
            _lock: lock,
            next_record: 1,
            keys: BTreeMap::new(),
            meta,
        };
        for event in read_dir_events(dir) {
            store.next_record = store.next_record.max(event.record + 1);
            if let Some(cleared) = event.fields.get("cleared_keys").and_then(|v| v.as_array()) {
                for key in cleared.iter().filter_map(|k| k.as_str()) {
                    store.keys.insert(key.to_string(), 0);
                }
            }
            if let Some(key) = event.key {
                store.keys.insert(key, event.record);
            }
        }
        Ok(store)
    }

    pub fn meta(&self) -> &Meta {
        &self.meta
    }

    pub fn has_key(&self, key: &str) -> Option<u64> {
        self.keys.get(key).copied()
    }

    /// Writes one event, numbering it, unless its key is already in the log. `time_ms` is now, for retention.
    pub fn append(&mut self, mut event: Event, now_ms: i64) -> io::Result<Added> {
        if let Some(record) = event.key.as_deref().and_then(|k| self.has_key(k)) {
            return Ok(Added::Known(record));
        }
        event.record = self.next_record;
        let chain = Chain::of(&event);
        let mut line = serde_json::to_vec(&event).map_err(io::Error::other)?;
        line.push(b'\n');
        let limits = self.limits(chain);
        let segs = segments(&self.dir, chain)?;
        let (path, new_file) = match segs.last() {
            Some((_, path))
                if fs::metadata(path)
                    .map(|m| m.len() + line.len() as u64 <= limits.segment_bytes)
                    .unwrap_or(false) =>
            {
                (path.clone(), false)
            }
            last => {
                let num = last.map_or(1, |(n, _)| n + 1);
                (
                    self.dir.join(format!("{}{num:06}.jsonl", chain.prefix())),
                    true,
                )
            }
        };
        {
            let mut f = OpenOptions::new().create(true).append(true).open(&path)?;
            f.write_all(&line)?;
            f.sync_all()?;
        }
        if new_file {
            fsync_dir(&self.dir)?;
        }
        if let Some(key) = event.key.take() {
            self.keys.insert(key, event.record);
        }
        self.next_record += 1;
        self.enforce(chain, now_ms)?;
        Ok(Added::New(event.record))
    }

    fn limits(&self, chain: Chain) -> Limits {
        match chain {
            Chain::Important => self.retention.important,
            Chain::Routine => self.retention.routine,
        }
    }

    /// Drops the oldest segments of a chain until it fits, and segments past its age. Never the newest segment.
    fn enforce(&mut self, chain: Chain, now_ms: i64) -> io::Result<()> {
        let limits = self.limits(chain);
        let segs = segments(&self.dir, chain)?;
        if segs.is_empty() {
            return Ok(());
        }
        let sizes: Vec<u64> = segs
            .iter()
            .map(|(_, p)| fs::metadata(p).map(|m| m.len()).unwrap_or(0))
            .collect();
        let total: u64 = sizes.iter().sum();
        // The leading segments past their age: each is judged by its newest event.
        let mut too_old = 0;
        if limits.max_age_days > 0 {
            for (_, p) in &segs {
                let newest_ms = read_segment(p).iter().filter_map(|e| e.time_ms).max();
                if newest_ms
                    .is_some_and(|t| now_ms - t > i64::from(limits.max_age_days) * 86_400_000)
                {
                    too_old += 1;
                } else {
                    break;
                }
            }
        }
        // `rules::segments_to_drop` (Verus-checked): the fewest that fit, the old ones first, never the newest.
        let n = rules::segments_to_drop(&sizes, total, limits.total_bytes, too_old);
        let mut dropped = 0u64;
        for (_, p) in &segs[..n] {
            dropped += read_segment(p).len() as u64;
            fs::remove_file(p)?;
        }
        if dropped > 0 {
            fsync_dir(&self.dir)?;
            match chain {
                Chain::Important => self.meta.dropped_important += dropped,
                Chain::Routine => self.meta.dropped_routine += dropped,
            }
            self.save_meta()?;
        }
        Ok(())
    }

    fn save_meta(&self) -> io::Result<()> {
        write_atomic(
            &self.dir.join("meta.json"),
            &serde_json::to_vec(&self.meta).map_err(io::Error::other)?,
        )
    }

    /// Forgets every event (Windows' "Clear log") and writes one event that says so (EventLog 104). That event carries the
    /// cleared sources' keys, so a saved crash still in RAM is not read in again, and its record number keeps the count going.
    pub fn clear(&mut self, now_ms: i64, who: &str) -> io::Result<u64> {
        let keys: Vec<String> = self.keys.keys().cloned().collect();
        let mut gone = 0u64;
        for chain in [Chain::Important, Chain::Routine] {
            for (_, path) in segments(&self.dir, chain)? {
                gone += read_segment(&path).len() as u64;
                fs::remove_file(path)?;
            }
        }
        fsync_dir(&self.dir)?;
        self.meta.cleared_ms = Some(now_ms);
        self.save_meta()?;
        let mut e = Event::new(
            providers::EVENTLOG,
            providers::id::LOG_CLEARED,
            Level::Info,
            Channel::System,
            format!("The event log was cleared by {who}: {gone} events removed."),
        )
        .field("removed", gone)
        .field("by", who)
        .field("cleared_keys", keys);
        e.time_ms = Some(now_ms);
        match self.append(e, now_ms)? {
            Added::New(r) | Added::Known(r) => Ok(r),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

/// Every event in the log, oldest record first, without taking the lock (a reader never blocks a writer; a line being
/// written is skipped).
pub fn read_dir_events(dir: &Path) -> Vec<Event> {
    let mut events = Vec::new();
    for chain in [Chain::Important, Chain::Routine] {
        if let Ok(segs) = segments(dir, chain) {
            for (_, path) in segs {
                events.extend(read_segment(&path));
            }
        }
    }
    events.sort_by_key(|e| e.record);
    fill_times(&mut events);
    events
}

/// Gives an event without a time one, when its boot's start became known later (a boot-started event written by a later
/// ingest or `backfill-boot`): the start plus the event's uptime, marked estimated. The log is append-only, so what was read
/// in before the time was known is completed when it is read.
pub fn fill_times(events: &mut [Event]) {
    let starts: Vec<(u32, i64)> = events
        .iter()
        .filter(|e| e.provider == providers::KERNEL_POWER && e.id == providers::id::BOOT_STARTED)
        .filter_map(|e| Some((e.boot?, e.time_ms?)))
        .collect();
    for e in events.iter_mut().filter(|e| e.time_ms.is_none()) {
        let (Some(b), Some(up)) = (e.boot, e.uptime_ms) else {
            continue;
        };
        if let Some(&(_, start)) = starts.iter().rev().find(|(sb, _)| *sb == b) {
            e.time_ms = Some(start + up as i64);
            e.time_estimated = true;
        }
    }
}

pub fn read_meta(dir: &Path) -> Meta {
    fs::read(dir.join("meta.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// A reader's question: Event Viewer's filter, as one small struct.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Filter {
    /// At most this level (Critical = 1 is the most severe): `Some(Warning)` keeps critical, error and warning.
    #[serde(default)]
    pub max_level: Option<Level>,
    #[serde(default)]
    pub providers: Vec<String>,
    #[serde(default)]
    pub ids: Vec<u32>,
    #[serde(default)]
    pub boot: Option<u32>,
    #[serde(default)]
    pub since_ms: Option<i64>,
    #[serde(default)]
    pub until_ms: Option<i64>,
    #[serde(default)]
    pub after_record: Option<u64>,
    #[serde(default)]
    pub bucket: Option<String>,
    /// Lowercase words that must all appear in the summary, cause or provider.
    #[serde(default)]
    pub text: Option<String>,
}

impl Filter {
    pub fn matches(&self, e: &Event) -> bool {
        if self.max_level.is_some_and(|l| e.level > l) {
            return false;
        }
        if !self.providers.is_empty()
            && !self
                .providers
                .iter()
                .any(|p| p.eq_ignore_ascii_case(&e.provider))
        {
            return false;
        }
        if !self.ids.is_empty() && !self.ids.contains(&e.id) {
            return false;
        }
        if self.boot.is_some() && self.boot != e.boot {
            return false;
        }
        if let Some(since) = self.since_ms {
            if e.time_ms.is_none_or(|t| t < since) {
                return false;
            }
        }
        if let Some(until) = self.until_ms {
            if e.time_ms.is_none_or(|t| t >= until) {
                return false;
            }
        }
        if self.after_record.is_some_and(|r| e.record <= r) {
            return false;
        }
        if let Some(b) = &self.bucket {
            if e.bucket.as_ref().is_none_or(|x| &x.id != b) {
                return false;
            }
        }
        if let Some(text) = &self.text {
            let hay = format!(
                "{} {} {}",
                e.summary,
                e.cause.as_ref().map_or("", |c| c.text.as_str()),
                e.provider
            )
            .to_lowercase();
            if !text
                .to_lowercase()
                .split_whitespace()
                .all(|w| hay.contains(w))
            {
                return false;
            }
        }
        true
    }
}

/// Keys already used in a list of events, for callers that read without a store.
pub fn keys_of(events: &[Event]) -> HashSet<&str> {
    events.iter().filter_map(|e| e.key.as_deref()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Channel;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("evlog-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn ev(level: Level, key: Option<&str>, t: i64) -> Event {
        let mut e = Event::new("Juno-Test", 1, level, Channel::System, "x".repeat(200));
        e.key = key.map(str::to_string);
        e.time_ms = Some(t);
        e
    }

    #[test]
    fn numbers_and_keys_survive_reopening() {
        let dir = tmp("keys");
        {
            let mut s = Store::open(&dir, Retention::default()).unwrap();
            assert_eq!(
                s.append(ev(Level::Critical, Some("pstore:boot1:seq1"), 0), 0)
                    .unwrap(),
                Added::New(1)
            );
            assert_eq!(
                s.append(ev(Level::Info, None, 0), 0).unwrap(),
                Added::New(2)
            );
        }
        let mut s = Store::open(&dir, Retention::default()).unwrap();
        assert_eq!(
            s.append(ev(Level::Critical, Some("pstore:boot1:seq1"), 0), 0)
                .unwrap(),
            Added::Known(1)
        );
        assert_eq!(
            s.append(ev(Level::Warning, None, 0), 0).unwrap(),
            Added::New(3)
        );
        let all = read_dir_events(&dir);
        assert_eq!(
            all.iter().map(|e| e.record).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn routine_noise_never_pushes_out_a_crash() {
        let dir = tmp("noise");
        let small = Limits {
            segment_bytes: 1024,
            total_bytes: 4096,
            max_age_days: 0,
        };
        let r = Retention {
            important: small,
            routine: small,
        };
        let mut s = Store::open(&dir, r).unwrap();
        s.append(ev(Level::Critical, Some("crash"), 0), 0).unwrap();
        for _ in 0..200 {
            s.append(ev(Level::Warning, None, 0), 0).unwrap();
        }
        let all = read_dir_events(&dir);
        assert!(all.iter().any(|e| e.key.as_deref() == Some("crash")));
        assert!(all.len() < 60, "{}", all.len());
        assert!(read_meta(&dir).dropped_routine > 100);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn old_routine_segments_age_out() {
        let dir = tmp("age");
        let r = Retention {
            important: Retention::default().important,
            routine: Limits {
                segment_bytes: 300,
                total_bytes: 1 << 20,
                max_age_days: 1,
            },
        };
        let mut s = Store::open(&dir, r).unwrap();
        s.append(ev(Level::Info, None, 0), 0).unwrap();
        s.append(ev(Level::Info, None, 3 * 86_400_000), 3 * 86_400_000)
            .unwrap();
        let all = read_dir_events(&dir);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].record, 2);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_torn_last_line_is_skipped() {
        let dir = tmp("torn");
        let mut s = Store::open(&dir, Retention::default()).unwrap();
        s.append(ev(Level::Info, None, 0), 0).unwrap();
        drop(s);
        let seg = segments(&dir, Chain::Routine).unwrap()[0].1.clone();
        let mut f = OpenOptions::new().append(true).open(&seg).unwrap();
        f.write_all(b"{\"record\":2,\"provi").unwrap();
        drop(f);
        assert_eq!(read_dir_events(&dir).len(), 1);
        let mut s = Store::open(&dir, Retention::default()).unwrap();
        assert_eq!(
            s.append(ev(Level::Info, None, 0), 0).unwrap(),
            Added::New(2)
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn clearing_keeps_numbers_and_keys() {
        let dir = tmp("clear");
        let mut s = Store::open(&dir, Retention::default()).unwrap();
        s.append(ev(Level::Critical, Some("k"), 0), 0).unwrap();
        assert_eq!(s.clear(5, "test").unwrap(), 2);
        let left = read_dir_events(&dir);
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].id, providers::id::LOG_CLEARED);
        assert_eq!(read_meta(&dir).cleared_ms, Some(5));
        drop(s);
        let mut s = Store::open(&dir, Retention::default()).unwrap();
        assert!(matches!(
            s.append(ev(Level::Critical, Some("k"), 0), 0).unwrap(),
            Added::Known(_)
        ));
        assert_eq!(
            s.append(ev(Level::Info, None, 0), 0).unwrap(),
            Added::New(3)
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_boot_start_learned_later_dates_older_events() {
        let dir = tmp("fill");
        let mut s = Store::open(&dir, Retention::default()).unwrap();
        let mut crash = Event::new(
            providers::CRASH,
            providers::id::KERNEL_CRASH,
            Level::Critical,
            Channel::System,
            "x",
        );
        crash.boot = Some(11);
        crash.uptime_ms = Some(5_491_524);
        s.append(crash, 0).unwrap();
        assert_eq!(read_dir_events(&dir)[0].time_ms, None);
        let mut start = Event::new(
            providers::KERNEL_POWER,
            providers::id::BOOT_STARTED,
            Level::Info,
            Channel::System,
            "b",
        );
        start.boot = Some(11);
        start.time_ms = Some(1_000_000);
        s.append(start, 0).unwrap();
        let e = read_dir_events(&dir)
            .into_iter()
            .find(|e| e.id == providers::id::KERNEL_CRASH)
            .unwrap();
        assert_eq!(e.time_ms, Some(1_000_000 + 5_491_524));
        assert!(e.time_estimated);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn filters() {
        let mut e = ev(Level::Warning, None, 1000);
        e.boot = Some(7);
        let f = Filter {
            max_level: Some(Level::Error),
            ..Default::default()
        };
        assert!(!f.matches(&e));
        let f = Filter {
            max_level: Some(Level::Warning),
            boot: Some(7),
            since_ms: Some(1000),
            text: Some("XX".into()),
            ..Default::default()
        };
        assert!(f.matches(&e));
        let f = Filter {
            until_ms: Some(1000),
            ..Default::default()
        };
        assert!(!f.matches(&e));
    }
}
