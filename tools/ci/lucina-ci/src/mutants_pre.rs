//! `mutants-precheck`: the text half of every `verification/<x>/mutants.sh`, in seconds and without Verus.
//!
//! A mutants script plants a bug by replacing a search text in a copy of a shipped file. When a formatter re-wraps that file the
//! search text is gone, the script prints `NOT FOUND (...)` or `NOT CHANGED (...)`, and the unit goes red on GitHub. This runs the
//! unit's OWN script from the repo root with `VERUS` set to a stub, so no script dialect is parsed:
//!
//! - the stub passes (exit 0, `verification results:: 1 verified, 0 errors`) on a path that is relative or whose file name is
//!   `base.rs` or `base-*.rs`: that is how the 62 scripts check their unchanged baseline;
//! - the very first call of a script is its baseline whatever the path (juno-charge verifies a copy under its temp dir);
//! - it fails (exit 1, `error: postcondition not satisfied (lucina-ci mutants-precheck stub)`,
//!   `verification results:: 0 verified, 1 errors`) on any other absolute path, a mutated copy under the script's own temp dir: so
//!   every mutant whose text is found counts as "refused", as it would be by Verus. The refusal is worded as Verus words it because
//!   some scripts grep the verifier's output for their own clause (`juno-surface`, `juno-wm-rules`) and would otherwise refuse to
//!   judge the stub at all;
//! - every `NOT FOUND` and `NOT CHANGED` line the script printed is a finding;
//! - a script that ends with a non-zero exit code and no finding line is inconclusive: it failed for a reason of its own that the
//!   text does not show, and a marker the script hides (`>/dev/null`, an indent) must not read as a pass;
//! - on every mutant call it also copies the file that carries the planted bug (the script deletes its work directory when it ends,
//!   so the tool reads the copies afterwards). Two shapes cover the 62 scripts: most plant into a copy of a shipped file kept under
//!   its repo-relative path below the work root, which is found by comparing every file there with the repository; the rest build the
//!   text for Verus themselves, and there the copy is the very file Verus was handed, compared with the baseline of that script it was
//!   cut from (a script runs one baseline per shipped file). Either way the replaced region is the longest common prefix and suffix
//!   trimmed, and counting its original text in the original says whether the search text matched once, as the script assumes, or
//!   several times.
//!
//! Several matches is `AMBIGUOUS (matches n times)`: bash replaces the FIRST match, which may not be the one the line means. It is
//! informational noise and evidence in neither direction — not a finding, and not a clean bill either — so the run says ONE line
//! about it and lists the regions only under `--verbose`; at the gate included it never changes the result. Measured on 2026-11-12,
//! about 700 regions in 64 of the 65 units match twice, and the reason is the trimming, not the script: a trimmed region is often a
//! token as common as `=`, `cap`, `false`, `0` or a single `s` (juno-kexec's file holds that one over a thousand times), and all 11
//! of juno-irqgate's notes that were read were false. Widening the same calls to whole differing lines leaves about 70 findings in
//! 23 units, and that is the set worth reading before any of it becomes a finding. A call whose copy cannot be read against an
//! original at all makes the unit inconclusive; no unit on this tree is in that state.
//!
//! A unit is inconclusive when its script stopped before its closing line, called Verus for something else first, timed out, printed
//! `NOT REFUSED` or left a mutant copy unreadable. Locally that is a note; at `--merge-gate` it is a failure unless the coordinator
//! allowed that unit in `docs/ci/mutants-precheck-skip.txt`.
//!
//! The script runs with a scrubbed environment: no variable of the session that calls the tool is passed (`PATH`, `HOME`, `TMPDIR`,
//! `LANG`, `VERUS` and the tool's own `LUCINA_*` only). That is a scrub, not a sandbox: the script still runs as the tool's own user,
//! so it resolves `~/.ssh` through the account database whatever `HOME` says, reaches the sockets under `/run/user/<uid>` and writes
//! anywhere that user may write, outside its directory. What keeps a script honest here is that it is read in a few seconds with no
//! verifier in it; isolation is `check-fast` and the GitHub lane, not this.
//!
//! The global list `scripts/verify-mutations.sh` (unit `mutations/global`, kind `mutations`) is covered the same text-only way:
//! every entry is `file|sed expression|what`, the expression is applied to the file in memory and must change it.
//!
//! Time: one script has `PER_SCRIPT_SECS`, the whole run `total_budget` — its floor for a short list, four seconds per unit for
//! a long one (a change under `tools/ci/` reaches every mutants unit), the ceiling for everything else. Past it the rest is not
//! checked and the run fails, never a quiet pass; `STEP_TIMEOUT_S` keeps the `mutants-text` step above that ceiling so the tool
//! names its own unchecked units. A timed-out script is killed as a tree, not only as its process group (see `tree`), which is what
//! reaches a child it left in a new session with `setsid`; a child already orphaned before the timeout is out of reach of a
//! standalone run and only `check-fast`, which sweeps the step's cgroup, kills it.
//!
//! What it cannot see, by design (see docs/ci/PROOF-WORKFLOW.md): that a mutant Verus would not refuse, and a baseline that does not
//! verify. Those stay with the Verus run on GitHub (or `check-fast --proofs --mutants`).

use crate::inventory::{self, Unit};
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// One script may not run longer than this; a script that does is inconclusive, not a failure.
pub const PER_SCRIPT_SECS: u64 = 30;
/// The floor of a whole run: past its budget the rest is not checked and the run fails (it must be visible, never a quiet pass).
pub const MIN_SECS: u64 = 100;
/// The allowance for one unit of the run. Measured on buildhost over the 65 units of the tree (2026-11-12): 52.5 s with each unit
/// alone in its own process, 1.4 to 3.1 s per unit inside one process on a box at load 27-59 with its swap full. Four seconds
/// each, so a change that reaches a handful of units still gets the floor and a change that reaches the whole tree gets the
/// sweep it needs. A change under `tools/ci/` reaches every mutants unit: `tools/ci/` is a `GLOBAL_INPUTS` entry.
pub const UNIT_BUDGET_SECS: u64 = 4;
/// The ceiling of a run, so a wedged box cannot hold it open forever: past it the run fails as any other overrun.
pub const MAX_SECS: u64 = 300;
/// The `mutants-text` step of `check-fast` must let the tool reach its own ceiling and print its reason: a step killed from
/// outside leaves a log with no unit named in it.
pub const STEP_TIMEOUT_S: u64 = MAX_SECS + 60;
// The tool's own bound has to fire before the step that runs it, and `clamp` panics on a floor above the ceiling.
const _: () = assert!(MAX_SECS < STEP_TIMEOUT_S);
const _: () = assert!(MIN_SECS <= MAX_SECS);

/// The wall-clock bound of one `run` over `units` units: the floor for a short list, four seconds per unit beyond it, the
/// ceiling for everything longer. A fixed bound sized for a short change truncates the sweep a `tools/ci/` change turns into,
/// and reports units unchecked while every one of them is conclusive.
pub fn total_budget(units: usize) -> Duration {
    Duration::from_secs(
        (units as u64)
            .saturating_mul(UNIT_BUDGET_SECS)
            .clamp(MIN_SECS, MAX_SECS),
    )
}

/// Keeps the base directory of each `run` call apart.
static RUN_SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// The stand-in for Verus. `LUCINA_STUB_LOG` (if set) gets one line per call: `base` or `mutant`.
/// With `LUCINA_PRE_CAP` set it keeps every text it accepted unchanged in `<LUCINA_PRE_CAP>/_bases/<n>`, and for each mutant call
/// copies into `<LUCINA_PRE_CAP>/<call number>/` the file Verus was handed as `_arg`, plus every file below `TMPDIR` that sits at a
/// path the repository knows and differs from it, under that path. The tool reads the capture after the script has removed its work dir.
pub const STUB: &str = r#"#!/bin/sh
# lucina-ci mutants-precheck: stands in for Verus. A relative path or a file named base.rs is the unchanged baseline: it verifies.
# (also base-<name>.rs). Any other absolute path is a mutated copy: it is refused.
f=
for a in "$@"; do
    case "$a" in
    -*) ;;
    *) f=$a ;;
    esac
done
case "$f" in
/*)
    case "${f##*/}" in
    base.rs | base-*.rs) kind=base ;;
    *) kind=mutant ;;
    esac
    ;;
*) kind=base ;;
esac
if [ -n "${LUCINA_STUB_LOG:-}" ]; then
    # The first call of a script is always its baseline (the scripts must print `baseline: verifies` first), even when it is called
    # on a copy under the script's temp dir.
    [ -s "$LUCINA_STUB_LOG" ] || kind=base
    echo "$kind" >> "$LUCINA_STUB_LOG"
fi
if [ -n "${LUCINA_PRE_CAP:-}" ]; then
    n=0
    [ -f "$LUCINA_PRE_CAP/n" ] && n=$(cat "$LUCINA_PRE_CAP/n")
    if [ "$kind" = base ]; then
        # Every text Verus is made to accept unchanged. A script runs one baseline per shipped file, before its own mutants or all
        # of them up front, so the tool later picks for each mutant the text that file was cut from.
        if [ -f "$f" ]; then
            k=0
            [ -f "$LUCINA_PRE_CAP/k" ] && k=$(cat "$LUCINA_PRE_CAP/k")
            k=$((k + 1))
            echo "$k" >"$LUCINA_PRE_CAP/k"
            mkdir -p "$LUCINA_PRE_CAP/_bases"
            cp "$f" "$LUCINA_PRE_CAP/_bases/$k"
        fi
    else
        n=$((n + 1))
        echo "$n" >"$LUCINA_PRE_CAP/n"
        d="$LUCINA_PRE_CAP/$n"
        mkdir -p "$d"
        [ -f "$f" ] && cp "$f" "$d/_arg"
        if [ -n "${LUCINA_PRE_REPO:-}" ]; then
            # Which file carries the planted bug: every file one directory below TMPDIR (the script's work dir) that also exists at
            # the same path in the repository and differs from it. Copied aside because the script deletes its work dir when it ends.
            t=${TMPDIR:-/tmp}
            t=${t%/}
            find "$t" -type f 2>/dev/null | while IFS= read -r p; do
                r=${p#"$t"/}
                case "$r" in
                */*) r=${r#*/} ;;
                *) continue ;;
                esac
                [ -f "$LUCINA_PRE_REPO/$r" ] || continue
                cmp -s "$p" "$LUCINA_PRE_REPO/$r" && continue
                mkdir -p "$d/$(dirname "$r")" && cp "$p" "$d/$r"
            done
        fi
    fi
