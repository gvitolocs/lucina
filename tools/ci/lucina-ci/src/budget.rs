//! The outer limit of the merge gate "fast local check".
//!
//! `.lucina/pipeline.toml` gives the gate `scripts/check-fast --base HEAD^1 --merge-gate` a `timeout_min`, and inside it every
//! step has a limit of its own. The outer one must hold the sum of the inner ones, or a correct change is killed while its build
//! is still inside its own limit. The worst case is computed here from the same constants and tables the gate runs with, and a
//! test fails when the pipeline file is below it (when the file is in the tree; the public copy has none).
//!
//! The gate does not pass `--proofs`, so the proof runs that flag starts are not counted: only the steps below run at the gate.

use super::{
    ACTIONLINT_STEP_TIMEOUT_S, DIFFGATE_STEP_TIMEOUT_S, INVENTORY_STEP_TIMEOUT_S,
    MERGE_GATE_BUDGET_STEP_TIMEOUT_S, MERGE_STEP_LIMIT_DEFAULT, MergeCaps, ROLES_STEP_TIMEOUT_S,
    SECRETS_STEP_TIMEOUT_S, SHELLCHECK_STEP_TIMEOUT_S, SPEC_STEP_TIMEOUT_S, TRUST_STEP_TIMEOUT_S,
    fast, mutants_pre,
};
use std::collections::BTreeSet;

/// How many standalone crates (a manifest under `tools/` with its own workspace) a change is assumed to touch. A change that
/// touches more than two such crates is rare; if one does, `check-fast --merge-gate` prints a line saying so, and the gate may
/// need its limit raised, or the change to be split. The budget also assumes `LUCINA_CHECK_FAST_MERGE_CAP_S` is not set.
pub const ASSUMED_STANDALONE_CRATES: usize = 2;
/// The standalone crate `MERGE_STEP_LIMITS` names a step of. It is counted first among the assumed crates, so the larger limit
/// of its tests is in the sum; the other assumed crates run under the default limit.
pub const NAMED_STANDALONE_CRATE: &str = "tools/ci/lucina-ci/Cargo.toml";
/// Seconds the gate may wait for its slot in the build queue before check-fast starts. The gate takes bq's reserved gate slot,
/// so the wait is short; the gate's timer starts when the pipeline starts the command, so the wait counts against the outer limit.
pub const BQ_WAIT_ALLOWANCE_S: u64 = 300;
/// Seconds of margin on top of every step running to its limit. It covers what happens inside the queue slot before the first step:
/// the launcher's `cargo build --release` of the CI tool (which can wait on the lock of the shared build directory), its install
/// copy, and the run's own self-copy of the binary; then `git` and the final report.
pub const MARGIN_S: u64 = 300;
/// The name of the gate in `.lucina/pipeline.toml`.
pub const GATE_NAME: &str = "fast local check";

/// The seconds a step of own timeout `own` runs at the most at the merge gate: its table limit or the default, never above `own`;
/// a required step no table names keeps its own timeout.
fn at_gate(name: &str, own: u64) -> u64 {
    match MergeCaps::default().limit_of(name, MERGE_STEP_LIMIT_DEFAULT) {
        Some(l) => own.min(l),
        None => own,
    }
}

/// The parts of the worst case, as (what, seconds).
///
/// Counted: the workspace steps (`fmt`, `clippy`, `unit-tests`) once, the three per-crate steps for each of
/// `ASSUMED_STANDALONE_CRATES` standalone crates, `diffgate`, `shellcheck`, `actionlint`, `secrets`, `merge-gate-budget`,
/// `inventory`, `spec`, `roles`, `trust` and `mutants-text` once each, then the queue wait and the margin. Not counted: the proofs
/// `--proofs` runs (the merge gate does not pass it).
pub fn merge_gate_parts() -> Vec<(String, u64)> {
    let mut owners: BTreeSet<fast::Owner> = BTreeSet::new();
    owners.insert(fast::Owner::Member("budget-member".into()));
    owners.insert(fast::Owner::Standalone(NAMED_STANDALONE_CRATE.into()));
    for i in 1..ASSUMED_STANDALONE_CRATES {
        owners.insert(fast::Owner::Standalone(format!(
            "tools/budget-{i}/Cargo.toml"
        )));
    }
    let mut parts: Vec<(String, u64)> = fast::cargo_steps(&owners, &["budget-member.rs".into()])
        .into_iter()
        .map(|(name, _, own)| {
            let s = at_gate(&name, own);
            (name, s)
        })
        .collect();
    for (name, own) in [
        ("diffgate", DIFFGATE_STEP_TIMEOUT_S),
        ("shellcheck", SHELLCHECK_STEP_TIMEOUT_S),
        ("actionlint", ACTIONLINT_STEP_TIMEOUT_S),
        ("secrets", SECRETS_STEP_TIMEOUT_S),
        ("merge-gate-budget", MERGE_GATE_BUDGET_STEP_TIMEOUT_S),
        ("inventory", INVENTORY_STEP_TIMEOUT_S),
        ("spec", SPEC_STEP_TIMEOUT_S),
        ("roles", ROLES_STEP_TIMEOUT_S),
        ("trust", TRUST_STEP_TIMEOUT_S),
        ("mutants-text", mutants_pre::STEP_TIMEOUT_S),
    ] {
        parts.push((name.to_string(), at_gate(name, own)));
    }
    parts.push(("bq wait allowance".into(), BQ_WAIT_ALLOWANCE_S));
    parts.push(("margin".into(), MARGIN_S));
    parts
}

