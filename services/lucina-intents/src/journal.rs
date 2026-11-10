// Lucina's undo journal: every write she makes on the laptop, one JSON line per action in `journal.jsonl`, append only, so
// it can be undone. Rotated journals (`journal.jsonl.1`, `.2`, `.3`) are history only; old file contents live in `blobs/`.
// Before any line is appended the journal on disk is checked with [`rules::journal_ok`] and the new line with
// [`rules::may_append`], so a corrupt or out-of-order journal is an Err and nothing is written. Folders are 0700, files 0600,
// everything opened with `O_NOFOLLOW`.

use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write as IoWrite};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{rules, spec};

/// How long a write can be undone: 7 days, in milliseconds.
pub const KEEP_MS: u64 = 7 * 24 * 3600 * 1000;
/// Rotate `journal.jsonl` once it reaches this many bytes.
pub const ROTATE_AT: u64 = 1 << 20;
/// All blobs together are kept under this (2 GiB on the laptop).
pub const MAX_BLOB_BYTES: u64 = 2 << 30;

const JOURNAL: &str = "journal.jsonl";

/// One line of the journal: a write (with what it wrote, when that can be undone) or an undo of an earlier write.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Line {
    pub seq: u64,
    pub ts: u64,
    pub action: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub call: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub target: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub intent: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub effect: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write: Option<spec::Write>,
    #[serde(default)]
    pub undoable: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub undoes: String,
    #[serde(default)]
    pub undoes_seq: u64,
    #[serde(default)]
    pub ok: bool,
}

impl Line {
    fn record(&self) -> rules::Record {
        rules::Record { seq: self.seq, undoes: self.undoes_seq, undoable: self.undoable }
    }
}

/// The journal in one folder: `journal.jsonl` plus the rotated ones and `blobs/`.
pub struct Journal {
    dir: PathBuf,
}

fn sha256_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let digest = Sha256::digest(bytes);
    let mut s = String::with_capacity(64);
    for b in digest {
        let _ = write!(s, "{b:02x}");
    }
    s
}

fn open_nofollow(path: &Path, read: bool, write: bool, create: bool, append: bool) -> std::io::Result<fs::File> {
    let mut o = OpenOptions::new();
    o.read(read).write(write).create(create).append(append).mode(0o600).custom_flags(libc::O_NOFOLLOW);
    o.open(path)
}

fn io_err(path: &Path, e: std::io::Error) -> String {
    format!("{}: {e}", path.display())
}

impl Journal {
    pub fn at(dir: &Path) -> Journal {
        Journal { dir: dir.to_path_buf() }
    }

    fn journal_path(&self) -> PathBuf {
        self.dir.join(JOURNAL)
    }

