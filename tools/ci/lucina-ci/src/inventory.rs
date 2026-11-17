//! Every proof unit of the repository, found on disk. `tools/ci/proof-inventory.tsv` is this list, committed: a proof that is
//! added, removed or moved changes the list, and `inventory --check` refuses a list that is not current, so nothing can sit
//! outside CI by being forgotten. A unit is one Verus crate or one script: no unit imports another's verified output (no
//! `--import`/`--export` anywhere in the repo), which is what makes running them on separate machines sound.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::process::Command;

pub const INVENTORY: &str = "tools/ci/proof-inventory.tsv";
pub const GROUPS: &str = "tools/ci/proof-groups.tsv";

/// A change here can change every verdict.
pub const GLOBAL_INPUTS: &[&str] = &[
    "tools/ci/",
    ".github/workflows/verus.yml",
    "references/upstream.lock",
    "scripts/verify.sh",
    "scripts/verify-shipped.sh",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unit {
    pub name: String,
    pub group: String,
    /// verus, shipped, mutants, mutations, models, tla, full.
    pub kind: String,
    pub target: String,
    pub inputs: Vec<String>,
}

impl Unit {
    pub fn phase(&self) -> &'static str {
        match self.kind.as_str() {
            "verus" | "shipped" => "proofs",
            "mutants" | "mutations" => "mutants",
            "models" => "models",
            "deep" => "deep",
            "full" => "full",
            _ => "tla",
        }
    }

    /// The shell command, run from the repo root with VERUS set.
    pub fn command(&self) -> String {
        match self.kind.as_str() {
            "verus" => {
                let extra = crate::runner::verus_args(&self.name);
                if extra.is_empty() {
                    format!("\"$VERUS\" {}", self.target)
                } else {
                    format!("\"$VERUS\" {extra} {}", self.target)
                }
            }
            "shipped" => format!(
                "VERIFY_SHIPPED_ONLY={} bash scripts/verify-shipped.sh",
                self.target
            ),
            "mutants" | "mutations" | "models" => format!("bash {}", self.target),
            "full" => format!("VERIFY_MODELS=0 bash {}", self.target),
            _ => self.target.clone(),
        }
    }

    /// Does a change to `path` need this unit run again?
    pub fn affected_by(&self, path: &str) -> bool {
        GLOBAL_INPUTS
            .iter()
            .chain(
                self.inputs
                    .iter()
                    .map(String::as_ref)
                    .collect::<Vec<&str>>()
                    .iter(),
            )
            .any(|p| is_under(path, p))
    }
}

fn is_under(path: &str, prefix: &str) -> bool {
    if prefix.ends_with('/') {
        path.starts_with(prefix)
    } else {
        path == prefix
    }
}

fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    let o = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .map_err(|e| e.to_string())?;
    if !o.status.success() {
        return Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&o.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&o.stdout).into_owned())
}

/// The header line by which a mutants script declares the proofs it holds: `# mutants-units: disk_rules patch_rules`.
/// Each name is what the script takes as its first argument, and each is a unit of its own.
const MUTANTS_MARKER: &str = "mutants-units:";
/// How far down the script the marker may sit: it belongs with the header comments that say what the script proves.
const MUTANTS_MARKER_TOP: usize = 40;

/// The names written on the `# mutants-units:` line, however the `#` and the colon are spaced; None for any other line.
fn marker_line(line: &str) -> Option<&str> {
    let after = line.trim_start().strip_prefix('#')?.trim_start();
    after.strip_prefix(MUTANTS_MARKER).map(str::trim)
}

