//! LucinaOS proof CI: see docs/ci/VERUS.md.
//!
//!   lucina-ci inventory --check [--snapshot-rules PATH] | --write   the list of proof units and the proof matrix page
//!   lucina-ci trust [--write|--check] [--json]  the trust audit: what each Verus/shipped unit assumes (docs/ci/TRUST-REPORT.md, ratchet
//!                                              in tools/ci/trust-baseline.tsv; --accept-baseline "<reason>" lets --write raise a count)
//!   lucina-ci plan ...                         which groups a commit needs (GitHub matrix)
//!   lucina-ci run --group G --phase P ...      run a group's units, each under its own memory cap
//!   lucina-ci selftest ...                     prove the cap, timeouts and verdicts work on this machine
//!   lucina-ci gate --plan F --reports D        every planned unit verified on this commit?
//!   lucina-ci merge-check ...                  the lagent merge gate: did GitHub verify this commit?
//!   lucina-ci spec --check [--base REF] [--allow WHY] [--merge-gate] | --list [--json]   freeze proof specifications against the base commit (src/spec.rs)
//!   lucina-ci spec amend --list | --template   the amendment log a spec change needs (src/amend.rs, docs/ci/SPEC-AMENDMENTS.md)
//!   lucina-ci roles --check [--base REF] [--merge-gate] | --list [--json|--summary] | --template KIND | --seed   who wrote the code, who proved it, who judged it (src/roles.rs, docs/ci/PROOF-ROLES.md)
//!   lucina-ci mutants-precheck [--base REF] [--unit NAME] [--all] [--root DIR] [--merge-gate] [--quiet] [--verbose]   the text half of the mutants scripts without Verus (src/mutants_pre.rs)
//!   lucina-ci roles --audit | --authors UNIT [--base REF]   the authorship walk alone: both sets per unit, or for one unit (report only)
//!   lucina-ci green-commit|sync-plan|sync-gate the automatic public sync and the install gate (docs/ci/PUBLIC-SYNC.md)

mod amend;
mod authorship;
mod coverage;
mod fast;
mod inventory;
mod mutants_pre;
mod roles;
mod runner;
mod snapshot;
mod spec;
mod srcid;
mod sweep;
mod sync;
mod trust;
mod verdict;

use inventory::Unit;
use runner::Spec;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use verdict::{CheckRun, MergeVerdict};

const PROOF_MATRIX: &str = "docs/ci/PROOF-MATRIX.md";
const REQUIRED_CHECKS: &[&str] = &["verus-gate", "rust-gate"];

struct Args(Vec<String>);

impl Args {
    fn val(&self, k: &str) -> Option<String> {
        self.0
            .iter()
            .position(|a| a == k)
            .and_then(|i| self.0.get(i + 1))
            .cloned()
    }
    fn vals(&self, k: &str) -> Vec<String> {
        self.0
            .iter()
            .enumerate()
            .filter(|(_, a)| *a == k)
            .filter_map(|(i, _)| self.0.get(i + 1))
            .cloned()
            .collect()
    }
    fn has(&self, k: &str) -> bool {
        self.0.iter().any(|a| a == k)
    }
    fn need(&self, k: &str) -> Result<String, String> {
        self.val(k).ok_or_else(|| format!("{k} is required"))
    }
    fn num(&self, k: &str, default: u64) -> Result<u64, String> {
        self.val(k).map_or(Ok(default), |v| {
            v.parse().map_err(|_| format!("{k}: not a number"))
        })
    }
}

fn repo_root() -> Result<PathBuf, String> {
    let o = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|e| e.to_string())?;
    if !o.status.success() {
        return Err("not inside a git repository".into());
    }
    Ok(PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()))
}

fn cmd_inventory(a: &Args) -> Result<i32, String> {
    let root = repo_root()?;
    let units = inventory::discover(&root)?;
    let tsv = inventory::to_tsv(&units);
    let md = inventory::to_markdown(&units);
    if a.has("--write") {
        fs::write(root.join(inventory::INVENTORY), &tsv).map_err(|e| e.to_string())?;
        fs::create_dir_all(root.join("docs/ci")).map_err(|e| e.to_string())?;
        fs::write(root.join(PROOF_MATRIX), &md).map_err(|e| e.to_string())?;
        println!("{} units written", units.len());
        return Ok(0);
    }
    let committed = fs::read_to_string(root.join(inventory::INVENTORY)).unwrap_or_default();
    let page = fs::read_to_string(root.join(PROOF_MATRIX)).unwrap_or_default();
    if committed != tsv || page != md {
        let old: BTreeSet<&str> = committed
            .lines()
            .map(|l| l.split('\t').next().unwrap_or(""))
            .collect();
        let new: BTreeSet<&str> = tsv
            .lines()
            .map(|l| l.split('\t').next().unwrap_or(""))
            .collect();
        for u in new.difference(&old) {
            eprintln!("not in the inventory: {u}");
        }
        for u in old.difference(&new) {
            eprintln!("in the inventory but gone: {u}");
        }
        eprintln!(
            "the proof inventory is not current: run lucina-ci inventory --write and commit the result"
        );
        return Ok(1);
    }
    let lic = coverage::check_licences(&root)?;
    if !lic.is_empty() {
        for (c, ids) in &lic {
            eprintln!(
                "crates/{c} carries {ids} and the snapshot includes it, but LICENSE does not name crates/{c}"
            );
        }
        eprintln!(
            "add each to the list of code that keeps its own licence in LICENSE (the public copy ships that file)"
        );
        return Ok(1);
    }
    let unguarded = coverage::unguarded_mutant_scripts(&root, &units);
    if !unguarded.is_empty() {
        for u in &unguarded {
            eprintln!(
                "{u}: its mutants.sh does not print `baseline: verifies` and the closing `refused by verification` line"
            );
        }
        eprintln!(
            "the verdict needs both (docs/ci/PROOF-WORKFLOW.md): without the baseline a mutant may be 'refused' by a file that never verified"
        );
        return Ok(1);
    }
    let unverified = coverage::unverified_named_mutants(&root, &units);
    if !unverified.is_empty() {
        for m in &unverified {
            eprintln!("{m}");
        }
        eprintln!(
            "a `# mutants-units:` name must be a proof CI runs: add the file to scripts/verify-shipped.sh, or fix the name (docs/ci/VERUS.md)"
        );
        return Ok(1);
    }
    // The public snapshot must hold every file a proof reads (the private tree only: the public one has no crates/ folder).
    let gaps = coverage::check(&root, &units)?;
    if !gaps.is_empty() {
        for (unit, file) in &gaps {
            eprintln!("{unit} reads {file}, which tools/ci/public-snapshot.rules leaves out");
        }
        eprintln!(
            "the public copy would fail on GitHub with a missing file: add the crate to a `group` line (or the path to `include`) in tools/ci/public-snapshot.rules"
        );
        return Ok(1);
    }
    // The same for the launcher's own tests: the public copy runs `cargo test` of itself at every sync precheck, and twice
    // in a row it died there on a `scripts/tests/` script the rules left out (2026-11-16 max-wait, 2026-11-17 priority).
    // --snapshot-rules points at another copy of the rules: how to ask whether an older one would have passed.
    let rules_path = a
        .val("--snapshot-rules")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("tools/ci/public-snapshot.rules"));
    let tp = coverage::check_test_paths(&root, &rules_path)?;
    if !tp.is_empty() {
        for m in &tp {
            eprintln!("{m}");
        }
        eprintln!(
            "add the path to an `include` line of tools/ci/public-snapshot.rules; if the test only carries the name as fixture text and never reads the real file, a `# private-only <path>` comment line there declares the omission instead"
        );
        return Ok(1);
    }
    println!(
        "{} units: inventory current, snapshot holds what the proofs and the launcher tests read",
        units.len()
    );
    Ok(0)
}

/// `{group, phase}` rows and the units, for the units a change needs.
fn select(units: &[Unit], changed: Option<&[String]>, only_group: Option<&str>) -> Vec<Unit> {
    units
        .iter()
        .filter(|u| match changed {
            None => true,
            Some(c) => c.iter().any(|p| u.affected_by(p)) || only_group == Some(u.group.as_str()),
        })
        .cloned()
        .collect()
}

/// The one unit a `proof/<unit>` branch is for, if it names a unit.
fn proof_unit(units: &[Unit], branch: &str) -> Option<Vec<Unit>> {
    let wanted = branch.strip_prefix("proof/")?;
    units
        .iter()
        .find(|u| u.name == wanted)
        .map(|u| vec![u.clone()])
}

fn changed_between(root: &Path, a: &str, b: &str) -> Result<Vec<String>, String> {
    let o = Command::new("git")
        .args(["diff", "--name-only", a, b])
        .current_dir(root)
        .output()
        .map_err(|e| e.to_string())?;
    if !o.status.success() {
        return Err(format!("git diff {a} {b} failed"));
    }
    Ok(String::from_utf8_lossy(&o.stdout)
        .lines()
        .map(str::to_string)
        .collect())
}

