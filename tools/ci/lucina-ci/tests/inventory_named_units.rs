//! The two discovery rules found missing in the Brimstone copy of this CI on 2026-11-17, on a throwaway repository with the
//! real binary (the fixture style of `inventory_gone_unit.rs`):
//!
//! - a `# mutants-units:` line makes one unit per name, run with that name, and no unit for the directory. Without it a script
//!   of five proofs was registered once and run with no argument, so only its default proof's mutants ever ran;
//! - `mutations/global` is a unit only where a global mutation list with entries in it exists. An absent script or an empty
//!   `mutations=()` proved nothing, and the unit passed trivially.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The fixture repository, removed also when a test panics.
struct Repo(PathBuf);

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn git(d: &Path, args: &[&str]) {
    let o = Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(d)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
}

/// A file of the fixture, with its folders.
fn write(repo: &Path, rel: &str, text: &str) {
    let p = repo.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

/// The three shipped proofs a named script of this fixture can point at, each with a `//@` line so the list must carry it.
const SHIPPED: [&str; 3] = ["disk_rules", "patch_rules", "wire"];

/// A mutants.sh that holds one proof per name and picks it from `$1`, Brimstone's shape: the default is the first name, and
/// the shipped file is built from the name itself.
const NAMED: &str = r#"#!/usr/bin/env bash
# What this proves, one file per name:
# mutants-units: disk_rules patch_rules wire
set -uo pipefail
name=${1:-disk_rules}
rules=${2:-js/bytecode/$name.rs}
"$VERUS" "$rules" > /dev/null 2>&1 && echo "baseline: verifies"
echo "mutants: 3 refused by verification"
exit 0
"#;

/// A mutants.sh with no marker, the shape of every LucinaOS script today: one unit, run with no argument.
const PLAIN: &str = r#"#!/usr/bin/env bash
set -uo pipefail
"$VERUS" js/bytecode/disk_rules.rs > /dev/null 2>&1 && echo "baseline: verifies"
echo "mutants: 3 refused by verification"
exit 0
"#;

/// The same marker with one name written twice: one unit name for two proofs, which discovery refuses.
const TWICE: &str = r#"#!/usr/bin/env bash
# mutants-units: disk_rules wire wire
set -uo pipefail
echo "baseline: verifies"
echo "refused by verification"
"#;

/// `scripts/verify-mutations.sh` in each of the three states the rule cares about.
const GLOBAL_WITH_ENTRY: &str = r#"#!/usr/bin/env bash
set -euo pipefail
mutations=(
    'js/bytecode/wire.rs|s/f fn/fn/|the claim is weaker'
)
"#;

const GLOBAL_EMPTY_LIST: &str = r#"#!/usr/bin/env bash
# Brimstone keeps no global list: every mutant lives with the unit it attacks.
set -euo pipefail
mutations=(
)
"#;

/// A repository that carries only what discovery reads, in the state the script and the global list are given. The global
/// list is `None` when the repository has no such script at all.
fn setup(tag: &str, script: &str, global: Option<&str>) -> Repo {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("named-units-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    write(
        &d,
        "tools/ci/proof-groups.tsv",
        "# group<TAB>keys\nfixture\tjs zzzfncache models full tla global-mutations\n",
    );
    let files: Vec<String> = SHIPPED
        .iter()
        .map(|s| format!("js/bytecode/{s}.rs"))
        .collect();
    write(
        &d,
        "scripts/verify-shipped.sh",
        &format!("#!/usr/bin/env bash\nfiles=({})\n", files.join(" ")),
    );
    for s in SHIPPED {
        write(
            &d,
            &format!("js/bytecode/{s}.rs"),
            "pub fn f() -> bool { true }\n//@ ensures f() == true;\n",
        );
    }
    write(&d, "verification/zzzfncache/mutants.sh", script);
    if let Some(g) = global {
        write(&d, "scripts/verify-mutations.sh", g);
    }
    git(&d, &["init", "-q", "-b", "main"]);
    git(&d, &["add", "-A"]);
    git(&d, &["commit", "-q", "-m", "base"]);
    Repo(d)
}

/// `lucina-ci <args>` in the fixture, as (exit code, stdout and stderr together).
fn run(repo: &Path, args: &[&str]) -> (i32, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_lucina-ci"))
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap();
    (
        o.status.code().unwrap_or(-1),
        format!(
            "stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        ),
    )
}

/// Writes the inventory and commits it, which is the state `--check` judges.
fn commit_inventory(repo: &Path) -> String {
    let (code, out) = run(repo, &["inventory", "--write"]);
    assert_eq!(code, 0, "inventory --write: {out}");
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "inventory"]);
    fs::read_to_string(repo.join("tools/ci/proof-inventory.tsv")).unwrap()
}

/// The `(unit, target)` pairs of one kind, in the order the inventory holds them.
fn units_of(tsv: &str, prefix: &str) -> Vec<(String, String)> {
    tsv.lines()
        .filter(|l| !l.starts_with('#'))
        .map(|l| l.split('\t').map(str::to_string).collect::<Vec<_>>())
        .filter(|f| f[0].starts_with(prefix))
        .map(|f| (f[0].clone(), f[3].clone()))
        .collect()
}