fi
if [ "$kind" = base ]; then
    echo "verification results:: 1 verified, 0 errors"
    exit 0
fi
echo "error: postcondition not satisfied (lucina-ci mutants-precheck stub)"
echo "verification results:: 0 verified, 1 errors"
exit 1
"#;

/// In the capture, the file Verus was handed at that call: a script that builds its own input mutates it and nothing else.
const ARG: &str = "_arg";
/// In the capture, the texts the script ran as its baselines.
const BASES: &str = "_bases";

/// The coordinator-owned allow list of units whose inconclusive result is accepted: `unit<TAB>reason` per line, `#` comments.
/// Absent means empty.
pub const SKIP_LIST: &str = "docs/ci/mutants-precheck-skip.txt";
/// `unit<TAB>reason`. A bare unit name gets the `mutants/` it names in the inventory. A line without a tab is an error: an
/// allow list nobody can read is not an allow list.
pub fn parse_skip_list(text: &str) -> Result<BTreeMap<String, String>, String> {
    let mut m = BTreeMap::new();
    for (i, l) in text.lines().enumerate() {
        let l = l.trim_end();
        if l.trim().is_empty() || l.starts_with('#') {
            continue;
        }
        let (unit, reason) = l
            .split_once('\t')
            .ok_or_else(|| format!("{SKIP_LIST}:{}: expected unit<TAB>reason", i + 1))?;
        let unit = unit.trim();
        let name = if unit.contains('/') {
            unit.to_string()
        } else {
            format!("mutants/{unit}")
        };
        m.insert(name, reason.trim().to_string());
    }
    Ok(m)
}

/// The allow list of a tree; an absent file is an empty list.
pub fn load_skip_list(root: &Path) -> Result<BTreeMap<String, String>, String> {
    let p = root.join(SKIP_LIST);
    if !p.exists() {
        return Ok(BTreeMap::new());
    }
    parse_skip_list(&fs::read_to_string(&p).map_err(|e| format!("{SKIP_LIST}: {e}"))?)
}

/// What a script printed that matters here.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Parsed {
    /// `NOT FOUND ...` and `NOT CHANGED ...` lines, as printed.
    pub findings: Vec<String>,
    /// `NOT REFUSED ...` lines: the stub did not fit that script.
    pub not_refused: usize,
}

pub fn parse_output(text: &str) -> Parsed {
    let mut p = Parsed::default();
    for l in text.lines() {
        if l.starts_with("NOT FOUND") || l.starts_with("NOT CHANGED") {
            p.findings.push(l.to_string());
        } else if l.starts_with("NOT REFUSED") {
            p.not_refused += 1;
        }
    }
    p
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Every mutant's text was found and changed the file; `n` mutants reached the (stub) verifier.
    Clean(usize),
    Fail(Vec<String>),
    /// A mutant whose replaced text matches more than once in the file it was taken from: the script takes the first match, which
    /// may not be the one meant. A note only, never a failure (docs/ci/PROOF-WORKFLOW.md).
    Ambiguous(Vec<String>),
    /// The run says nothing about at least one mutant: the script stopped early, timed out, did not fit the stub, or a mutated copy
    /// could not be read against its original.
    Inconclusive(String),
}

/// Which mutants units to check. `only` names one (`mutants/x` or `x`); `all` takes every one; otherwise the units a changed file reaches.
pub fn select<'a>(
    units: &'a [Unit],
    changed: &[String],
    only: Option<&str>,
    all: bool,
) -> Result<Vec<&'a Unit>, String> {
    /// The kinds this precheck covers: the per-unit scripts and the global mutation list.
    fn covered(u: &Unit) -> bool {
        matches!(u.kind.as_str(), "mutants" | "mutations")
    }
    let mutants = units.iter().filter(|u| covered(u));
    if let Some(name) = only {
        let full = if name.contains('/') {
            name.to_string()
        } else {
            format!("mutants/{name}")
        };
        return units
            .iter()
            .find(|u| covered(u) && u.name == full)
            .map(|u| vec![u])
            .ok_or_else(|| format!("no mutants unit {full}"));
    }
    if all {
        return Ok(mutants.collect());
    }
    Ok(mutants
        .filter(|u| changed.iter().any(|p| u.affected_by(p)))
        .collect())
}

/// The units of a tree: the committed inventory when it is there, else every `verification/*/mutants.sh` on disk (an export).
pub fn units_of(root: &Path) -> Result<Vec<Unit>, String> {
    if root.join(inventory::INVENTORY).exists() {
        return inventory::load(root);
    }
    let mut v = Vec::new();
    let mut dirs: Vec<String> = fs::read_dir(root.join("verification"))
        .map_err(|e| format!("verification/: {e}"))?
        .flatten()
        .filter(|e| e.path().join("mutants.sh").exists())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    dirs.sort();
    for d in dirs {
        v.push(Unit {
            name: format!("mutants/{d}"),
            group: "none".into(),
            kind: "mutants".into(),
            target: format!("verification/{d}/mutants.sh"),
            inputs: vec![format!("verification/{d}/")],
        });
    }
    Ok(v)
}

/// Writes the stub into `dir` and returns its path.
pub fn write_stub(dir: &Path) -> Result<PathBuf, String> {
    use std::os::unix::fs::PermissionsExt;
    fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let p = dir.join("verus-stub");
    fs::write(&p, STUB).map_err(|e| format!("{}: {e}", p.display()))?;
    fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    Ok(p)
}

fn kill_group(pid: u32) {
    let _ = Command::new("kill")
        .args(["-s", "KILL", "--", &format!("-{pid}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Every pid below `pid` in the process tree, `pid` first, then its descendants and so on.
///
/// A script that leaves a child in a new session (`setsid x &`) takes that child out of the reach of the process-group kill, but
/// while its own parent lives the link is in `/proc`, and a session leader is its own process group — so signalling the group of
/// every pid on this list reaches it. What this cannot see is a child already orphaned before the timeout (its parent died and it
/// was re-parented to init): only a cgroup sweep, which `check-fast` does for the whole step, reaches that one.
fn tree(pid: u32) -> Vec<u32> {
    let mut kids: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for e in fs::read_dir("/proc").into_iter().flatten().flatten() {
        let Ok(p) = e.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        // "pid (comm) state ppid ...", and comm holds anything the program was called with, spaces and `)` included.
        let Ok(stat) = fs::read_to_string(e.path().join("stat")) else {
            continue;
        };
        let Some(after) = stat.rfind(')').map(|i| &stat[i + 1..]) else {
            continue;
        };
        let mut f = after.split_whitespace();
        let _state = f.next();
        if let Some(pp) = f.next().and_then(|x| x.parse::<u32>().ok()) {
            kids.entry(pp).or_default().push(p);
        }
    }
    let mut out = Vec::new();
    let mut stack = vec![pid];
    while let Some(p) = stack.pop() {
        out.push(p);
        if let Some(c) = kids.get(&p) {
            stack.extend(c.iter().copied());
        }
    }
    out
}

/// The PATH handed to anything the precheck runs: its own, or a plain system one.
fn scrubbed_path() -> std::ffi::OsString {
    std::env::var_os("PATH").unwrap_or_else(|| std::ffi::OsString::from("/usr/bin:/bin"))
}

/// The environment of a script: `PATH`, `HOME` (a private directory), `TMPDIR`, `LANG` and what the precheck itself needs.
/// No variable of the session that runs the tool is passed. This is a scrub, not a sandbox: the script runs as this user, so it
/// resolves `~/.ssh` through the account database whatever `HOME` says, reaches the sockets under `/run/user/<uid>`, and writes
/// anywhere that user may write, outside its directory.
fn script_env(cmd: &mut Command, root: &Path, stub: &Path, work: &Path, log: &Path) {
    cmd.env_clear()
        .env("PATH", scrubbed_path())
        .env("HOME", work.join("home"))
        .env("TMPDIR", work.join("tmp"))
        .env(
            "LANG",
            std::env::var_os("LANG").unwrap_or_else(|| std::ffi::OsString::from("C")),
        )
        .env("VERUS", stub)
        .env("LUCINA_STUB_LOG", log)
        .env("LUCINA_PRE_CAP", work.join("cap"))
        .env("LUCINA_PRE_REPO", root);
}

/// The byte range of the replaced region of two versions of one file: the longest common prefix and suffix removed.
pub fn region(orig: &[u8], mutant: &[u8]) -> (usize, usize) {
    let max = orig.len().min(mutant.len());
    let mut a = 0;
    while a < max && orig[a] == mutant[a] {
        a += 1;
    }
    let mut b = 0;
    while b < max - a && orig[orig.len() - 1 - b] == mutant[mutant.len() - 1 - b] {
        b += 1;
    }
    (a, orig.len() - b)
}

/// How often `needle` occurs in `hay` (non-overlapping). An empty needle or a file that is not text says nothing: 0.
pub fn count_occurrences(hay: &[u8], needle: &[u8]) -> usize {
    let (Ok(s), Ok(n)) = (std::str::from_utf8(hay), std::str::from_utf8(needle)) else {
        return 0;
    };
    if n.is_empty() {
        return 0;
    }
    let (mut count, mut at) = (0, 0);
    while let Some(k) = s[at..].find(n) {
        count += 1;
        at += k + n.len();
    }
    count
}

/// A region as shown in a finding: on one line, clipped.
fn shown(b: &[u8]) -> String {
    let t = String::from_utf8_lossy(b)
        .replace('\n', "\\n")
        .replace('\t', " ");
    clip(&t)
}

/// Every file below `dir`, as repo-relative paths (the capture keeps the tree the script made).
fn captured(dir: &Path, rel: &Path, out: &mut Vec<String>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let p = e.path();
        let r = if rel.as_os_str().is_empty() {
            PathBuf::from(&name)
        } else {
            rel.join(&name)
        };
        if p.is_dir() {
            captured(&p, &r, out);
        } else if p.is_file() {
            out.push(r.to_string_lossy().into_owned());
        }
    }
}

/// What the copies of one run say: the `AMBIGUOUS` findings, and the first call whose copy could not be read against an original.
pub struct Analysed {
    pub findings: Vec<String>,
    pub unclear: Option<String>,
}

/// What the tool says about a mutant call whose copy it cannot read against anything: neither a shipped file at its repo path nor
/// one baseline of the script it could have been cut from.
fn unreadable(i: usize, bases: usize) -> String {
    format!(
        "mutant call {i}: no copy of the planted bug is readable against the repository or against a baseline of the script ({bases} kept), so its search text is not checked"
    )
}

/// The text a captured Verus input was cut from: the baseline it shares the most text with, provided that is over half of it.
/// A script runs one baseline per shipped file, some of them all together at its start, so which mutant came from which baseline
/// is not readable from the order of the calls.
fn best_base(bases: &[Vec<u8>], mutant: &[u8]) -> Option<Vec<u8>> {
    bases
        .iter()
        .filter(|o| !o.is_empty() && shared(o, mutant) * 2 >= o.len())
        .max_by_key(|o| shared(o, mutant))
        .cloned()
}

/// How much of `orig` stands unchanged at the two ends of the diff with `mutant`: everything but the replaced region.
fn shared(orig: &[u8], mutant: &[u8]) -> usize {
    let (a, b) = region(orig, mutant);
    a + orig.len() - b
}

/// Compares every captured mutant copy with the text it stands for, and counts how often the original of the replaced region
/// occurs there. A search text that occurs more than once is a finding: the plant lands on whichever copy bash meets first, so
/// the mutant is not necessarily the bug the line claims. A call whose copy cannot be compared at all is reported as unclear.
pub fn analyse_capture(root: &Path, cap: &Path, calls: usize) -> Analysed {
    let (mut findings, mut unclear) = (Vec::new(), None);
    let bases: Vec<Vec<u8>> = fs::read_dir(cap.join(BASES))
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| fs::read(e.path()).ok())
                .collect()
        })
        .unwrap_or_default();
    for i in 1..=calls {
        let dir = cap.join(i.to_string());
        let mut files = Vec::new();
        captured(&dir, Path::new(""), &mut files);
        files.retain(|f| f != ARG);
        files.sort();
        // Either the script planted into a copy of a shipped file at its repo path, or into a file it built for Verus alone:
        // then the text it stands for is the baseline that copy was cut from.
        let (orig, mutant, file) = match files.len() {
            1 => {
                let file = files.remove(0);
                match (fs::read(dir.join(&file)), fs::read(root.join(&file))) {
                    (Ok(m), Ok(o)) => (o, m, file),
                    _ => {
                        unclear.get_or_insert_with(|| {
                            format!(
                                "mutant call {i}: {file} cannot be read on one side or the other"
                            )
                        });
                        continue;
                    }
                }
            }
            0 => {
                let named = format!("{ARG} (the file Verus was handed)");
                let cut = fs::read(dir.join(ARG))
                    .ok()
                    .and_then(|m| best_base(&bases, &m).map(|o| (o, m, named.clone())));
                match cut {
                    Some(t) => t,
                    None => {
                        unclear.get_or_insert_with(|| unreadable(i, bases.len()));
                        continue;
                    }
                }
            }
            n => {
                unclear.get_or_insert_with(|| {
                    format!("mutant call {i}: {n} files differ from the repository ({}), so the planted bug is not attributable", files.join(", "))
                });
                continue;
            }
        };
        let (mut a, mut b) = region(&orig, &mutant);
        // The cut may fall inside a multi-byte character; widen it to the character, which only ever adds shared text.
        while a > 0 && a < orig.len() && orig[a] & 0xc0 == 0x80 {
            a -= 1;
        }
        while b < orig.len() && orig[b] & 0xc0 == 0x80 {
            b += 1;
        }
        let count = count_occurrences(&orig, &orig[a..b]);
        if count > 1 {
            findings.push(format!(
                "AMBIGUOUS (matches {count} times) in {file}, mutant call {i}: {}",
                shown(&orig[a..b])
            ));
        }
    }
    Analysed { findings, unclear }
}