/// The names a mutants.sh declares, or None when it declares none. Without the marker the script is one unit and is run with
/// no argument, which for a script that picks its proof from `$1` means only the default proof's mutants ever run
/// (found in the Brimstone copy of this CI, 2026-11-17). A marker that names nothing, repeats a name or uses a name CI
/// cannot put in a unit name is refused here, so a half-written marker is never quietly read as no marker.
fn mutant_unit_names(text: &str) -> Result<Option<Vec<String>>, String> {
    let hits: Vec<usize> = text
        .lines()
        .enumerate()
        .filter(|(_, l)| marker_line(l).is_some())
        .map(|(i, _)| i + 1)
        .collect();
    let line = match hits.as_slice() {
        [] => return Ok(None),
        [n] => *n,
        many => {
            return Err(format!(
                "lines {} each carry the `{MUTANTS_MARKER}` marker: one marker line",
                many.iter()
                    .map(usize::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    };
    if line > MUTANTS_MARKER_TOP {
        return Err(format!(
            "the `{MUTANTS_MARKER}` line is line {line}: it must be in the first {MUTANTS_MARKER_TOP} lines, where the script says what it proves"
        ));
    }
    let names: Vec<String> = marker_line(text.lines().nth(line - 1).unwrap_or(""))
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_string)
        .collect();
    if names.is_empty() {
        return Err(format!(
            "line {line} carries the marker with no name after it: name the proofs or drop the line"
        ));
    }
    for n in &names {
        if !n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(format!(
                "line {line} names `{n}`: a unit name is letters, digits and underscore"
            ));
        }
    }
    let mut sorted = names.clone();
    sorted.sort();
    if let Some(d) = sorted.windows(2).find(|w| w[0] == w[1]) {
        return Err(format!("line {line} names `{}` twice", d[0]));
    }
    Ok(Some(names))
}

/// Whether scripts/verify-mutations.sh carries a global mutation list: an entry between `mutations=(` and its `)`, on the
/// opening line or on lines of its own. The same walk `spec.rs` and the precheck use to read the entries.
fn has_mutation_entries(text: &str) -> bool {
    let mut inside = false;
    for l in text.lines() {
        let t = l.trim();
        if !inside {
            if let Some(rest) = t.strip_prefix("mutations=(") {
                let rest = rest.trim();
                if let Some(inner) = rest.strip_suffix(')') {
                    // The whole list on the opening line: `mutations=()` is empty, `mutations=('a|s/x/y/|w')` is not.
                    return !inner.trim().is_empty();
                }
                inside = true;
            }
            continue;
        }
        if t.starts_with(')') {
            break;
        }
        if !t.is_empty() && !t.starts_with('#') {
            return true;
        }
    }
    false
}

/// `crates/foo/src/x.rs` -> `crates/foo/`: a proof reads a crate through `mod` lines that this scan does not follow, so the
/// whole crate directory is its input. Anything else stays the exact file.
fn owner_dir(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').collect();
    let n = match parts.first() {
        Some(&"crates") | Some(&"office") | Some(&"artcraft") | Some(&"fortuna")
        | Some(&"kernel") | Some(&"drivers") | Some(&"services") | Some(&"apps")
        | Some(&"boards") | Some(&"specs") => 2,
        Some(&"userspace") if parts.get(1) == Some(&"crates") => 3,
        _ => 0,
    };
    if n > 0 && parts.len() > n {
        format!("{}/", parts[..n].join("/"))
    } else {
        path.to_string()
    }
}

/// `a/b/../c` -> `a/c`; None if it climbs out of the repo.
fn normalize(p: &str) -> Option<String> {
    let mut out: Vec<&str> = Vec::new();
    for c in p.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                out.pop()?;
            }
            c => out.push(c),
        }
    }
    Some(out.join("/"))
}

/// Paths named by `#[path = "..."]` and `include!("...")` in Rust text, as written.
fn path_refs(text: &str) -> Vec<String> {
    let mut v = Vec::new();
    for line in text.lines() {
        let l = line.trim_start();
        let l = l.strip_prefix("//@").map(str::trim_start).unwrap_or(l);
        if let Some(i) = l.find("#[path") {
            if let Some(s) = quoted(&l[i..]) {
                v.push(s);
            }
        } else if let Some(i) = l
            .find("include!(")
            .or_else(|| l.find("include_str!("))
            .or_else(|| l.find("include_bytes!("))
            && let Some(s) = quoted(&l[i..])
        {
            v.push(s);
        }
    }
    v
}

fn quoted(s: &str) -> Option<String> {
    let a = s.find('"')?;
    let b = s[a + 1..].find('"')?;
    Some(s[a + 1..a + 1 + b].to_string())
}

