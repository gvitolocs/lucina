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

/// Repository paths a mutant script names: every `crates/...` word in it (a `../../` before it is dropped). Shell variables and globs
/// end a word, so `crates/foo/src/$x.rs` yields `crates/foo/src/`.
pub fn script_paths(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find("crates/") {
        let tail = &rest[i..];
        let end = tail
            .find(|c: char| !(c.is_ascii_alphanumeric() || "_-./".contains(c)))
            .unwrap_or(tail.len());
        let word = tail[..end].trim_end_matches('.');
        // "crates/" alone, or inside a longer word such as "mycrates/", is not a path.
        let glued = rest[..i]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
        if word.len() > "crates/".len() && !glued && !out.iter().any(|w| w == word) {
            out.push(word.to_string());
        }
        rest = &tail[end.max(1)..];
    }
    out
}

/// `(unit, file)` for every file a Verus or shipped proof reads that the snapshot rules leave out.
pub fn gaps(root: &Path, units: &[Unit], rules: &Rules) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for u in units
        .iter()
        .filter(|u| matches!(u.kind.as_str(), "mutants" | "mutations"))
    {
        // The script is the unit's target (a path, maybe with variables in front of `bash`).
        let script = u
            .target
            .split_whitespace()
            .rev()
            .find(|w| w.ends_with(".sh"))
            .unwrap_or("");
        let Ok(text) = std::fs::read_to_string(root.join(script)) else {
            continue;
        };
        for p in script_paths(&text) {
            if root.join(&p).exists() && !snapshot::included(rules, &p) {
                out.push((u.name.clone(), p));
            }
        }
    }
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
    fn mutant_scripts_name_their_crate_paths() {
        let sh = "cp ../../crates/zzz-leftout/src/mru.rs \"$d/m.rs\"\nsed -i s/x/y/ crates/zzz-kept/src/$f.rs # mycrates/nope\nfor t in crates/zzz-kept/tests/*.rs; do :; done\necho crates/";
        assert_eq!(
            script_paths(sh),
            [
                "crates/zzz-leftout/src/mru.rs",
                "crates/zzz-kept/src/",
                "crates/zzz-kept/tests/"
            ]
        );
        assert!(script_paths("echo hi").is_empty());
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
        // A mutant script that names the left-out crate is found too (2026-11-11: juno-wm-rules, reported as a surviving mutant).
        std::fs::write(
            t.join("verification/zzz-proof/mutants.sh"),
            "cp ../../crates/zzz-leftout/src/rules.rs \"$d\"\ncp crates/zzz-kept/src/board.rs \"$d\"\n",
        )
        .unwrap();
        let mut m = unit(
            "mutants/zzz-proof",
            "VARS=1 bash verification/zzz-proof/mutants.sh",
        );
        m.kind = "mutants".into();
        let both = [
            unit("verus/zzz-proof/rules", "verification/zzz-proof/rules.rs"),
            m,
        ];
        let g = gaps(&t, &both, &rules);
        assert!(
            g.contains(&(
                "mutants/zzz-proof".to_string(),
                "crates/zzz-leftout/src/rules.rs".to_string()
            )),
            "{g:?}"
        );
        assert!(
            !g.iter()
                .any(|(u, f)| u == "mutants/zzz-proof" && f.contains("zzz-kept"))
        );
        // Named in a group: no gap.
        let rules = snapshot::parse_rules(
            "include verification/\ngroup kernel zzz-kept\ngroup services zzz-leftout\n",
        )
        .unwrap();
        assert!(gaps(&t, &both, &rules).is_empty());
        std::fs::remove_dir_all(&t).ok();
    }
}
