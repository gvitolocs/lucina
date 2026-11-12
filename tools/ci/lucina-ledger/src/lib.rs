//! `lucina-ledger`: the read-only reader of lagent's commit ledger, for the CI gates of LucinaOS.
//!
//! The writer is agentd's `ledger` module (SecretaryIOS): `commit-ledger.jsonl`, mode 0600, one JSON line per attested
//! commit with the fields below in this order, `prev` the lowercase hex SHA-256 of the previous line's bytes without its
//! newline (64 zeros on the first line), and a commit attested once per repo. This crate never writes: [`read`] returns the
//! entries a chain holds, or says which 1-based line does not fit and why, so a gate can refuse instead of guessing. A
//! missing file is [`Error::Missing`], which is not a broken ledger: "nothing attested yet" and "the ledger does not add up"
//! lead to different messages and different fixes.
//!
//! Threat model: honest mistakes and forgetfulness, not a malicious agent (every agent is the same Unix user). The chain
//! detects an edited or removed line in the middle; it cannot detect lines cut from the end, or an edit of the last line.
//!
//! Intended caller: the proof-roles gate (`lucina-ci roles`, docs/ci/PROOF-ROLES.md), which reads the ledger through
//! `LAGENT_COMMIT_LEDGER` instead of trusting a hand-typed `Co-Authored-By` trailer: [`path_from_env`] for where the file
//! is, [`read`] for what it says, [`Ledger::lookup`] for the entry of one commit, and [`mismatch`] to compare that entry
//! with the commit's trailers. Wiring belongs to the proof lead: this crate depends on nothing in the repository, and
//! nothing here is wired into lucina-ci.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// `prev` of the first line.
const ZERO: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// The values `kind` takes.
const KINDS: [&str; 3] = ["lane", "subagent", "lead"];

/// One ledger line: the writer's fields, in the writer's order, so a line the writer serialised round-trips byte for byte.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub v: u32,
    pub commit: String,
    pub repo: String,
    pub workspace: String,
    pub model: String,
    pub kind: String,
    pub lane: String,
    pub session: String,
    pub declared_by: String,
    pub time: String,
    pub prev: String,
}

/// A ledger that read clean: its path and its entries, oldest line first.
#[derive(Debug, Clone)]
pub struct Ledger {
    path: PathBuf,
    entries: Vec<Entry>,
}

/// Why [`read`] could not hand over a valid ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// No file at that path: nothing is attested yet, which is not damage.
    Missing(PathBuf),
    /// The chain or one of its lines is not what the format says: the 1-based line and what is wrong with it.
    Broken { line: usize, reason: String },
    /// The file is there but could not be read.
    Unreadable { path: PathBuf, reason: String },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Missing(p) => write!(f, "{}: no ledger file", p.display()),
            Error::Broken { line, reason } => write!(f, "line {line}: {reason}"),
            Error::Unreadable { path, reason } => write!(f, "{}: {reason}", path.display()),
        }
    }
}

impl std::error::Error for Error {}

/// Where the ledger is: `LAGENT_COMMIT_LEDGER` when it is set and not empty, else None. There is no default path, so a
/// caller that gets None decides what "no ledger" means.
pub fn path_from_env() -> Option<PathBuf> {
    std::env::var_os("LAGENT_COMMIT_LEDGER")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Read the file and verify its chain the way the writer does before it appends: line 1's `prev` is 64 zeros, every later
/// line's `prev` is the hash of the previous line's exact bytes, and every line carries each field of the format with a
/// value the format allows. An empty file is an empty ledger.
pub fn read(path: impl AsRef<Path>) -> Result<Ledger, Error> {
    let path = path.as_ref();
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(Error::Missing(path.to_path_buf()));
        }
        Err(e) => {
            return Err(Error::Unreadable {
                path: path.to_path_buf(),
                reason: e.to_string(),
            });
        }
    };
    Ok(Ledger {
        path: path.to_path_buf(),
        entries: verify(&text)?,
    })
}