/// Tokens like `crates/foo` in a script (the mutants plant their bugs in copies of those crates).
fn crate_tokens(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for prefix in [
        "crates/",
        "office/",
        "artcraft/",
        "kernel/",
        "drivers/",
        "services/",
        "apps/",
        "boards/",
        "specs/",
    ] {
        let mut rest = text;
        while let Some(i) = rest.find(prefix) {
            let after = &rest[i + prefix.len()..];
            let name: String = after
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
                .collect();
            // A token glued to a longer word (`some_crates/x`) is not a repo path.
            let glued = rest[..i]
                .chars()
                .last()
                .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
            if !name.is_empty() && !glued {
                out.insert(format!("{prefix}{name}/"));
            }
            rest = &after[name.len()..];
        }
    }
    out
}

fn rs_files(root: &Path, dir: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.trim_end_matches('/').to_string()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = fs::read_dir(root.join(&d)) else {
            continue;
        };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let p = format!("{d}/{name}");
            let Ok(t) = e.file_type() else { continue };
            if t.is_dir() && name != "target" {
                stack.push(p);
            } else if name.ends_with(".rs") {
                out.push(p);
            }
        }
    }
    out
}

/// The files and crate directories a proof file reads, transitively.
fn closure(root: &Path, start: &[String]) -> BTreeSet<String> {
    let mut inputs: BTreeSet<String> = BTreeSet::new();
    let mut queue: Vec<String> = start.to_vec();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    while let Some(f) = queue.pop() {
        if !seen.insert(f.clone()) {
            continue;
        }
        let owner = owner_dir(&f);
        if owner.ends_with('/') && inputs.insert(owner.clone()) {
            queue.extend(rs_files(root, &owner));
        } else if !owner.ends_with('/') {
            inputs.insert(f.clone());
        }
        let Ok(text) = fs::read_to_string(root.join(&f)) else {
            continue;
        };
        let parent = Path::new(&f)
            .parent()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        for r in path_refs(&text) {
            if let Some(n) = normalize(&format!("{parent}/{r}")) {
                queue.push(n);
            }
        }
    }
    inputs
}

/// `key -> group` from the hand-kept groups file: `group<TAB>key key key`.
pub fn read_groups(root: &Path) -> Result<BTreeMap<String, String>, String> {
    let text = fs::read_to_string(root.join(GROUPS)).map_err(|e| format!("{GROUPS}: {e}"))?;
    let mut map = BTreeMap::new();
    for line in text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
    {
        let (g, keys) = line
            .split_once('\t')
            .ok_or_else(|| format!("{GROUPS}: no tab in: {line}"))?;
        for k in keys.split_whitespace() {
            if map.insert(k.to_string(), g.to_string()).is_some() {
                return Err(format!("{GROUPS}: {k} is listed twice"));
            }
        }
    }
    Ok(map)
}