fn merge_base(root: &Path, a: &str, b: &str) -> Option<String> {
    let o = Command::new("git")
        .args(["merge-base", a, b])
        .current_dir(root)
        .output()
        .ok()?;
    o.status
        .success()
        .then(|| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// `mutants-precheck [--base REF] [--unit NAME] [--all] [--root DIR] [--merge-gate] [--quiet] [--verbose]`: see src/mutants_pre.rs.
/// Without `--unit` and `--all` the units are those a file changed against the merge base with REF (and the working tree) reaches.
/// `--verbose` is what lists the twice-matching regions; without them the run says one line and no more.
fn cmd_mutants_precheck(a: &Args) -> Result<i32, String> {
    let root = match a.val("--root") {
        Some(r) => PathBuf::from(r),
        None => repo_root()?,
    };
    let units = mutants_pre::units_of(&root)?;
    let only = a.val("--unit");
    let all = a.has("--all");
    let mut changed: Vec<String> = Vec::new();
    if only.is_none() && !all {
        let base_ref = a
            .val("--base")
            .ok_or("give --base REF, --unit NAME or --all")?;
        let base = merge_base(&root, &base_ref, "HEAD")
            .ok_or_else(|| format!("no merge base with {base_ref}"))?;
        let mut set: BTreeSet<String> =
            changed_between(&root, &base, "HEAD")?.into_iter().collect();
        for args in [
            &["diff", "--name-only", "HEAD"][..],
            &["ls-files", "-o", "--exclude-standard"][..],
        ] {
            if let Ok(o) = Command::new("git").args(args).current_dir(&root).output() {
                set.extend(
                    String::from_utf8_lossy(&o.stdout)
                        .lines()
                        .map(str::to_string),
                );
            }
        }
        changed = set.into_iter().collect();
    }
    let picked = mutants_pre::select(&units, &changed, only.as_deref(), all)?;
    mutants_pre::run(
        &root,
        &picked,
        only.is_some() || all || !a.has("--quiet"),
        a.has("--merge-gate"),
        mutants_pre::total_budget(picked.len()),
        a.has("--verbose"),
    )
}

/// `--event --ref --before --sha --output --plan`. Pull requests and main get everything; another branch gets what its changes
/// reach, and any doubt about the base means everything.
/// `trust [--write|--check] [--json] [--accept-baseline "<reason>"]`: the trust audit (src/trust.rs).
fn cmd_trust(a: &Args) -> Result<i32, String> {
    let root = repo_root()?;
    let units = inventory::load(&root)?;
    let scanned = trust::compute(&root, &units);
    let actual = trust::counts(&scanned);
    let read = |p: &str| fs::read_to_string(root.join(p)).ok();
    let report = trust::render_report_gaps(&scanned, &read(trust::GAPS).unwrap_or_default());
    let baseline = match read(trust::BASELINE) {
        Some(t) => Some(trust::parse_baseline(&t)?),
        None => None,
    };
    let (allow, problems) = trust::parse_allow(&read(trust::ALLOW).unwrap_or_default());
    let max_rlimit: std::collections::BTreeMap<String, u64> = scanned
        .iter()
        .filter_map(|u| u.rlimit_max().map(|v| (u.unit.clone(), v)))
        .collect();
    let outcome = trust::ratchet_values(&actual, &max_rlimit, &allow, &problems);
    let json = a.has("--json");
    let say = |s: &str| {
        if json {
            eprintln!("{s}");
        } else {
            println!("{s}");
        }
    };
    for w in &outcome.warnings {
        eprintln!("warning: {w}");
    }
    if a.has("--write") {
        // Every nonzero count needs an allow entry; without one, a reason must be given on the command line.
        if !outcome.errors.is_empty() {
            match a.val("--accept-baseline") {
                Some(r) if !r.trim().is_empty() => {
                    println!("allow check waived on purpose: {r}");
                    for e in &outcome.errors {
                        println!("  {e}");
                    }
                }
                _ => {
                    for e in &outcome.errors {
                        eprintln!("{e}");
                    }
                    eprintln!(
                        "trust --write refuses a count no allow entry covers: fix it, add a reasoned line to {}, or pass --accept-baseline \"<reason>\"",
                        trust::ALLOW
                    );
                    return Ok(1);
                }
            }
        }
        fs::create_dir_all(root.join("docs/ci")).map_err(|e| e.to_string())?;
        fs::write(root.join(trust::REPORT), &report).map_err(|e| e.to_string())?;
        fs::write(root.join(trust::BASELINE), trust::baseline_tsv(&actual))
            .map_err(|e| e.to_string())?;
        let t = |c| scanned.iter().map(|u| u.count(c)).sum::<usize>();
        say(&format!(
            "{} units: {} assumption, {} escape, {} unsafe, {} loosen, {} rlimit, {} flag written",
            scanned.len(),
            t(trust::Class::Assumption),
            t(trust::Class::Escape),
            t(trust::Class::Unsafe),
            t(trust::Class::Loosen),
            t(trust::Class::Rlimit),
            t(trust::Class::Flag)
        ));
        if json {
            print!("{}", trust::render_json(&scanned, Some(&outcome)));
        }
        return Ok(0);
    }
    if a.has("--check") {
        let mut bad = outcome.errors.clone();
        if baseline.is_none() {
            bad.push(format!(
                "{} is missing: run lucina-ci trust --write",
                trust::BASELINE
            ));
        }
        if let Some(base) = a.val("--base") {
            bad.extend(trust_silent_drops(&root, &base, &scanned, &actual)?);
        }
        if read(trust::REPORT).as_deref() != Some(report.as_str()) {
            bad.push(format!(
                "{} is not current: run lucina-ci trust --write and commit the result",
                trust::REPORT
            ));
        }
        for e in &bad {
            eprintln!("{e}");
        }
        if json {
            let o = trust::Outcome {
                errors: bad.clone(),
                warnings: outcome.warnings.clone(),
            };
            print!("{}", trust::render_json(&scanned, Some(&o)));
        } else if bad.is_empty() {
            println!("trust: {} units, ratchet holds", scanned.len());
        }
        return Ok(if bad.is_empty() { 0 } else { 1 });
    }
    if json {
        print!("{}", trust::render_json(&scanned, Some(&outcome)));
    } else {
        print!("{report}");
    }
    Ok(0)
}

/// `trust --check --base REF`: counts that fell against the base commit's baseline while the source lines holding those sites are
/// all still there (`trust::silent_drops`). Reads the base files with `git show`.
fn trust_silent_drops(
    root: &Path,
    base: &str,
    scanned: &[trust::UnitTrust],
    actual: &trust::Counts,
) -> Result<Vec<String>, String> {
    let show = |rev: &str, p: &str| -> Option<String> {
        let o = Command::new("git")
            .args(["show", &format!("{rev}:{p}")])
            .current_dir(root)
            .output()
            .ok()?;
        o.status
            .success()
            .then(|| String::from_utf8_lossy(&o.stdout).into_owned())
    };
    let Some(text) = show(base, trust::BASELINE) else {
        return Ok(vec![]);
    };
    let base_counts = trust::parse_baseline(&text)?;
    let mut units: BTreeMap<String, Vec<trust::FileDelta>> = BTreeMap::new();
    for u in scanned.iter().filter(|u| !u.is_flags()) {
        let read_base = |f: &str| show(base, f);
        let mut paths: BTreeSet<String> = trust::included_files(&read_base, &u.target)
            .into_iter()
            .collect();
        paths.extend(u.files.iter().map(|f| f.path.clone()));
        units.insert(
            u.unit.clone(),
            paths
                .into_iter()
                .map(|p| trust::FileDelta {
                    base: show(base, &p),
                    work: fs::read_to_string(root.join(&p)).ok(),
                    path: p,
                })
                .collect(),
        );
    }
    let mut errors = trust::silent_drops(&base_counts, actual, &units);
    // The hand-kept gaps file may not go missing (the report would then print "open trust gaps: 0").
    if show(base, trust::GAPS).is_some() && !root.join(trust::GAPS).exists() {
        errors.push(format!(
            "trust: {} exists at the base commit and is missing now; the report would lose its open trust gaps",
            trust::GAPS
        ));
    }
    Ok(errors)
}

fn cmd_plan(a: &Args) -> Result<i32, String> {
    let root = repo_root()?;
    let units = inventory::load(&root)?;
    let event = a.val("--event").unwrap_or_default();
    let git_ref = a.val("--ref").unwrap_or_default();
    let sha = a.need("--sha")?;
    let before = a.val("--before").unwrap_or_default();
    let branch = git_ref.strip_prefix("refs/heads/").unwrap_or("");
    let only_group = branch.strip_prefix("proof/");
    let full = event != "push" || branch == "main";
    // The deep tier runs on demand (--deep), nightly, and on main; never on an ordinary push or pull request.
    let deep = a.has("--deep") || event == "schedule" || (event == "push" && branch == "main");
    let pool: Vec<Unit> = units
        .iter()
        .filter(|u| deep || u.phase() != "deep")
        .cloned()
        .collect();
    let changed: Option<Vec<String>> = if full {
        None
    } else {
        let zero = before.is_empty() || before.chars().all(|c| c == '0');
        let base = if zero {
            merge_base(&root, "origin/main", &sha)
        } else {
            Some(before.clone())
        };
        base.and_then(|b| changed_between(&root, &b, &sha).ok())
    };
    // `proof/<unit>`: exactly that unit, on its own runner, whatever the diff says. `main` keeps the full gate.
    let one = if event == "push" {
        proof_unit(&units, branch)
    } else {
        None
    };
    let single = one.is_some();
    let chosen = one.unwrap_or_else(|| select(&pool, changed.as_deref(), only_group));
    // A deep unit gets a runner of its own; the others share one per group and phase.
    let rows: BTreeSet<(String, String, String)> = chosen
        .iter()
        .map(|u| {
            let unit = if u.phase() == "deep" {
                u.name.clone()
            } else {
                String::new()
            };
            (u.group.clone(), u.phase().to_string(), unit)
        })
        .collect();
    let items: Vec<String> = rows
        .iter()
        .map(|(g, p, u)| format!("{{\"group\":\"{g}\",\"phase\":\"{p}\",\"unit\":\"{u}\"}}"))
        .collect();
    let matrix = format!("{{\"include\":[{}]}}", items.join(","));
    let plan: String = chosen.iter().map(|u| format!("{}\n", u.name)).collect();
    let scope = if single {
        "unit"
    } else if changed.is_none() {
        "all"
    } else {
        "affected"
    };
    fs::write(a.need("--plan")?, &plan).map_err(|e| e.to_string())?;
    let line = format!(
        "matrix={matrix}\nscope={scope}\ncount={}\nany={}\n",
        chosen.len(),
        !chosen.is_empty()
    );
    match a.val("--output") {
        Some(out) => fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(out)
            .and_then(|mut f| std::io::Write::write_all(&mut f, line.as_bytes()))
            .map_err(|e| e.to_string())?,
        None => print!("{line}"),
    }
    println!(
        "plan: {scope}, {} of {} units, {} jobs",
        chosen.len(),
        units.len(),
        rows.len()
    );
    Ok(0)
}

fn spec_for(
    unit: &Unit,
    b: &runner::Budget,
    tc: &runner::Toolchain,
    out: &str,
    timeout_s: u64,
) -> Spec {
    Spec {
        unit: unit.name.clone(),
        group: unit.group.clone(),
        kind: unit.kind.clone(),
        target: unit.target.clone(),
        command: unit.command(),
        out: out.to_string(),
        cap_bytes: b.cap_bytes,
        class: b.class.to_string(),
        mem_total: b.mem_total,
        timeout_s,
        verus: tc.verus.clone(),
        version: tc.version.clone(),
        verus_sha: tc.verus_sha.clone(),
        verifier_sha: tc.verifier_sha.clone(),
        rust: tc.rust.clone(),
        z3: tc.z3.clone(),
    }
}

fn budget_from(a: &Args) -> Result<runner::Budget, String> {
    let small = std::env::var("VERUS_SMALL_RUNNER_CAP_GIB")
        .ok()
        .filter(|s| !s.is_empty());
    let small = small
        .map(|s| {
            s.parse::<u64>()
                .map_err(|_| "VERUS_SMALL_RUNNER_CAP_GIB: not a number".to_string())
        })
        .transpose()?;
    runner::budget(a.num("--mem-cap-gib", 12)?, small, runner::mem_total()?)
}

/// One line per unit: what ran, how long, how much.
fn print_line(out: &str, u: &str) -> Option<String> {
    let json =
        fs::read_to_string(Path::new(out).join(format!("{}.json", u.replace('/', "__")))).ok()?;
    let num = |k: &str| -> f64 {
        json.find(&format!("\"{k}\":"))
            .and_then(|i| {
                json[i + k.len() + 3..]
                    .split([',', '}'])
                    .next()
                    .and_then(|s| s.parse().ok())
            })
            .unwrap_or(0.0)
    };
    let outcome = runner::report_field(&json, "outcome")?;
    Some(format!(
        "{outcome:<16} {u}  {:.1}s  peak {:.2} GiB  verified {}",
        num("duration_s"),
        num("peak_bytes") / (1u64 << 30) as f64,
        num("verified")
    ))
}

fn cmd_run(a: &Args) -> Result<i32, String> {
    let root = repo_root()?;
    std::env::set_current_dir(&root).map_err(|e| e.to_string())?;
    let units = inventory::load(&root)?;
    let wanted: Option<BTreeSet<String>> = a
        .val("--plan")
        .map(|p| fs::read_to_string(p).map(|t| t.lines().map(str::to_string).collect()))
        .transpose()
        .map_err(|e| e.to_string())?;
    let group = a.val("--group");
    let phase = a.val("--phase");
    let name = a.val("--unit");
    let chosen: Vec<&Unit> = units
        .iter()
        .filter(|u| group.as_ref().is_none_or(|g| &u.group == g))
        .filter(|u| phase.as_ref().is_none_or(|p| u.phase() == p))
        .filter(|u| name.as_ref().is_none_or(|n| &u.name == n))
        .filter(|u| wanted.as_ref().is_none_or(|w| w.contains(&u.name)))
        .collect();
    if chosen.is_empty() {
        return Err("no proof unit matches".into());
    }
    let b = budget_from(a)?;
    let verus = a.need("--verus")?;
    let tc = runner::check_toolchain(&root, &verus)?;
    let out = a.need("--out")?;
    let timeout_s = a.num("--unit-timeout-min", 30)? * 60;
    let mut worst = 0;
    println!(
        "cap {} GiB ({}), MemTotal {:.1} GiB, verus {}",
        b.cap_bytes >> 30,
        b.class,
        b.mem_total as f64 / (1u64 << 30) as f64,
        tc.version
    );
    for u in chosen {
        let code = runner::run_unit(&spec_for(u, &b, &tc, &out, timeout_s))?;
        println!(
            "{}",
            print_line(&out, &u.name).unwrap_or_else(|| format!("no report {}", u.name))
        );
        if code != 0 {
            let log = Path::new(&out).join(format!("{}.log", u.name.replace('/', "__")));
            let t = fs::read_to_string(&log).unwrap_or_default();
            let tail: Vec<&str> = t.lines().rev().take(25).collect();
            for l in tail.iter().rev() {
                println!("    {l}");
            }
        }
        worst = worst.max(code);
    }
    Ok(worst)
}

/// Touches `mib` of memory and waits: run as a grandchild to prove that the whole process tree counts against the cap.
fn cmd_alloc(a: &Args) -> Result<i32, String> {
    let mib = a.num("--mib", 0)? as usize;
    let mut blocks: Vec<Vec<u8>> = Vec::new();
    for _ in 0..mib / 16 {
        let mut v = vec![0u8; 16 << 20];
        for p in v.iter_mut().step_by(4096) {
            *p = 1;
        }
        blocks.push(v);
    }
    println!("allocated {} MiB", blocks.len() * 16);
    Ok(0)
}

/// What must be true of the machinery itself, on this machine. Each case is a real run through the same path as a proof.
fn cmd_selftest(a: &Args) -> Result<i32, String> {
    let root = repo_root()?;
    std::env::set_current_dir(&root).map_err(|e| e.to_string())?;
    let b = budget_from(a)?;
    let out = a.need("--out")?;
    let exe = std::env::current_exe()
        .map_err(|e| e.to_string())?
        .display()
        .to_string();
    let verus = a.val("--verus");
    let tc = match &verus {
        Some(v) => runner::check_toolchain(&root, v)?,
        None => runner::Toolchain {
            verus: String::new(),
            version: String::new(),
            verus_sha: String::new(),
            verifier_sha: String::new(),
            rust: String::new(),
            z3: String::new(),
        },
    };
    let full_mib = b.cap_bytes >> 20;
    let mut cases: Vec<(String, String, String, &str, u64, u64)> = vec![
        // name, kind, command, expected outcome, cap MiB, timeout s
        (
            "selftest/oom-grandchild".into(),
            "script".into(),
            format!("bash -c '{exe} __alloc --mib 700; true'; true"),
            "oom",
            256,
            120,
        ),
        (
            "selftest/under-cap".into(),
            "script".into(),
            format!("{exe} __alloc --mib 64"),
            "verified",
            256,
            120,
        ),
        (
            "selftest/timeout".into(),
            "script".into(),
            "sleep 120".into(),
            "timeout",
            256,
            3,
        ),
        (
            "selftest/oom-at-real-cap".into(),
            "script".into(),
            format!("{exe} __alloc --mib {}", full_mib + 512),
            "oom",
            full_mib,
            300,
        ),
    ];
    if verus.is_some() {
        for (f, want) in [
            ("good", "verified"),
            ("bad", "proof_failed"),
            ("vacuous", "vacuous"),
        ] {
            cases.push((
                format!("selftest/verus-{f}"),
                "fixture".into(),
                format!("\"$VERUS\" tools/ci/fixtures/{f}.rs"),
                want,
                full_mib,
                300,
            ));
        }
    }
    let mut failed = 0;
    for (name, kind, command, want, cap, timeout_s) in cases {
        let unit = Unit {
            name: name.clone(),
            group: "selftest".into(),
            kind,
            target: command.clone(),
            inputs: vec![],
        };
        let mut s = spec_for(&unit, &b, &tc, &out, timeout_s);
        s.command = command;
        s.cap_bytes = cap << 20;
        runner::run_unit(&s)?;
        let got =
            fs::read_to_string(Path::new(&out).join(format!("{}.json", name.replace('/', "__"))))
                .ok()
                .and_then(|j| runner::report_field(&j, "outcome"))
                .unwrap_or_else(|| "no report".into());
        let ok = got == want;
        println!(
            "{} {name}: wanted {want}, got {got}",
            if ok { "ok  " } else { "FAIL" }
        );
        failed += !ok as i32;
    }
    Ok((failed > 0) as i32)
}

/// Every planned unit has a report, from this commit, saying verified. Anything else fails, a missing report above all.
fn cmd_gate(a: &Args) -> Result<i32, String> {
    let plan = fs::read_to_string(a.need("--plan")?).map_err(|e| format!("plan: {e}"))?;
    let dir = a.need("--reports")?;
    let sha = a.need("--sha")?;
    let mut reports = BTreeMap::new();
    for e in fs::read_dir(&dir)
        .map_err(|e| format!("reports: {e}"))?
        .flatten()
    {
        if e.path().extension().is_some_and(|x| x == "json") {
            let t = fs::read_to_string(e.path()).unwrap_or_default();
            if let Some(u) = runner::report_field(&t, "unit") {
                reports.insert(u, t);
            }
        }
    }
    let planned: Vec<&str> = plan.lines().filter(|l| !l.is_empty()).collect();
    let mut bad = 0;
    let mut small = 0;
    for u in &planned {
        let verdict = match reports.get(*u) {
            None => "missing report".to_string(),
            Some(t) if runner::report_field(t, "commit").as_deref() != Some(sha.as_str()) => {
                "report is from another commit".to_string()
            }
            Some(t) => {
                if runner::report_field(t, "runner_class").as_deref() == Some("small-runner") {
                    small += 1;
                }
                runner::report_field(t, "outcome").unwrap_or_else(|| "no outcome".into())
            }
        };
        if verdict != "verified" {
            println!("NOT VERIFIED {u}: {verdict}");
            bad += 1;
        }
    }
    println!(
        "{} planned, {} not verified, {} on a small runner",
        planned.len(),
        bad,
        small
    );
    Ok((bad > 0) as i32)
}

fn parse_runs(text: &str) -> Vec<CheckRun> {
    text.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            (f.len() >= 4).then(|| CheckRun {
                name: f[0].into(),
                status: f[1].into(),
                conclusion: f[2].into(),
                app: f[3].into(),
            })
        })
        .collect()
}

