//! `lucina-ci inventory --check` when a proof unit's target script is deleted: the committed inventory still names the unit,
//! discovery no longer finds it (`unit_present` in inventory.rs drops a `tla`/`models`/`full` unit whose target this repo lacks),
//! and the check must FAIL with `in the inventory but gone: <unit>` rather than fall silent — a dropped line is a proof that
//! leaves CI unnoticed. Two such units are covered, `models/stateright` (`scripts/verify-models.sh`) and `full/verify-sh`
//! (`scripts/verify.sh`). The real binary runs over a throwaway git repository, as `merge_gate_timeout.rs` does.

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

/// A repository that carries only what `inventory::discover` reads: the groups file (a key for every `group_of` call it makes —
/// `models`, `full`, `tla`, `global-mutations`), the two scripts it parses for their `files=(...)` line and crate tokens, and the
/// two scripts that are themselves a unit's target. No `verification/` folder, no `crates/` folder: nothing for the coverage and
/// licence passes of `--check` to complain about, so exit 0 there means the inventory really is current.
///
/// The inventory is written and committed, which is the state the check judges.
fn setup(tag: &str) -> Repo {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("invgone-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    write(
        &d,
        "tools/ci/proof-groups.tsv",
        "# group<TAB>keys\nfixture\tmodels full tla global-mutations\n",
    );
    write(
        &d,
        "scripts/verify-shipped.sh",
        "#!/usr/bin/env bash\nset -euo pipefail\nfiles=()\n",
    );
    write(
        &d,
        "scripts/verify-mutations.sh",
        "#!/usr/bin/env bash\nset -euo pipefail\n",
    );
    write(
        &d,
        "scripts/verify-models.sh",
        "#!/usr/bin/env bash\necho models\n",
    );
    write(
        &d,
        "scripts/verify.sh",
        "#!/usr/bin/env bash\necho everything\n",
    );
    git(&d, &["init", "-q", "-b", "main"]);
    git(&d, &["add", "-A"]);
    git(&d, &["commit", "-q", "-m", "base"]);
    let (code, out) = run(&d, &["--write"]);
    assert_eq!(code, 0, "inventory --write: {out}");
    git(&d, &["add", "-A"]);
    git(&d, &["commit", "-q", "-m", "inventory"]);
    Repo(d)
}

/// `lucina-ci inventory <flags>` in the fixture, as (exit code, stdout, stderr).
fn run(repo: &Path, flags: &[&str]) -> (i32, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_lucina-ci"))
        .arg("inventory")
        .args(flags)
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

/// The unit's line in the committed inventory, which is what the check compares against.
fn assert_in_committed_inventory(repo: &Path, unit: &str) {
    let tsv = fs::read_to_string(repo.join("tools/ci/proof-inventory.tsv")).unwrap();
    assert!(
        tsv.lines().any(|l| l.starts_with(&format!("{unit}\t"))),
        "{unit} is not in the inventory written at setup, so the check below proves nothing:\n{tsv}"
    );
}

/// Delete the target, put it back, and take the check through both states.
fn gone_and_restored(tag: &str, unit: &str, script: &str) {
    let repo = setup(tag);
    let repo = &repo.0;
    assert_in_committed_inventory(repo, unit);
    let (code, out) = run(repo, &["--check"]);
    assert_eq!(code, 0, "the fresh inventory is current: {out}");

    let held = fs::read(repo.join(script)).unwrap();
    fs::remove_file(repo.join(script)).unwrap();
    let (code, out) = run(repo, &["--check"]);
    // 1 = the inventory is not current. 2 would be the tool refusing to answer at all.
    assert_eq!(code, 1, "{script} deleted: {out}");
    let stderr = out.split("stderr:\n").nth(1).unwrap_or_default();
    assert!(
        stderr
            .lines()
            .any(|l| l == format!("in the inventory but gone: {unit}")),
        "the check must name the unit it is missing, on stderr: {out}"
    );
    // The other failure direction is the silent one: discovery letting the line go by without saying it is gone.
    assert!(
        !stderr
            .lines()
            .any(|l| l == format!("not in the inventory: {unit}")),
        "{unit} is gone from disk, so it cannot be a newly found unit: {out}"
    );
    let stdout = out
        .split("stderr:")
        .next()
        .unwrap_or_default()
        .strip_prefix("stdout:\n")
        .unwrap_or_default();
    assert!(
        !stdout.contains("inventory current"),
        "a unit with no target is never reported as current: {out}"
    );

    fs::write(repo.join(script), held).unwrap();
    let (code, out) = run(repo, &["--check"]);
    assert_eq!(code, 0, "{script} restored: {out}");
}

#[test]
fn a_deleted_models_script_leaves_models_stateright_in_the_inventory_and_fails_the_check() {
    gone_and_restored("models", "models/stateright", "scripts/verify-models.sh");
}

#[test]
fn a_deleted_verify_script_leaves_full_verify_sh_in_the_inventory_and_fails_the_check() {
    gone_and_restored("full", "full/verify-sh", "scripts/verify.sh");
}