/// Finds every unit on disk. A shipped file with `//@` proof lines that verify-shipped.sh does not list is an error, as is a
/// unit with no group: both would be proofs CI does not run.
pub fn discover(root: &Path) -> Result<Vec<Unit>, String> {
    let groups = read_groups(root)?;
    let group_of = |key: &str| -> Result<String, String> {
        groups.get(key).cloned().ok_or_else(|| {
            format!(
                "{key}: no group in {GROUPS}. Add it there, then run: lucina-ci inventory --write"
            )
        })
    };
    let mut units = Vec::new();

    let files = git(root, &["ls-files", "--", "verification"])?;
    for f in files.lines() {
        let parts: Vec<&str> = f.split('/').collect();
        if parts.len() != 3 {
            return Err(format!(
                "{f}: proofs live in verification/<dir>/<file>; inventory cannot place this one"
            ));
        }
        let dir = parts[1];
        if f.ends_with(".rs") {
            let stem = parts[2].trim_end_matches(".rs");
            let mut inputs = closure(root, &[f.to_string()]);
            inputs.insert(format!("verification/{dir}/"));
            units.push(Unit {
                name: format!("verus/{dir}/{stem}"),
                group: group_of(dir)?,
                kind: "verus".into(),
                target: f.to_string(),
                inputs: inputs.into_iter().collect(),
            });
        } else if parts[2] == "mutants.sh" {
            let text = fs::read_to_string(root.join(f)).map_err(|e| format!("{f}: {e}"))?;
            let names = mutant_unit_names(&text).map_err(|e| format!("{f}: {e}"))?;
            let mut start: Vec<String> = Vec::new();
            for c in crate_tokens(&text) {
                start.extend(rs_files(root, &c));
            }
            start.extend(
                git(
                    root,
                    &["ls-files", "--", &format!("verification/{dir}/*.rs")],
                )?
                .lines()
                .map(str::to_string),
            );
            let mut inputs = closure(root, &start);
            inputs.insert(format!("verification/{dir}/"));
            let inputs: Vec<String> = inputs.into_iter().collect();
            // One unit per declared name, each run with that name as its argument; the marker replaces the plain
            // `mutants/<dir>` unit rather than adding to it, so no proof is counted twice.
            for (name, target) in match names {
                None => vec![(format!("mutants/{dir}"), f.to_string())],
                Some(ns) => ns
                    .into_iter()
                    .map(|n| (format!("mutants/{dir}/{n}"), format!("{f} {n}")))
                    .collect(),
            } {
                units.push(Unit {
                    name,
                    group: group_of(dir)?,
                    kind: "mutants".into(),
                    target,
                    inputs: inputs.clone(),
                });
            }
        }
    }

    let script =
        fs::read_to_string(root.join("scripts/verify-shipped.sh")).map_err(|e| e.to_string())?;
    let listed = shipped_list(&script).ok_or("scripts/verify-shipped.sh: no files=(...) line")?;
    let with_lines: BTreeSet<String> = git(root, &["grep", "-lE", "^ *//@", "--", "*.rs"])
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect();
    for f in &with_lines {
        if !f.starts_with("verification/") && !f.starts_with("tools/ci/") && !listed.contains(f) {
            return Err(format!(
                "{f} has //@ proof lines but scripts/verify-shipped.sh does not list it"
            ));
        }
    }
    for f in &listed {
        let parts: Vec<&str> = f.split('/').collect();
        let (krate, stem) = (
            parts[parts.len() - 3],
            parts[parts.len() - 1].trim_end_matches(".rs"),
        );
        units.push(Unit {
            name: format!("shipped/{krate}/{stem}"),
            group: group_of(krate)?,
            kind: "shipped".into(),
            target: f.clone(),
            inputs: closure(root, std::slice::from_ref(f)).into_iter().collect(),
        });
    }

    // The global mutation list is a unit only where the repository keeps one with entries in it. An absent script or an empty
    // list proves nothing, so emitting the unit anyway gives a check that passes trivially (found in the Brimstone copy of
    // this CI, 2026-11-17: an empty `mutations=()` stood in for no list at all).
    if let Ok(text) = fs::read_to_string(root.join("scripts/verify-mutations.sh"))
        && has_mutation_entries(&text)
    {
        let mut inputs: BTreeSet<String> = [
            "scripts/verify-mutations.sh".to_string(),
            "verification/".to_string(),
        ]
        .into();
        inputs.extend(crate_tokens(&text));
        units.push(Unit {
            name: "mutations/global".into(),
            group: group_of("global-mutations")?,
            kind: "mutations".into(),
            target: "scripts/verify-mutations.sh".into(),
            inputs: inputs.into_iter().collect(),
        });
    }

    // The models call the shipped crates through Cargo: any crate or the lock file can change them.
    units.push(Unit {
        name: "models/stateright".into(),
        group: group_of("models")?,
        kind: "models".into(),
        target: "scripts/verify-models.sh".into(),
        inputs: vec![
            "Cargo.lock".into(),
            "Cargo.toml".into(),
            "crates/".into(),
            "scripts/verify-models.sh".into(),
        ],
    });

    // The deep tier: every heavy TLA+ configuration (`*.heavy.cfg`, too big for a laptop), once with tla-mc and once with TLC, the
    // cross-check, each on its own runner. Only on demand, nightly and on main: docs/ci/VERUS.md.
    let heavy = git(
        root,
        &["ls-files", "--", "specs/juno/*/*.heavy.cfg"],
    )?;
    let dirs: BTreeSet<String> = heavy.lines().filter_map(model_dir).collect();
    for d in dirs {
        let inputs = vec![
            format!("specs/juno/{d}/"),
            "specs/juno/run.sh".to_string(),
        ];
        let base = format!("ONLY={d} VERIFY_HEAVY=1 TLA_MAX_STATES=2000000000");
        units.push(Unit {
            name: format!("deep/tla-mc/{d}"),
            group: group_of("deep")?,
            kind: "deep".into(),
            target: format!("{base} bash specs/juno/run.sh"),
            inputs: inputs.clone(),
        });
        units.push(Unit { name: format!("deep/tlc/{d}"), group: group_of("deep")?, kind: "deep".into(), target: format!("{base} CHECKER=tlc TLC_HEAP=10g JAVA=java TLA_JAR=\"$HOME/tla2tools.jar\" bash specs/juno/run.sh"), inputs });
    }
    // The whole of scripts/verify.sh on one machine: the end-to-end check that no proof slipped between the units.
    units.push(Unit {
        name: "full/verify-sh".into(),
        group: group_of("full")?,
        kind: "full".into(),
        target: "scripts/verify.sh".into(),
        inputs: vec!["verification/".into()],
    });
    units.push(Unit {
        name: "tla/juno".into(),
        group: group_of("tla")?,
        kind: "tla".into(),
        target: "specs/juno/run.sh".into(),
        inputs: vec!["specs/juno/".into()],
    });
    units.push(Unit {
        name: "tla/minerva".into(),
        group: group_of("tla")?,
        kind: "tla".into(),
        target: "userspace/formal/run-lucina-checks".into(),
        inputs: vec!["userspace/formal/".into()],
    });

    units.retain(|u| unit_present(&u.kind, root.join(&u.target).exists()));
    units.sort_by(|a, b| a.name.cmp(&b.name));
    if let Some(w) = units.windows(2).find(|w| w[0].name == w[1].name) {
        return Err(format!("two proof units are both named {}", w[0].name));
    }
    Ok(units)
}

