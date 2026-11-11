//! One JSON line per call: when, which call, which tool, whether it worked, how long it took, the decision it ran under, and
//! `target`, `undo`, `effect` (stage B and C; empty for other tools). A redacted credential is logged by its kind only
//! (`redacted: OPENAI_API_KEY`). Never the call's arguments or its output.
//! `/data/lucina/devd/audit.jsonl` (folder 0700, file 0600), or `/run/lucina/devd` on a boot without `/data`, so nothing is
//! written under the mount point before `/data` is there. At 1 MiB it moves to `audit.jsonl.1`, and the one before goes.
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde_json::json;

pub const ROTATE_AT: u64 = 1 << 20;

pub struct Audit {
    dir: PathBuf,
}

/// One call's line.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Entry {
    pub ts_ms: u64,
    pub id: String,
    pub tool: String,
    pub ok: bool,
    pub ms: u64,
    /// The decision the server ran the call under, or empty.
    pub decision_id: String,
    /// A node id and role, or an intent's name (stage B and C), or empty.
    pub target: String,
    /// The undo action's id (stage C), or empty.
    pub undo: String,
    /// The effect class (read, write, destructive, external, owner_only; stage C), or empty.
    pub effect: String,
    /// The kinds of credential replaced in the output, never their values.
    pub redacted: Vec<String>,
}

impl Audit {
    /// Where this machine keeps it: `root` is `/` on the laptop.
    pub fn under(root: &Path) -> Audit {
        let dir = if root.join("run/lucina/data-ready").exists() {
            root.join("data/lucina/devd")
        } else {
            root.join("run/lucina/devd")
        };
        Audit { dir }
    }

    pub fn at(dir: &Path) -> Audit {
        Audit {
            dir: dir.to_path_buf(),
        }
    }

    pub fn path(&self) -> PathBuf {
        self.dir.join("audit.jsonl")
    }

    /// A call with no decision, target, undo, effect or redaction: [`Audit::record_call`] with those empty.
    pub fn record(
        &self,
        ts_ms: u64,
        id: &str,
        tool: &str,
        ok: bool,
        ms: u64,
    ) -> std::io::Result<()> {
        self.record_call(&Entry {
            ts_ms,
            id: id.into(),
            tool: tool.into(),
            ok,
            ms,
            ..Default::default()
        })
    }

    /// One call's line, every field always present: the decision it ran under, stage C's `target` (the intent or node she
    /// acted on), `undo` (the undo action id) and `effect` (read, write, destructive, external), and the kinds of credential
    /// redacted. Names and ids only, never values.
    pub fn record_call(&self, e: &Entry) -> std::io::Result<()> {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.dir)?;
        std::fs::set_permissions(&self.dir, std::fs::Permissions::from_mode(0o700))?;
        let path = self.path();
        if std::fs::metadata(&path).is_ok_and(|m| m.len() >= ROTATE_AT) {
            std::fs::rename(&path, self.dir.join("audit.jsonl.1"))?;
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)?;
        let line = json!({
            "ts": e.ts_ms, "id": e.id, "tool": e.tool, "ok": e.ok, "ms": e.ms, "decision_id": e.decision_id,
            "target": e.target, "undo": e.undo, "effect": e.effect, "redacted": e.redacted.join(","),
        });
        writeln!(f, "{line}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(ts_ms: u64, id: &str, tool: &str, ok: bool, ms: u64) -> Entry {
        Entry {
            ts_ms,
            id: id.into(),
            tool: tool.into(),
            ok,
            ms,
            ..Default::default()
        }
    }

    fn dir(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("lucina-devd-audit-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn a_line_per_call_with_private_modes() {
        let d = dir("lines");
        let a = Audit::at(&d.join("devd"));
        a.record_call(&entry(1, "c1", "status", true, 12)).unwrap();
        a.record_call(&Entry {
            redacted: vec!["OPENAI_API_KEY".into()],
            decision_id: "d7".into(),
            ..entry(2, "c2", "bash", false, 30)
        })
        .unwrap();
        let text = std::fs::read_to_string(a.path()).unwrap();
        let lines: Vec<serde_json::Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[1],
            json!({"ts": 2, "id": "c2", "tool": "bash", "ok": false, "ms": 30, "decision_id": "d7", "target": "", "undo": "", "effect": "", "redacted": "OPENAI_API_KEY"})
        );
        assert_eq!(
            std::fs::metadata(d.join("devd"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(a.path()).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn it_turns_over_at_one_mib_and_keeps_one_old_file() {
        let d = dir("rotate");
        let a = Audit::at(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(a.path(), vec![b'x'; ROTATE_AT as usize]).unwrap();
        std::fs::write(d.join("audit.jsonl.1"), b"older").unwrap();
        a.record(3, "c3", "tools", true, 1).unwrap();
        assert_eq!(
            std::fs::metadata(d.join("audit.jsonl.1")).unwrap().len(),
            ROTATE_AT
        );
        assert_eq!(
            std::fs::read_to_string(a.path()).unwrap().lines().count(),
            1
        );
        assert!(!d.join("audit.jsonl.2").exists());
    }

    #[test]
    fn without_data_it_stays_in_memory() {
        let d = dir("where");
        std::fs::create_dir_all(d.join("run/lucina")).unwrap();
        assert_eq!(
            Audit::under(&d).path(),
            d.join("run/lucina/devd/audit.jsonl")
        );
        std::fs::write(d.join("run/lucina/data-ready"), b"").unwrap();
        assert_eq!(
            Audit::under(&d).path(),
            d.join("data/lucina/devd/audit.jsonl")
        );
    }
}