/// The script of a unit and the arguments its name carries. A mutants unit that declares `# mutants-units:` names targets
/// `verification/<x>/mutants.sh <name>` (inventory.rs), and the precheck has to run the script with that name: with none the
/// script takes its default proof, so the other names' mutants are never reached.
pub fn script_and_args(target: &str) -> (&str, Vec<&str>) {
    let mut words = target.split_whitespace();
    let script = words.next().unwrap_or(target);
    (script, words.collect())
}

/// Runs one unit's script under the stub. `work` is a private directory for this unit (TMPDIR of the script, the log, the copies).
pub fn check_unit(root: &Path, unit: &Unit, stub: &Path, work: &Path, limit: Duration) -> Outcome {
    let tmp = work.join("tmp");
    let cap = work.join("cap");
    let home = work.join("home");
    for d in [&tmp, &cap, &home] {
        if let Err(e) = fs::create_dir_all(d) {
            return Outcome::Inconclusive(format!("{}: {e}", d.display()));
        }
    }
    let (script, args) = script_and_args(&unit.target);
    if !root.join(script).exists() {
        return Outcome::Inconclusive(format!("{} is missing", unit.target));
    }
    let out = work.join("script.out");
    let log = work.join("stub.log");
    let file = match fs::File::create(&out) {
        Ok(f) => f,
        Err(e) => return Outcome::Inconclusive(format!("{}: {e}", out.display())),
    };
    let file2 = match file.try_clone() {
        Ok(f) => f,
        Err(e) => return Outcome::Inconclusive(e.to_string()),
    };
    let mut cmd = Command::new("bash");
    cmd.arg(script).args(args).current_dir(root);
    script_env(&mut cmd, root, stub, work, &log);
    let mut child = match cmd
        .stdin(Stdio::null())
        .stdout(file)
        .stderr(file2)
        .process_group(0)
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return Outcome::Inconclusive(format!("cannot run bash: {e}")),
    };
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if start.elapsed() >= limit => {
                // Deepest first: a parent killed before its child could re-parent that child out of reach. The script itself is
                // killed as the group it leads; every other pid on the list is killed as the group it leads, which is what a
                // `setsid` child is.
                let below = tree(child.id());
                kill_group(child.id());
                for p in below.iter().rev().filter(|p| **p != child.id()) {
                    kill_group(*p);
                }
                let _ = child.wait();
                return Outcome::Inconclusive(format!("timed out after {} s", limit.as_secs()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return Outcome::Inconclusive(e.to_string()),
        }
    };
    let text = fs::read_to_string(&out).unwrap_or_default();
    let p = parse_output(&text);
    if !p.findings.is_empty() {
        return Outcome::Fail(p.findings);
    }
    let mutants = fs::read_to_string(&log)
        .unwrap_or_default()
        .lines()
        .filter(|l| *l == "mutant")
        .count();
    if p.not_refused > 0 {
        return Outcome::Inconclusive(format!(
            "NOT REFUSED printed {} time(s): the stub does not fit this script's way of calling Verus",
            p.not_refused
        ));
    }
    // inventory.rs makes every script print both lines; one missing means the script stopped before its mutants, or before its end.
    let reached_end =
        text.contains("baseline: verifies") && text.contains("refused by verification");
    if mutants == 0 || !reached_end {
        let last = text.lines().rev().find(|l| !l.trim().is_empty());
        return Outcome::Inconclusive(format!(
            "the script did not run to its closing line, so not every mutant was checked ({mutants} reached the verifier); last line: {}",
            last.unwrap_or("(no output)")
        ));
    }
    // A script that ends non-zero without a finding line of its own failed for a reason the tool cannot read. Taking the exit code
    // on faith would be fail-open: hiding the marker (`>/dev/null`, an indent) was enough to make a red unit look checked.
    if let Some(s) = status.filter(|s| !s.success()) {
        let how = match s.code() {
            Some(c) => format!("exited {c}"),
            None => "was killed by a signal".to_string(),
        };
        let last = text.lines().rev().find(|l| !l.trim().is_empty());
        return Outcome::Inconclusive(format!(
            "the script {how} without printing a finding line, so its own check failed (last line: {}); nothing is known about its {mutants} mutant(s)",
            last.unwrap_or("(no output)")
        ));
    }
    let a = analyse_capture(root, &cap, mutants);
    if let Some(why) = a.unclear {
        return Outcome::Inconclusive(why);
    }
    if !a.findings.is_empty() {
        return Outcome::Ambiguous(a.findings);
    }
    Outcome::Clean(mutants)
}

fn clip(s: &str) -> String {
    if s.chars().count() > 220 {
        format!("{}...", s.chars().take(220).collect::<String>())
    } else {
        s.to_string()
    }
}

/// One entry of the global mutation list: `file|sed expression|what it breaks`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mutation {
    pub file: String,
    pub expr: String,
    pub what: String,
}

/// Reads the `mutations=( ... )` array of `scripts/verify-mutations.sh`: one quoted `file|sed|what` entry per line. The script
/// splits each entry on `|`, so an expression cannot hold one. Returns the entries and the lines that do not have that shape
/// (a quoted line, no continuation): those are reported, never skipped, or the list would be silently uncovered.
pub fn parse_mutations(text: &str) -> (Vec<Mutation>, Vec<String>) {
    let (mut list, mut malformed) = (Vec::new(), Vec::new());
    let mut inside = false;
    for (n, l) in text.lines().enumerate() {
        let t = l.trim();
        if !inside {
            inside = t.starts_with("mutations=(");
            continue;
        }
        if t.starts_with(')') {
            break;
        }
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let quoted = t.starts_with('\'') || t.starts_with('"');
        let e = t.trim_matches(|c| c == '\'' || c == '"');
        let parts: Vec<&str> = e.split('|').collect();
        if !(quoted
            && !t.ends_with('\\')
            && parts.len() == 3
            && !parts[0].is_empty()
            && !parts[1].is_empty())
        {
            malformed.push(format!("{}:{}", n + 1, clip(t)));
            continue;
        }
        list.push(Mutation {
            file: parts[0].to_string(),
            expr: parts[1].to_string(),
            what: parts[2].to_string(),
        });
    }
    (list, malformed)
}

/// `sed -e EXPR FILE` with the same scrubbed environment, its output into `out`.
fn sed_into(root: &Path, expr: &str, file: &Path, out: &Path) -> Result<(), String> {
    use std::fs::File;
    let f = File::create(out).map_err(|e| format!("{}: {e}", out.display()))?;
    let o = Command::new("sed")
        .args(["-e", expr])
        .arg(file)
        .current_dir(root)
        .env_clear()
        .env("PATH", scrubbed_path())
        .stdin(Stdio::null())
        .stdout(f)
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("sed: {e}"))?;
    if o.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&o.stderr).trim().to_string())
    }
}