/// Do both sides of a merge touch the input of one proof unit? Then the branch's result says nothing about the merged tree.
fn semantic_gap(root: &Path, base: &str, head: &str) -> Result<Vec<String>, String> {
    let units = inventory::load(root)?;
    let mb = merge_base(root, base, head).ok_or("no merge base")?;
    if mb == base {
        return Ok(Vec::new());
    }
    let ours = changed_between(root, &mb, head)?;
    let theirs = changed_between(root, &mb, base)?;
    Ok(units
        .iter()
        .filter(|u| {
            ours.iter().any(|p| u.affected_by(p)) && theirs.iter().any(|p| u.affected_by(p))
        })
        .map(|u| u.name.clone())
        .collect())
}

/// Exit 0 only when GitHub Actions verified `--sha` and no semantic gap exists.
fn cmd_merge_check(a: &Args) -> Result<i32, String> {
    let root = repo_root()?;
    let repo = a.need("--repo")?;
    let sha = a.need("--sha")?;
    let required: Vec<String> = if a.has("--require") {
        a.vals("--require")
    } else {
        REQUIRED_CHECKS.iter().map(|s| s.to_string()).collect()
    };
    let jq = r#".check_runs | sort_by(.id) | reverse | .[] | [.name, .status, (.conclusion // ""), .app.slug, .id] | @tsv"#;
    let path = format!("repos/{repo}/commits/{sha}/check-runs?per_page=100");
    let o = Command::new("gh")
        .args(["api", "--paginate", &path, "--jq", jq])
        .output()
        .map_err(|e| format!("cannot run gh: {e}"))?;
    if !o.status.success() {
        println!(
            "merge-check: GitHub did not answer for {sha}: refusing (a missing result is not a pass)"
        );
        return Ok(1);
    }
    let text = String::from_utf8_lossy(&o.stdout).into_owned();
    let runs = parse_runs(&text);
    // A finished failed gate run in which no compile, test or proof step started (the runner's network failed in a setup step) is
    // not a verdict on the commit: dropped, and the newest remaining result decides (docs/ci/AUTO-SYNC.md). A failure that ran stays.
    let ids: Vec<String> = text
        .lines()
        .map(|l| l.split('\t').nth(4).unwrap_or("").to_string())
        .collect();
    let infra: Vec<bool> = runs
        .iter()
        .zip(&ids)
        .map(|(r, id)| {
            let finished_red = r.status == "completed" && r.conclusion != "success";
            let non_verdict = finished_red
                && r.app == "github-actions"
                && required.contains(&r.name)
                && gate_run_is_infra(&repo, id);
            if non_verdict {
                println!(
                    "merge-check: not counting {} run {id} ({}): no compile, test or proof step started, only the runner's setup failed",
                    r.name, r.conclusion
                );
            }
            non_verdict
        })
        .collect();
    let runs = sync::drop_non_verdicts(&runs, &infra);
    match verdict::merge_verdict(&runs, &required) {
        MergeVerdict::Pass => {}
        MergeVerdict::Pending(w) => {
            println!("merge-check: still running ({w}): wait for CI, then run lagent merge again");
            return Ok(1);
        }
        MergeVerdict::Refuse(w) => {
            println!("merge-check: refused: {w}");
            return Ok(1);
        }
    }
    if let Some(base) = a.val("--base") {
        let gap = semantic_gap(&root, &base, &sha)?;
        if !gap.is_empty() {
            println!(
                "merge-check: main moved in the inputs of {} unit(s) this branch also changes (first: {}): merge main into the branch, push, let CI run",
                gap.len(),
                gap[0]
            );
            return Ok(1);
        }
    }
    println!("merge-check: {} verified {sha}", required.join(", "));
    Ok(0)
}

fn required_checks(a: &Args) -> Vec<String> {
    if a.has("--require") {
        a.vals("--require")
    } else {
        REQUIRED_CHECKS.iter().map(|s| s.to_string()).collect()
    }
}

/// Check runs of one commit, newest first; `Err` when GitHub does not answer.
fn fetch_runs(repo: &str, sha: &str) -> Result<Vec<CheckRun>, String> {
    let jq = r#".check_runs | sort_by(.id) | reverse | .[] | [.name, .status, (.conclusion // ""), .app.slug] | @tsv"#;
    let path = format!("repos/{repo}/commits/{sha}/check-runs?per_page=100");
    let o = Command::new("gh")
        .args(["api", "--paginate", &path, "--jq", jq])
        .output()
        .map_err(|e| format!("cannot run gh: {e}"))?;
    if !o.status.success() {
        return Err(format!("GitHub did not answer for {sha}"));
    }
    Ok(parse_runs(&String::from_utf8_lossy(&o.stdout)))
}

/// Workflow runs of one commit, newest first: what a required check that has not appeared is still waiting on, since that check is
/// the last JOB of its run. `Err` when GitHub does not answer.
fn fetch_workflow_runs(repo: &str, sha: &str) -> Result<Vec<sync::WorkflowRun>, String> {
    let jq = r#".workflow_runs | sort_by(.id) | reverse | .[] | [.id, .name, .status, (.conclusion // "")] | @tsv"#;
    let path = format!("repos/{repo}/actions/runs?head_sha={sha}&per_page=100");
    let o = Command::new("gh")
        .args(["api", "--paginate", &path, "--jq", jq])
        .output()
        .map_err(|e| format!("cannot run gh: {e}"))?;
    if !o.status.success() {
        return Err(format!(
            "GitHub did not answer for the workflow runs of {sha}"
        ));
    }
    Ok(String::from_utf8_lossy(&o.stdout)
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            (f.len() >= 4).then(|| sync::WorkflowRun {
                id: f[0].into(),
                name: f[1].into(),
                status: f[2].into(),
                conclusion: f[3].into(),
            })
        })
        .collect())
}

/// The log line for the workflow-runs API giving no answer: the sync-gate verdict is then the grace period's alone, and the line
/// must say exactly that (grep-able; judged word for word by `the_runs_unknown_line_reads_exactly_what_the_gate_says`).
fn runs_unknown_line(e: &str) -> String {
    format!("sync-gate: runs API gave no answer; decided by the grace period alone ({e})")
}

/// `green-commit --repo R --private DIR [--verify FULLSHA]`: the newest private commit whose public sync is proven.
/// Prints the full sha. Exit 0 found, 1 none is proven (or `--verify`'s commit is not), 3 GitHub did not answer.
fn cmd_green_commit(a: &Args) -> Result<i32, String> {
    let repo = a.need("--repo")?;
    let private = a.need("--private")?;
    let only = a.val("--verify");
    let required = required_checks(a);
    let limit = a.num("--limit", 40)?;
    let jq = r#".[] | [.sha, (.commit.message | split("\n") | .[0])] | @tsv"#;
    let o = Command::new("gh")
        .args([
            "api",
            &format!("repos/{repo}/commits?sha=main&per_page={limit}"),
            "--jq",
            jq,
        ])
        .output()
        .map_err(|e| format!("cannot run gh: {e}"))?;
    if !o.status.success() {
        eprintln!("green-commit: GitHub did not answer: no commit can be called proven");
        return Ok(3);
    }
    let resolve = |short: &str| -> Option<String> {
        let o = Command::new("git")
            .args([
                "-C",
                &private,
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{short}^{{commit}}"),
            ])
            .output()
            .ok()?;
        o.status
            .success()
            .then(|| String::from_utf8_lossy(&o.stdout).trim().to_string())
    };
    // Fetch check runs lazily, newest first, and stop at the first proven answer.
    let mut commits = Vec::new();
    for l in String::from_utf8_lossy(&o.stdout).lines() {
        let Some((sha, msg)) = l.split_once('\t') else {
            continue;
        };
        let Some(short) = sync::parse_sync_subject(msg) else {
            continue;
        };
        let Some(full) = resolve(&short) else {
            continue;
        };
        if only.as_deref().is_some_and(|v| v != full) {
            continue;
        }
        let runs = match fetch_runs(&repo, sha) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("green-commit: {e}");
                return Ok(3);
            }
        };
        let c = sync::PublicCommit {
            sha: sha.into(),
            message: msg.into(),
            runs,
        };
        if let Some((priv_sha, pub_sha)) = sync::newest_proven(
            std::slice::from_ref(&c),
            &required,
            only.as_deref(),
            &resolve,
        ) {
            println!("{priv_sha}");
            eprintln!("green-commit: {priv_sha} is proven by public commit {pub_sha}");
            return Ok(0);
        }
        commits.push(c);
    }
    match only {
        Some(v) => eprintln!(
            "green-commit: {v} has no proven public sync (not synced yet, red, or still running)"
        ),
        None => eprintln!("green-commit: none of the last {limit} public syncs is proven"),
    }
    Ok(1)
}

/// `sync-plan --main SHA --state FILE`: prints `sync <sha>` (exit 0) or `skip: <why>` (exit 10).
fn cmd_sync_plan(a: &Args) -> Result<i32, String> {
    let main = a.need("--main")?;
    let state = fs::read_to_string(a.need("--state")?)
        .ok()
        .and_then(|t| sync::parse_state(&t));
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    match sync::plan(&main, state.as_ref(), now) {
        sync::Plan::Sync(s) => {
            println!("sync {s}");
            Ok(0)
        }
        sync::Plan::Skip(w) => {
            println!("skip: {w}");
            Ok(10)
        }
    }
}

