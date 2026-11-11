//! A proof must not read a file the public snapshot leaves out (docs/ci/AUTO-SYNC.md). On 2026-11-11 the net-installer proof read
//! `services/lucina-release/src/rules.rs`, a crate no `group` line of `tools/ci/public-snapshot.rules` named, so the sync branch failed on
//! GitHub with "couldn't find file". This finds that on buildhost, at merge time: every `#[path = "..."]` file a Verus proof pulls in,
//! followed through the files it pulls in, must be one the snapshot rules include.

use crate::inventory::Unit;
use crate::snapshot::{self, Rules};
use std::path::Path;

/// The `#[path = "..."]` strings of a Rust source, in order.
pub fn path_attributes(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = src;
    while let Some(i) = rest.find("#[path") {
        rest = &rest[i + 6..];
        let Some(eq) = rest.find('=') else { break };
        let after = rest[eq + 1..].trim_start();
        if let Some(body) = after.strip_prefix('"')
            && let Some(end) = body.find('"')
        {
            out.push(body[..end].to_string());
        }
    }
    out
}

/// `dir` + a relative path with `..` segments, as a clean repo-relative path (None: it leaves the repository).
pub fn resolve(dir: &str, rel: &str) -> Option<String> {
    let mut parts: Vec<&str> = dir.split('/').filter(|p| !p.is_empty()).collect();
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            s => parts.push(s),
        }
    }
    Some(parts.join("/"))
}

/// Every file a proof's source pulls in by `#[path]`, directly or through another, as repo-relative paths.
pub fn read_files(root: &Path, target: &str) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    let mut todo = vec![target.to_string()];
    while let Some(file) = todo.pop() {
        let Ok(src) = std::fs::read_to_string(root.join(&file)) else {
            continue;
        };
        let dir = file.rsplit_once('/').map_or("", |(d, _)| d);
        for rel in path_attributes(&src) {
            if let Some(p) = resolve(dir, &rel)
                && !seen.contains(&p)
            {
                seen.push(p.clone());
                todo.push(p);
            }
        }
    }
    seen
}

/// `(unit, file)` for every file a Verus or shipped proof reads that the snapshot rules leave out.
pub fn gaps(root: &Path, units: &[Unit], rules: &Rules) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for u in units
        .iter()
        .filter(|u| matches!(u.kind.as_str(), "verus" | "shipped"))
    {
        if !u.target.ends_with(".rs") {
            continue;
        }
        for f in read_files(root, &u.target) {
            if root.join(&f).exists() && !snapshot::included(rules, &f) {
                out.push((u.name.clone(), f));
            }
        }
    }
    out
}

/// Only in the private tree: the public one has the rewritten layout and no `crates/` folder.
pub fn check(root: &Path, units: &[Unit]) -> Result<Vec<(String, String)>, String> {
    let rules_path = root.join("tools/ci/public-snapshot.rules");
    if !root.join("crates").is_dir() || !rules_path.is_file() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(&rules_path).map_err(|e| e.to_string())?;
    let rules = snapshot::parse_rules(&text)?;
    Ok(gaps(root, units, &rules))
}

#[cfg(test)]
mod tests {
    use super::*;

    // The public copy runs these tests too, after its path rewrite: the names below belong to no real crate, so the rewrite leaves
    // them alone (2026-11-11: with real crate names the same test failed on GitHub).
    #[test]
    fn path_attributes_are_found() {
        let src = "#![feature(x)]\n#[path = \"../../crates/a/src/r.rs\"]\npub mod r;\n  #[path=\"b.rs\"] mod b;\n#[cfg(test)]\nmod t;";
        assert_eq!(path_attributes(src), ["../../crates/a/src/r.rs", "b.rs"]);
        assert!(path_attributes("fn main() {}").is_empty());
    }

    #[test]
    fn relative_paths_resolve_inside_the_repository() {
        assert_eq!(
            resolve(
                "verification/zzz-proof",
                "../../crates/zzz-leftout/src/rules.rs"
            )
            .unwrap(),
            "crates/zzz-leftout/src/rules.rs"
        );
        assert_eq!(resolve("a", "./b.rs").unwrap(), "a/b.rs");
        assert_eq!(resolve("a", "../../../x"), None);
    }

    fn unit(name: &str, target: &str) -> Unit {
        Unit {
            name: name.into(),
            group: "g".into(),
            kind: "verus".into(),
            target: target.into(),
            inputs: Vec::new(),
        }
    }

    #[test]
    fn a_proof_reading_a_crate_the_snapshot_leaves_out_is_found() {
        let t = std::env::temp_dir().join(format!("lucina-ci-coverage-{}", std::process::id()));
        std::fs::create_dir_all(t.join("verification/zzz-proof")).unwrap();
        std::fs::create_dir_all(t.join("crates/zzz-leftout/src")).unwrap();
        std::fs::create_dir_all(t.join("crates/zzz-kept/src")).unwrap();
        std::fs::write(t.join("crates/zzz-leftout/src/rules.rs"), "").unwrap();
        std::fs::write(t.join("crates/zzz-kept/src/board.rs"), "").unwrap();
        std::fs::write(
            t.join("verification/zzz-proof/rules.rs"),
            "#[path = \"../../crates/zzz-leftout/src/rules.rs\"]\npub mod rules;\n#[path = \"../../crates/zzz-kept/src/board.rs\"]\npub mod board;\n",
        )
        .unwrap();
        let rules =
            snapshot::parse_rules("include verification/\ngroup kernel zzz-kept\n").unwrap();
        let u = [unit(
            "verus/zzz-proof/rules",
            "verification/zzz-proof/rules.rs",
        )];
        let g = gaps(&t, &u, &rules);
        assert_eq!(
            g,
            [(
                "verus/zzz-proof/rules".to_string(),
                "crates/zzz-leftout/src/rules.rs".to_string()
            )]
        );
        // Named in a group: no gap.
        let rules = snapshot::parse_rules(
            "include verification/\ngroup kernel zzz-kept\ngroup services zzz-leftout\n",
        )
        .unwrap();
        assert!(gaps(&t, &u, &rules).is_empty());
        std::fs::remove_dir_all(&t).ok();
    }
}