/// The global list, text-only and without Verus: every sed expression must change its file. A sed expression that matches nothing
/// is the same accident a `NOT FOUND` is in a per-unit script: the mutation no longer plants anything.
pub fn check_mutations_list(root: &Path, unit: &Unit, work: &Path) -> Outcome {
    if let Err(e) = fs::create_dir_all(work) {
        return Outcome::Inconclusive(format!("{}: {e}", work.display()));
    }
    let text = match fs::read_to_string(root.join(&unit.target)) {
        Ok(t) => t,
        Err(e) => return Outcome::Inconclusive(format!("{}: {e}", unit.target)),
    };
    let (list, malformed) = parse_mutations(&text);
    if list.is_empty() && malformed.is_empty() {
        return Outcome::Inconclusive(format!(
            "{}: no `mutations=( ... )` list of `file|sed|what` lines was found",
            unit.target
        ));
    }
    let mut findings: Vec<String> = malformed
        .into_iter()
        .map(|l| format!("NOT COVERED (the line is not a quoted `file|sed|what`): {l}"))
        .collect();
    let mut changed = 0;
    for (i, m) in list.iter().enumerate() {
        let file = root.join(&m.file);
        let Ok(orig) = fs::read(&file) else {
            findings.push(format!(
                "NOT FOUND: {} names {}, which is not in the tree",
                m.what, m.file
            ));
            continue;
        };
        let out = work.join(format!("sed{i}.out"));
        if let Err(e) = sed_into(root, &m.expr, &file, &out) {
            findings.push(format!("NOT CHANGED (sed failed): {}: {e}", m.what));
            continue;
        }
        match fs::read(&out) {
            Ok(mutant) if mutant == orig => findings.push(format!(
                "NOT CHANGED: {}: the sed expression no longer matches {}",
                m.what, m.file
            )),
            Ok(_) => changed += 1,
            Err(e) => findings.push(format!("NOT CHANGED (no output): {}: {e}", m.what)),
        }
    }
    if findings.is_empty() {
        return Outcome::Clean(changed);
    }
    Outcome::Fail(findings)
}