/// `alert-plan --seen no|held|told --newest yes|no`: prints `record notify` words (sync::alert_plan).
fn cmd_alert_plan(a: &Args) -> Result<i32, String> {
    let seen = match a.need("--seen")?.as_str() {
        "no" => sync::SeenCause::No,
        "held" => sync::SeenCause::HeldBack,
        "told" => sync::SeenCause::Told,
        o => return Err(format!("--seen {o}: no, held or told")),
    };
    let p = sync::alert_plan(seen, a.val("--newest").as_deref() == Some("yes"));
    println!(
        "{} {}",
        if p.record { "record" } else { "quiet" },
        if p.notify { "notify" } else { "hold" }
    );
    Ok(0)
}

/// `sweep-target --dir ROOT --limit-gib N [--to-gib M] [--keep TARGET] [--dry-run]`: keeps the fast-check cache under N GiB (sweep.rs).
/// `--single` caps ONE Cargo target dir instead of a root of them: its least recently used build artifacts go, nothing else, and it
/// exits 3 when the target is in use and was left alone. `--limit-kib` and `--to-kib` are the same caps in KiB, for a test.
fn cmd_sweep_target(a: &Args) -> Result<i32, String> {
    let dir = PathBuf::from(a.need("--dir")?);
    let limit = a.num("--limit-gib", 30)?;
    let to = a.num("--to-gib", limit * 4 / 5)?;
    if a.has("--single") {
        // The KiB marks are so a test can run the real thing over a folder of a few KiB; the GiB marks are what people and the
        // timer give.
        let (lim, to) = if a.has("--limit-kib") {
            let lim = a.num("--limit-kib", 0)?;
            (lim, a.num("--to-kib", lim * 4 / 5)?)
        } else {
            sweep::caps_kib(limit, to)
        };
        return sweep::run_single(&dir, lim, to, a.has("--dry-run"));
    }
    let keep = a.val("--keep").map(PathBuf::from);
    sweep::run(&dir, limit, to, keep.as_deref(), a.has("--dry-run"))
}

/// `output-check --kind K < unit.log`: what a unit's log must show for its pass to count (verdict::check_output); exit 1 with the reason.
fn cmd_output_check(a: &Args) -> Result<i32, String> {
    use std::io::Read;
    let mut log = String::new();
    std::io::stdin()
        .read_to_string(&mut log)
        .map_err(|e| format!("stdin: {e}"))?;
    let c = verdict::check_output(&a.need("--kind")?, &log);
    if let Some(w) = c.weak {
        println!("weak refusals: {w}");
    }
    match c.problem {
        Some(p) => {
            println!("not evidence: {p}");
            Ok(1)
        }
        None => {
            println!("ok");
            Ok(0)
        }
    }
}

/// `infra-check`: a failed job's log on stdin; exit 0 when the runner's network failed, not a test (sync::is_infra_failure).
fn cmd_infra_check() -> Result<i32, String> {
    use std::io::Read;
    let mut log = String::new();
    std::io::stdin()
        .read_to_string(&mut log)
        .map_err(|e| format!("stdin: {e}"))?;
    Ok(if sync::is_infra_failure(&log) { 0 } else { 1 })
}

/// `sync-gate --repo R --sha PUBSHA --fast-forward yes|no [--scan-clean yes]`: exit 0 main may move, 10 wait, 11 a required check
/// no run for the commit will produce (transient), 1 stop for good, 3 no answer.
fn cmd_sync_gate(a: &Args) -> Result<i32, String> {
    let repo = a.need("--repo")?;
    let sha = a.need("--sha")?;
    let ff = a.val("--fast-forward").as_deref() == Some("yes");
    let clean = a.val("--scan-clean").as_deref() != Some("no");
    let runs = match fetch_runs(&repo, &sha) {
        Ok(r) => r,
        Err(e) => {
            println!("sync-gate: {e}");
            return Ok(3);
        }
    };
    let since = a.num("--since-push-secs", sync::START_GRACE_SECS)?;
    // The runs of the commit tell a check that has not appeared yet (its workflow is still going) from one that never will. When
    // GitHub does not answer them, the wait is decided by the grace period alone, as it was before 2026-11-16.
    let workflow = fetch_workflow_runs(&repo, &sha).unwrap_or_else(|e| {
        eprintln!("{}", runs_unknown_line(&e));
        Vec::new()
    });
    let step = sync::gate_step(&runs, &workflow, &required_checks(a), since);
    match sync::may_move_main(clean, &step, ff) {
        Ok(()) => {
            println!("sync-gate: both gates success on {sha}");
            Ok(0)
        }
        Err(w) => {
            println!("sync-gate: {w}");
            Ok(match step {
                sync::GateStep::Wait(_) if clean && ff => 10,
                sync::GateStep::Stuck(_) if clean && ff => 11,
                _ => 1,
            })
        }
    }
}

/// Did this gate's run fail only because the runner's setup did? The gate is a job of a workflow run: its failed sibling jobs of the
/// same attempt are read, and every one must be an infrastructure failure (`sync::is_infra_failure`, the narrow classifier) and
/// at least one must exist. Anything gh cannot answer counts as a real failure.
fn gate_run_is_infra(repo: &str, check_run_id: &str) -> bool {
    let gh = |args: &[&str]| -> Option<String> {
        let o = Command::new("gh").args(args).output().ok()?;
        o.status
            .success()
            .then(|| String::from_utf8_lossy(&o.stdout).into_owned())
    };
    let Some(job) = gh(&[
        "api",
        &format!("repos/{repo}/actions/jobs/{check_run_id}"),
        "--jq",
        "[.run_id, .run_attempt] | @tsv",
    ]) else {
        return false;
    };
    let mut w = job.split_whitespace();
    let (Some(run), Some(attempt)) = (w.next(), w.next()) else {
        return false;
    };
    let Some(jobs) = gh(&[
        "api",
        &format!("repos/{repo}/actions/runs/{run}/attempts/{attempt}/jobs"),
        "--jq",
        r#".jobs[] | select(.conclusion == "failure") | [.id, .name] | @tsv"#,
    ]) else {
        return false;
    };
    let mut failed = 0;
    for l in jobs.lines() {
        let Some((id, name)) = l.split_once('\t') else {
            continue;
        };
        if name.ends_with("-gate") {
            continue;
        }
        failed += 1;
        let Some(log) = gh(&["run", "view", "--job", id, "-R", repo, "--log"]) else {
            return false;
        };
        if !sync::is_infra_failure(&log) {
            return false;
        }
    }
    failed > 0
}

/// Members that can build from this checkout alone: those with no absolute path or `file://` git dependency, and none reaching
/// one through a sibling. `manifests`: member directory -> Cargo.toml text.
fn portable_members(
    manifests: &BTreeMap<String, String>,
    exists: &dyn Fn(&str) -> bool,
) -> (Vec<String>, Vec<String>) {
    let mut bad: BTreeSet<String> = BTreeSet::new();
    let mut edges: Vec<(String, String)> = Vec::new();
    for (dir, text) in manifests {
        if !exists(&format!("{dir}/Cargo.toml")) {
            bad.insert(dir.clone());
        }
        for line in text.lines() {
            let l = line.trim_start();
            if l.starts_with('#') {
                continue;
            }
            if l.contains("\"/home/") || l.contains("file:///") {
                bad.insert(dir.clone());
            }
            if let Some(i) = l.find("path = \"") {
                let p: String = l[i + 8..].chars().take_while(|c| *c != '"').collect();
                let mut parts: Vec<&str> = dir.split('/').collect();
                for c in p.split('/') {
                    match c {
                        ".." => {
                            parts.pop();
                        }
                        "." | "" => {}
                        c => parts.push(c),
                    }
                }
                if !exists(&parts.join("/")) {
                    bad.insert(dir.clone());
                }
                edges.push((dir.clone(), parts.join("/")));
            }
        }
    }
    loop {
        let more: Vec<String> = edges
            .iter()
            .filter(|(a, b)| !bad.contains(a) && bad.contains(b))
            .map(|(a, _)| a.clone())
            .collect();
        if more.is_empty() {
            break;
        }
        bad.extend(more);
    }
    let keep = manifests
        .keys()
        .filter(|d| !bad.contains(*d))
        .cloned()
        .collect();
    (keep, bad.into_iter().collect())
}

fn cmd_portable_workspace(a: &Args) -> Result<i32, String> {
    let root = repo_root()?;
    let path = root.join("Cargo.toml");
    let text = fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let line = text
        .lines()
        .find(|l| l.starts_with("members = ["))
        .ok_or("Cargo.toml: no members line")?;
    let members: Vec<String> = line
        .split('"')
        .skip(1)
        .step_by(2)
        .map(str::to_string)
        .collect();
    let mut manifests = BTreeMap::new();
    for m in &members {
        manifests.insert(
            m.clone(),
            fs::read_to_string(root.join(m).join("Cargo.toml")).unwrap_or_default(),
        );
    }
    let (keep, dropped) = portable_members(&manifests, &|p| root.join(p).exists());
    println!(
        "{} of {} workspace members can build on a runner; {} need sibling repositories and are not checked here:",
        keep.len(),
        members.len(),
        dropped.len()
    );
    for d in &dropped {
        println!("- {d}");
    }
    if a.has("--write") {
        let kept: Vec<String> = keep.iter().map(|m| format!("\"{m}\"")).collect();
        let new = format!("members = [{}]", kept.join(", "));
        fs::write(&path, text.replace(line, &new)).map_err(|e| e.to_string())?;
    }
    if a.has("--delete") {
        // A member some proof reads stays on disk, outside the workspace: removing it would remove the proof.
        let mut shipped =
            fs::read_to_string(root.join("scripts/verify-shipped.sh")).unwrap_or_default();
        for f in String::from_utf8_lossy(
            &Command::new("git")
                .args(["ls-files", "verification"])
                .current_dir(&root)
                .output()
                .map_err(|e| e.to_string())?
                .stdout,
        )
        .lines()
        {
            shipped.push_str(&fs::read_to_string(root.join(f)).unwrap_or_default());
        }
        for d in &dropped {
            if !shipped.contains(&format!("{d}/")) && root.join(d).exists() {
                fs::remove_dir_all(root.join(d)).map_err(|e| e.to_string())?;
            }
        }
    }
    Ok(0)
}

/// `--rules F --out D`: the public copy of the tracked files. Nothing is pushed.
fn cmd_snapshot(a: &Args) -> Result<i32, String> {
    let root = repo_root()?;
    let rules =
        snapshot::parse_rules(&fs::read_to_string(a.need("--rules")?).map_err(|e| e.to_string())?)?;
    let out = PathBuf::from(a.need("--out")?);
    fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let (copied, left_out) = snapshot::build(&root, &rules, &out)?;
    println!("{copied} files copied, {left_out} left out");
    Ok(0)
}

fn main() -> ExitCode {
    let mut all: Vec<String> = std::env::args().skip(1).collect();
    if all.is_empty() {
        eprintln!("usage: lucina-ci inventory|trust|plan|run|selftest|gate|merge-check ...");
        return ExitCode::from(2);
    }
    let cmd = all.remove(0);
    if cmd == "__inner" {
        return ExitCode::from(runner::inner(&all) as u8);
    }
    let a = Args(all);
    let r = match cmd.as_str() {
        "inventory" => cmd_inventory(&a),
        "trust" => cmd_trust(&a),
        "plan" => cmd_plan(&a),
        "run" => cmd_run(&a),
        "selftest" => cmd_selftest(&a),
        "gate" => cmd_gate(&a),
        "merge-check" => cmd_merge_check(&a),
        "green-commit" => cmd_green_commit(&a),
        "sync-plan" => cmd_sync_plan(&a),
        "sync-gate" => cmd_sync_gate(&a),
        "infra-check" => cmd_infra_check(),
        "output-check" => cmd_output_check(&a),
        "sweep-target" => cmd_sweep_target(&a),
        "alert-plan" => cmd_alert_plan(&a),
        "portable-workspace" => cmd_portable_workspace(&a),
        "snapshot" => cmd_snapshot(&a),
        "spec" => spec::cmd(&a.0),
        "roles" => roles::cmd(&a.0),
        "mutants-precheck" => cmd_mutants_precheck(&a),
        "check-fast" => cmd_check_fast(&a),
        "perf" => cmd_perf(&a),
        "__alloc" => cmd_alloc(&a),
        _ => Err(format!("unknown command {cmd}")),
    };
    match r {
        Ok(c) => ExitCode::from(c as u8),
        Err(e) => {
            eprintln!("lucina-ci: {e}");
            ExitCode::from(2)
        }
    }
}