/// Whether a change edits the pipeline file, which makes check-fast run the `merge-gate-budget` step at the merge gate.
pub fn pipeline_changed(changed: &BTreeSet<String>) -> bool {
    changed.contains(".lucina/pipeline.toml")
}

/// The longest a `--merge-gate` check-fast run can legitimately take, in seconds.
pub fn merge_gate_worst_case_secs() -> u64 {
    merge_gate_parts().iter().map(|(_, s)| s).sum()
}

/// The `timeout_min` of the gate named `name` in a pipeline file: the first `timeout_min` after its `name = "..."` line and before
/// the next table header.
pub fn gate_timeout_min(toml: &str, name: &str) -> Option<u64> {
    let mut in_gate = false;
    for l in toml.lines() {
        let t = l.trim();
        if t.starts_with('[') {
            in_gate = false;
        } else if t.starts_with("name") && t.contains('=') {
            in_gate = t.split_once('=')?.1.trim().trim_matches('"') == name;
        } else if in_gate && t.starts_with("timeout_min") {
            return t.split_once('=')?.1.split('#').next()?.trim().parse().ok();
        }
    }
    None
}

/// `Ok` when the gate's outer limit in `toml` holds the worst case; `Err` says both numbers and which file to change.
pub fn check_pipeline(toml: &str) -> Result<(), String> {
    let worst = merge_gate_worst_case_secs();
    let min = gate_timeout_min(toml, GATE_NAME).ok_or_else(|| {
        format!("no gate named {GATE_NAME:?} with a timeout_min in the pipeline file")
    })?;
    if min * 60 < worst {
        return Err(format!(
            "the gate {GATE_NAME:?} has timeout_min = {min} ({} s) in .lucina/pipeline.toml, below the {worst} s its own steps may take \
             (`budget::merge_gate_worst_case_secs`, {} min rounded up): raise timeout_min in .lucina/pipeline.toml, or lower the step limits in tools/ci/lucina-ci",
            min * 60,
            worst.div_ceil(60)
        ));
    }
    Ok(())
}