/// Whether a discovered unit belongs in the inventory of a repository. A `tla`, `models` or `full` unit names a file
/// this repository may simply not carry; emitting it anyway gives a unit with no files, which `spec --check` and
/// `roles --check` refuse to classify. The other kinds are never judged on their target: a `deep` target is a command
/// line, not a path.
fn unit_present(kind: &str, target_exists: bool) -> bool {
    !matches!(kind, "tla" | "models" | "full") || target_exists
}

/// The paths in the `files=(a b c)` line.
fn shipped_list(script: &str) -> Option<Vec<String>> {
    let line = script.lines().find(|l| l.starts_with("files=("))?;
    let inner = line.strip_prefix("files=(")?.strip_suffix(')')?;
    Some(inner.split_whitespace().map(str::to_string).collect())
}

pub fn to_tsv(units: &[Unit]) -> String {
    let mut s = String::from(
        "# Generated: lucina-ci inventory --write. Do not edit. unit<TAB>group<TAB>kind<TAB>target<TAB>inputs\n",
    );
    for u in units {
        s.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\n",
            u.name,
            u.group,
            u.kind,
            u.target,
            u.inputs.join(",")
        ));
    }
    s
}

pub fn from_tsv(text: &str) -> Result<Vec<Unit>, String> {
    let mut v = Vec::new();
    for line in text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
    {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() != 5 {
            return Err(format!("{INVENTORY}: bad line: {line}"));
        }
        v.push(Unit {
            name: f[0].into(),
            group: f[1].into(),
            kind: f[2].into(),
            target: f[3].into(),
            inputs: f[4].split(',').map(str::to_string).collect(),
        });
    }
    Ok(v)
}

pub fn load(root: &Path) -> Result<Vec<Unit>, String> {
    from_tsv(&fs::read_to_string(root.join(INVENTORY)).map_err(|e| format!("{INVENTORY}: {e}"))?)
}