    fn ensure_dirs(&self) -> Result<(), String> {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.dir)
            .map_err(|e| format!("{}: {e}", self.dir.display()))?;
        fs::set_permissions(&self.dir, fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("{}: {e}", self.dir.display()))?;
        let blobs = self.dir.join("blobs");
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&blobs)
            .map_err(|e| format!("{}: {e}", blobs.display()))?;
        fs::set_permissions(&blobs, fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("{}: {e}", blobs.display()))?;
        Ok(())
    }

    /// Every line of the current journal, checked: a parse error names its line, a failed [`rules::journal_ok`] is an Err.
    pub fn lines(&self) -> Result<Vec<Line>, String> {
        let path = self.journal_path();
        let file = match open_nofollow(&path, true, false, false, false) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(io_err(&path, e)),
        };
        let mut out = Vec::new();
        for (i, l) in BufReader::new(file).lines().enumerate() {
            let l = l.map_err(|e| format!("{JOURNAL} line {}: {e}", i + 1))?;
            if l.trim().is_empty() {
                continue;
            }
            let line: Line = serde_json::from_str(&l).map_err(|e| format!("{JOURNAL} line {}: {e}", i + 1))?;
            out.push(line);
        }
        let records: Vec<rules::Record> = out.iter().map(Line::record).collect();
        if !rules::journal_ok(&records) {
            return Err(format!("{JOURNAL} is not a valid journal"));
        }
        Ok(out)
    }

    /// The seq a new line gets: the last line's + 1, or 1 for an empty journal; after a rotation the old journal's last seq
    /// (in `journal.jsonl.1`) carries on.
    fn next_seq(&self, lines: &[Line]) -> Result<u64, String> {
        if let Some(last) = lines.last() {
            return Ok(last.seq.saturating_add(1));
        }
        let old = self.dir.join(format!("{JOURNAL}.1"));
        if let Ok(mut f) = open_nofollow(&old, true, false, false, false) {
            let mut text = String::new();
            f.read_to_string(&mut text).map_err(|e| io_err(&old, e))?;
            if let Some(l) = text.lines().rev().find(|l| !l.trim().is_empty()) {
                let line: Line = serde_json::from_str(l)
                    .map_err(|e| format!("{}.1 last line: {e}", JOURNAL))?;
                return Ok(line.seq.saturating_add(1));
            }
        }
        Ok(1)
    }

    /// Refuse the append when the journal on disk or the new record breaks the rules.
    fn check_append(&self, lines: &[Line], rec: rules::Record) -> Result<(), String> {
        let records: Vec<rules::Record> = lines.iter().map(Line::record).collect();
        if !rules::journal_ok(&records) {
            return Err(format!("{JOURNAL} is not a valid journal"));
        }
        if !rules::may_append(&records, rec) {
            return Err(format!("the rules refuse appending seq {}", rec.seq));
        }
        Ok(())
    }

    /// Rotate when the current journal is at [`ROTATE_AT`]: `.2`→`.3`, `.1`→`.2`, current→`.1`, then a new empty one.
    fn maybe_rotate(&self) -> Result<(), String> {
        let cur = self.journal_path();
        if let Ok(m) = fs::symlink_metadata(&cur)
            && m.len() >= ROTATE_AT
        {
            let _ = fs::remove_file(self.dir.join(format!("{JOURNAL}.3")));
            let _ = fs::rename(self.dir.join(format!("{JOURNAL}.2")), self.dir.join(format!("{JOURNAL}.3")));
            let _ = fs::rename(self.dir.join(format!("{JOURNAL}.1")), self.dir.join(format!("{JOURNAL}.2")));
            fs::rename(&cur, self.dir.join(format!("{JOURNAL}.1")))
                .map_err(|e| format!("rotating {JOURNAL}: {e}"))?;
        }
        Ok(())
    }

    fn append_line(&self, line: &Line) -> Result<(), String> {
        self.ensure_dirs()?;
        let text = serde_json::to_string(line).map_err(|e| e.to_string())?;
        let path = self.journal_path();
        let mut f = open_nofollow(&path, false, true, true, true).map_err(|e| io_err(&path, e))?;
        f.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("{JOURNAL}: {e}"))?;
        f.write_all(text.as_bytes()).and_then(|_| f.write_all(b"\n")).map_err(|e| format!("{JOURNAL}: {e}"))?;
        Ok(())
    }

    /// Record one call. `write` is `None` when the call changed nothing undoable; the line is journaled either way.
    pub fn record(
        &self,
        ts_ms: u64,
        call: &str,
        intent: &str,
        effect: rules::Effect,
        write: Option<spec::Write>,
    ) -> Result<Line, String> {
        let undoable = write.as_ref().is_some_and(|w| w.entry().is_ok());
        let lines = self.lines()?;
        let seq = self.next_seq(&lines)?;
        let line = Line {
            seq,
            ts: ts_ms,
            action: format!("act_{seq}_{ts_ms}"),
            call: call.into(),
            target: write.as_ref().map_or(String::new(), |w| w.target.clone()),
            intent: intent.into(),
            effect: spec::effect_name(effect).into(),
            write,
            undoable,
            undoes: String::new(),
            undoes_seq: 0,
            ok: false,
        };
        self.check_append(&lines, line.record())?;
        self.maybe_rotate()?;
        self.append_line(&line)?;
        Ok(line)
    }

    /// The write line of `action` and its rules entry, if it can be undone now.
    pub fn undoable(&self, action: &str, now_ms: u64) -> Result<(Line, rules::Entry), String> {
        let lines = self.lines()?;
        let w = lines
            .iter()
            .find(|l| l.action == action && l.undoes.is_empty())
            .ok_or_else(|| format!("no action {action} in the journal"))?;
        let entry = w
            .write
            .as_ref()
            .and_then(|wr| wr.entry().ok())
            .ok_or_else(|| format!("{action} changed nothing that can be undone"))?;
        if let Some(u) = lines.iter().find(|l| l.undoes == action) {
            return Err(format!("{action} was undone already ({})", u.action));
        }
        if now_ms.saturating_sub(w.ts) > KEEP_MS {
            return Err(format!("{action} is older than 7 days"));
        }
        Ok((w.clone(), entry))
    }

    /// Record that `action` was undone. A failed undo is not recorded, so the write stays undoable.
    pub fn record_undo(&self, ts_ms: u64, action: &str, ok: bool) -> Result<Option<Line>, String> {
        if !ok {
            return Ok(None);
        }
        let (wline, _entry) = self.undoable(action, ts_ms)?;
        let lines = self.lines()?;
        let seq = self.next_seq(&lines)?;
        let line = Line {
            seq,
            ts: ts_ms,
            action: format!("act_{seq}_{ts_ms}"),
            call: String::new(),
            target: String::new(),
            intent: String::new(),
            effect: String::new(),
            write: None,
            undoable: false,
            undoes: action.into(),
            undoes_seq: wline.seq,
            ok: true,
        };
        let records: Vec<rules::Record> = lines.iter().map(Line::record).collect();
        if !rules::journal_ok(&records) {
            return Err(format!("{JOURNAL} is not a valid journal"));
        }
        if !rules::can_undo(&records, wline.seq) || !rules::may_append(&records, line.record()) {
            return Err(format!("the rules refuse undoing {action}"));
        }
        self.maybe_rotate()?;
        self.append_line(&line)?;
        Ok(Some(line))
    }

    /// The last `limit` write lines that can still be undone, newest first.
    pub fn recent(&self, limit: usize, now_ms: u64) -> Result<Vec<Line>, String> {
        let lines = self.lines()?;
        let mut out: Vec<Line> = lines
            .iter()
            .filter(|l| {
                l.undoes.is_empty()
                    && l.write.as_ref().is_some_and(|w| w.entry().is_ok())
                    && !lines.iter().any(|u| u.undoes == l.action)
                    && now_ms.saturating_sub(l.ts) <= KEEP_MS
            })
            .cloned()
            .collect();
        out.reverse();
        out.truncate(limit);
        Ok(out)
    }

    /// Keep a file's old bytes before Lucina changes it: `blobs/<sha256>`, written once.
    pub fn keep_blob(&self, bytes: &[u8]) -> Result<String, String> {
        let hex = sha256_hex(bytes);
        self.ensure_dirs()?;
        let path = self.dir.join("blobs").join(&hex);
        match OpenOptions::new().write(true).create_new(true).mode(0o600).custom_flags(libc::O_NOFOLLOW).open(&path) {
            Ok(mut f) => {
                f.set_permissions(fs::Permissions::from_mode(0o600))
                    .map_err(|e| format!("blobs/{hex}: {e}"))?;
                f.write_all(bytes).map_err(|e| format!("blobs/{hex}: {e}"))?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(format!("blobs/{hex}: {e}")),
        }
        Ok(format!("blob:{hex}"))
    }

    /// The bytes of `blob:<sha256>`; Err if missing or if the content's sha256 does not match the name.
    pub fn blob(&self, name: &str) -> Result<Vec<u8>, String> {
        let hex = name.strip_prefix("blob:").ok_or_else(|| format!("{name}: not a blob name"))?;
        if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(format!("{name}: not a blob name"));
        }
        let path = self.dir.join("blobs").join(hex);
        let mut f = open_nofollow(&path, true, false, false, false).map_err(|e| io_err(&path, e))?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf).map_err(|e| format!("blobs/{hex}: {e}"))?;
        if sha256_hex(&buf) != hex {
            return Err(format!("blob:{hex}: the content does not match its name"));
        }
        Ok(buf)
    }

    /// Retention: drop blobs older than 7 days (by mtime), then the oldest while all blobs together exceed `max_bytes`.
    pub fn prune(&self, now: SystemTime, max_bytes: u64) -> Result<usize, String> {
        let blobs = self.dir.join("blobs");
        let rd = match fs::read_dir(&blobs) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(format!("{}: {e}", blobs.display())),
        };
        let keep = Duration::from_millis(KEEP_MS);
        let mut entries: Vec<(PathBuf, SystemTime, u64)> = Vec::new();
        for e in rd {
            let e = e.map_err(|e| format!("{}: {e}", blobs.display()))?;
            let m = e.metadata().map_err(|err| format!("{}: {err}", e.path().display()))?;
            if !m.is_file() {
                continue;
            }
            let mtime = m.modified().map_err(|err| format!("{}: {err}", e.path().display()))?;
            entries.push((e.path(), mtime, m.len()));
        }
        let mut removed = 0;
        let mut kept: Vec<(PathBuf, SystemTime, u64)> = Vec::new();
        for (p, mtime, len) in entries {
            let old = now.duration_since(mtime).is_ok_and(|age| age > keep);
            if old {
                fs::remove_file(&p).map_err(|e| format!("{}: {e}", p.display()))?;
                removed += 1;
            } else {
                kept.push((p, mtime, len));
            }
        }
        kept.sort_by_key(|(_, mtime, _)| *mtime);
        let mut total: u64 = kept.iter().map(|(_, _, len)| len).sum();
        for (p, _, len) in &kept {
            if total <= max_bytes {
                break;
            }
            fs::remove_file(p).map_err(|e| format!("{}: {e}", p.display()))?;
            total = total.saturating_sub(*len);
            removed += 1;
        }
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lucina-journal-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    fn note_write(id: &str, body: &[u8]) -> spec::Write {
        spec::Write {
            kind: rules::WriteKind::Create,
            target: format!("note:{id}"),
            before: spec::GONE.into(),
            after: spec::there(body).into(),
            undo: spec::Undo::Inverse { intent: "notes.trash".into(), args: json!({"id": id}) },
        }
    }

    #[test]
    fn seq_goes_1_2_3_and_action_ids_carry_it() {
        let j = Journal::at(&tmp("seq"));
        let l1 = j.record(1000, "c1", "notes.create", rules::Effect::Write, Some(note_write("n_1", b"a"))).unwrap();
        let l2 = j.record(2000, "c2", "notes.create", rules::Effect::Write, Some(note_write("n_2", b"b"))).unwrap();
        let l3 = j.record(3000, "c3", "notes.search", rules::Effect::Read, None).unwrap();
        assert_eq!((l1.seq, l2.seq, l3.seq), (1, 2, 3));
        assert_eq!(l1.action, "act_1_1000");
        assert_eq!(l3.action, "act_3_3000");
        assert!(l1.undoable && l2.undoable && !l3.undoable);
        assert_eq!(l3.write, None);
        assert_eq!(j.lines().unwrap().len(), 3);
    }

    #[test]
    fn create_then_undo_then_a_second_undo_is_refused() {
        let j = Journal::at(&tmp("undo"));
        let w = j.record(1000, "c1", "notes.create", rules::Effect::Write, Some(note_write("n_1", b"a"))).unwrap();
        let u = j.record_undo(2000, &w.action, true).unwrap().unwrap();
        assert_eq!(u.undoes, w.action);
        assert_eq!(u.undoes_seq, 1);
        assert!(u.ok);
        let err = j.record_undo(3000, &w.action, true).unwrap_err();
        assert!(err.contains("undone already"), "{err}");
        assert_eq!(j.lines().unwrap().len(), 2);
    }

    #[test]
    fn a_failed_undo_writes_nothing_and_the_action_stays_undoable() {
        let j = Journal::at(&tmp("failed-undo"));
        let w = j.record(1000, "c1", "notes.create", rules::Effect::Write, Some(note_write("n_1", b"a"))).unwrap();
        assert!(j.record_undo(2000, &w.action, false).unwrap().is_none());
        assert_eq!(j.lines().unwrap().len(), 1);
        assert!(j.undoable(&w.action, 2000).is_ok());
    }

    #[test]
    fn a_write_with_no_write_is_not_undoable() {
        let j = Journal::at(&tmp("no-write"));
        let l = j.record(1000, "c1", "notes.search", rules::Effect::Read, None).unwrap();
        let err = j.undoable(&l.action, 1000).unwrap_err();
        assert!(err.contains("changed nothing"), "{err}");
    }

    #[test]
    fn an_undo_line_cannot_be_undone() {
        let j = Journal::at(&tmp("undo-of-undo"));
        let w = j.record(1000, "c1", "notes.create", rules::Effect::Write, Some(note_write("n_1", b"a"))).unwrap();
        let u = j.record_undo(2000, &w.action, true).unwrap().unwrap();
        assert!(j.undoable(&u.action, 2000).is_err());
        assert!(j.record_undo(3000, &u.action, true).is_err());
    }

    #[test]
    fn older_than_7_days_is_refused() {
        let j = Journal::at(&tmp("old"));
        let w = j.record(1000, "c1", "notes.create", rules::Effect::Write, Some(note_write("n_1", b"a"))).unwrap();
        assert!(j.undoable(&w.action, 1000 + KEEP_MS).is_ok());
        let err = j.undoable(&w.action, 1000 + KEEP_MS + 1).unwrap_err();
        assert!(err.contains("older than 7 days"), "{err}");
    }

    #[test]
    fn a_seq_gap_in_the_file_makes_record_fail_and_write_nothing() {
        let dir = tmp("gap");
        let j = Journal::at(&dir);
        j.record(1000, "c1", "notes.create", rules::Effect::Write, Some(note_write("n_1", b"a"))).unwrap();
        let path = dir.join(JOURNAL);
        let mut text = fs::read_to_string(&path).unwrap();
        let l2 = j.record(2000, "c2", "notes.create", rules::Effect::Write, Some(note_write("n_2", b"b"))).unwrap();
        text.push_str(&serde_json::to_string(&Line { seq: 4, ..l2.clone() }).unwrap());
        text.push('\n');
        fs::write(&path, &text).unwrap();
        assert!(j.record(3000, "c3", "notes.search", rules::Effect::Read, None).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), text);
    }

    #[test]
    fn a_corrupt_json_line_names_its_line_number() {
        let dir = tmp("corrupt");
        let j = Journal::at(&dir);
        j.record(1000, "c1", "notes.create", rules::Effect::Write, Some(note_write("n_1", b"a"))).unwrap();
        let path = dir.join(JOURNAL);
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(b"{not json\n").unwrap();
        drop(f);
        let err = j.lines().unwrap_err();
        assert!(err.contains("line 2"), "{err}");
    }

    #[test]
    fn rotation_keeps_seq_and_the_old_action_is_gone() {
        let j = Journal::at(&tmp("rotate"));
        let big = "x".repeat((ROTATE_AT + 100) as usize);
        let w = j.record(1000, &big, "notes.create", rules::Effect::Write, Some(note_write("n_1", b"a"))).unwrap();
        let l2 = j.record(2000, "c2", "notes.create", rules::Effect::Write, Some(note_write("n_2", b"b"))).unwrap();
        assert_eq!(l2.seq, 2);
        assert!(j.dir.join(format!("{JOURNAL}.1")).exists());
        let err = j.undoable(&w.action, 2000).unwrap_err();
        assert!(err.contains("no action"), "{err}");
        assert!(j.undoable(&l2.action, 2000).is_ok());
    }

    #[test]
    fn blob_round_trip_and_a_tampered_blob_is_refused() {
        let j = Journal::at(&tmp("blob"));
        let name = j.keep_blob(b"old bytes").unwrap();
        assert!(name.starts_with("blob:"));
        assert_eq!(j.blob(&name).unwrap(), b"old bytes");
        assert_eq!(j.keep_blob(b"old bytes").unwrap(), name);
        let hex = name.strip_prefix("blob:").unwrap();
        fs::write(j.dir.join("blobs").join(hex), b"tampered").unwrap();
        assert!(j.blob(&name).is_err());
        assert!(j.blob("blob:zz").is_err());
        assert!(j.blob("nope").is_err());
    }

    #[test]
    fn prune_drops_by_age_then_by_size() {
        let j = Journal::at(&tmp("prune"));
        let a = j.keep_blob(&[b'a'; 100]).unwrap();
        let b = j.keep_blob(&[b'b'; 100]).unwrap();
        let now = SystemTime::now();
        let old = now - Duration::from_millis(KEEP_MS + 1000);
        let fa = OpenOptions::new().write(true).open(j.dir.join("blobs").join(a.strip_prefix("blob:").unwrap())).unwrap();
        fa.set_modified(old).unwrap();
        drop(fa);
        assert_eq!(j.prune(now, MAX_BLOB_BYTES).unwrap(), 1);
        assert!(j.blob(&b).is_ok());
        assert_eq!(j.prune(now, 50).unwrap(), 1);
        assert_eq!(j.prune(now, MAX_BLOB_BYTES).unwrap(), 0);
    }

    #[test]
    fn folders_are_0700_and_files_0600() {
        let dir = tmp("modes");
        let j = Journal::at(&dir);
        j.record(1000, "c1", "notes.create", rules::Effect::Write, Some(note_write("n_1", b"a"))).unwrap();
        let name = j.keep_blob(b"x").unwrap();
        let mode = |p: PathBuf| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(dir.clone()), 0o700);
        assert_eq!(mode(dir.join("blobs")), 0o700);
        assert_eq!(mode(dir.join(JOURNAL)), 0o600);
        assert_eq!(mode(dir.join("blobs").join(name.strip_prefix("blob:").unwrap())), 0o600);
    }

    #[test]
    fn recent_lists_undoable_writes_newest_first() {
        let j = Journal::at(&tmp("recent"));
        let w1 = j.record(1000, "c1", "notes.create", rules::Effect::Write, Some(note_write("n_1", b"a"))).unwrap();
        let w2 = j.record(2000, "c2", "notes.create", rules::Effect::Write, Some(note_write("n_2", b"b"))).unwrap();
        j.record(3000, "c3", "notes.search", rules::Effect::Read, None).unwrap();
        j.record_undo(4000, &w1.action, true).unwrap();
        let r = j.recent(10, 4000).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].action, w2.action);
        assert_eq!(j.recent(1, 4000).unwrap().len(), 1);
    }
}