/// Checks the given units in turn: a `mutants` unit runs its own script under the stub, a `mutations` unit (the global list) is
/// read and applied text-only. Prints one line per unit; 1 when a unit has a finding, when `--merge-gate` turns an inconclusive
/// unit into one, or when `budget` ran out. Without `--merge-gate` an inconclusive unit is only a note.
/// `budget` comes from `total_budget`: a run that runs out of it fails and names what it never reached.
/// `ambiguity_detail` (`--verbose`) is the only way to get the list of twice-matching regions: about 700 of them in 64 units are
/// informational noise, so the run says one line about them and no more (docs/ci/PROOF-WORKFLOW.md).
pub fn run(
    root: &Path,
    units: &[&Unit],
    verbose: bool,
    merge_gate: bool,
    budget: Duration,
    ambiguity_detail: bool,
) -> Result<i32, String> {
    if units.is_empty() {
        println!("mutants-precheck: no mutants unit is reached by this change");
        return Ok(0);
    }
    let allowed = load_skip_list(root)?;
    // One base per call: two runs in the same process (the tests) must not delete each other's work.
    let base = std::env::temp_dir().join(format!(
        "mutants-pre-{}-{}",
        std::process::id(),
        RUN_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&base);
    let stub = write_stub(&base.join("stub"))?;
    let started = Instant::now();
    let (mut bad, mut clean, mut inconclusive, mut ambiguous, mut skipped, mut unchecked) =
        (0, 0, 0, 0, 0, 0);
    let mut amb_regions = 0;
    for (i, u) in units.iter().enumerate() {
        if started.elapsed() >= budget {
            unchecked = units.len() - i;
            println!(
                "FAIL mutants-precheck: out of time ({} s): {unchecked} unit(s) not checked, first {}",
                budget.as_secs(),
                u.name
            );
            break;
        }
        let t = Instant::now();
        let work = base.join(format!("u{i}"));
        let o = if u.kind == "mutations" {
            check_mutations_list(root, u, &work)
        } else {
            check_unit(root, u, &stub, &work, Duration::from_secs(PER_SCRIPT_SECS))
        };
        let secs = t.elapsed().as_secs_f64();
        match o {
            Outcome::Clean(n) => {
                clean += 1;
                if verbose {
                    println!(
                        "ok   {:<34} {secs:>5.1}s  {n} mutant(s) found their text",
                        u.name
                    );
                }
            }
            Outcome::Inconclusive(why) => match allowed.get(&u.name) {
                Some(reason) => {
                    skipped += 1;
                    println!(
                        "??   {:<34} {secs:>5.1}s  inconclusive, allowed by {SKIP_LIST} ({reason})",
                        u.name
                    );
                }
                None if merge_gate => {
                    bad += 1;
                    println!(
                        "FAIL {:<34} {secs:>5.1}s  inconclusive, so the change is not checked for this unit: {why}",
                        u.name
                    );
                    println!(
                        "       either repair {} or let the coordinator allow the unit in a line `{}<TAB>reason` of {SKIP_LIST}",
                        u.target, u.name
                    );
                }
                None => {
                    inconclusive += 1;
                    println!("??   {:<34} {secs:>5.1}s  inconclusive: {why}", u.name);
                }
            },
            Outcome::Ambiguous(lines) => {
                ambiguous += 1;
                amb_regions += lines.len();
                // A note about how the script was written, not about the change, and not evidence either way: about 700 regions
                // in 64 of the 65 units match twice, almost all of them a token as common as `=`. So the run says one line about
                // them; the list itself is `--verbose` only.
                if ambiguity_detail {
                    println!(
                        "??   {:<34} {secs:>5.1}s  {} mutant(s) whose replaced text matches more than once (informational, not a finding)",
                        u.name,
                        lines.len()
                    );
                    for l in lines.iter().take(5) {
                        println!("       {}", clip(l));
                    }
                    if lines.len() > 5 {
                        println!("       … {} more", lines.len() - 5);
                    }
                }
            }
            Outcome::Fail(lines) => {
                bad += 1;
                println!(
                    "FAIL {:<34} {secs:>5.1}s  {} mutant(s) whose search text is gone from the shipped file (re-wrapped by a formatter?)",
                    u.name,
                    lines.len()
                );
                for l in &lines {
                    println!("       {}", clip(l));
                }
                println!(
                    "       the script plants its bugs in: {}",
                    u.inputs.join(", ")
                );
                println!(
                    "       fix the `try` lines in {} to the file as it is now",
                    u.target
                );
            }
        }
        // The copies of one unit are only needed while that unit is read: TMPDIR is a tmpfs, so they are memory.
        let _ = fs::remove_dir_all(&work);
    }
    let _ = fs::remove_dir_all(&base);
    if ambiguous > 0 {
        // One line for the whole run, whatever the mode: the note is not about this change and repeats unit by unit.
        println!(
            "mutants-precheck: {amb_regions} ambiguous region(s) in {ambiguous} unit(s): informational, not a finding; see --verbose"
        );
    }
    println!(
        "mutants-precheck: {} unit(s), {clean} clean, {bad} failed, {ambiguous} ambiguous, {inconclusive} inconclusive, {skipped} allowed by the skip list, {unchecked} not checked, {:.1}s",
        units.len(),
        started.elapsed().as_secs_f64()
    );
    Ok((bad > 0 || unchecked > 0) as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(name: &str, inputs: &[&str]) -> Unit {
        Unit {
            name: format!("mutants/{name}"),
            group: "g".into(),
            kind: "mutants".into(),
            target: format!("verification/{name}/mutants.sh"),
            inputs: inputs.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn scratch(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("mutants-pre-test-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn stub_run(dir: &Path, arg: &str) -> (i32, String) {
        let stub = write_stub(dir).unwrap();
        let o = Command::new(&stub).arg(arg).output().unwrap();
        (
            o.status.code().unwrap(),
            String::from_utf8_lossy(&o.stdout).into_owned(),
        )
    }

    #[test]
    fn stub_verifies_baselines_and_refuses_mutants() {
        let d = scratch("stub");
        for base in [
            "verification/x/rules.rs",
            "rules.rs",
            "/w/base.rs",
            "/a/b/c/base.rs",
            "/w/base-snap_rules.rs",
        ] {
            let (c, out) = stub_run(&d, base);
            assert_eq!(c, 0, "{base}");
            assert!(out.contains("verification results:: 1 verified, 0 errors"));
        }
        for mutant in [
            "/w/mutant.rs",
            "/w/verification/x/rules.rs",
            "/w/rules.rs",
            "/w/not-base.rs",
            "/w/rebase.rs",
            "/w/basement.rs",
        ] {
            let (c, out) = stub_run(&d, mutant);
            assert_eq!(c, 1, "{mutant}");
            // worded as Verus words it, at the start of a line, because juno-surface and juno-wm-rules grep for exactly that.
            assert!(
                out.lines()
                    .any(|l| l.starts_with("error: postcondition not satisfied")),
                "{out}"
            );
            assert!(out.contains("verification results:: 0 verified, 1 errors"));
        }
        // flags before the path are skipped
        let stub = d.join("verus-stub");
        let o = Command::new(&stub)
            .args(["--expand-errors", "/w/mutant.rs"])
            .output()
            .unwrap();
        assert_eq!(o.status.code(), Some(1));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn stub_first_call_is_the_baseline_wherever_it_is() {
        let d = scratch("first");
        let stub = write_stub(&d).unwrap();
        let log = d.join("log");
        let call = |p: &str| {
            Command::new(&stub)
                .arg(p)
                .env("LUCINA_STUB_LOG", &log)
                .output()
                .unwrap()
                .status
                .code()
        };
        assert_eq!(call("/w/verification/x/rules.rs"), Some(0));
        assert_eq!(call("/w/verification/x/rules.rs"), Some(1));
        assert_eq!(call("/w/base.rs"), Some(0));
        assert_eq!(fs::read_to_string(&log).unwrap(), "base\nmutant\nbase\n");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn parser_takes_only_the_three_markers_at_line_start() {
        let p = parse_output(
            "baseline: verifies\n\
             NOT FOUND (a thing (code)): let x = 1;\n\
             refused: NOT FOUND in the middle\n\
             NOT CHANGED: y\n\
             NOT REFUSED: z -> w\n\
             refused: q -> r (1 errors)\n\
             mutants: 3 refused by verification, 0 refused without a verification run\n",
        );
        assert_eq!(
            p.findings,
            vec![
                "NOT FOUND (a thing (code)): let x = 1;".to_string(),
                "NOT CHANGED: y".to_string()
            ]
        );
        assert_eq!(p.not_refused, 1);
        assert_eq!(parse_output(""), Parsed::default());
    }

    #[test]
    fn selection_by_change_name_and_all() {
        let units = vec![
            unit("a", &["crates/a/", "verification/a/"]),
            unit("b", &["crates/b/", "verification/b/"]),
            Unit {
                name: "mutations/global".into(),
                group: "g".into(),
                kind: "mutations".into(),
                target: "scripts/verify-mutations.sh".into(),
                inputs: vec!["scripts/verify-mutations.sh".into(), "verification/".into()],
            },
            Unit {
                name: "verus/a/rules".into(),
                group: "g".into(),
                kind: "verus".into(),
                target: "verification/a/rules.rs".into(),
                inputs: vec!["crates/a/".into()],
            },
        ];
        let names = |v: Vec<&Unit>| v.iter().map(|u| u.name.clone()).collect::<Vec<_>>();
        let ch = vec!["crates/b/src/lib.rs".to_string()];
        assert_eq!(
            names(select(&units, &ch, None, false).unwrap()),
            vec!["mutants/b"]
        );
        // a change under verification/ reaches the unit whose spec it is, and the global list
        assert_eq!(
            names(select(&units, &["verification/a/x.rs".into()], None, false).unwrap()),
            vec!["mutants/a", "mutations/global"]
        );
        // a change to the global list itself reaches it and nothing else
        assert_eq!(
            names(select(&units, &["scripts/verify-mutations.sh".into()], None, false).unwrap()),
            vec!["mutations/global"]
        );
        assert!(
            select(&units, &["README.md".into()], None, false)
                .unwrap()
                .is_empty()
        );
        // a change under tools/ci reaches every unit (GLOBAL_INPUTS)
        assert_eq!(
            select(&units, &["tools/ci/x".into()], None, false)
                .unwrap()
                .len(),
            3
        );
        assert_eq!(select(&units, &[], None, true).unwrap().len(), 3);
        assert_eq!(
            names(select(&units, &[], Some("a"), false).unwrap()),
            vec!["mutants/a"]
        );
        assert_eq!(
            names(select(&units, &[], Some("mutations/global"), false).unwrap()),
            vec!["mutations/global"]
        );
        assert!(select(&units, &[], Some("verus/a/rules"), false).is_err());
        assert!(select(&units, &[], Some("nope"), false).is_err());
    }

    /// A tiny repo with one unit whose script is shaped like juno-ring's.
    fn fake_repo(tag: &str, shipped: &str) -> (PathBuf, Unit) {
        let root = scratch(tag);
        fs::create_dir_all(root.join("crates/fake/src")).unwrap();
        fs::create_dir_all(root.join("verification/fake")).unwrap();
        fs::write(root.join("crates/fake/src/lib.rs"), shipped).unwrap();
        fs::write(root.join("verification/fake/rules.rs"), "// proof\n").unwrap();
        fs::write(
            root.join("verification/fake/mutants.sh"),
            r#"#!/usr/bin/env bash
set -uo pipefail
verus=${VERUS:-/nonexistent/verus}
work=$(mktemp -d "${TMPDIR:-/tmp}/fake-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
fail=0
try() {
    local from=$1 to=$2
    mkdir -p "$work/crates/fake/src"
    cp crates/fake/src/lib.rs "$work/crates/fake/src/"
    local target=$work/crates/fake/src/lib.rs
    local text; text=$(<"$target")
    case "$text" in
        *"$from"*) ;;
        *) echo "NOT FOUND: $from"; fail=1; return ;;
    esac
    printf "%s\n" "${text/"$from"/"$to"}" > "$target"
    if cmp -s "$target" crates/fake/src/lib.rs; then echo "NOT CHANGED: $from"; fail=1; return; fi
    if "$verus" "$work/verification/fake/rules.rs" > "$work/out" 2>&1; then
        echo "NOT REFUSED: $from -> $to"; fail=1
    else
        echo "refused: $from -> $to"
    fi
}
if "$verus" verification/fake/rules.rs > "$work/baseline" 2>&1; then echo "baseline: verifies"; else echo "mutants: the unchanged files do not verify"; exit 1; fi
try 'let room = PAGE - offset;' 'let room = PAGE - offset + 1;'
try $'    } else {\n        head_page\n    };' $'    } else {\n        head_page + 1\n    };'
echo "mutants: 2 refused by verification, 0 refused without a verification run"
exit $fail
"#,
        )
        .unwrap();
        (
            root.clone(),
            unit("fake", &["crates/fake/", "verification/fake/"]),
        )
    }

    const ORIGINAL: &str = "fn f() {\n    let room = PAGE - offset;\n    if x {\n        a\n    } else {\n        head_page\n    };\n}\n";
    // rustfmt re-wrapped the second search text
    const REWRAPPED: &str =
        "fn f() {\n    let room = PAGE - offset;\n    if x { a } else { head_page };\n}\n";

    #[test]
    fn integration_original_passes_and_rewrapped_names_the_mutant() {
        let (root, u) = fake_repo("ok", ORIGINAL);
        let work = root.join(".work");
        let stub = write_stub(&work.join("stub")).unwrap();
        assert_eq!(
            check_unit(&root, &u, &stub, &work.join("u"), Duration::from_secs(20)),
            Outcome::Clean(2)
        );
        fs::write(root.join("crates/fake/src/lib.rs"), REWRAPPED).unwrap();
        match check_unit(&root, &u, &stub, &work.join("u2"), Duration::from_secs(20)) {
            Outcome::Fail(f) => {
                assert_eq!(f.len(), 1, "{f:?}");
                assert!(f[0].starts_with("NOT FOUND"), "{f:?}");
                // a multi-line search text shows its first line only
                assert!(f[0].contains("} else {"), "{f:?}");
            }
            o => panic!("{o:?}"),
        }
        let _ = fs::remove_dir_all(&root);
    }

    /// A script that proves several files and picks one by its first argument, which is what `# mutants-units:` declares
    /// (inventory.rs). Without the argument it runs only its default proof, and the precheck must say so rather than pass it.
    const NAMED: &str = r#"#!/usr/bin/env bash
set -uo pipefail
verus=${VERUS:?}
name=${1:-disk_rules}
work=$(mktemp -d "${TMPDIR:-/tmp}/named-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
fail=0
if [ "$name" != patch_rules ]; then echo "no mutants for $name"; exit 0; fi
try() {
    local from=$1 to=$2
    mkdir -p "$work/crates/n/src"
    cp crates/n/src/lib.rs "$work/crates/n/src/"
    local target=$work/crates/n/src/lib.rs
    local text; text=$(<"$target")
    case "$text" in
        *"$from"*) ;;
        *) echo "NOT FOUND: $from"; fail=1; return ;;
    esac
    printf "%s\n" "${text/"$from"/"$to"}" > "$target"
    if "$verus" "$work/verification/n/rules.rs" > "$work/out" 2>&1; then
        echo "NOT REFUSED: $from -> $to"; fail=1
    else
        echo "refused: $from -> $to"
    fi
}
if "$verus" verification/n/rules.rs > "$work/baseline" 2>&1; then echo "baseline: verifies"; else echo "mutants: the unchanged files do not verify"; exit 1; fi
try 'let room = PAGE - offset;' 'let room = PAGE - offset + 1;'
echo "mutants: 1 refused by verification, 0 refused without a verification run"
exit $fail
"#;

    fn named_units() -> (PathBuf, Unit, Unit) {
        let root = scratch("named");
        fs::create_dir_all(root.join("crates/n/src")).unwrap();
        fs::create_dir_all(root.join("verification/n")).unwrap();
        fs::write(root.join("crates/n/src/lib.rs"), ORIGINAL).unwrap();
        fs::write(root.join("verification/n/rules.rs"), "// proof\n").unwrap();
        fs::write(root.join("verification/n/mutants.sh"), NAMED).unwrap();
        let named = Unit {
            name: "mutants/n/patch_rules".into(),
            group: "g".into(),
            kind: "mutants".into(),
            target: "verification/n/mutants.sh patch_rules".into(),
            inputs: vec!["crates/n/".into(), "verification/n/".into()],
        };
        let bare = Unit {
            name: "mutants/n".into(),
            target: "verification/n/mutants.sh".into(),
            ..named.clone()
        };
        (root, named, bare)
    }

    #[test]
    fn a_named_unit_reaches_the_script_as_its_argument() {
        let (root, named, bare) = named_units();
        let work = root.join(".work");
        let stub = write_stub(&work.join("stub")).unwrap();
        // The name arrives, so that proof's one mutant is checked.
        assert_eq!(
            check_unit(
                &root,
                &named,
                &stub,
                &work.join("u"),
                Duration::from_secs(20)
            ),
            Outcome::Clean(1)
        );
        // The same script with no argument prints its default branch and none of its mutants: inconclusive, the hole found
        // in the Brimstone copy of this CI, where one unit stood for five proofs.
        match check_unit(
            &root,
            &bare,
            &stub,
            &work.join("b"),
            Duration::from_secs(20),
        ) {
            Outcome::Inconclusive(why) => {
                assert!(why.contains("did not run to its closing line"), "{why}");
            }
            o => panic!("{o:?}"),
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn integration_run_returns_one_for_a_failure_and_zero_for_clean() {
        let (root, u) = fake_repo("run", ORIGINAL);
        assert_eq!(
            run(&root, &[&u], true, false, total_budget(1), false).unwrap(),
            0
        );
        fs::write(root.join("crates/fake/src/lib.rs"), REWRAPPED).unwrap();
        assert_eq!(
            run(&root, &[&u], true, false, total_budget(1), false).unwrap(),
            1
        );
        assert_eq!(
            run(&root, &[], true, false, total_budget(0), false).unwrap(),
            0
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn out_of_time_is_a_failure_that_names_what_was_never_checked() {
        let (root, u) = fake_repo("late", ORIGINAL);
        // No time at all: the unit is not checked, and that is a failure. A precheck that ran out and passed would be worse
        // than one that never runs, because the change would look checked.
        assert_eq!(
            run(&root, &[&u], true, false, Duration::ZERO, false).unwrap(),
            1
        );
        // The same unit, with the budget a run of one unit is given, passes: so it was the time and not the script.
        assert_eq!(
            run(&root, &[&u], true, false, total_budget(1), false).unwrap(),
            0
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn the_budget_follows_the_size_of_the_list_and_its_step() {
        // A short list keeps the floor: an ordinary change is no slower than it was.
        for n in [0usize, 1, 5, 25] {
            assert_eq!(
                total_budget(n),
                Duration::from_secs(MIN_SECS),
                "{n} unit(s)"
            );
        }
        // A change under tools/ci/ reaches every mutants unit (`tools/ci/` is a GLOBAL_INPUTS entry) and the tree holds 65 of
        // them: measured 52.5 s one per process and 1.4 to 3.1 s per unit inside one run at load 27-59 with the swap full, a
        // fixed 100 s truncated that sweep at 46 of the 65 units and called it a failure, every unit it reached conclusive.
        assert_eq!(
            total_budget(65),
            Duration::from_secs(65 * UNIT_BUDGET_SECS),
            "a run over the whole tree is bounded by the tree"
        );
        assert!(total_budget(65) > Duration::from_secs(MIN_SECS));
        // Bounded however long the list; the step above the ceiling is a compile-time assert next to the constants.
        assert_eq!(total_budget(usize::MAX), Duration::from_secs(MAX_SECS));
        // The ceiling is MAX_SECS / UNIT_BUDGET_SECS units: past it a sweep of the tree truncates again and these constants
        // have to move. Better that this fails here than that the run does it in CI.
        // A crate built from an export without the tree says it skipped instead of failing: it has nothing to count.
        let real = match Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .canonicalize()
        {
            Ok(p) => p,
            Err(_) => {
                println!(
                    "skipped: no repository above this crate, so the size of the real list is not known here"
                );
                return;
            }
        };
        let Ok(us) = units_of(&real) else {
            println!(
                "skipped: {} holds no verification tree, so the size of the real list is not known here",
                real.display()
            );
            return;
        };
        let n = us
            .iter()
            .filter(|x| x.kind == "mutants" || x.kind == "mutations")
            .count() as u64;
        assert!(
            n.saturating_mul(UNIT_BUDGET_SECS) <= MAX_SECS,
            "{n} mutants units want {} s, over the {MAX_SECS} s ceiling: raise MAX_SECS and STEP_TIMEOUT_S",
            n * UNIT_BUDGET_SECS
        );
    }

    /// The crate the reviewer's three fixtures plant into: two texts that are there, one that is not.
    const X_LIB: &str = "pub fn f(a: u32) -> u32 {\n    if a > 3 { a - 3 } else { 0 }\n}\npub fn g(b: u32) -> u32 {\n    b.wrapping_mul(7)\n}\n";
    /// `try` as the real scripts write it, with the baseline call, and the two lines a script that got to its end prints.
    const TRY: &str = r#"#!/usr/bin/env bash
set -uo pipefail
verus=${VERUS:-/nonexistent/verus}
work=$(mktemp -d "${TMPDIR:-/tmp}/x-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
fail=0; refused=0; weak=0
try() {
    local from=$1 to=$2
    mkdir -p "$work/crates/x/src"
    cp crates/x/src/lib.rs "$work/crates/x/src/lib.rs"
    local target=$work/crates/x/src/lib.rs text
    text=$(<"$target")
    if [[ "$text" != *"$from"* ]]; then echo "NOT FOUND: $from"; fail=1; return; fi
    printf "%s\n" "${text/"$from"/"$to"}" > "$target"
    if cmp -s "$target" crates/x/src/lib.rs; then echo "NOT CHANGED: $from"; fail=1; return; fi
    if "$verus" "$target" > "$work/out" 2>&1; then
        echo "NOT REFUSED: $from -> $to"; fail=1
    else
        refused=$((refused + 1)); echo "refused: $from -> $to"
    fi
}
if "$verus" crates/x/src/lib.rs > "$work/baseline" 2>&1; then echo "baseline: verifies"; else echo "base fails"; exit 1; fi
"#;
    /// Two mutants that reach the verifier, so the script is not simply short of calls.
    const HONEST: &str = "try 'a - 3' 'a - 2'\ntry 'wrapping_mul(7)' 'wrapping_mul(6)'\n";
    const END: &str = "echo \"mutants: $refused refused by verification, $weak refused without a verification run\"\nexit $fail\n";

    fn exit_repo(tag: &str, body: &str) -> (PathBuf, Unit) {
        let root = scratch(tag);
        fs::create_dir_all(root.join("crates/x/src")).unwrap();
        fs::create_dir_all(root.join("verification/x")).unwrap();
        fs::write(root.join("crates/x/src/lib.rs"), X_LIB).unwrap();
        fs::write(
            root.join("verification/x/mutants.sh"),
            format!("{TRY}{HONEST}{body}{END}"),
        )
        .unwrap();
        (root, unit("x", &["crates/x/", "verification/x/"]))
    }

    /// All three end 1 while printing no line the tool can read as a finding: a red unit that looked checked.
    #[test]
    fn a_non_zero_exit_the_tool_cannot_see_a_finding_on_is_inconclusive() {
        for (tag, body, why) in [
            // the NOT FOUND of the third try is thrown away, and `fail=1` is the only trace left.
            (
                "fx-hidden",
                "try 'a - 99' 'a - 98' >/dev/null\n",
                "exited 1",
            ),
            // it is there, but indented: the markers are matched at line start, as in every real script.
            (
                "fx-indented",
                "try 'a - 99' 'a - 98' > >(sed 's/^/  /')\nsleep 0.2\n",
                "exited 1",
            ),
            // it fails for a reason of its own, after honest mutants and its closing line.
            (
                "fx-own-failure",
                "echo \"MISSING rules file\"; fail=1\n",
                "exited 1",
            ),
        ] {
            let (root, u) = exit_repo(tag, body);
            let work = root.join(".w");
            let stub = write_stub(&work.join("s")).unwrap();
            match check_unit(&root, &u, &stub, &work.join("u"), Duration::from_secs(20)) {
                Outcome::Inconclusive(w) => assert!(w.contains(why), "{tag}: {w}"),
                o => panic!("{tag}: {o:?}"),
            }
            // a local run notes it, the gate does not pass on it, and nothing but the skip list makes it green again.
            assert_eq!(
                run(&root, &[&u], false, false, total_budget(1), false).unwrap(),
                0,
                "{tag}: a note locally"
            );
            assert_eq!(
                run(&root, &[&u], false, true, total_budget(1), false).unwrap(),
                1,
                "{tag}: a failure at the gate"
            );
            fs::create_dir_all(root.join("docs/ci")).unwrap();
            fs::write(root.join(SKIP_LIST), "x\tthe coordinator allows this one\n").unwrap();
            assert_eq!(
                run(&root, &[&u], false, true, total_budget(1), false).unwrap(),
                0,
                "{tag}: only the allow list lets it through"
            );
            let _ = fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn a_hidden_not_found_line_that_exits_zero_still_fails_on_its_own_text() {
        // The other half: with the marker gone and a clean exit there is nothing left to read, so a run that does not note the
        // exit code must still not call this clean. Same script, the failing `try` dropped: clean.
        let (root, u) = exit_repo("fx-good-exit", "");
        assert_eq!(
            run(&root, &[&u], false, true, total_budget(1), false).unwrap(),
            0,
            "two honest mutants and exit 0 is a pass"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn inconclusive_is_a_note_locally_and_a_failure_at_the_gate() {
        let (root, u) = fake_repo("gate", ORIGINAL);
        // A script that stops before its closing line: nothing is known about its mutants.
        fs::write(
            root.join("verification/fake/mutants.sh"),
            "#!/usr/bin/env bash\necho 'baseline: verifies'\necho 'refused: a'\nexit 1\n",
        )
        .unwrap();
        assert_eq!(
            run(&root, &[&u], false, false, total_budget(1), false).unwrap(),
            0,
            "a local run only notes it"
        );
        assert_eq!(
            run(&root, &[&u], false, true, total_budget(1), false).unwrap(),
            1,
            "the merge gate must not pass on it"
        );
        fs::create_dir_all(root.join("docs/ci")).unwrap();
        fs::write(
            root.join(SKIP_LIST),
            "# the coordinator's allow list\nmutants/fake\tits script is between two dialects\n",
        )
        .unwrap();
        assert_eq!(
            run(&root, &[&u], false, true, total_budget(1), false).unwrap(),
            0,
            "allowed"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_script_that_never_reaches_the_verifier_is_inconclusive_not_clean() {
        let root = scratch("early");
        fs::create_dir_all(root.join("verification/e")).unwrap();
        fs::write(
            root.join("verification/e/mutants.sh"),
            "#!/usr/bin/env bash\necho 'BASE FAILS'\nexit 1\n",
        )
        .unwrap();
        let work = root.join(".w");
        let stub = write_stub(&work.join("s")).unwrap();
        let o = check_unit(
            &root,
            &unit("e", &[]),
            &stub,
            &work.join("u"),
            Duration::from_secs(10),
        );
        assert!(
            matches!(&o, Outcome::Inconclusive(w) if w.contains("BASE FAILS")),
            "{o:?}"
        );
        let missing = check_unit(
            &root,
            &unit("zz", &[]),
            &stub,
            &work.join("v"),
            Duration::from_secs(10),
        );
        assert!(matches!(missing, Outcome::Inconclusive(_)));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_script_that_dies_after_some_mutants_is_inconclusive() {
        let root = scratch("died");
        fs::create_dir_all(root.join("verification/d")).unwrap();
        fs::write(
            root.join("verification/d/mutants.sh"),
            "#!/usr/bin/env bash\necho 'baseline: verifies'\n\"$VERUS\" /tmp/zz/mutant.rs\necho 'refused: a'\nexit 1\n",
        )
        .unwrap();
        let work = root.join(".w");
        let stub = write_stub(&work.join("s")).unwrap();
        let o = check_unit(
            &root,
            &unit("d", &[]),
            &stub,
            &work.join("u"),
            Duration::from_secs(10),
        );
        assert!(
            matches!(&o, Outcome::Inconclusive(w) if w.contains("closing line")),
            "{o:?}"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_slow_script_times_out_as_inconclusive() {
        let root = scratch("slow");
        fs::create_dir_all(root.join("verification/s")).unwrap();
        fs::write(
            root.join("verification/s/mutants.sh"),
            "#!/usr/bin/env bash\nsleep 30\n",
        )
        .unwrap();
        let work = root.join(".w");
        let stub = write_stub(&work.join("s")).unwrap();
        let t = Instant::now();
        let o = check_unit(
            &root,
            &unit("s", &[]),
            &stub,
            &work.join("u"),
            Duration::from_millis(300),
        );
        assert!(
            matches!(&o, Outcome::Inconclusive(w) if w.contains("timed out")),
            "{o:?}"
        );
        assert!(t.elapsed() < Duration::from_secs(10));
        let _ = fs::remove_dir_all(&root);
    }

    /// Is `pid` still a live process? Read from `/proc`, not with `kill -0`, because `kill -0` answers for a zombie too: after a
    /// SIGKILL the task stays in the table until it is scheduled off its exit work and reaped, which under load takes seconds and
    /// says nothing about whether the kill landed. `Z` (and `X`) is dead. The command line must still name `sleep`, or the pid has
    /// been given to somebody else and is not ours.
    fn alive(pid: &str) -> bool {
        let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
            return false;
        };
        // "pid (comm) state ppid ...", and comm holds anything the program was called with, spaces and `)` included.
        let state = stat
            .rsplit_once(')')
            .map(|(_, rest)| rest.split_whitespace().next().unwrap_or(""))
            .unwrap_or("");
        !state.is_empty()
            && state != "Z"
            && state != "X"
            && String::from_utf8_lossy(
                &fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default(),
            )
            .contains("sleep")
    }

    /// How long the kill is given to show up in `/proc` before the test calls it a leak: the signal is delivered synchronously,
    /// only its effect is scheduled, and a loaded box schedules slowly. This waits for a death it does not cause.
    const DEATH_GRACE: Duration = Duration::from_secs(10);

    /// Poll `alive` until it says gone or the grace is over; true when it is gone.
    fn died(pid: &str) -> bool {
        let t = Instant::now();
        while alive(pid) {
            if t.elapsed() >= DEATH_GRACE {
                return false;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        true
    }

    /// The leak the reviewer found in fixture f4: `sleep 95 &` died with the process group, `setsid sleep 97` did not. A timeout
    /// now walks the tree and signals every group on it, so a script of this shape leaves nothing behind. Standalone that is the
    /// whole story; `check-fast` also sweeps the step's cgroup, which reaches even an orphan (docs/ci/PROOF-WORKFLOW.md).
    ///
    /// The child is waited for, not assumed dead the moment `check_unit` returns: the SIGKILL is sent synchronously but the task
    /// leaves `/proc` only once it has been scheduled off its exit work and reaped, and a box at load 40 was slow enough for that
    /// to read as a leak. A grandchild that is still live after `DEATH_GRACE` still fails here, and the same test on the branch
    /// before the tree kill failed it in one run of the suite (that is the negative control for the wait).
    #[test]
    fn a_timeout_kills_a_grandchild_that_left_the_process_group() {
        // Negative control first, so the wait below cannot pass by reading a live process as gone: a plain `sleep` of ours reads
        // alive, and after it is killed and reaped it reads dead. A leaked grandchild is a live sleep, like this one.
        let mut control = Command::new("sleep").arg("30").spawn().unwrap();
        let cpid = control.id().to_string();
        // `spawn` returns once the child exists, not once its exec has landed, and `cmdline` is empty until it has: give the
        // control the same moment any real process needs to become what it was called as.
        let t = Instant::now();
        while !alive(&cpid) && t.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            alive(&cpid),
            "the liveness check never reads a live sleep as alive: it cannot detect a leak either"
        );
        let _ = Command::new("kill").args(["-KILL", &cpid]).status();
        let _ = control.wait();
        assert!(!alive(&cpid), "a killed and reaped sleep still reads alive");

        let root = scratch("leak");
        fs::create_dir_all(root.join("verification/l")).unwrap();
        fs::write(
            root.join("verification/l/mutants.sh"),
            "#!/usr/bin/env bash\nsetsid sleep 30 & echo $! > \"$LUCINA_PRE_REPO/leak.pid\"\nsleep 45\n",
        )
        .unwrap();
        let work = root.join(".w");
        let stub = write_stub(&work.join("s")).unwrap();
        let t = Instant::now();
        let o = check_unit(
            &root,
            &unit("l", &[]),
            &stub,
            &work.join("u"),
            Duration::from_secs(2),
        );
        assert!(
            matches!(&o, Outcome::Inconclusive(w) if w.contains("timed out")),
            "{o:?}"
        );
        assert!(
            t.elapsed() < Duration::from_secs(30),
            "the timeout itself ran {:.1} s late",
            t.elapsed().as_secs_f64()
        );
        // The pid file is there, so the session did start, and both sleeps had far longer to run than the 2 s the script got.
        let pid = fs::read_to_string(root.join("leak.pid")).unwrap();
        let pid = pid.trim();
        assert!(
            pid.parse::<u32>().is_ok(),
            "the script never got as far as the setsid: {pid:?}"
        );
        assert!(
            died(pid),
            "the setsid grandchild {pid} was still live {DEATH_GRACE:?} after the timed-out script was killed"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn not_refused_is_a_dialect_problem_not_a_failure() {
        let root = scratch("nr");
        fs::create_dir_all(root.join("verification/n")).unwrap();
        fs::write(
            root.join("verification/n/mutants.sh"),
            "#!/usr/bin/env bash\n\"$VERUS\" mutant.rs\necho 'NOT REFUSED: x'\n",
        )
        .unwrap();
        let work = root.join(".w");
        let stub = write_stub(&work.join("s")).unwrap();
        let o = check_unit(
            &root,
            &unit("n", &[]),
            &stub,
            &work.join("u"),
            Duration::from_secs(10),
        );
        assert!(
            matches!(&o, Outcome::Inconclusive(w) if w.contains("NOT REFUSED")),
            "{o:?}"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn units_of_an_export_without_an_inventory_scans_the_scripts() {
        let root = scratch("export");
        for d in ["b", "a"] {
            fs::create_dir_all(root.join(format!("verification/{d}"))).unwrap();
            fs::write(root.join(format!("verification/{d}/mutants.sh")), "").unwrap();
        }
        fs::create_dir_all(root.join("verification/c")).unwrap();
        let us = units_of(&root).unwrap();
        assert_eq!(
            us.iter().map(|u| u.name.as_str()).collect::<Vec<_>>(),
            vec!["mutants/a", "mutants/b"]
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn skip_list_is_units_and_tabs_with_reasons() {
        let m = parse_skip_list(
            "# why a unit is allowed to stay inconclusive\n\njuno-ring\tits script is between two dialects\nmutants/other\ttimed out on the slow lane\n",
        )
        .unwrap();
        assert_eq!(m.len(), 2);
        assert_eq!(m["mutants/juno-ring"], "its script is between two dialects");
        assert_eq!(m["mutants/other"], "timed out on the slow lane");
        assert!(parse_skip_list("").unwrap().is_empty());
        assert_eq!(
            load_skip_list(Path::new("/nonexistent-tree"))
                .unwrap()
                .len(),
            0
        );
        assert!(
            parse_skip_list("no tab here")
                .unwrap_err()
                .contains("expected unit<TAB>reason")
        );
    }

    /// The region the script actually replaced, and how often the original text of it occurs.
    #[test]
    fn region_and_count() {
        let orig = b"let k = AAAA;\nlet k = AAAA;\n";
        let mutant = b"let k = BBBB;\nlet k = AAAA;\n";
        let (a, b) = region(orig, mutant);
        assert_eq!(&orig[a..b], b"AAAA");
        assert_eq!(count_occurrences(orig, &orig[a..b]), 2);
        // the replaced text occurs once: no finding
        let once = b"let k = AAAA;\n";
        let (a, b) = region(once, b"let k = BBBB;\n");
        assert_eq!(count_occurrences(once, &once[a..b]), 1);
        assert_eq!(count_occurrences(orig, b""), 0);
        // an insertion: the original region is empty, which says nothing
        let (a, b) = region(b"ab\n", b"aXb\n");
        assert_eq!(a, b);
    }

    /// A unit whose search text is in the shipped file twice: the script replaces the first match, the tool must say so.
    fn twice_repo(tag: &str) -> (PathBuf, Unit) {
        let root = scratch(tag);
        fs::create_dir_all(root.join("crates/two/src")).unwrap();
        fs::create_dir_all(root.join("verification/two")).unwrap();
        fs::write(
            root.join("crates/two/src/lib.rs"),
            "fn a() {\n    let k = AAAA;\n}\nfn b() {\n    let k = AAAA;\n}\n",
        )
        .unwrap();
        fs::write(root.join("verification/two/mutants.sh"), SCRIPT).unwrap();
        (root, unit("two", &["crates/two/", "verification/two/"]))
    }

    const SCRIPT: &str = r#"#!/usr/bin/env bash
set -uo pipefail
verus=${VERUS:-/nonexistent/verus}
work=$(mktemp -d "${TMPDIR:-/tmp}/two-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
try() {
    local from=$1 to=$2
    mkdir -p "$work/crates/two/src"
    cp crates/two/src/lib.rs "$work/crates/two/src/"
    local target=$work/crates/two/src/lib.rs text
    text=$(<"$target")
    printf "%s\n" "${text/"$from"/"$to"}" > "$target"
    "$verus" "$work/verification/two/rules.rs" >/dev/null 2>&1
    echo "refused: $from -> $to"
}
"$verus" verification/two/rules.rs >/dev/null 2>&1 && echo "baseline: verifies"
try 'let k = AAAA;' 'let k = BBBB;'
echo "mutants: 1 refused by verification, 0 refused without a verification run"
"#;

    #[test]
    fn a_search_text_that_matches_twice_is_a_finding() {
        let (root, u) = twice_repo("twice");
        let work = root.join(".w");
        let stub = write_stub(&work.join("s")).unwrap();
        let o = check_unit(&root, &u, &stub, &work.join("u"), Duration::from_secs(20));
        match o {
            Outcome::Ambiguous(f) => {
                assert_eq!(f.len(), 1, "{f:?}");
                assert!(f[0].starts_with("AMBIGUOUS (matches 2 times)"), "{f:?}");
                assert!(f[0].contains("crates/two/src/lib.rs"), "{f:?}");
                assert!(f[0].contains("AAAA"), "{f:?}");
            }
            o => panic!("{o:?}"),
        }
        // the same script on a file where the text is unique is clean
        fs::write(
            root.join("crates/two/src/lib.rs"),
            "fn a() {\n    let k = AAAA;\n}\n",
        )
        .unwrap();
        assert_eq!(
            check_unit(&root, &u, &stub, &work.join("v"), Duration::from_secs(20)),
            Outcome::Clean(1)
        );
        let _ = fs::remove_dir_all(&root);
    }

    /// A script whose mutant the tool cannot read against any original: nothing under the work root carries the planted bug at a
    /// repo-relative path, and the file Verus was handed does not exist to be compared with the baseline.
    #[test]
    fn a_copy_that_cannot_be_located_is_inconclusive() {
        let (root, u) = twice_repo("nolocate");
        fs::write(
            root.join("verification/two/mutants.sh"),
            r#"#!/usr/bin/env bash
set -uo pipefail
work=$(mktemp -d "${TMPDIR:-/tmp}/lost-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
"$VERUS" verification/two/rules.rs >/dev/null 2>&1 && echo "baseline: verifies"
mkdir -p "$work/scratch"
printf 'mutated\n' > "$work/scratch/lib.rs"
"$VERUS" "$work/scratch/rules.rs" >/dev/null 2>&1
echo "refused: a"
echo "mutants: 1 refused by verification, 0 refused without a verification run"
"#,
        )
        .unwrap();
        let work = root.join(".w");
        let stub = write_stub(&work.join("s")).unwrap();
        let o = check_unit(&root, &u, &stub, &work.join("u"), Duration::from_secs(20));
        assert!(
            matches!(&o, Outcome::Inconclusive(w) if w.contains("mutant call 1") && w.contains("no copy of the planted bug")),
            "{o:?}"
        );
        let _ = fs::remove_dir_all(&root);
    }

    /// Half the scripts build the text for Verus themselves, in a file the repository does not know. Their original is the baseline
    /// the script ran first, and it is read against that.
    #[test]
    fn a_mutant_built_for_verus_is_read_against_the_baseline_it_came_from() {
        let (root, u) = twice_repo("wrapped");
        fs::write(
            root.join("verification/two/mutants.sh"),
            r#"#!/usr/bin/env bash
set -uo pipefail
work=$(mktemp -d "${TMPDIR:-/tmp}/wrap-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
text=$(<crates/two/src/lib.rs)
printf '%s\n' "$text" > "$work/base.rs"
"$VERUS" "$work/base.rs" >/dev/null 2>&1 && echo "baseline: verifies"
printf '%s\n' "${text/AAAA/BBBB}" > "$work/mutant.rs"
"$VERUS" "$work/mutant.rs" >/dev/null 2>&1 && echo "NOT REFUSED" || echo "refused: a"
echo "mutants: 1 refused by verification, 0 refused without a verification run"
"#,
        )
        .unwrap();
        let work = root.join(".w");
        let stub = write_stub(&work.join("s")).unwrap();
        match check_unit(&root, &u, &stub, &work.join("u"), Duration::from_secs(20)) {
            Outcome::Ambiguous(f) => {
                assert_eq!(f.len(), 1, "{f:?}");
                assert!(f[0].starts_with("AMBIGUOUS (matches 2 times)"), "{f:?}");
                assert!(f[0].contains("_arg"), "{f:?}");
                assert!(f[0].contains("AAAA"), "{f:?}");
            }
            o => panic!("{o:?}"),
        }
        // A text for Verus that is not one of its baselines with one region replaced says nothing: it is not attributed to a mutant.
        fs::write(
            root.join("verification/two/mutants.sh"),
            r#"#!/usr/bin/env bash
set -uo pipefail
work=$(mktemp -d "${TMPDIR:-/tmp}/other-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
printf 'use vstd::prelude::*;\nverus!\n' > "$work/base.rs"
"$VERUS" "$work/base.rs" >/dev/null 2>&1 || exit 1
echo "baseline: verifies"
printf 'an unrelated file\n' > "$work/mutant.rs"
"$VERUS" "$work/mutant.rs" >/dev/null 2>&1
echo "refused: a"
echo "mutants: 1 refused by verification, 0 refused without a verification run"
"#,
        )
        .unwrap();
        let o = check_unit(&root, &u, &stub, &work.join("v"), Duration::from_secs(20));
        assert!(
            matches!(&o, Outcome::Inconclusive(w) if w.contains("no copy of the planted bug")),
            "{o:?}"
        );
        let _ = fs::remove_dir_all(&root);
    }

    /// A script that runs a fresh baseline before the mutants of another shipped file (juno-crash-layout, juno-wm-rules): each call
    /// is read against the baseline it was cut from, which is neither the first nor the last one of the run.
    #[test]
    fn a_mutant_is_read_against_the_baseline_it_was_cut_from() {
        let (root, u) = twice_repo("two-baselines");
        fs::write(
            root.join("verification/two/mutants.sh"),
            r#"#!/usr/bin/env bash
set -uo pipefail
work=$(mktemp -d "${TMPDIR:-/tmp}/twob-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
printf 'AAAA\n' > "$work/base.rs"
"$VERUS" "$work/base.rs" >/dev/null 2>&1 && echo "baseline: verifies"
printf 'x\nBBBB\ny\nBBBB\n' > "$work/base.rs"
"$VERUS" "$work/base.rs" >/dev/null 2>&1 || exit 1
printf 'x\nCCCC\ny\nBBBB\n' > "$work/mutant.rs"
"$VERUS" "$work/mutant.rs" >/dev/null 2>&1
echo "refused: a"
echo "mutants: 1 refused by verification, 0 refused without a verification run"
"#,
        )
        .unwrap();
        let work = root.join(".w");
        let stub = write_stub(&work.join("s")).unwrap();
        match check_unit(&root, &u, &stub, &work.join("u"), Duration::from_secs(20)) {
            Outcome::Ambiguous(f) => {
                assert!(f[0].starts_with("AMBIGUOUS (matches 2 times)"), "{f:?}");
                assert!(f[0].contains("BBBB"), "{f:?}");
            }
            o => panic!("{o:?}"),
        }
        let _ = fs::remove_dir_all(&root);
    }

    /// An `AMBIGUOUS` finding is a note about the script, not a verdict on the change: it never fails a run, gate included.
    #[test]
    fn ambiguous_is_a_note_even_at_the_gate() {
        let (root, u) = twice_repo("ambig-gate");
        assert_eq!(
            run(&root, &[&u], false, true, total_budget(1), false).unwrap(),
            0,
            "the gate must not stop on an ambiguity note"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn the_script_gets_a_scrubbed_environment() {
        let root = scratch("env");
        fs::create_dir_all(root.join("verification/v")).unwrap();
        // /proc/<pid>/environ is the environment the script was execed with, before bash added SHLVL and PWD of its own.
        fs::write(
            root.join("verification/v/mutants.sh"),
            "#!/usr/bin/env bash\ncat \"/proc/$$/environ\" | tr '\\0' '\\n' | sed 's/=.*//' | sort > \"$LUCINA_PRE_REPO/env.txt\"\n",
        )
        .unwrap();
        let work = root.join(".w");
        let stub = write_stub(&work.join("s")).unwrap();
        check_unit(
            &root,
            &unit("v", &[]),
            &stub,
            &work.join("u"),
            Duration::from_secs(10),
        );
        let names = fs::read_to_string(root.join("env.txt")).unwrap();
        let got: Vec<&str> = names.lines().filter(|l| !l.is_empty()).collect();
        // Nothing of the session that runs the tool: no agent socket, no broker token, no cargo or CARGO_* of the build.
        assert_eq!(
            got,
            vec![
                "HOME",
                "LANG",
                "LUCINA_PRE_CAP",
                "LUCINA_PRE_REPO",
                "LUCINA_STUB_LOG",
                "PATH",
                "TMPDIR",
                "VERUS"
            ],
            "{names}"
        );
        assert!(!names.contains("SSH_AUTH_SOCK"), "{names}");
        assert!(!names.contains("CARGO"), "{names}");
        let _ = fs::remove_dir_all(&root);
    }

    /// The global list: text-only, every sed expression must change its file.
    fn fake_global_list(tag: &str) -> (PathBuf, Unit) {
        let root = scratch(tag);
        fs::create_dir_all(root.join("scripts")).unwrap();
        fs::create_dir_all(root.join("verification/g")).unwrap();
        fs::write(
            root.join("verification/g/rules.rs"),
            "fn f() {\n    let v = 1u32;\n    v\n}\n",
        )
        .unwrap();
        fs::write(
            root.join("scripts/verify-mutations.sh"),
            r#"#!/usr/bin/env bash
set -euo pipefail
mutations=(
    'verification/g/rules.rs|s/let v = 1u32;/let v = 2u32;/|the constant'
    'verification/g/rules.rs|s/let gone = nothing;/let gone = else;/|a stale expression'
    'verification/g/gone.rs|s/a/b/|a file that is not there'
    not quoted|s/a/b/|x
)
"#,
        )
        .unwrap();
        (
            root.clone(),
            Unit {
                name: "mutations/global".into(),
                group: "g".into(),
                kind: "mutations".into(),
                target: "scripts/verify-mutations.sh".into(),
                inputs: vec!["scripts/verify-mutations.sh".into()],
            },
        )
    }

    #[test]
    fn the_global_list_is_parsed_and_each_sed_must_change_its_file() {
        let (root, u) = fake_global_list("list");
        let text = fs::read_to_string(root.join("scripts/verify-mutations.sh")).unwrap();
        let (list, malformed) = parse_mutations(&text);
        assert_eq!(list.len(), 3);
        assert_eq!(list[0].what, "the constant");
        assert_eq!(list[0].expr, "s/let v = 1u32;/let v = 2u32;/");
        assert_eq!(malformed.len(), 1, "{malformed:?}");
        match check_mutations_list(&root, &u, &root.join(".w")) {
            Outcome::Fail(f) => {
                assert_eq!(f.len(), 3, "{f:?}");
                assert!(
                    f.iter()
                        .any(|l| l.starts_with("NOT CHANGED") && l.contains("a stale")),
                    "{f:?}"
                );
                assert!(
                    f.iter()
                        .any(|l| l.starts_with("NOT FOUND")
                            && l.contains("a file that is not there")),
                    "{f:?}"
                );
                assert!(f.iter().any(|l| l.starts_with("NOT COVERED")), "{f:?}");
            }
            o => panic!("{o:?}"),
        }
        // and the same list with the two dead entries removed is clean
        fs::write(
            root.join("scripts/verify-mutations.sh"),
            "#!/usr/bin/env bash\nmutations=(\n    'verification/g/rules.rs|s/let v = 1u32;/let v = 2u32;/|the constant'\n)\n",
        )
        .unwrap();
        let u2 = Unit {
            target: "scripts/verify-mutations.sh".into(),
            ..u
        };
        assert_eq!(
            check_mutations_list(&root, &u2, &root.join(".w2")),
            Outcome::Clean(1)
        );
        let _ = fs::remove_dir_all(&root);
    }
}