/// One step of `check-fast` under its own scope. Returns (passed, seconds).
/// `gate_limit` is the merge-gate limit actually enforced on this step: `Some` prints it beside the measured time
/// (`ok   unit-tests-tools-ci-lucina-ci  197.3s / 300s`) and `near-limit` when the step used more than 80% of it.
/// The columns before it are exactly what they were, so nothing that reads the line has to change.
#[allow(clippy::too_many_arguments)]
fn fast_step(
    name: &str,
    command: &str,
    kind: &str,
    timeout_s: u64,
    gate_limit: Option<u64>,
    b: &runner::Budget,
    tc: &runner::Toolchain,
    out: &str,
) -> Result<(bool, f64), String> {
    let unit = Unit {
        name: format!("fast/{name}"),
        group: "fast".into(),
        kind: kind.into(),
        target: name.into(),
        inputs: vec![],
    };
    let mut s = spec_for(&unit, b, tc, out, timeout_s);
    s.command = command.to_string();
    let t = std::time::Instant::now();
    let code = runner::run_unit(&s)?;
    let secs = t.elapsed().as_secs_f64();
    let ok = code == 0;
    let mut line = format!(
        "{} {name:<28} {secs:>6.1}s",
        if ok { "ok  " } else { "FAIL" }
    );
    if let Some(l) = gate_limit {
        line.push_str(&format!(" / {l}s"));
        if ok && secs > MERGE_NEAR_LIMIT_SHARE * l as f64 {
            line.push_str("  near-limit");
        }
    }
    println!("{line}");
    if !ok {
        let t = fs::read_to_string(
            Path::new(out).join(format!("fast__{}.log", name.replace('/', "__"))),
        )
        .unwrap_or_default();
        let lines: Vec<&str> = t.lines().collect();
        // A refused spec change must show its whole list (the reviewer reads it): up to 300 lines, else the first 240 and the last 3.
        let shown: Vec<String> = if name == "spec" && lines.len() <= 300 {
            lines.iter().map(|l| l.to_string()).collect()
        } else if name == "spec" {
            let mut v: Vec<String> = lines[..240].iter().map(|l| l.to_string()).collect();
            v.push(format!(
                "... {} lines omitted: run `lucina-ci spec --check` for all of them ...",
                lines.len() - 243
            ));
            v.extend(lines[lines.len() - 3..].iter().map(|l| l.to_string()));
            v
        } else {
            lines[lines.len().saturating_sub(30)..]
                .iter()
                .map(|l| l.to_string())
                .collect()
        };
        for l in &shown {
            println!("    {l}");
        }
    }
    Ok((ok, secs))
}

fn tool_exists(t: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {t}")])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// The merge-gate limit of one step, in seconds, beside the default of 180. Two steps do not fit 180 s on a loaded
/// buildhost, and a gate that fails on the clock while the tests pass teaches no one anything, so each is named here with
/// the room it needs. Measured 2026-11-17 in this worktree, `scripts/check-fast --base main` three times in a `bq` short
/// slot (docs/ci/SHIFT-LEFT.md, "Known slow tests"): `unit-tests-tools-ci-lucina-ci` 160.2 s at load 26, 300.5 s (killed
/// by its own timeout) at load 30, 297.4 s at load 41; `mutants-text` 107.0, 162.3, 116.7 s at those loads.
/// A limit never raises a step above its own timeout (the `t` of its `step(...)` call): it only ever cuts, so
/// `unit-tests-tools-ci-lucina-ci` here means "give it its whole 300 s at the gate", and `mutants-text` is a cut from
/// the 360 s the step is built with (`mutants_pre::STEP_TIMEOUT_S`, above the tool's own 260 s sweep budget).
const MERGE_STEP_LIMITS: &[(&str, u64)] = &[
    ("unit-tests-tools-ci-lucina-ci", 300),
    ("mutants-text", 240),
];
/// The limit of a step named in no table and no override.
const MERGE_STEP_LIMIT_DEFAULT: u64 = 180;
/// A step that used more than this share of its limit prints `near-limit`, so the next raise of a limit comes from
/// a line in the log rather than from a merge that failed.
const MERGE_NEAR_LIMIT_SHARE: f64 = 0.8;

/// `LUCINA_CHECK_FAST_MERGE_CAP_S`: the gate's own override of every limit above.
/// A plain number is the limit of EVERY step (the semantics since 2026-11-12, and what a test injects so it never
/// sleeps a real 180 s); `step=SECONDS` entries separated by commas give one step another limit and win over a plain
/// number beside them, which is how a test drives a step the table names. Tokens that are neither are ignored.
#[derive(Default)]
struct MergeCaps {
    plain: Option<u64>,
    per_step: Vec<(String, u64)>,
}

impl MergeCaps {
    /// The override as read from the environment.
    fn from_env() -> Self {
        Self::parse(&std::env::var("LUCINA_CHECK_FAST_MERGE_CAP_S").unwrap_or_default())
    }

    fn parse(text: &str) -> Self {
        let mut c = MergeCaps::default();
        for tok in text.split(',') {
            let tok = tok.trim();
            if let Some((name, secs)) = tok.split_once('=') {
                if let Ok(s) = secs.trim().parse::<u64>() {
                    c.per_step.push((name.trim().to_string(), s));
                }
            } else if let Ok(s) = tok.parse::<u64>() {
                c.plain = Some(s);
            }
        }
        c
    }

    /// The most `name` may run at the merge gate, or `None` when the gate does not cut it: its own override, then the
    /// override for every step, then the table, then `default` for a step that is not required. A required step the
    /// override and the table both leave alone keeps its own timeout.
    fn limit_of(&self, name: &str, default: u64) -> Option<u64> {
        if let Some((_, s)) = self.per_step.iter().find(|(n, _)| n == name) {
            return Some(*s);
        }
        if self.plain.is_some() {
            return self.plain;
        }
        if let Some((_, s)) = MERGE_STEP_LIMITS.iter().find(|(n, _)| *n == name) {
            return Some(*s);
        }
        if FAST_STEPS_REQUIRED.contains(&name) {
            return None;
        }
        Some(default)
    }
}

/// The merge-gate limit of one step: what `LUCINA_CHECK_FAST_MERGE_CAP_S` says, else the table, else 180.
/// The clock is the step's OWN run time: `scripts/check-fast` takes its queue slot (`bq run --class short --max-wait ...`)
/// before this binary starts, so a step's limit starts after the wait.
fn merge_step_limit(name: &str) -> Option<u64> {
    let caps = MergeCaps::from_env();
    caps.limit_of(name, merge_step_cap())
}

/// The default merge-gate limit: `LUCINA_CHECK_FAST_MERGE_CAP_S` when it is one plain number, else 180. It is what a step
/// no override and no table row names gets; `merge_step_limit` resolves a named one.
fn merge_step_cap() -> u64 {
    MergeCaps::from_env()
        .plain
        .unwrap_or(MERGE_STEP_LIMIT_DEFAULT)
}

/// One step of `check-fast`, with the merge-gate limit applied. `Some(entry)` is a failure. At the merge gate a step past its limit
/// is a failure that says so (`past its N s limit at merge`), never a skip: the limit is `merge_step_limit` for the step — 180 by
/// default, more for the two steps in `MERGE_STEP_LIMITS`, anything the override sets — and it never cuts above the step's own
/// timeout. A required step no override names (`spec`, `roles`, `trust`, `inventory`, and `mutants-text` unless the table lists it,
/// see FAST_STEPS_REQUIRED) keeps its full limit.
#[allow(clippy::too_many_arguments)]
fn gated_step(
    name: &str,
    cmd: &str,
    kind: &str,
    t: u64,
    merge_gate: bool,
    b: &runner::Budget,
    tc: &runner::Toolchain,
    out: &str,
) -> Result<Option<String>, String> {
    let limit = merge_gate.then(|| merge_step_limit(name)).flatten();
    let t = match limit {
        Some(l) => t.min(l),
        None => t,
    };
    if fast_step(name, cmd, kind, t, merge_gate.then_some(t), b, tc, out)?.0 {
        return Ok(None);
    }
    let json =
        fs::read_to_string(Path::new(out).join(format!("fast__{}.json", name.replace('/', "__"))))
            .unwrap_or_default();
    if merge_gate && runner::report_field(&json, "outcome").as_deref() == Some("timeout") {
        let m = format!(
            "{name}: past its {t} s limit at merge: the machine is loaded; retry when it is quieter, or run scripts/check-fast in the worktree"
        );
        println!("    {m}");
        return Ok(Some(m));
    }
    Ok(Some(name.to_string()))
}

/// The steps this binary can run, in order. `check-fast` prints them first, and refuses to run when a required one is not here.
const FAST_STEPS: &[&str] = &[
    "diffgate",
    "build",
    "shellcheck",
    "actionlint",
    "secrets",
    "inventory",
    "spec",
    "roles",
    "trust",
    "mutants-text",
    "proofs",
];
/// Steps that must be registered in the binary, and that are refusals (not skips) when they run out of time at the merge gate.
const FAST_STEPS_REQUIRED: &[&str] = &["inventory", "spec", "roles", "trust", "mutants-text"];

/// The required steps that `registered` lacks: a `check-fast` built from a tree that predates a step must not pass for the full gate.
fn missing_required_steps(registered: &[&str]) -> Vec<&'static str> {
    FAST_STEPS_REQUIRED
        .iter()
        .copied()
        .filter(|r| !registered.contains(r))
        .collect()
}

/// Prints a line when `check-fast` ends any way other than through its summary line (a panic, an early `?`), so a log never just stops.
struct FastEnd {
    started: std::time::Instant,
    done: bool,
}

impl Drop for FastEnd {
    fn drop(&mut self) {
        if !self.done {
            println!(
                "check-fast: aborted after {:.0}s, no summary: the run did not finish",
                self.started.elapsed().as_secs_f64()
            );
        }
    }
}

/// The merge waivers of the job that runs this gate (LA-C014): `lagent merge --allow rule=reason` writes
/// `<job dir>/waivers.jsonl` — one JSON object per line with exactly the four string fields `rule`, `reason`, `by`, `job` —
/// and names it in `LAGENT_WAIVERS_FILE` for its gate commands. The variable is set only when the job was given at least one
/// `--allow`, so an ordinary run of this loop reads nothing. Why: the merge gate waived its refusals and the `diffgate` step
/// of `check-fast --merge-gate` refused the very same lines again with "0 waived" (finding GATE-01, job_4ee819df2427).
const WAIVERS_FILE_ENV: &str = "LAGENT_WAIVERS_FILE";
/// Which waiver rules the `diffgate` step forwards: one name per line, `#` comments (tools/ci/merge-waiver-rules.txt).
/// Data and not a constant here because adding a rule is a gate change a human reads, like every other file of `tools/ci/`.
const WAIVER_RULES_FILE: &str = "tools/ci/merge-waiver-rules.txt";
/// The fields of a waivers line, all four of them strings and nothing else.
const WAIVER_FIELDS: [&str; 4] = ["rule", "reason", "by", "job"];

/// One waiver of this merge: its rule and the reason exactly as `lagent merge --allow` was given it.
struct Waiver {
    rule: String,
    reason: String,
}

fn json_skip_ws(b: &[char], i: &mut usize) {
    while b.get(*i).is_some_and(|c| c.is_ascii_whitespace()) {
        *i += 1;
    }
}

/// The four hex digits of a `\u` escape (read past the `u`), a surrogate pair joined as JSON says it must.
fn json_codepoint(b: &[char], i: &mut usize) -> Result<char, String> {
    let hex4 = |i: &mut usize| -> Result<u32, String> {
        let d: String = b
            .get(*i..*i + 4)
            .ok_or("\\u needs four hex digits")?
            .iter()
            .collect();
        let v = u32::from_str_radix(&d, 16).map_err(|_| "\\u needs four hex digits")?;
        *i += 4;
        Ok(v)
    };
    let hi = hex4(i)?;
    if !(0xd800..0xdc00).contains(&hi) {
        return char::from_u32(hi).ok_or_else(|| "\\u names no character".to_string());
    }
    if b.get(*i) != Some(&'\\') || b.get(*i + 1) != Some(&'u') {
        return Err("a high surrogate needs its low pair".into());
    }
    *i += 2;
    let lo = hex4(i)?;
    if !(0xdc00..0xe000).contains(&lo) {
        return Err("a high surrogate needs its low pair".into());
    }
    char::from_u32(0x10000 + (hi - 0xd800) * 0x400 + (lo - 0xdc00))
        .ok_or_else(|| "\\u names no character".to_string())
}

/// One JSON string at `i`: the six short escapes, a backslash-u codepoint, and no raw control character.
fn json_string(b: &[char], i: &mut usize) -> Result<String, String> {
    if b.get(*i) != Some(&'"') {
        return Err("not a string".into());
    }
    *i += 1;
    let mut out = String::new();
    loop {
        let Some(&c) = b.get(*i) else {
            return Err("unterminated string".into());
        };
        *i += 1;
        match c {
            '"' => return Ok(out),
            '\\' => {
                let Some(&e) = b.get(*i) else {
                    return Err("unterminated escape".into());
                };
                *i += 1;
                match e {
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    '/' => out.push('/'),
                    'b' => out.push('\u{8}'),
                    'f' => out.push('\u{c}'),
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    'u' => out.push(json_codepoint(b, i)?),
                    _ => return Err(format!("unknown escape \\{e}")),
                }
            }
            c if (c as u32) < 0x20 => return Err("an unescaped control character".into()),
            c => out.push(c),
        }
    }
}