/// `lucina-ci merge-gate-budget`: print the worst case in minutes, with its parts.
pub fn cmd_merge_gate_budget() -> Result<i32, String> {
    for (what, s) in merge_gate_parts() {
        println!("{s:>6} s  {what}");
    }
    let worst = merge_gate_worst_case_secs();
    println!(
        "{worst:>6} s  total = {} min; timeout_min to write: {}",
        worst.div_ceil(60),
        worst.div_ceil(60).div_ceil(5) * 5
    );
    let root = super::repo_root()?;
    let toml =
        std::fs::read_to_string(root.join(".lucina/pipeline.toml")).map_err(|e| e.to_string())?;
    match check_pipeline(&toml) {
        Ok(()) => {
            println!("pipeline.toml: ok");
            Ok(0)
        }
        Err(m) => {
            println!("pipeline.toml: {m}");
            Ok(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// The pipeline file at `p`; any read error is returned as it is (the caller decides what a missing file means).
    fn read_pipeline(p: &Path) -> std::io::Result<String> {
        std::fs::read_to_string(p)
    }

    /// What the real-gate check does with the pipeline file: `Ok(Some(text))` check it, `Ok(None)` skip (the public copy has no
    /// pipeline file), `Err` fail. Only a missing file in the public copy (no `tools/ci/public-snapshot.rules`, which no snapshot
    /// carries) is a skip; a missing file in the private tree, or any other read error anywhere, is a failure.
    fn decide(
        marker_present: bool,
        read: std::io::Result<String>,
    ) -> Result<Option<String>, String> {
        match read {
            Ok(s) => Ok(Some(s)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !marker_present => Ok(None),
            Err(e) => Err(format!(".lucina/pipeline.toml cannot be read: {e}")),
        }
    }

    fn real_pipeline_opt() -> Option<String> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let marker = root.join("../public-snapshot.rules").exists();
        let p = root.join("../../../.lucina/pipeline.toml");
        decide(marker, read_pipeline(&p)).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
    }

    /// A scratch directory that is removed when it drops, also when the test panics.
    struct Scratch(std::path::PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Scratch {
            let dir =
                std::env::temp_dir().join(format!("gate-budget-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn the_real_gate_holds_the_worst_case() {
        let Some(real) = real_pipeline_opt() else {
            // Only visible with `--nocapture`: the public copy has no pipeline file, so this check runs in the private repo only.
            eprintln!(
                "budget: no .lucina/pipeline.toml in the public copy; the real-gate check is skipped here"
            );
            return;
        };
        if let Err(m) = check_pipeline(&real) {
            panic!("{m}");
        }
    }

    #[test]
    fn a_pipeline_file_that_is_there_is_read() {
        let d = Scratch::new("read");
        let f = d.0.join("pipeline.toml");
        std::fs::write(&f, "[[gate]]\nname = \"x\"\n").unwrap();
        assert_eq!(
            read_pipeline(&f).unwrap(),
            "[[gate]]\nname = \"x\"\n".to_string()
        );
        assert_eq!(
            decide(false, read_pipeline(&f)),
            Ok(Some("[[gate]]\nname = \"x\"\n".to_string()))
        );
    }

    #[test]
    fn a_missing_file_is_a_skip_only_in_the_public_copy() {
        let d = Scratch::new("gone");
        let gone = d.0.join("no-such-dir/pipeline.toml");
        assert_eq!(decide(false, read_pipeline(&gone)), Ok(None));
        let e = decide(true, read_pipeline(&gone)).expect_err("the private tree requires the file");
        assert!(e.contains("cannot be read"), "{e}");
    }

    #[test]
    fn a_read_error_other_than_not_found_fails_in_both_trees() {
        let d = Scratch::new("bad");
        // A directory where the file should be.
        assert!(decide(false, read_pipeline(&d.0)).is_err());
        assert!(decide(true, read_pipeline(&d.0)).is_err());
        // Bytes that are not UTF-8.
        let f = d.0.join("pipeline.toml");
        std::fs::write(&f, [0xff, 0xfe, 0x00]).unwrap();
        assert!(decide(false, read_pipeline(&f)).is_err());
        assert!(decide(true, read_pipeline(&f)).is_err());
    }

    #[test]
    fn a_too_low_limit_fails_and_names_both_numbers() {
        let low = "[[gate]]\nname = \"fast local check\"\nrun = \"x\"\ntimeout_min = 12\n";
        let e = check_pipeline(low).expect_err("12 minutes is below the worst case");
        assert!(
            e.contains("720 s") && e.contains(&format!("{} s", merge_gate_worst_case_secs())),
            "{e}"
        );
        assert!(e.contains(".lucina/pipeline.toml"), "{e}");
    }

    #[test]
    fn a_too_low_limit_in_a_pipeline_file_on_disk_fails() {
        let d = Scratch::new("low");
        let f = d.0.join("pipeline.toml");
        let real = "[[gate]]\nname = \"fast local check\"\npaths = []\nrun = \"bash scripts/check-fast --base HEAD^1 --merge-gate\"\ntimeout_min = 50\n";
        if let Some(file) = real_pipeline_opt() {
            let lines: Vec<&str> = file.lines().collect();
            let at = lines
                .iter()
                .position(|l| l.trim() == "name = \"fast local check\"")
                .expect("the real file has the gate")
                - 1;
            assert_eq!(
                format!("{}\n", lines[at..at + 5].join("\n")),
                real,
                "the synthetic gate block drifted from .lucina/pipeline.toml"
            );
        }
        let min = gate_timeout_min(real, GATE_NAME).unwrap();
        let low = real.replacen(&format!("timeout_min = {min}"), "timeout_min = 12", 99);
        assert!(low.contains("timeout_min = 12"), "{low}");
        std::fs::write(&f, low).unwrap();
        let e = check_pipeline(&std::fs::read_to_string(&f).unwrap())
            .expect_err("12 minutes is below the worst case");
        assert!(
            e.contains("timeout_min = 12")
                && e.contains(&format!("{} s", merge_gate_worst_case_secs())),
            "{e}"
        );
    }

    #[test]
    fn the_worst_case_counts_what_the_gate_runs() {
        let parts = merge_gate_parts();
        let get = |n: &str| {
            parts
                .iter()
                .filter(|(k, _)| k == n)
                .map(|(_, s)| *s)
                .sum::<u64>()
        };
        // The workspace steps, once.
        assert_eq!(get("fmt"), 60);
        assert_eq!(get("clippy"), 180);
        assert_eq!(get("unit-tests"), 180);
        // The named standalone crate gets the 300 s its tests have in `MERGE_STEP_LIMITS`; the other assumed crate, 180.
        assert_eq!(get("fmt-tools-ci-lucina-ci"), 60);
        assert_eq!(get("clippy-tools-ci-lucina-ci"), 180);
        assert_eq!(get("unit-tests-tools-ci-lucina-ci"), 300);
        assert_eq!(get("fmt-tools-budget-1"), 60);
        assert_eq!(get("clippy-tools-budget-1"), 180);
        assert_eq!(get("unit-tests-tools-budget-1"), 180);
        let standalone = parts.iter().filter(|(k, _)| k.contains("tools-")).count();
        assert_eq!(standalone, 3 * ASSUMED_STANDALONE_CRATES);
        // The steps check-fast builds itself.
        assert_eq!(get("diffgate"), 60);
        assert_eq!(get("shellcheck"), 30);
        assert_eq!(get("actionlint"), 30);
        assert_eq!(get("secrets"), 60);
        assert_eq!(get("merge-gate-budget"), 30);
        assert_eq!(get("inventory"), 30);
        assert_eq!(get("spec"), 60);
        assert_eq!(get("roles"), 300);
        assert_eq!(get("trust"), 60);
        assert_eq!(get("mutants-text"), 240);
        assert_eq!(get("bq wait allowance"), 300);
        assert_eq!(get("margin"), 300);
        assert_eq!(merge_gate_worst_case_secs(), 2880);
    }

    /// A row added to `MERGE_STEP_LIMITS` for a step the budget does not count would be silently left out of the sum.
    #[test]
    fn every_row_of_the_limit_table_is_counted() {
        let parts = merge_gate_parts();
        for (name, _) in super::super::MERGE_STEP_LIMITS {
            assert!(
                parts.iter().any(|(k, _)| k == name),
                "MERGE_STEP_LIMITS names {name}, which budget::merge_gate_parts does not count"
            );
        }
    }

    /// Every step name `check-fast` registers as required is counted (the proofs step is not: it needs `--proofs`).
    #[test]
    fn every_required_step_is_counted() {
        let parts = merge_gate_parts();
        for name in super::super::FAST_STEPS_REQUIRED {
            assert!(
                parts.iter().any(|(k, _)| k == name),
                "FAST_STEPS_REQUIRED names {name}, which budget::merge_gate_parts does not count"
            );
        }
    }

    /// The wait the launcher gives the gate and the allowance the budget counts are one number: the shell literal is read here.
    #[test]
    fn the_launcher_waits_for_the_gate_slot_no_longer_than_the_budget_allows() {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../scripts/check-fast");
        let sh = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
        assert!(
            sh.contains(r#"--max-wait "$maxwait""#),
            "the launcher's bq call no longer passes --max-wait \"$maxwait\""
        );
        let mut lits: Vec<u64> = Vec::new();
        for v in sh.lines().filter_map(|l| l.trim().strip_prefix("maxwait=")) {
            match v.parse() {
                Ok(n) => lits.push(n),
                Err(_) => {
                    panic!("scripts/check-fast: `maxwait={v}` is not a plain number of seconds")
                }
            }
        }
        assert_eq!(
            lits.len(),
            2,
            "the launcher sets maxwait twice (plain run, merge gate): {lits:?}"
        );
        assert_eq!(lits[0], 3600, "a plain run keeps its hour");
        assert_eq!(
            lits[1], BQ_WAIT_ALLOWANCE_S,
            "scripts/check-fast gives the merge gate --max-wait {} but budget.rs counts BQ_WAIT_ALLOWANCE_S = {BQ_WAIT_ALLOWANCE_S}: change both",
            lits[1]
        );
    }

    #[test]
    fn only_a_change_to_the_pipeline_file_selects_the_budget_step() {
        let only: BTreeSet<String> = [".lucina/pipeline.toml".to_string()].into();
        let other: BTreeSet<String> = [
            "scripts/check-fast".to_string(),
            ".lucina/other.toml".to_string(),
        ]
        .into();
        assert!(pipeline_changed(&only));
        assert!(!pipeline_changed(&other));
    }

    #[test]
    fn a_later_table_does_not_lend_its_timeout_to_the_gate() {
        let t = "[[gate]]\nname = \"fast local check\"\n[other]\ntimeout_min = 99\n";
        assert_eq!(gate_timeout_min(t, GATE_NAME), None);
    }

    #[test]
    fn the_gate_is_found_among_others() {
        let t = "[[gate]]\nname = \"a\"\ntimeout_min = 5\n[[gate]]\nname = \"fast local check\"\npaths = []\ntimeout_min = 70 # why\n[[gate]]\nname = \"b\"\ntimeout_min = 9\n";
        assert_eq!(gate_timeout_min(t, GATE_NAME), Some(70));
        assert_eq!(gate_timeout_min(t, "b"), Some(9));
        assert_eq!(gate_timeout_min(t, "none"), None);
    }
}
