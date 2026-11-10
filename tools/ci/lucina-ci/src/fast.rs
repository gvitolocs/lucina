//! `check-fast`: what an agent runs in its worktree before a commit. Only what the change touched, each step capped in memory and
//! time, in one shared Cargo target. It never replaces a gate: `lagent merge` and GitHub still decide. docs/ci/SHIFT-LEFT.md.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

/// Where a changed file's crate lives.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone)]
pub enum Owner {
    /// A root workspace member: its package name.
    Member(String),
    /// A crate with its own workspace: its manifest.
    Standalone(String),
    /// A Rust file no fast step can build (its gate runs on merge).
    Other(String),
}

/// The package name in a manifest.
pub fn package_name(toml: &str) -> Option<String> {
    let mut in_package = false;
    for l in toml.lines() {
        let t = l.trim();
        if t.starts_with('[') {
            in_package = t == "[package]";
        } else if in_package && t.starts_with("name") {
            return t
                .split_once('=')?
                .1
                .trim()
                .trim_matches('"')
                .to_string()
                .into();
        }
    }
    None
}

pub fn root_members(root_toml: &str) -> BTreeSet<String> {
    root_toml
        .lines()
        .find(|l| l.starts_with("members = ["))
        .map(|l| {
            l.split('"')
                .skip(1)
                .step_by(2)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// The crate a changed `.rs` or `Cargo.toml` belongs to, by the nearest manifest.
pub fn owner_of(root: &Path, members: &BTreeSet<String>, path: &str) -> Option<Owner> {
    if !(path.ends_with(".rs") || path.ends_with("Cargo.toml")) {
        return None;
    }
    let mut dir = Path::new(path).parent();
    while let Some(d) = dir {
        let s = d.display().to_string();
        if s.is_empty() {
            return None;
        }
        let manifest = root.join(d).join("Cargo.toml");
        if let Ok(text) = fs::read_to_string(&manifest) {
            if members.contains(&s) {
                return package_name(&text).map(Owner::Member);
            }
            if text.contains("[workspace]") && s.starts_with("tools/") {
                return Some(Owner::Standalone(format!("{s}/Cargo.toml")));
            }
            return Some(Owner::Other(s));
        }
        dir = d.parent();
    }
    None
}

/// Steps to run, as (name, shell command, timeout in seconds).
pub fn cargo_steps(owners: &BTreeSet<Owner>, changed: &[String]) -> Vec<(String, String, u64)> {
    let mut steps = Vec::new();
    let members: Vec<String> = owners
        .iter()
        .filter_map(|o| {
            if let Owner::Member(n) = o {
                Some(format!("-p {n}"))
            } else {
                None
            }
        })
        .collect();
    if !members.is_empty() {
        let p = members.join(" ");
        // Only warnings in the files this change touched fail the loop: older warnings elsewhere in a crate are not its doing.
        let paths: Vec<String> = changed
            .iter()
            .filter(|c| c.ends_with(".rs"))
            .map(|c| c.replace('.', "\\."))
            .collect();
        let re = format!("^({}):[0-9]+:[0-9]+: (warning|error)", paths.join("|"));
        steps.push((
            "fmt".to_string(),
            format!("out=$(cargo fmt {p} -- --check 2>&1); rc=$?; if printf '%s\\n' \"$out\" | grep -E \"^Diff in .*({}):\"; then exit 1; fi; if printf '%s\\n' \"$out\" | grep -q '^Diff in '; then exit 0; fi; exit $rc", paths.join("|")),
            60,
        ));
        steps.push((
            "clippy".to_string(),
            format!("out=$(cargo clippy --quiet --locked {p} --all-targets --no-deps --message-format=short 2>&1); rc=$?; if printf '%s\\n' \"$out\" | grep -E \"{re}\"; then exit 1; fi; exit $rc"),
            300,
        ));
        steps.push((
            "unit-tests".to_string(),
            format!("cargo test --quiet --locked {p} --lib"),
            300,
        ));
    }
    for o in owners {
        if let Owner::Standalone(m) = o {
            let d = m.trim_end_matches("/Cargo.toml").replace('/', "-");
            steps.push((
                format!("fmt-{d}"),
                format!("cargo fmt --manifest-path {m} -- --check"),
                60,
            ));
            steps.push((
                format!("clippy-{d}"),
                format!(
                    "cargo clippy --quiet --locked --manifest-path {m} --all-targets -- -D warnings"
                ),
                300,
            ));
            steps.push((
                format!("unit-tests-{d}"),
                format!("cargo test --quiet --locked --manifest-path {m}"),
                300,
            ));
        }
    }
    steps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_name_is_from_the_package_table() {
        let t = "[workspace]\nname = \"no\"\n[package]\nversion = \"1\"\nname = \"yes\"\n";
        assert_eq!(package_name(t).as_deref(), Some("yes"));
    }

    #[test]
    fn members_line() {
        assert_eq!(
            root_members("[workspace]\nmembers = [\"a\", \"b\"]\n").len(),
            2
        );
    }

    #[test]
    fn steps_cover_members_together_and_standalone_apart() {
        let o: BTreeSet<Owner> = [
            Owner::Member("a".into()),
            Owner::Member("b".into()),
            Owner::Standalone("tools/ci/x/Cargo.toml".into()),
            Owner::Other("office/y".into()),
        ]
        .into_iter()
        .collect();
        let s = cargo_steps(&o, &["crates/a/src/lib.rs".to_string()]);
        assert!(s[1].1.contains("crates/a/src/lib\\.rs"));
        assert_eq!(s.len(), 6);
        assert!(s[0].1.contains("-p a -p b"));
        assert!(
            s.iter()
                .any(|x| x.1.contains("--manifest-path tools/ci/x/Cargo.toml"))
        );
        assert!(
            !s.iter().any(|x| x.1.contains("office")),
            "a crate we cannot build is not faked"
        );
    }

    #[test]
    fn nothing_to_do_means_no_steps() {
        assert!(cargo_steps(&BTreeSet::new(), &[]).is_empty());
    }
}