/// One flat JSON object whose every value is a string, as pairs in the order written. A number, a nested object or array, a
/// missing colon, a trailing comma or text after the closing brace is an error: what this gate cannot read exactly it does
/// not act on.
fn json_object(b: &[char], i: &mut usize) -> Result<Vec<(String, String)>, String> {
    json_skip_ws(b, i);
    if b.get(*i) != Some(&'{') {
        return Err("not a JSON object".into());
    }
    *i += 1;
    let mut out = Vec::new();
    json_skip_ws(b, i);
    if b.get(*i) == Some(&'}') {
        *i += 1;
        return Ok(out);
    }
    loop {
        json_skip_ws(b, i);
        let k = json_string(b, i).map_err(|e| format!("field name: {e}"))?;
        json_skip_ws(b, i);
        if b.get(*i) != Some(&':') {
            return Err(format!("`{k}` has no value after its name"));
        }
        *i += 1;
        json_skip_ws(b, i);
        if b.get(*i) != Some(&'"') {
            return Err(format!("the value of `{k}` is not a string"));
        }
        let v = json_string(b, i)?;
        out.push((k, v));
        json_skip_ws(b, i);
        match b.get(*i) {
            Some(',') => *i += 1,
            Some('}') => {
                *i += 1;
                return Ok(out);
            }
            _ => {
                return Err(format!(
                    "`{}` after a field, expected `,` or `}}`",
                    b.get(*i).map_or("end".into(), |c| c.to_string())
                ));
            }
        }
    }
}

/// One waivers line: an object of exactly the four string fields, with a reason that says something.
fn parse_waiver_line(line: &str) -> Result<Waiver, String> {
    let b: Vec<char> = line.chars().collect();
    let mut i = 0;
    let pairs = json_object(&b, &mut i)?;
    json_skip_ws(&b, &mut i);
    if i != b.len() {
        return Err("text after the closing `}`".into());
    }
    let names: Vec<&str> = pairs.iter().map(|(k, _)| k.as_str()).collect();
    for f in WAIVER_FIELDS {
        if !names.contains(&f) {
            return Err(format!("no `{f}` field"));
        }
    }
    if pairs.len() != WAIVER_FIELDS.len() {
        return Err(format!(
            "{} fields where a waivers line holds {}",
            pairs.len(),
            WAIVER_FIELDS.len()
        ));
    }
    let field = |f: &str| {
        pairs
            .iter()
            .find(|(k, _)| k == f)
            .map_or(String::new(), |(_, v)| v.clone())
    };
    let reason = field("reason");
    if reason.trim().is_empty() {
        return Err("`reason` is empty".into());
    }
    Ok(Waiver {
        rule: field("rule"),
        reason,
    })
}

/// The rules whose waivers reach diffgate (`WAIVER_RULES_FILE`). A list that cannot be read forwards nothing, which leaves
/// every waiver unused and every refusal refused: the safe direction for a gate.
fn forwarded_waiver_rules(root: &Path) -> Vec<String> {
    fs::read_to_string(root.join(WAIVER_RULES_FILE))
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_string)
        .collect()
}

/// The waivers this merge job was given: `None` when `LAGENT_WAIVERS_FILE` is not set, which is every run that is not a
/// merge with an allow and leaves the step exactly as it was; `Err` when it is set but is not the file the merge job writes.
/// Every line is read or none is, so a file with one bad line waives nothing.
fn merge_waivers() -> Result<Option<Vec<Waiver>>, String> {
    let Some(p) = std::env::var_os(WAIVERS_FILE_ENV) else {
        return Ok(None);
    };
    let p = PathBuf::from(p);
    let text =
        fs::read_to_string(&p).map_err(|e| format!("{} cannot be read: {e}", p.display()))?;
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let w =
            parse_waiver_line(line).map_err(|e| format!("{} line {}: {e}", p.display(), n + 1))?;
        out.push(w);
    }
    if out.is_empty() {
        return Err(format!("{} holds no waiver line", p.display()));
    }
    Ok(Some(out))
}

