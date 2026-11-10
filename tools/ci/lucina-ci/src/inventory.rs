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
            "full" => "full",
            _ => "tla",
        }
    }

    /// The shell command, run from the repo root with VERUS set.
    pub fn command(&self) -> String {
        match self.kind.as_str() {
            "verus" => format!("\"$VERUS\" {}", self.target),
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
            units.push(Unit {
                name: format!("mutants/{dir}"),
                group: group_of(dir)?,
                kind: "mutants".into(),
                target: f.to_string(),
                inputs: inputs.into_iter().collect(),
            });
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

    let global_mutants =
        fs::read_to_string(root.join("scripts/verify-mutations.sh")).map_err(|e| e.to_string())?;
    let mut inputs: BTreeSet<String> = [
        "scripts/verify-mutations.sh".to_string(),
        "verification/".to_string(),
    ]
    .into();
    inputs.extend(crate_tokens(&global_mutants));
    units.push(Unit {
        name: "mutations/global".into(),
        group: group_of("global-mutations")?,
        kind: "mutations".into(),
        target: "scripts/verify-mutations.sh".into(),
        inputs: inputs.into_iter().collect(),
    });

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

    units.retain(|u| u.kind != "tla" || root.join(&u.target).exists());
    units.sort_by(|a, b| a.name.cmp(&b.name));
    if let Some(w) = units.windows(2).find(|w| w[0].name == w[1].name) {
        return Err(format!("two proof units are both named {}", w[0].name));
    }
    Ok(units)
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn tsv_round_trips() {
        let us = vec![unit(&["a/", "b"])];
        assert_eq!(from_tsv(&to_tsv(&us)).unwrap(), us);
    }
}