impl Ledger {
    /// The file this was read from.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The entries, oldest line first.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// The entry of that commit in that repo, or None when the ledger says nothing about it. `read` refuses a ledger that
    /// attests one commit twice in one repo, so at most one line can match.
    pub fn lookup(&self, repo: &str, commit: &str) -> Option<&Entry> {
        self.entries
            .iter()
            .find(|e| e.repo == repo && e.commit == commit)
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn is_full_hex(s: &str) -> bool {
    s.len() == 40
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn broken(line: usize, reason: String) -> Error {
    Error::Broken { line, reason }
}

fn verify(text: &str) -> Result<Vec<Entry>, Error> {
    let mut entries = Vec::new();
    let mut prev = ZERO.to_string();
    let mut seen: BTreeMap<(String, String), usize> = BTreeMap::new();
    if !text.is_empty() && !text.ends_with('\n') {
        return Err(broken(
            text.lines().count(),
            "the last line has no newline (a partial write)".into(),
        ));
    }
    for (i, line) in text.lines().enumerate() {
        let n = i + 1;
        let e: Entry = serde_json::from_str(line)
            .map_err(|e| broken(n, format!("not a ledger entry: {e}")))?;
        if e.v != 1 {
            return Err(broken(n, format!("unknown version {}", e.v)));
        }
        if !is_full_hex(&e.commit) {
            return Err(broken(
                n,
                format!("commit {:?} is not 40 lowercase hex", e.commit),
            ));
        }
        if !KINDS.contains(&e.kind.as_str()) {
            return Err(broken(
                n,
                format!("kind {:?} is not one of {}", e.kind, KINDS.join(", ")),
            ));
        }
        if e.prev != prev {
            // The chain links each line to the one before it, so a changed line shows up in the next line's
            // `prev`: blame the earlier line, which is the one that changed.
            return Err(if n == 1 {
                broken(
                    1,
                    "the first line's prev is not 64 zeros (the line was changed)".into(),
                )
            } else {
                broken(
                    n - 1,
                    format!(
                        "line {n}'s prev no longer matches this line's hash: line {} (or line {n}) was changed or removed",
                        n - 1
                    ),
                )
            });
        }
        if let Some(first) = seen.insert((e.repo.clone(), e.commit.clone()), n) {
            return Err(broken(
                n,
                format!(
                    "commit {} of {} is attested twice (line {first} and line {n}), so which of the two is the entry the daemon wrote is not decidable here",
                    e.commit, e.repo
                ),
            ));
        }
        prev = sha256_hex(line.as_bytes());
        entries.push(e);
    }
    Ok(entries)
}

/// The normalised name of a `Co-Authored-By` trailer: trimmed, one trailing parenthesised qualifier dropped, inner
/// whitespace collapsed, ASCII lowercase. Two spellings of the same model differ that way: `Qoder (auto-free)` and `Qoder`
/// are the family `qoder`, `Claude Sonnet 5.5` is `claude sonnet 5.5`.
///
/// This is the writer's rule, not one of our own: ported from its `trailer_name_key` and `drop_trailing_qualifier` as they
/// stand in SecretaryIOS b866ed4daa `crates/lucina-agentd/src/ledger.rs`. The reader of the ledger and the agent that
/// writes it must answer the same question the same way, or a commit the writer accepted is read back as a mismatch.
/// What that rule gives and a `rfind` does not: `Qoder(x)` is the family `qoder(x)`, not `qoder` — a group that is not set
/// apart from the name by whitespace is part of the name.
pub fn trailer_family(name: &str) -> String {
    let name = name.trim();
    let name = drop_trailing_qualifier(name).unwrap_or(name);
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

/// The name without its trailing `(qualifier)`, when the whole name ends in a parenthesised group that is separated from
/// the name by whitespace. Only the outermost group goes, so a middle qualifier stays and two families are never merged.
fn drop_trailing_qualifier(name: &str) -> Option<&str> {
    if !name.ends_with(')') {
        return None;
    }
    let bytes = name.as_bytes();
    let mut open_depth = 0usize;
    for i in (0..bytes.len()).rev() {
        match bytes[i] {
            b')' => open_depth += 1,
            b'(' => {
                open_depth -= 1;
                if open_depth == 0 {
                    // The '(' the closing one belongs to; the name has to precede it, with whitespace, or the group is
                    // part of the name itself.
                    return name[..i].strip_suffix(char::is_whitespace);
                }
            }
            _ => {}
        }
    }
    None
}

/// Does the ledger's entry for a commit square with that commit's trailer names, given the model-id to trailer-name table
/// (`[attribution.models]` of the trunk's `.lucina/pipeline.toml`, else the writer's default)? None when they agree:
/// exactly one trailer is of the family the table gives for `entry.model`, and no other trailer is of a family the table
/// gives for another model. Else a sentence saying what the ledger says and what the trailers say.
pub fn mismatch(
    entry: &Entry,
    trailer_names: &[&str],
    table: &BTreeMap<String, String>,
) -> Option<String> {
    let Some(expected) = table.get(&entry.model) else {
        let known: Vec<&str> = table.keys().map(String::as_str).collect();
        return Some(format!(
            "commit {} of {} is attested as model {:?}, which is not in the attribution table (its keys: {})",
            short(&entry.commit),
            entry.repo,
            entry.model,
            known.join(", ")
        ));
    };
    let says = format!(
        "the ledger attests commit {} of {} as {} (\"{expected}\")",
        short(&entry.commit),
        entry.repo,
        entry.model
    );
    let want = trailer_family(expected);
    let names = trailer_names
        .iter()
        .map(|t| t.trim())
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>();
    let same = names.iter().filter(|t| trailer_family(t) == want).count();
    if same > 1 {
        return Some(format!(
            "{says}, but the trailers name \"{expected}\" {same} times, not once"
        ));
    }
    if same == 1 {
        // Another model of the table in the same trailer block: the writer refuses that as ambiguous. A name
        // the table does not know (a human co-author) says nothing about which model wrote it.
        if let Some(other) = names
            .iter()
            .find(|t| trailer_family(t) != want && of_table(t, table))
        {
            return Some(format!(
                "{says}, but the trailers name both \"{expected}\" and \"{other}\", so it is not clear which model wrote it"
            ));
        }
        return None;
    }
    if names.is_empty() {
        return Some(format!(
            "{says}, but the commit has no Co-Authored-By trailer"
        ));
    }
    Some(format!(
        "{says}, but the trailers name {}",
        names
            .iter()
            .map(|t| format!("\"{t}\""))
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

/// Is `name` one of the trailer names this table gives for some model?
fn of_table(name: &str, table: &BTreeMap<String, String>) -> bool {
    let f = trailer_family(name);
    table.values().any(|v| trailer_family(v) == f)
}

fn short(commit: &str) -> &str {
    if commit.len() >= 12 {
        &commit[..12]
    } else {
        commit
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // ---- the fixtures, written the way the writer writes them, in a tempdir of our own ----

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    /// A fresh tempdir; it and everything in it go when the guard drops. No real ledger is ever opened.
    struct Dir(PathBuf);

    impl Dir {
        fn new(tag: &str) -> Self {
            let d = std::env::temp_dir().join(format!(
                "lucina-ledger-{tag}-{}-{}",
                std::process::id(),
                SEQ.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            Dir(d)
        }

        /// The ledger path inside it, holding `text`.
        fn write(&self, text: &str) -> PathBuf {
            let p = self.0.join("commit-ledger.jsonl");
            std::fs::write(&p, text).unwrap();
            p
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// 40 lowercase hex, distinct per index.
    fn commit_of(i: usize) -> String {
        format!("{i:040x}")
    }

    /// A draft as the daemon hands one to its `append`: no `prev` yet.
    fn draft(repo: &str, i: usize, model: &str) -> Entry {
        Entry {
            v: 1,
            commit: commit_of(i),
            repo: repo.into(),
            workspace: String::new(),
            model: model.into(),
            kind: "lane".into(),
            lane: "claude-sonnet".into(),
            session: "tester@host".into(),
            declared_by: "flag".into(),
            time: "2026-11-12T09:00:00Z".into(),
            prev: String::new(),
        }
    }

    /// The writer's serialising and chaining: `prev` from the previous line's exact bytes, one line per entry, every
    /// line ended by a newline.
    fn chain_text(drafts: &[Entry]) -> String {
        let mut prev = ZERO.to_string();
        let mut out = String::new();
        for d in drafts {
            let mut e = d.clone();
            e.prev = prev.clone();
            let line = serde_json::to_string(&e).unwrap();
            prev = sha256_hex(line.as_bytes());
            out.push_str(&line);
            out.push('\n');
        }
        out
    }

    fn lines_of(text: &str) -> Vec<String> {
        text.lines().map(str::to_string).collect()
    }

    fn table(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn default_table() -> BTreeMap<String, String> {
        table(&[
            ("claude-sonnet-5-5", "Claude Sonnet 5.5"),
            ("claude-opus-5-5", "Claude Opus 5.5"),
            ("qoder", "Qoder"),
        ])
    }

    fn three_lines() -> String {
        chain_text(&[
            draft("proj", 1, "claude-sonnet-5-5"),
            draft("proj", 2, "claude-opus-5-5"),
            draft("proj", 3, "qoder"),
        ])
    }

    /// Replace one whole line of `text`, which is safe because every line of a chain is distinct.
    fn replace_line(text: &str, idx: usize, new: &str) -> String {
        let ls = lines_of(text);
        ls.iter()
            .enumerate()
            .map(|(i, l)| if i == idx { new } else { l })
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
    }

    // ---- read: a whole chain, and each way the writer would say it does not add up ----

    #[test]
    fn a_valid_three_line_chain_reads_and_lookup_finds_each() {
        let d = Dir::new("valid");
        let text = three_lines();
        let ls = lines_of(&text);
        let p = d.write(&text);
        let l = read(&p).unwrap();
        assert_eq!(l.entries().len(), 3);
        assert_eq!(l.path(), p.as_path());
        assert_eq!(l.entries()[0].prev, ZERO);
        assert_eq!(l.entries()[1].prev, sha256_hex(ls[0].as_bytes()));
        assert_eq!(l.entries()[2].prev, sha256_hex(ls[1].as_bytes()));
        for (i, model) in ["claude-sonnet-5-5", "claude-opus-5-5", "qoder"]
            .iter()
            .enumerate()
        {
            let e = l.lookup("proj", &commit_of(i + 1)).expect("entry");
            assert_eq!(e.model, *model, "{e:?}");
            assert_eq!(e.kind, "lane");
            assert_eq!(e.session, "tester@host");
        }
        assert!(l.lookup("proj", &commit_of(4)).is_none());
    }

    #[test]
    fn a_commit_attested_in_another_repo_is_not_found() {
        let d = Dir::new("otherrepo");
        // One commit hash in two repos is allowed: repo and commit together are what is unique.
        let text = chain_text(&[
            draft("proj", 7, "qoder"),
            draft("other", 7, "claude-sonnet-5-5"),
        ]);
        let l = read(d.write(&text)).unwrap();
        assert_eq!(l.entries().len(), 2);
        assert_eq!(l.lookup("proj", &commit_of(7)).unwrap().model, "qoder");
        assert_eq!(
            l.lookup("other", &commit_of(7)).unwrap().model,
            "claude-sonnet-5-5"
        );
        assert!(l.lookup("third", &commit_of(7)).is_none());
    }

    #[test]
    fn a_flipped_byte_in_line_two_blames_line_two() {
        let d = Dir::new("flip");
        let text = three_lines();
        let ls = lines_of(&text);
        // The middle line still parses and its own prev still fits: only line 3's prev shows the change, so the
        // reader blames line 2, the way the writer does.
        let tampered = ls[1].replace("tester", "tester2");
        assert_ne!(tampered, ls[1]);
        let e = read(d.write(&replace_line(&text, 1, &tampered))).unwrap_err();
        assert_eq!(
            e,
            broken(
                2,
                "line 3's prev no longer matches this line's hash: line 2 (or line 3) was changed or removed".into()
            )
        );
    }

    #[test]
    fn a_removed_middle_line_is_detected() {
        let d = Dir::new("removed");
        let text = three_lines();
        let ls = lines_of(&text);
        // With line 2 gone, what was line 3 sits at line 2 and its prev no longer matches line 1, so the reader
        // names line 1 and says changed or removed: it cannot tell the two apart, and it never accepts the file.
        let cut = format!("{}\n{}\n", ls[0], ls[2]);
        let e = read(d.write(&cut)).unwrap_err();
        assert_eq!(
            e,
            broken(
                1,
                "line 2's prev no longer matches this line's hash: line 1 (or line 2) was changed or removed".into()
            )
        );
    }

    #[test]
    fn the_first_prev_must_be_64_zeros() {
        let d = Dir::new("firstprev");
        let text = three_lines();
        let tampered = text.replacen(&format!("\"prev\":\"{ZERO}\""), "\"prev\":\"x\"", 1);
        let e = read(d.write(&tampered)).unwrap_err();
        assert_eq!(
            e,
            broken(
                1,
                "the first line's prev is not 64 zeros (the line was changed)".into()
            )
        );
    }

    #[test]
    fn a_line_not_of_this_format_is_reported_with_its_line() {
        let d = Dir::new("fields");
        let ls = lines_of(&three_lines());
        // Each of these is line 2 itself not being of the format, so line 2 is named, not a neighbour.
        let cases: Vec<(String, &str)> = vec![
            (
                ls[1].replace("\"kind\":\"lane\"", "\"kind\":\"lead-typed\""),
                "kind \"lead-typed\" is not one of lane, subagent, lead",
            ),
            (
                ls[1].replace(
                    &format!("\"commit\":\"{}\"", commit_of(2)),
                    "\"commit\":\"abc123\"",
                ),
                "commit \"abc123\" is not 40 lowercase hex",
            ),
            (ls[1].replace("\"v\":1", "\"v\":2"), "unknown version 2"),
            (
                ls[1].replace("\"repo\":\"proj\",", ""),
                "not a ledger entry: missing field `repo`",
            ),
            (
                ls[1].replace("\"time\":\"2026-11-12T09:00:00Z\",", ""),
                "not a ledger entry: missing field `time`",
            ),
            ("not json at all".into(), "not a ledger entry:"),
        ];
        for (bad, needle) in cases {
            let e = read(d.write(&replace_line(&three_lines(), 1, &bad))).unwrap_err();
            let Error::Broken { line, reason } = e.clone() else {
                panic!("{e}");
            };
            assert_eq!(line, 2, "{bad}\n-> {reason}");
            assert!(reason.contains(needle), "{reason}\nwant {needle}");
        }
    }

    #[test]
    fn a_last_line_without_its_newline_is_a_partial_write() {
        let d = Dir::new("partial");
        let text = chain_text(&[draft("proj", 1, "qoder"), draft("proj", 2, "qoder")]);
        let cut = text.trim_end_matches('\n').to_string();
        assert_eq!(
            read(d.write(&cut)).unwrap_err(),
            broken(2, "the last line has no newline (a partial write)".into())
        );
    }

    #[test]
    fn a_commit_attested_twice_in_one_repo_is_an_error() {
        let d = Dir::new("dup");
        let text = chain_text(&[
            draft("proj", 1, "qoder"),
            draft("proj", 2, "claude-sonnet-5-5"),
            draft("proj", 1, "claude-opus-5-5"),
        ]);
        let e = read(d.write(&text)).unwrap_err();
        let Error::Broken { line, reason } = e else {
            panic!("{e}");
        };
        assert_eq!(line, 3, "{reason}");
        assert!(reason.contains("attested twice"), "{reason}");
        assert!(reason.contains(&commit_of(1)), "{reason}");
        assert!(reason.contains("line 1 and line 3"), "{reason}");
    }

    #[test]
    fn a_missing_file_is_its_own_variant() {
        let d = Dir::new("missing");
        let p = d.0.join("commit-ledger.jsonl");
        assert!(!p.exists());
        let e = read(&p).unwrap_err();
        assert_eq!(e, Error::Missing(p.clone()));
        assert_eq!(e.to_string(), format!("{}: no ledger file", p.display()));
        // Not a Broken: a caller must be able to tell the two apart.
        assert!(!matches!(e, Error::Broken { .. }));
    }

    #[test]
    fn an_empty_file_is_an_empty_ledger() {
        let d = Dir::new("empty");
        let l = read(d.write("")).unwrap();
        assert!(l.entries().is_empty());
        assert!(l.lookup("proj", &commit_of(1)).is_none());
    }

    // ---- where the ledger is ----

    /// The environment is process-wide, so the three states of the variable share one guard.
    static ENV: Mutex<()> = Mutex::new(());

    #[test]
    fn path_from_env_follows_the_variable() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: the guard is held for the whole test and no other test of this crate touches the environment.
        unsafe {
            std::env::remove_var("LAGENT_COMMIT_LEDGER");
            assert_eq!(path_from_env(), None);
            std::env::set_var("LAGENT_COMMIT_LEDGER", "");
            assert_eq!(path_from_env(), None, "an empty value is not a path");
            std::env::set_var("LAGENT_COMMIT_LEDGER", "/srv/state/commit-ledger.jsonl");
            assert_eq!(
                path_from_env(),
                Some(PathBuf::from("/srv/state/commit-ledger.jsonl"))
            );
            std::env::remove_var("LAGENT_COMMIT_LEDGER");
        }
    }

    // ---- trailer names ----

    #[test]
    fn trailer_family_normalises_a_trailer_name() {
        assert_eq!(trailer_family("Qoder (auto-free)"), "qoder");
        assert_eq!(trailer_family("Qoder"), "qoder");
        assert_eq!(trailer_family("  qoder   "), "qoder");
        assert_eq!(trailer_family("Claude Sonnet 5.5"), "claude sonnet 5.5");
        assert_eq!(
            trailer_family("Claude   Sonnet\t5.5 (worker)"),
            "claude sonnet 5.5"
        );
        assert_eq!(trailer_family("CLAUDE OPUS 5.5"), "claude opus 5.5");
        // one qualifier only: what is left after it goes is still the name
        assert_eq!(trailer_family("Qoder (a) (b)"), "qoder (a)");
        assert_eq!(trailer_family("Qoder (unclosed"), "qoder (unclosed");
        assert_eq!(trailer_family(""), "");
    }

    // keep identical to the attest tests in SecretaryIOS
    // ("Qoder (auto-free)",         Some("qoder")),
    // ("Qoder (efficient)",         Some("qoder")),
    // ("qoder",                     None),
    // ("Qoder  ",                   Some("qoder")),
    // ("  Qoder (auto-free)  ",     Some("qoder")),
    // ("Qoder(x)",                  None),  // "qoder(x)", NOT "qoder"
    // ("Qoder (x) y",               None),
    // ("Qoderx",                    None),
    // ("Claude Sonnet 5.5 (fast)",  Some("claude sonnet 5.5")),
    // ("Claude Opus 5.5",           None),
    // ("A (b (c))",                 Some("a")),
    // ("(x)",                       None),
    // ("",                          None),

    /// A `Co-Authored-By` name and its family: the name with one trailing qualifier dropped and the rest normalised, or
    /// `None` where no qualifier goes and the name is already trimmed and single-spaced, so that its lowercase is the
    /// family. Keep identical to the attest tests in SecretaryIOS (b866ed4daa `crates/lucina-agentd/src/ledger.rs`).
    const TRAILER_CASES: &[(&str, Option<&str>)] = &[
        ("Qoder (auto-free)", Some("qoder")),
        ("Qoder (efficient)", Some("qoder")),
        ("qoder", None),
        ("Qoder  ", Some("qoder")),
        ("  Qoder (auto-free)  ", Some("qoder")),
        ("Qoder(x)", None),
        ("Qoder (x) y", None),
        ("Qoderx", None),
        ("Claude Sonnet 5.5 (fast)", Some("claude sonnet 5.5")),
        ("Claude Opus 5.5", None),
        ("A (b (c))", Some("a")),
        ("(x)", None),
        ("", None),
    ];

    #[test]
    fn trailer_family_gives_attests_answer_on_every_case() {
        for &(name, want) in TRAILER_CASES {
            let kept = name.to_ascii_lowercase();
            let expect = want.unwrap_or(&kept);
            assert_eq!(trailer_family(name), expect, "trailer_family({name:?})");
        }
        // The one case the rfind rule got wrong, spelled out: no whitespace before the group, so the group stays.
        assert_eq!(trailer_family("Qoder(x)"), "qoder(x)");
        assert_ne!(trailer_family("Qoder(x)"), trailer_family("Qoder"));
    }

    #[test]
    fn mismatch_agrees_with_the_writer_on_each_of_its_cases() {
        let t = default_table();
        let qoder = draft("proj", 4, "qoder");
        // The case the gate wants: a trailer that differs from the table only by its qualifier.
        assert_eq!(mismatch(&qoder, &["Qoder (auto-free)"], &t), None);
        assert_eq!(mismatch(&qoder, &["Qoder"], &t), None);
        // A name the table does not know (a human co-author) says nothing about the model.
        assert_eq!(mismatch(&qoder, &["Qoder", "Some Owner"], &t), None);
        // Another model.
        let e = mismatch(&qoder, &["Claude Sonnet 5.5"], &t).unwrap();
        assert!(e.contains("as qoder") && e.contains("\"Qoder\""), "{e}");
        assert!(e.contains("the trailers name \"Claude Sonnet 5.5\""), "{e}");
        // No trailer at all.
        let e = mismatch(&qoder, &[], &t).unwrap();
        assert!(e.contains("no Co-Authored-By trailer"), "{e}");
        assert!(e.contains("the ledger attests commit"), "{e}");
        // Two models of the table in one block.
        let e = mismatch(&qoder, &["Qoder", "Claude Sonnet 5.5"], &t).unwrap();
        assert!(e.contains("not clear which model"), "{e}");
        assert!(
            e.contains("Qoder") && e.contains("Claude Sonnet 5.5"),
            "{e}"
        );
        // The same model named twice is not exactly one trailer.
        let e = mismatch(&qoder, &["Qoder", "Qoder (auto-free)"], &t).unwrap();
        assert!(e.contains("2 times"), "{e}");
        // Attest's rule on the one spelling the writer keeps as it is: `Qoder(x)` is the family `qoder(x)`, so a commit
        // declared as qoder but trailed `Qoder(x)` does not square, while the same name with a space does.
        assert_eq!(mismatch(&qoder, &["Qoder (x)"], &t), None);
        let e = mismatch(&qoder, &["Qoder(x)"], &t).unwrap();
        assert!(
            e.contains("as qoder") && e.contains("the trailers name \"Qoder(x)\""),
            "{e}"
        );
        let e = mismatch(&qoder, &["Qoder(x) (auto-free)"], &t).unwrap();
        assert!(
            e.contains("the trailers name \"Qoder(x) (auto-free)\""),
            "{e}"
        );
        // A model the table does not know.
        let e = mismatch(&draft("proj", 5, "gpt-9"), &["Qoder"], &t).unwrap();
        assert!(e.contains("not in the attribution table"), "{e}");
        assert!(e.contains("gpt-9"), "{e}");
        assert!(
            e.contains("claude-opus-5-5, claude-sonnet-5-5, qoder"),
            "{e}"
        );
    }

    // ---- the writer and the reader agree byte for byte ----

    /// One line exactly as agentd's `append` serialises it: the field order of its `Entry` and the values of its own
    /// attest test (repo `proj` from its throwaway checkout, model `claude-sonnet-5-5`, kind `lane`, lane
    /// `claude-sonnet`, session `tester@host`, declared by `flag`, an empty workspace, `prev` 64 zeros as the first
    /// line). `time` is the wall clock, so one fixed RFC 3339 value stands in for it.
    const WRITER_LINE: &str = r#"{"v":1,"commit":"d1b3c9f8a7e6054231897f65e4d0c3b2a1908f76","repo":"proj","workspace":"","model":"claude-sonnet-5-5","kind":"lane","lane":"claude-sonnet","session":"tester@host","declared_by":"flag","time":"2026-11-12T09:00:00Z","prev":"0000000000000000000000000000000000000000000000000000000000000000"}"#;

    #[test]
    fn a_writers_own_line_reads_and_chain_hashes_byte_for_byte() {
        let d = Dir::new("writer");
        // A second line, chained by the writer's rule onto the first line's exact bytes.
        let mut next = draft("proj", 9, "qoder");
        next.prev = sha256_hex(WRITER_LINE.as_bytes());
        let text = format!("{WRITER_LINE}\n{}\n", serde_json::to_string(&next).unwrap());
        let l = read(d.write(&text)).unwrap();
        assert_eq!(l.entries().len(), 2);
        let e = &l.entries()[0];
        assert_eq!(e.v, 1);
        assert_eq!(e.commit, "d1b3c9f8a7e6054231897f65e4d0c3b2a1908f76");
        assert_eq!(e.repo, "proj");
        assert!(e.workspace.is_empty());
        assert_eq!(e.model, "claude-sonnet-5-5");
        assert_eq!(e.kind, "lane");
        assert_eq!(e.lane, "claude-sonnet");
        assert_eq!(e.session, "tester@host");
        assert_eq!(e.declared_by, "flag");
        assert_eq!(e.time, "2026-11-12T09:00:00Z");
        assert_eq!(e.prev, ZERO);
        assert_eq!(l.entries()[1].prev, sha256_hex(WRITER_LINE.as_bytes()));
        // Serialising what was read gives back the same bytes: the field order is the writer's.
        assert_eq!(serde_json::to_string(e).unwrap(), WRITER_LINE);
        assert_eq!(l.lookup("proj", &e.commit).unwrap(), e);
        // And a change to this line is what the reader blames.
        let tampered = WRITER_LINE.replace("tester@host", "tester@hosX");
        let e = read(d.write(&replace_line(&text, 0, &tampered))).unwrap_err();
        assert_eq!(
            e,
            broken(
                1,
                "line 2's prev no longer matches this line's hash: line 1 (or line 2) was changed or removed".into()
            )
        );
    }
}