/// `docs/ci/PROOF-MATRIX.md`, generated from the same list.
pub fn to_markdown(units: &[Unit]) -> String {
    let mut groups: BTreeMap<&str, Vec<&Unit>> = BTreeMap::new();
    for u in units {
        groups.entry(&u.group).or_default().push(u);
    }
    let count = |k: &str| units.iter().filter(|u| u.kind == k).count();
    let mut s = String::from(
        "# Proof matrix\n\nGenerated by `lucina-ci inventory --write` from what is on disk. Do not edit.\n\n",
    );
    s += &format!(
        "{} units: {} Verus proof files, {} shipped files with `//@` proofs, {} mutant scripts, {} global mutation list, {} Stateright model set, {} TLA+ runners.\n\n",
        units.len(),
        count("verus"),
        count("shipped"),
        count("mutants"),
        count("mutations"),
        count("models"),
        count("tla")
    );
    s += "| Group | Unit | Kind | Target |\n| --- | --- | --- | --- |\n";
    for (g, us) in groups {
        for u in us {
            s += &format!("| {g} | `{}` | {} | `{}` |\n", u.name, u.kind, u.target);
        }
    }
    s
}

/// The model directory of a heavy configuration: the folder that holds the `.cfg`, whatever the layout above it
/// (`specs/juno/<dir>/X.heavy.cfg` here, `specs/juno/<dir>/X.heavy.cfg` in the public copy).
fn model_dir(path: &str) -> Option<String> {
    std::path::Path::new(path)
        .parent()?
        .file_name()?
        .to_str()
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_heavy_config_names_its_model_directory_in_either_layout() {
        assert_eq!(
            model_dir("specs/juno/idle-wake/JunoIdleWake3.heavy.cfg").as_deref(),
            Some("idle-wake")
        );
        assert_eq!(
            model_dir("specs/juno/idle-wake/JunoIdleWake3.heavy.cfg").as_deref(),
            Some("idle-wake")
        );
    }

    #[test]
    fn a_unit_whose_target_this_repo_lacks_is_not_discovered() {
        for kind in ["tla", "models", "full"] {
            assert!(
                !unit_present(kind, false),
                "{kind}: kept although its target is gone"
            );
            assert!(
                unit_present(kind, true),
                "{kind}: dropped although its target is there"
            );
        }
        // The other kinds name directories, command lines or lists of files: never judged by their target.
        for kind in ["verus", "shipped", "mutants", "mutations", "deep"] {
            assert!(
                unit_present(kind, false),
                "{kind}: dropped for a target this check does not look at"
            );
        }
    }

    fn unit(inputs: &[&str]) -> Unit {
        Unit {
            name: "u".into(),
            group: "g".into(),
            kind: "verus".into(),
            target: "t".into(),
            inputs: inputs.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn only_the_named_verus_unit_gets_extra_arguments() {
        assert_eq!(unit(&[]).command(), "\"$VERUS\" t");
        let mut raised = unit(&[]);
        raised.name = "verus/juno-surface/rules".into();
        raised.target = "verification/juno-surface/rules.rs".into();
        assert_eq!(
            raised.command(),
            "\"$VERUS\" --rlimit 20 verification/juno-surface/rules.rs"
        );
        // The table is keyed on the unit name, so the same file under another kind gets nothing.
        raised.kind = "mutants".into();
        assert_eq!(raised.command(), "bash verification/juno-surface/rules.rs");
    }

    #[test]
    fn a_change_affects_only_units_that_read_it() {
        let u = unit(&["crates/a/", "verification/x/rules.rs"]);
        assert!(u.affected_by("crates/a/src/lib.rs"));
        assert!(u.affected_by("verification/x/rules.rs"));
        assert!(!u.affected_by("crates/ab/src/lib.rs"));
        assert!(!u.affected_by("verification/x/rules.rs.orig"));
        assert!(!u.affected_by("README.md"));
    }

    #[test]
    fn toolchain_and_ci_changes_affect_every_unit() {
        let u = unit(&[]);
        assert!(u.affected_by("references/upstream.lock"));
        assert!(u.affected_by("tools/ci/lucina-ci/src/main.rs"));
        assert!(u.affected_by("scripts/verify-shipped.sh"));
    }

    #[test]
    fn path_attributes_are_found_also_in_proof_lines() {
        let t = "#[path = \"../../crates/x/src/lib.rs\"]\nmod x;\n   //@ include!(\"y.rs\");\nfn a() {}\n";
        assert_eq!(
            path_refs(t),
            vec!["../../crates/x/src/lib.rs".to_string(), "y.rs".to_string()]
        );
    }

    #[test]
    fn owner_dirs() {
        assert_eq!(owner_dir("crates/a/src/lib.rs"), "crates/a/");
        assert_eq!(
            owner_dir("userspace/crates/b/src/x.rs"),
            "userspace/crates/b/"
        );
        assert_eq!(owner_dir("verification/c/d.rs"), "verification/c/d.rs");
    }

    #[test]
    fn normalize_stops_at_the_root() {
        assert_eq!(
            normalize("verification/a/../../crates/b.rs").as_deref(),
            Some("crates/b.rs")
        );
        assert_eq!(normalize("a/../../b"), None);
    }

    #[test]
    fn crate_tokens_ignore_glued_words() {
        let t: Vec<String> = crate_tokens("cp kernel/lucina-idle/src/x.rs; my_crates/nope")
            .into_iter()
            .collect();
        assert_eq!(t, vec!["kernel/lucina-idle/".to_string()]);
    }

    #[test]
    fn the_marker_declares_the_names_and_a_script_without_it_declares_none() {
        let sh = "#!/usr/bin/env bash\n# What this proves.\n# mutants-units: disk_rules patch_rules wire\nset -uo pipefail\n";
        assert_eq!(
            mutant_unit_names(sh).unwrap(),
            Some(vec![
                "disk_rules".to_string(),
                "patch_rules".to_string(),
                "wire".to_string()
            ])
        );
        // The spacing of the comment and of the names is the script's own, and a script that takes no argument is one unit.
        assert_eq!(
            mutant_unit_names("#!/bin/sh\n#mutants-units:   a_b2   \nname=${1:-a_b2}\n").unwrap(),
            Some(vec!["a_b2".to_string()])
        );
        assert_eq!(
            mutant_unit_names("#!/bin/sh\necho 'not a marker: mutants-units: a'\n").unwrap(),
            None
        );
    }

    #[test]
    fn a_marker_that_cannot_be_read_as_names_is_refused() {
        // The empty and duplicated lists are the ones that would otherwise register nothing or the same file twice.
        for (sh, why) in [
            ("#!/bin/sh\n# mutants-units:\n", "no name"),
            (
                "#!/bin/sh\n# mutants-units: wire\n# mutants-units: patch\n",
                "one marker line",
            ),
            ("#!/bin/sh\n# mutants-units: wire wire\n", "twice"),
            (
                "#!/bin/sh\n# mutants-units: wire-rules\n",
                "letters, digits and underscore",
            ),
        ] {
            let e = mutant_unit_names(sh).unwrap_err();
            assert!(e.contains(why), "{sh:?} gave {e}");
        }
        // A marker the script's own header has grown away from is refused too, never read as no marker.
        let deep = format!(
            "{}# mutants-units: wire\n",
            "# fill to the top\n".repeat(MUTANTS_MARKER_TOP)
        );
        let e = mutant_unit_names(&deep).unwrap_err();
        assert!(e.contains("first 40 lines"), "{e}");
    }

    #[test]
    fn an_empty_global_mutation_list_is_not_a_list() {
        // The shape Brimstone carried as a stub: the script is there, so the unit was, and it proved nothing.
        for empty in [
            "#!/bin/sh\nmutations=(\n)\n",
            "#!/bin/sh\nmutations=(\n\n    # every mutant lives with its unit\n)\n",
            "#!/bin/sh\nmutations=()\n",
            "#!/bin/sh\necho no list\n",
        ] {
            assert!(!has_mutation_entries(empty), "{empty:?}");
        }
        for one in [
            "#!/bin/sh\nmutations=(\n    'a.rs|s/x/y/|weakens one'\n)\n",
            "#!/bin/sh\nmutations=('a.rs|s/x/y/|weakens one')\n",
            "#!/bin/sh\nmutations=(\n    # a comment\n    'a.rs|s/x/y/|weakens one'\n    'b.rs|s/p/q/|weakens two'\n)\n",
        ] {
            assert!(has_mutation_entries(one), "{one:?}");
        }
    }

    #[test]
    fn tsv_round_trips() {
        let us = vec![unit(&["a/", "b"])];
        assert_eq!(from_tsv(&to_tsv(&us)).unwrap(), us);
    }
}