/// The diffgate call of the step and the one line it prints about the merge waivers. A forwarded reason is passed through
/// unchanged, so diffgate's counted form `RULE=N:reason` reaches it byte for byte; the quotes are the shell's problem
/// (`shell_quote`), since the step runs its command through `bash -c`.
fn diffgate_with_waivers(base: &str, waivers: &[Waiver], rules: &[String]) -> (String, String) {
    let mut cmd = format!("diffgate --repo . --base {base}");
    let (fwd, kept): (Vec<&Waiver>, Vec<&Waiver>) =
        waivers.iter().partition(|w| rules.contains(&w.rule));
    for w in &fwd {
        cmd.push_str(&format!(
            " --allow {}",
            shell_quote(&format!("{}={}", w.rule, w.reason))
        ));
    }
    let fwd_list = if fwd.is_empty() {
        String::new()
    } else {
        format!(
            " ({})",
            fwd.iter()
                .map(|w| format!("{}: {}", w.rule, w.reason))
                .collect::<Vec<_>>()
                .join("; ")
        )
    };
    let kept_list = if kept.is_empty() {
        String::new()
    } else {
        format!(
            " ({})",
            kept.iter()
                .map(|w| w.rule.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let summary = format!(
        "diffgate waivers: {} forwarded{fwd_list}, {} not forwarded{kept_list}",
        fwd.len(),
        kept.len()
    );
    (cmd, summary)
}

/// `check-fast [--base REF] [--proofs] [--all-proofs] [--mutants] [--no-build]`
fn cmd_check_fast(a: &Args) -> Result<i32, String> {
    let root = repo_root()?;
    std::env::set_current_dir(&root).map_err(|e| e.to_string())?;
    let started = std::time::Instant::now();
    let mut end = FastEnd {
        started,
        done: false,
    };
    let tree = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".into());
    let built = env!("LUCINA_CI_SRC_ID");
    println!(
        "lucina-ci src {built} (tree {tree}) checks: {}",
        FAST_STEPS.join(" ")
    );
    let tree_id = srcid::compute(&root.join("tools/ci/lucina-ci"));
    if let Some(m) = srcid::mismatch(built, tree_id.as_deref()) {
        end.done = true;
        eprintln!("check-fast: {m}");
        println!("check-fast: refused, binary and tree sources differ");
        return Ok(1);
    }
    let missing = missing_required_steps(FAST_STEPS);
    if !missing.is_empty() {
        end.done = true;
        eprintln!(
            "check-fast: this lucina-ci ({tree}) has no step {}: refusing to run an older gate than the tree needs",
            missing.join(", ")
        );
        println!(
            "check-fast: refused, missing step(s) {}",
            missing.join(", ")
        );
        return Ok(1);
    }
    let base_ref = a.val("--base").unwrap_or_else(|| "main".into());
    let base = merge_base(&root, &base_ref, "HEAD")
        .ok_or_else(|| format!("no merge base with {base_ref}"))?;
    let mut changed: BTreeSet<String> =
        changed_between(&root, &base, "HEAD")?.into_iter().collect();
    let git_lines = |args: &[&str]| -> Vec<String> {
        Command::new("git")
            .args(args)
            .output()
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    };
    changed.extend(git_lines(&["diff", "--name-only", "HEAD"]));
    changed.extend(git_lines(&["ls-files", "-o", "--exclude-standard"]));
    let existing: Vec<String> = changed
        .iter()
        .filter(|p| root.join(p).exists())
        .cloned()
        .collect();
    if changed.is_empty() {
        end.done = true;
        println!("check-fast: nothing changed against {base_ref}");
        return Ok(0);
    }
    println!(
        "check-fast: {} changed file(s) against {base_ref}",
        changed.len()
    );
    let b = runner::budget(a.num("--mem-cap-gib", 8)?, None, runner::mem_total()?)?;
    let out = std::env::temp_dir()
        .join(format!("check-fast-{}", std::process::id()))
        .display()
        .to_string();
    let none = runner::Toolchain {
        verus: String::new(),
        version: String::new(),
        verus_sha: String::new(),
        verifier_sha: String::new(),
        rust: String::new(),
        z3: String::new(),
    };
    let mut failed: Vec<String> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    // At `lagent merge` a step that runs past its limit FAILS the merge: a gate that reports "0 failed" for tests it did not run is
    // not a gate (2026-11-12: unit-tests skipped at the merge gate under load on SecretaryIOS). docs/ci/SHIFT-LEFT.md.
    let merge_gate = a.has("--merge-gate");
    let step = |name: &str,
                cmd: &str,
                kind: &str,
                t: u64,
                tc: &runner::Toolchain,
                failed: &mut Vec<String>|
     -> Result<(), String> {
        if let Some(f) = gated_step(name, cmd, kind, t, merge_gate, &b, tc, &out)? {
            failed.push(f);
        }
        Ok(())
    };

    if tool_exists("diffgate") {
        // With no LAGENT_WAIVERS_FILE the command is the plain one this step has always run (finding GATE-01).
        match merge_waivers() {
            Ok(None) => step(
                "diffgate",
                &format!("diffgate --repo . --base {base}"),
                "script",
                60,
                &none,
                &mut failed,
            )?,
            Ok(Some(ws)) => {
                let (cmd, summary) =
                    diffgate_with_waivers(&base, &ws, &forwarded_waiver_rules(&root));
                println!("{summary}");
                step("diffgate", &cmd, "script", 60, &none, &mut failed)?;
            }
            // A waivers file this step cannot read exactly is not a reason to waive nothing quietly: the step fails,
            // diffgate never runs, and the message says which line of which file is what.
            Err(e) => step(
                "diffgate",
                &format!(
                    "echo {} >&2; exit 1",
                    shell_quote(&format!(
                        "diffgate waivers: {e}: refusing, nothing is waived"
                    ))
                ),
                "script",
                60,
                &none,
                &mut failed,
            )?,
        }
    } else {
        skipped.push("diffgate: not installed here".into());
    }
    let members =
        fast::root_members(&fs::read_to_string(root.join("Cargo.toml")).unwrap_or_default());
    let owners: BTreeSet<fast::Owner> = existing
        .iter()
        .filter_map(|p| fast::owner_of(&root, &members, p))
        .collect();
    for o in &owners {
        if let fast::Owner::Other(d) = o {
            skipped.push(format!(
                "{d}: not buildable by the fast loop; its gate runs on merge"
            ));
        }
    }
    if !a.has("--no-build") {
        for (name, cmd, t) in fast::cargo_steps(&owners, &existing) {
            step(&name, &cmd, "script", t, &none, &mut failed)?;
        }
    } else {
        skipped.push("build, clippy and unit tests: --no-build".into());
    }
    let sh: Vec<&str> = existing
        .iter()
        .filter(|p| p.ends_with(".sh"))
        .map(String::as_str)
        .collect();
    if !sh.is_empty() {
        if tool_exists("shellcheck") {
            step(
                "shellcheck",
                &format!("shellcheck -S error {}", sh.join(" ")),
                "script",
                30,
                &none,
                &mut failed,
            )?;
        } else {
            skipped.push("shellcheck: not installed here".into());
        }
    }
    if existing.iter().any(|p| p.starts_with(".github/workflows/")) && tool_exists("actionlint") {
        step(
            "actionlint",
            "actionlint -color=false .github/workflows/*.yml",
            "script",
            30,
            &none,
            &mut failed,
        )?;
    }
    if tool_exists("gitleaks") {
        let config = if root.join(".gitleaks.toml").exists() {
            "--config .gitleaks.toml"
        } else {
            ""
        };
        let list = existing
            .iter()
            .filter(|p| !p.starts_with("third_party/"))
            .map(|p| format!("'{}'", p.replace('\'', "'\\''")))
            .collect::<Vec<_>>()
            .join(" ");
        if !list.is_empty() {
            let cmd = format!(
                "for f in {list}; do gitleaks dir --no-banner --redact {config} \"$f\" || exit 1; done"
            );
            step("secrets", &cmd, "script", 60, &none, &mut failed)?;
        }
    } else {
        skipped.push("secret scan: gitleaks not installed here".into());
    }

    // The proofs the change reaches. Inventory first: a new proof must be listed.
    let exe = std::env::current_exe()
        .map_err(|e| e.to_string())?
        .display()
        .to_string();
    if root.join(inventory::INVENTORY).exists() {
        step(
            "inventory",
            &format!("{exe} inventory --check"),
            "script",
            30,
            &none,
            &mut failed,
        )?;
        // The proof specifications may not be weakened silently (src/spec.rs): compared with the base commit's tree.
        step(
            "spec",
            &format!(
                "{exe} spec --check --base {base}{}",
                if merge_gate { " --merge-gate" } else { "" }
            ),
            "script",
            60,
            &none,
            &mut failed,
        )?;
        // The model that writes the code must not prove it or judge the proof (src/roles.rs): compared with the base commit's tree.
        step(
            "roles",
            &format!(
                "{exe} roles --check --base {base}{}",
                if merge_gate { " --merge-gate" } else { "" }
            ),
            "script",
            300,
            &none,
            &mut failed,
        )?;
    } else {
        skipped.push("proof inventory: this worktree predates it".into());
        if merge_gate {
            failed.push("inventory, spec, roles: skipped at the merge gate because tools/ci/proof-inventory.tsv is missing".into());
        }
    }
    if root.join(trust::BASELINE).exists() {
        step(
            "trust",
            &format!("{exe} trust --check --base {base}"),
            "script",
            60,
            &none,
            &mut failed,
        )?;
    } else if merge_gate {
        failed.push(format!(
            "trust: skipped at the merge gate because {} is missing",
            trust::BASELINE
        ));
    }
    // The text half of the mutants scripts, without Verus (src/mutants_pre.rs): a formatter that re-wraps a shipped file must not turn a
    // unit red on GitHub. Only the units the change reaches; nothing reached is silent.
    if root.join(inventory::INVENTORY).exists() {
        step(
            "mutants-text",
            &format!(
                "{exe} mutants-precheck --quiet --base {base}{}",
                if merge_gate { " --merge-gate" } else { "" }
            ),
            "script",
            mutants_pre::STEP_TIMEOUT_S,
            &none,
            &mut failed,
        )?;
    } else if merge_gate {
        failed.push(format!(
            "mutants-text: skipped at the merge gate because {} is missing",
            inventory::INVENTORY
        ));
    }
    let units = inventory::load(&root).unwrap_or_default();
    let list: Vec<String> = changed.iter().cloned().collect();
    let hits: Vec<&Unit> = units
        .iter()
        .filter(|u| {
            matches!(u.kind.as_str(), "verus" | "shipped")
                || (a.has("--mutants") && u.kind == "mutants")
        })
        .filter(|u| list.iter().any(|p| u.affected_by(p)))
        .collect();
    let limit = if a.has("--all-proofs") {
        usize::MAX
    } else {
        12
    };
    if hits.len() > limit {
        skipped.push(format!("{} of {} affected proofs: over the limit of {limit}, GitHub runs them (or use --all-proofs)", hits.len() - limit, hits.len()));
    }
    if !hits.is_empty() && !a.has("--proofs") {
        skipped.push(format!(
            "{} proofs this change reaches: they run on GitHub (--proofs to run them here)",
            hits.len()
        ));
    } else if !hits.is_empty() {
        match a
            .val("--verus")
            .or_else(|| std::env::var("VERUS").ok())
            .or_else(|| {
                std::env::var("HOME")
                    .ok()
                    .map(|h| format!("{h}/tools/verus/verus-x86-linux/verus"))
            })
            .filter(|v| Path::new(v).exists())
        {
            None => skipped.push(format!(
                "{} proofs: no Verus on this machine, GitHub runs them",
                hits.len()
            )),
            Some(v) => {
                let tc = runner::check_toolchain(&root, &v)?;
                for u in hits.iter().take(limit) {
                    // One verification at a time on buildhost (verify-gate): never wait for the lock here, the loop must stay fast.
                    let cmd = format!(
                        "flock -n -E 75 /tmp/lucina-verify.lock bash -c {}",
                        shell_quote(&u.command())
                    );
                    let unit_name = u.name.replace('/', "-");
                    let kind = if u.kind == "mutants" {
                        "mutants"
                    } else {
                        "script"
                    };
                    if !fast_step(&unit_name, &cmd, kind, 120, None, &b, &tc, &out)?.0 {
                        let json = fs::read_to_string(
                            Path::new(&out).join(format!("fast__{unit_name}.json")),
                        )
                        .unwrap_or_default();
                        if json.contains("\"exit_code\":75,") {
                            skipped.push(format!(
                                "{}: the verify lock is busy, GitHub runs it",
                                u.name
                            ));
                        } else {
                            failed.push(unit_name);
                        }
                    }
                }
            }
        }
    }
    let _ = fs::remove_dir_all(&out);
    for s in &skipped {
        println!("skipped  {s}");
    }
    end.done = true;
    println!(
        "check-fast: {:.0}s, {} failed, {} skipped",
        started.elapsed().as_secs_f64(),
        failed.len(),
        skipped.len()
    );
    Ok(!failed.is_empty() as i32)
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// `perf --baselines F --out D`: each line of F is name, command, max seconds, max peak MiB. Over either limit fails.
fn cmd_perf(a: &Args) -> Result<i32, String> {
    let root = repo_root()?;
    std::env::set_current_dir(&root).map_err(|e| e.to_string())?;
    let out = a.need("--out")?;
    let b = budget_from(a)?;
    let none = runner::check_toolchain(&root, &a.need("--verus")?)?;
    let text = fs::read_to_string(a.need("--baselines")?).map_err(|e| e.to_string())?;
    let mut bad = 0;
    for line in text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
    {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() != 4 {
            return Err(format!("bad baseline line: {line}"));
        }
        let (name, command, max_s, max_mib): (&str, &str, f64, f64) = (
            f[0],
            f[1],
            f[2].parse().map_err(|_| "max seconds")?,
            f[3].parse().map_err(|_| "max MiB")?,
        );
        let unit = Unit {
            name: format!("perf/{name}"),
            group: "perf".into(),
            kind: "script".into(),
            target: name.into(),
            inputs: vec![],
        };
        let mut s = spec_for(&unit, &b, &none, &out, (max_s * 3.0) as u64 + 60);
        s.command = command.to_string();
        runner::run_unit(&s)?;
        let json = fs::read_to_string(Path::new(&out).join(format!("perf__{name}.json")))
            .unwrap_or_default();
        let num = |k: &str| -> f64 {
            json.find(&format!("\"{k}\":"))
                .and_then(|i| {
                    json[i + k.len() + 3..]
                        .split([',', '}'])
                        .next()
                        .and_then(|s| s.parse().ok())
                })
                .unwrap_or(f64::MAX)
        };
        let (secs, mib) = (num("duration_s"), num("peak_bytes") / (1u64 << 20) as f64);
        let ok = runner::report_field(&json, "outcome").as_deref() == Some("verified")
            && secs <= max_s
            && mib <= max_mib;
        println!(
            "{} {name:<24} {secs:>7.1}s / {max_s:.0}s   {mib:>8.0} MiB / {max_mib:.0} MiB",
            if ok { "ok  " } else { "FAIL" }
        );
        bad += !ok as i32;
    }
    Ok((bad > 0) as i32)
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_source_id_hashes_paths_and_bytes_and_a_mismatch_is_refused() {
        let f = |p: &str, b: &str| (p.to_string(), b.as_bytes().to_vec());
        let a = srcid::hash_files(&[f("src/a.rs", "x"), f("Cargo.toml", "t")]);
        // order of the input does not matter, content and path do
        assert_eq!(
            a,
            srcid::hash_files(&[f("Cargo.toml", "t"), f("src/a.rs", "x")])
        );
        assert_ne!(
            a,
            srcid::hash_files(&[f("src/a.rs", "y"), f("Cargo.toml", "t")])
        );
        assert_ne!(
            a,
            srcid::hash_files(&[f("src/b.rs", "x"), f("Cargo.toml", "t")])
        );
        assert_ne!(
            srcid::hash_files(&[f("a", "bc")]),
            srcid::hash_files(&[f("ab", "c")])
        );
        assert_eq!(a.len(), 16);
        assert!(srcid::mismatch(&a, Some(&a)).is_none());
        let m = srcid::mismatch(&a, Some("0000000000000000")).unwrap();
        assert!(m.contains(&a) && m.contains("0000000000000000"), "{m}");
        assert!(srcid::mismatch(&a, None).is_some());
        // the binary under test was built from the sources next to it
        let here = srcid::compute(Path::new(env!("CARGO_MANIFEST_DIR")));
        assert_eq!(here.as_deref(), Some(env!("LUCINA_CI_SRC_ID")));
    }

    #[test]
    fn merge_step_cap_defaults_to_180_seconds() {
        if std::env::var_os("LUCINA_CHECK_FAST_MERGE_CAP_S").is_none() {
            assert_eq!(merge_step_cap(), 180);
        }
    }

    /// The two slow steps have room; every other step is at 180; a required step the table does not name is at its own limit.
    #[test]
    fn merge_limits_are_per_step_with_an_180_default() {
        let no_override = MergeCaps::default();
        let limit = |n: &str| no_override.limit_of(n, MERGE_STEP_LIMIT_DEFAULT);
        assert_eq!(limit("unit-tests-tools-ci-lucina-ci"), Some(300));
        assert_eq!(limit("mutants-text"), Some(240));
        assert_eq!(limit("clippy-tools-ci-lucina-ci"), Some(180));
        assert_eq!(limit("diffgate"), Some(180));
        assert_eq!(limit("unit-tests-tools-bq"), Some(180));
        assert_eq!(limit("roles"), None);
        assert_eq!(limit("trust"), None);
        // a limit only ever cuts: the table's 300 does not lift a step whose own timeout is 60
        for (name, own, enforced) in [
            ("unit-tests-tools-ci-lucina-ci", 300_u64, 300_u64),
            ("clippy-tools-ci-lucina-ci", 300, 180),
            ("diffgate", 60, 60),
            ("mutants-text", 360, 240),
            ("roles", 300, 300),
        ] {
            let at_gate = match limit(name) {
                Some(l) => own.min(l),
                None => own,
            };
            assert_eq!(at_gate, enforced, "{name} at the merge gate");
        }
    }

    /// The override replaces every table limit with one number, and a `step=SECONDS` entry gives one step its own.
    #[test]
    fn the_override_replaces_all_the_step_limits() {
        let c = MergeCaps {
            plain: Some(7),
            per_step: vec![("unit-tests-tools-ci-lucina-ci".to_string(), 40)],
        };
        let l = |n: &str| c.limit_of(n, MERGE_STEP_LIMIT_DEFAULT);
        assert_eq!(l("unit-tests-tools-ci-lucina-ci"), Some(40));
        assert_eq!(l("mutants-text"), Some(7), "7 beats the table's 240");
        assert_eq!(l("secrets"), Some(7));
        assert_eq!(
            l("roles"),
            Some(7),
            "the override is every step, required or not"
        );
        let nine = MergeCaps::parse("9");
        assert_eq!(nine.limit_of("unit-tests-tools-ci-lucina-ci", 180), Some(9));
        assert_eq!(nine.limit_of("roles", 180), Some(9));
    }

    /// The forms `LUCINA_CHECK_FAST_MERGE_CAP_S` is read in: a plain number, a per-step list, both together, junk ignored.
    #[test]
    fn the_override_is_read_as_a_number_a_list_or_both() {
        for (text, plain, steps) in [
            ("7", Some(7), vec![]),
            ("a=3", None, vec![("a", 3)]),
            ("5,a=3,b=4", Some(5), vec![("a", 3), ("b", 4)]),
            (" a = 3 ,, junk= , x", None, vec![("a", 3)]),
            ("nonsense", None, vec![]),
            ("", None, vec![]),
        ] {
            let c = MergeCaps::parse(text);
            assert_eq!(c.plain, plain, "{text}");
            let got: Vec<(&str, u64)> = c.per_step.iter().map(|(n, s)| (n.as_str(), *s)).collect();
            assert_eq!(got, steps, "{text}");
        }
    }

    // The fetch itself shells out to gh, which a unit test cannot stub; the LINE it prints when it fails is what this pins, and
    // cmd_sync_gate prints only this fn's return value (cmd_sync_gate in this file, `unwrap_or_else`).
    #[test]
    fn the_runs_unknown_line_reads_exactly_what_the_gate_says() {
        let e = "GitHub did not answer for the workflow runs of abc1234";
        assert_eq!(
            runs_unknown_line(e),
            "sync-gate: runs API gave no answer; decided by the grace period alone (GitHub did not answer for the workflow runs of abc1234)"
        );
        assert_eq!(
            runs_unknown_line("cannot run gh: no such file"),
            "sync-gate: runs API gave no answer; decided by the grace period alone (cannot run gh: no such file)"
        );
    }

    #[test]
    fn check_fast_refuses_a_binary_without_a_required_step() {
        assert!(missing_required_steps(FAST_STEPS).is_empty());
        for gone in FAST_STEPS_REQUIRED {
            let reg: Vec<&str> = FAST_STEPS.iter().copied().filter(|s| s != gone).collect();
            assert_eq!(missing_required_steps(&reg), vec![*gone]);
        }
        assert_eq!(
            missing_required_steps(&["diffgate"]),
            vec!["inventory", "spec", "roles", "trust", "mutants-text"]
        );
    }

    use super::*;

    fn unit(name: &str, group: &str, inputs: &[&str]) -> Unit {
        Unit {
            name: name.into(),
            group: group.into(),
            kind: "verus".into(),
            target: "t".into(),
            inputs: inputs.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn selection() {
        let us = [
            unit("a", "x", &["crates/a/"]),
            unit("b", "y", &["crates/b/"]),
        ];
        assert_eq!(select(&us, None, None).len(), 2, "no diff means everything");
        let changed = vec!["crates/a/src/lib.rs".to_string()];
        assert_eq!(select(&us, Some(&changed), None)[0].name, "a");
        assert_eq!(select(&us, Some(&changed), None).len(), 1);
        assert_eq!(
            select(&us, Some(&changed), Some("y")).len(),
            2,
            "a proof/<group> branch always runs its group"
        );
        assert_eq!(select(&us, Some(&[]), None).len(), 0);
    }

    #[test]
    fn a_member_reaching_a_sibling_repo_is_dropped_transitively() {
        let m: BTreeMap<String, String> = [
            (
                "crates/a",
                "[dependencies]\nx = { path = \"/home/user/Projects/X/y\" }\n",
            ),
            ("crates/b", "[dependencies]\na = { path = \"../a\" }\n"),
            ("crates/c", "[dependencies]\nb = { path = \"../b\" }\n"),
            (
                "crates/d",
                "[dependencies]\n# x = { path = \"/home/user/x\" }\ne = { path = \"../e\" }\n",
            ),
            ("crates/e", "[dependencies]\n"),
            (
                "crates/f",
                "[dependencies]\ng = { git = \"file:///home/user/g\" }\n",
            ),
        ]
        .into_iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect();
        let (keep, dropped) = portable_members(&m, &|_| true);
        assert_eq!(keep, vec!["crates/d".to_string(), "crates/e".to_string()]);
        assert_eq!(dropped.len(), 4);
    }

    #[test]
    fn a_member_with_a_removed_path_dep_is_dropped() {
        let m: BTreeMap<String, String> = [
            (
                "crates/a".to_string(),
                "x = { path = \"../gone\" }\n".to_string(),
            ),
            ("crates/b".to_string(), String::new()),
        ]
        .into_iter()
        .collect();
        let (keep, dropped) = portable_members(&m, &|p| p != "crates/gone");
        assert_eq!(keep, vec!["crates/b".to_string()]);
        assert_eq!(dropped, vec!["crates/a".to_string()]);
    }

    #[test]
    fn a_proof_branch_names_one_unit() {
        let us = [
            unit("verus/a/rules", "x", &[]),
            unit("verus/b/rules", "x", &[]),
        ];
        let one = proof_unit(&us, "proof/verus/a/rules").unwrap();
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].name, "verus/a/rules");
        assert!(
            proof_unit(&us, "proof/x").is_none(),
            "a group is not a unit"
        );
        assert!(proof_unit(&us, "main").is_none());
    }

    #[test]
    fn check_runs_parse() {
        let r = parse_runs("verus-gate\tcompleted\tsuccess\tgithub-actions\nbad line\n");
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].conclusion, "success");
    }

    // ---- the merge waivers of a gate job (finding GATE-01). The file is the one `lagent merge` writes, byte for byte:
    // {"rule":"...","reason":"...","by":"...","job":"job_..."} per line. LAGENT_WAIVERS_FILE is process-wide, so what a unit
    // test can judge is the reading of one line and the command it makes; tests/merge_gate_waivers.rs runs the real step.

    /// One waivers line as the merge job writes it, with `by` and `job` from job_4ee819df2427 of the failing merge. The
    /// reason is JSON-escaped the way its writer escapes it, quotes included.
    fn waiver_line(rule: &str, reason: &str) -> String {
        let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
        format!(
            "{{\"rule\":\"{}\",\"reason\":\"{}\",\"by\":\"nez@nez_opt\",\"job\":\"job_4ee819df2427\"}}",
            esc(rule),
            esc(reason)
        )
    }

    /// The real allow of job_4ee819df2427 (line 9 of its log), which its diffgate step then refused with "0 waived" (line 160).
    const GAL_REASON: &str = "real-server variants kept as #[ignore = needs LUCINA_MEDIA_BIN], reported ignored not \
                              passed; the same assertions run against the in-process Fake; the ignored variants run in a \
                              scheduled non-gating job; WM-C004 / GAL-01; coordinator allow 2026-11-17";

    fn rules(skipped: bool) -> Vec<String> {
        if skipped {
            vec!["skipped-test".to_string()]
        } else {
            vec![]
        }
    }

    /// The repository this test file sits in: the rules list is data, so the tests read the real one.
    fn this_repo() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .to_path_buf()
    }

    #[test]
    fn a_waivers_line_of_the_four_string_fields_is_read() {
        let w = parse_waiver_line(&waiver_line("skipped-test", GAL_REASON)).unwrap();
        assert_eq!(w.rule, "skipped-test");
        assert_eq!(w.reason, GAL_REASON, "the reason is kept as it was given");
    }

    #[test]
    fn a_line_that_is_not_a_json_object_is_refused() {
        for (bad, what) in [
            ("", "an empty line"),
            ("not json", "plain text"),
            ("[\"rule\"]", "an array"),
            ("{\"rule\":\"a\" ", "an object with no closing brace"),
            ("{}{\"rule\":\"a\"}", "text before the object"),
        ] {
            assert!(
                parse_waiver_line(bad).is_err(),
                "{what}: {bad:?} was taken for a waiver"
            );
        }
    }

    #[test]
    fn a_line_missing_one_of_the_four_fields_is_refused() {
        for missing in WAIVER_FIELDS {
            let line = match missing {
                "rule" => "{\"reason\":\"why\",\"by\":\"nez@nez_opt\",\"job\":\"job_1\"}",
                "reason" => "{\"rule\":\"skipped-test\",\"by\":\"nez@nez_opt\",\"job\":\"job_1\"}",
                "by" => "{\"rule\":\"skipped-test\",\"reason\":\"why\",\"job\":\"job_1\"}",
                _ => "{\"rule\":\"skipped-test\",\"reason\":\"why\",\"by\":\"nez@nez_opt\"}",
            };
            let e = parse_waiver_line(line)
                .err()
                .unwrap_or_else(|| panic!("{missing} was not asked for"));
            assert!(
                e.contains(missing),
                "{e} names the field that is gone: {missing}"
            );
        }
    }

    #[test]
    fn a_field_that_is_not_a_string_is_refused() {
        for bad in [
            "{\"rule\":\"skipped-test\",\"reason\":\"why\",\"by\":null,\"job\":\"job_1\"}",
            "{\"rule\":\"skipped-test\",\"reason\":7,\"by\":\"nez\",\"job\":\"job_1\"}",
            "{\"rule\":[\"skipped-test\"],\"reason\":\"why\",\"by\":\"nez\",\"job\":\"job_1\"}",
            "{\"rule\":\"skipped-test\",\"reason\":\"why\",\"by\":{\"n\":\"nez\"},\"job\":\"job_1\"}",
            "{\"rule\":\"skipped-test\",\"reason\":\"why\",\"by\":true,\"job\":\"job_1\"}",
        ] {
            assert!(
                parse_waiver_line(bad).is_err(),
                "a non-string value was taken: {bad}"
            );
        }
    }

    #[test]
    fn an_extra_or_a_repeated_field_is_refused() {
        for bad in [
            "{\"rule\":\"skipped-test\",\"reason\":\"why\",\"by\":\"nez\",\"job\":\"job_1\",\"gate\":\"g\"}",
            "{\"rule\":\"skipped-test\",\"rule\":\"other\",\"reason\":\"why\",\"by\":\"nez\",\"job\":\"job_1\"}",
        ] {
            assert!(
                parse_waiver_line(bad).is_err(),
                "a fifth field was taken: {bad}"
            );
        }
    }

    #[test]
    fn an_empty_or_blank_reason_is_refused() {
        for reason in ["", " ", "     "] {
            let e = parse_waiver_line(&waiver_line("skipped-test", reason))
                .err()
                .unwrap_or_else(|| panic!("a reason of {reason:?} says nothing and was taken"));
            assert!(e.contains("reason"), "{e}");
        }
    }

    /// The counted form of diffgate (`RULE=N:reason`) arrives byte for byte, quotes, brackets and semicolons included.
    #[test]
    fn a_counted_waiver_is_forwarded_with_its_reason_unchanged() {
        let reason = "7:seven tests are #[ignore = \"slow\"]; see a/b";
        let w = parse_waiver_line(&waiver_line("skipped-test", reason)).unwrap();
        assert_eq!(
            w.reason, reason,
            "the escapes of the file are the reason's own characters"
        );
        let (cmd, summary) = diffgate_with_waivers("abc123", &[w], &rules(true));
        assert_eq!(
            cmd,
            "diffgate --repo . --base abc123 --allow 'skipped-test=7:seven tests are #[ignore = \"slow\"]; see a/b'"
        );
        assert_eq!(
            summary,
            format!("diffgate waivers: 1 forwarded (skipped-test: {reason}), 0 not forwarded")
        );
    }

    #[test]
    fn an_uncounted_waiver_is_forwarded_with_its_reason_unchanged() {
        let w = parse_waiver_line(&waiver_line("skipped-test", GAL_REASON)).unwrap();
        let (cmd, summary) = diffgate_with_waivers("2ef62d3b", &[w], &rules(true));
        assert!(
            cmd.starts_with("diffgate --repo . --base 2ef62d3b --allow 'skipped-test=")
                && cmd.ends_with("'"),
            "{cmd}"
        );
        assert!(
            cmd.contains(GAL_REASON),
            "the reason is passed through unchanged: {cmd}"
        );
        assert_eq!(
            summary,
            format!("diffgate waivers: 1 forwarded (skipped-test: {GAL_REASON}), 0 not forwarded")
        );
    }

    /// `gate-change` and `spec-change` are the merge tool's decisions, not rules diffgate knows: forwarding one would fail
    /// the call with "--allow: unknown rule".
    #[test]
    fn a_rule_that_is_not_on_the_list_is_not_forwarded() {
        let ws = [
            parse_waiver_line(&waiver_line(
                "gate-change",
                "tools/ci/ of the merge; coordinator allow",
            ))
            .unwrap(),
            parse_waiver_line(&waiver_line(
                "spec-change",
                "a statement moved; coordinator allow",
            ))
            .unwrap(),
        ];
        let (cmd, summary) =
            diffgate_with_waivers("abc123", &ws, &forwarded_waiver_rules(&this_repo()));
        assert_eq!(
            cmd, "diffgate --repo . --base abc123",
            "the plain command, as today"
        );
        assert_eq!(
            summary,
            "diffgate waivers: 0 forwarded, 2 not forwarded (gate-change, spec-change)"
        );
    }

    /// The list is data: whatever it names is forwarded, so a rule added later is covered by this test and needs no code.
    #[test]
    fn every_rule_of_the_forwarding_list_is_forwarded() {
        let list = forwarded_waiver_rules(&this_repo());
        assert!(!list.is_empty(), "{WAIVER_RULES_FILE} reads as empty");
        for r in &list {
            let reason = "5:five of them; the coordinator allow of today";
            let w = parse_waiver_line(&waiver_line(r, reason)).unwrap();
            let (cmd, summary) = diffgate_with_waivers("abc123", &[w], &list);
            assert_eq!(
                cmd,
                format!("diffgate --repo . --base abc123 --allow '{r}={reason}'"),
                "{r} is on the list but not forwarded as itself"
            );
            assert_eq!(
                summary,
                format!("diffgate waivers: 1 forwarded ({r}: {reason}), 0 not forwarded")
            );
        }
    }

    #[test]
    fn the_forwarding_list_holds_skipped_test_and_nothing_else_today() {
        assert_eq!(
            forwarded_waiver_rules(&this_repo()),
            vec!["skipped-test".to_string()]
        );
    }

    #[test]
    fn a_list_that_is_not_there_forwards_nothing() {
        let w = parse_waiver_line(&waiver_line("skipped-test", "the reason")).unwrap();
        let (cmd, summary) = diffgate_with_waivers(
            "abc123",
            &[w],
            &forwarded_waiver_rules(Path::new("/nonexistent-root")),
        );
        assert_eq!(cmd, "diffgate --repo . --base abc123");
        assert_eq!(
            summary,
            "diffgate waivers: 0 forwarded, 1 not forwarded (skipped-test)"
        );
    }

    /// The whole line of the log, forwarded and refused in the same call: one count for each side.
    #[test]
    fn the_summary_line_counts_both_sides_of_the_list() {
        let ws = [
            parse_waiver_line(&waiver_line("skipped-test", GAL_REASON)).unwrap(),
            parse_waiver_line(&waiver_line("gate-change", "tools/ci/; coordinator allow")).unwrap(),
        ];
        let (cmd, summary) = diffgate_with_waivers("38d67292", &ws, &rules(true));
        assert_eq!(cmd.matches(" --allow ").count(), 1, "{cmd}");
        assert_eq!(
            summary,
            format!(
                "diffgate waivers: 1 forwarded (skipped-test: {GAL_REASON}), 1 not forwarded (gate-change)"
            )
        );
    }
}