#[test]
fn a_marker_registers_one_named_unit_per_proof_and_no_unit_for_the_directory() {
    let repo = setup("three", NAMED, Some(GLOBAL_WITH_ENTRY));
    let tsv = commit_inventory(&repo.0);
    assert_eq!(
        units_of(&tsv, "mutants/"),
        vec![
            (
                "mutants/zzzfncache/disk_rules".to_string(),
                "verification/zzzfncache/mutants.sh disk_rules".to_string()
            ),
            (
                "mutants/zzzfncache/patch_rules".to_string(),
                "verification/zzzfncache/mutants.sh patch_rules".to_string()
            ),
            (
                "mutants/zzzfncache/wire".to_string(),
                "verification/zzzfncache/mutants.sh wire".to_string()
            ),
        ],
        "{tsv}"
    );
    // The inventory is current in that state, so the check that runs in CI is satisfied by it.
    let (code, out) = run(&repo.0, &["inventory", "--check"]);
    assert_eq!(code, 0, "the named inventory is current: {out}");
}

#[test]
fn a_script_without_the_marker_stays_one_unit_run_with_no_argument() {
    let repo = setup("plain", PLAIN, Some(GLOBAL_WITH_ENTRY));
    let tsv = commit_inventory(&repo.0);
    assert_eq!(
        units_of(&tsv, "mutants/"),
        vec![(
            "mutants/zzzfncache".to_string(),
            "verification/zzzfncache/mutants.sh".to_string()
        )],
        "{tsv}"
    );
    let (code, out) = run(&repo.0, &["inventory", "--check"]);
    assert_eq!(code, 0, "{out}");
}

#[test]
fn a_marker_with_a_repeated_name_is_refused() {
    let repo = setup("twice", TWICE, Some(GLOBAL_WITH_ENTRY));
    let (code, out) = run(&repo.0, &["inventory", "--write"]);
    assert_ne!(code, 0, "a name written twice must not register: {out}");
    assert!(out.contains("twice"), "{out}");
    assert!(
        !repo.0.join("tools/ci/proof-inventory.tsv").exists(),
        "a refused discovery writes no inventory: {out}"
    );
}

#[test]
fn a_named_unit_without_a_shipped_proof_of_that_name_is_refused_by_the_check() {
    // `partition` is a name of the marker and of no shipped file: the script's own convention puts it at
    // js/bytecode/partition.rs, which CI does not verify.
    let script = NAMED.replace(
        "# mutants-units: disk_rules patch_rules wire",
        "# mutants-units: disk_rules patch_rules wire partition",
    );
    let repo = setup("missing", &script, Some(GLOBAL_WITH_ENTRY));
    commit_inventory(&repo.0);
    let (code, out) = run(&repo.0, &["inventory", "--check"]);
    assert_eq!(code, 1, "a name with no shipped proof is a finding: {out}");
    let stderr = out.split("stderr:\n").nth(1).unwrap_or_default();
    assert!(stderr.contains("mutants/zzzfncache/partition:"), "{out}");
    assert!(
        stderr.contains("js/bytecode/partition.rs"),
        "the message names the file the script derives: {out}"
    );
}

#[test]
fn a_script_that_states_no_convention_for_its_names_is_not_judged() {
    // The name goes into a `.src` word only: no file is derivable from it generically, so the check asks nothing.
    let script = NAMED.replace(
        "rules=${2:-js/bytecode/$name.rs}",
        "part=${2:-js/bytecode/$name.src}",
    );
    let repo = setup("nocconv", &script, Some(GLOBAL_WITH_ENTRY));
    commit_inventory(&repo.0);
    let (code, out) = run(&repo.0, &["inventory", "--check"]);
    assert_eq!(code, 0, "nothing derived, nothing refused: {out}");
}

#[test]
fn the_global_mutation_unit_exists_only_where_a_list_with_entries_exists() {
    // Absent script: no unit and no error — the state a repository with no global list is in.
    let absent = setup("absent", PLAIN, None);
    let tsv = commit_inventory(&absent.0);
    assert!(units_of(&tsv, "mutations/").is_empty(), "{tsv}");
    let (code, out) = run(&absent.0, &["inventory", "--check"]);
    assert_eq!(code, 0, "an absent list is not a discovery error: {out}");
    // Present with an empty list: the same, and no `mutations/global` that passes trivially.
    let empty = setup("empty", PLAIN, Some(GLOBAL_EMPTY_LIST));
    let tsv = commit_inventory(&empty.0);
    assert!(units_of(&tsv, "mutations/").is_empty(), "{tsv}");
    let (code, out) = run(&empty.0, &["inventory", "--check"]);
    assert_eq!(code, 0, "an empty list is not a discovery error: {out}");
    // Present with one entry: the unit is there, as it is on LucinaOS.
    let one = setup("one", PLAIN, Some(GLOBAL_WITH_ENTRY));
    let tsv = commit_inventory(&one.0);
    assert_eq!(
        units_of(&tsv, "mutations/"),
        vec![(
            "mutations/global".to_string(),
            "scripts/verify-mutations.sh".to_string()
        )],
        "{tsv}"
    );
    let (code, out) = run(&one.0, &["inventory", "--check"]);
    assert_eq!(code, 0, "{out}");
}
