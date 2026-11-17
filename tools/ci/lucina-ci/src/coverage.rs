//! A proof must not read a file the public snapshot leaves out (docs/ci/AUTO-SYNC.md). On 2026-11-11 the net-installer proof read
//! `services/lucina-release/src/rules.rs`, a crate no `group` line of `tools/ci/public-snapshot.rules` named, so the sync branch failed on
//! GitHub with "couldn't find file". This finds that on buildhost, at merge time: every `#[path = "..."]` file a Verus proof pulls in,
//! followed through the files it pulls in, must be one the snapshot rules include. The same rule for the launcher's own tests
//! (`test_path_gaps`): the public copy runs `cargo test` of itself at every auto-sync precheck, and on 2026-11-16 and 2026-11-17 two
//! launcher tests in a row died there because the `scripts/tests/` script each runs was left out of the copy.

use crate::inventory::Unit;
use crate::snapshot::{self, Rules};
use crate::spec;
use std::collections::BTreeMap;
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

/// Mutant scripts that do not print what the verdict needs: `baseline: verifies` and the closing `refused by verification` line. Caught at
/// merge time, because the run on GitHub would call such a unit `infra_error`.
pub fn unguarded_mutant_scripts(root: &Path, units: &[Unit]) -> Vec<String> {
    let mut out = Vec::new();
    for u in units.iter().filter(|u| u.kind == "mutants") {
        let script = u
            .target
            .split_whitespace()
            .rev()
            .find(|w| w.ends_with(".sh"))
            .unwrap_or("");
        let Ok(text) = std::fs::read_to_string(root.join(script)) else {
            continue;
        };
        if !text.contains("baseline: verifies") || !text.contains("refused by verification") {
            out.push(format!("{} ({script})", u.name));
        }
    }
    out
}

/// The bash name characters of an assignment's left side: `x=…`, nothing else.
fn is_shell_ident(s: &str) -> bool {
    let mut c = s.chars();
    let Some(f) = c.next() else { return false };
    (f.is_ascii_alphabetic() || f == '_') && c.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The variable a mutants script reads its first argument into: `name=${1:-disk_rules}` or `name=${1:?usage}`. None for a
/// script that takes no argument, which is the state `inventory` registers as the single `mutants/<dir>` unit.
fn argument_variable(text: &str) -> Option<String> {
    for l in text.lines() {
        let t = l.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let Some((lhs, rhs)) = t.split_once('=') else {
            continue;
        };
        if is_shell_ident(lhs) && rhs.trim_start().starts_with("${1") {
            return Some(lhs.to_string());
        }
    }
    None
}

/// `true` when the variable use at `i` of `s` is a whole one: `$name` and `${name}`, not `$names`.
fn var_at(s: &str, i: usize, var: &str) -> bool {
    let after = &s[i..];
    if after.starts_with(&format!("${{{var}}}")) {
        return true;
    }
    after.starts_with(&format!("${var}"))
        && after[var.len() + 1..]
            .chars()
            .next()
            .is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '_'))
}

/// The word with each whole `$<var>` / `${<var>}` replaced by `name`, and whether anything was replaced.
fn interpolate(word: &str, var: &str, name: &str) -> (String, bool) {
    let mut out = String::new();
    let mut rest = word;
    let mut did = false;
    while let Some(i) = rest.find('$') {
        out.push_str(&rest[..i]);
        if var_at(rest, i, var) {
            out.push_str(name);
            let used = if rest[i..].starts_with(&format!("${{{var}}}")) {
                var.len() + 3
            } else {
                var.len() + 1
            };
            rest = &rest[i + used..];
            did = true;
        } else {
            out.push('$');
            rest = &rest[i + 1..];
        }
    }
    out.push_str(rest);
    (out, did)
}

/// The files a mutants script builds from one of its names: every word that interpolates `$<var>` and ends in `.rs`, with the
/// shell around it stripped (`${2:-dir/$name.rs}` and `"dir/$name.rs"` both give `dir/<name>.rs`). Empty when the script
/// states no such convention: discovery cannot invent a file for a name, so it asks nothing of it.
fn files_for_name(text: &str, var: &str, name: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for l in text.lines() {
        let t = l.trim_start();
        if t.starts_with('#') {
            continue;
        }
        for w in t.split_whitespace() {
            let mut w = w.trim_matches(|c| c == '"' || c == '\'');
            // `${2:-src/dir/$name.rs}` is a default value: the path is what follows the `:-`, up to the closing brace.
            if let Some(k) = w.find(":-")
                && w[..k].contains("${")
            {
                w = &w[k + 2..];
            }
            w = w
                .trim_matches(|c| c == '"' || c == '\'')
                .trim_end_matches('}');
            if !w.ends_with(".rs") {
                continue;
            }
            let (file, did) = interpolate(w, var, name);
            if did && !out.contains(&file) {
                out.push(file);
            }
        }
    }
    out
}

/// Named mutant units whose name leads to a file no shipped unit verifies: the script says in its own text which file the
/// name is the proof of, so a name that points elsewhere is a proof CI does not run. A script that states no such convention
/// is not judged (there is nothing generic to judge it by), and the plain `mutants/<dir>` unit has no name to check.
pub fn unverified_named_mutants(root: &Path, units: &[Unit]) -> Vec<String> {
    let mut out = Vec::new();
    for u in units.iter().filter(|u| u.kind == "mutants") {
        let mut words = u.target.split_whitespace();
        let (Some(script), Some(name)) = (words.next(), words.next()) else {
            continue;
        };
        let Ok(text) = std::fs::read_to_string(root.join(script)) else {
            continue;
        };
        let Some(var) = argument_variable(&text) else {
            continue;
        };
        let files = files_for_name(&text, &var, name);
        if files.is_empty() {
            continue;
        }
        if !units
            .iter()
            .any(|s| s.kind == "shipped" && files.contains(&s.target))
        {
            out.push(format!(
                "{}: the name `{name}` is {} by the script's own convention, and no shipped unit verifies that file",
                u.name, files[0]
            ));
        }
    }
    out
}

/// Licence ids a crate carries: its `license` field and the `SPDX-License-Identifier` in the first lines of its sources.
pub fn licences_of(crate_dir: &Path) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut add = |s: String| {
        let s = s.trim().trim_matches('"').to_string();
        if !s.is_empty() && !out.contains(&s) {
            out.push(s);
        }
    };
    if let Ok(t) = std::fs::read_to_string(crate_dir.join("Cargo.toml")) {
        for l in t.lines() {
            if let Some(v) = l.trim().strip_prefix("license")
                && let Some(v) = v.trim_start().strip_prefix('=')
            {
                add(v.to_string());
            }
        }
    }
    fn walk(d: &Path, depth: usize, add: &mut dyn FnMut(String)) {
        let Ok(rd) = std::fs::read_dir(d) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() && depth < 6 {
                walk(&p, depth + 1, add);
            } else if p.extension().is_some_and(|x| x == "rs")
                && let Ok(t) = std::fs::read_to_string(&p)
            {
                for l in t.lines().take(8) {
                    if let Some(i) = l.find("SPDX-License-Identifier:") {
                        add(l[i + "SPDX-License-Identifier:".len()..].to_string());
                    }
                }
            }
        }
    }
    walk(&crate_dir.join("src"), 0, &mut add);
    walk(&crate_dir.join("tests"), 0, &mut add);
    out
}

/// `(crate, licences)` for every crate in the snapshot that carries a licence other than MIT and is not named in the LICENSE file, as
/// `crates/<name>`. The public copy's LICENSE says what keeps its own licence; this keeps that list from drifting (2026-11-11: the Linux
/// ports lucina-apple-aic, lucina-apple-uart and lucina-errseq, and the MPL crates).
pub fn licence_gaps(root: &Path, rules: &Rules) -> Vec<(String, String)> {
    let license = std::fs::read_to_string(root.join("LICENSE")).unwrap_or_default();
    let mut out = Vec::new();
    for (name, _) in &rules.groups {
        let dir = root.join("crates").join(name);
        if !dir.is_dir() {
            continue;
        }
        let ids = licences_of(&dir);
        // MIT itself, or an expression that offers MIT ("MIT OR Apache-2.0"): the LucinaOS licence applies, nothing to list.
        let mit_ok = |i: &String| {
            i == "MIT" || (i.contains(" OR ") && i.split(" OR ").any(|p| p.trim() == "MIT"))
        };
        if ids.iter().any(|i| !mit_ok(i)) && !license.contains(&format!("crates/{name}")) {
            out.push((name.clone(), ids.join(", ")));
        }
    }
    out
}

/// Only in the private tree: the public one has the rewritten layout and no `crates/` folder.
pub fn check(root: &Path, units: &[Unit]) -> Result<Vec<(String, String)>, String> {
    let r = check_proofs(root, units)?;
    Ok(r)
}

pub fn check_licences(root: &Path) -> Result<Vec<(String, String)>, String> {
    let rules_path = root.join("tools/ci/public-snapshot.rules");
    if !root.join("crates").is_dir() || !rules_path.is_file() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(&rules_path).map_err(|e| e.to_string())?;
    Ok(licence_gaps(root, &snapshot::parse_rules(&text)?))
}

fn check_proofs(root: &Path, units: &[Unit]) -> Result<Vec<(String, String)>, String> {
    let rules_path = root.join("tools/ci/public-snapshot.rules");
    if !root.join("crates").is_dir() || !rules_path.is_file() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(&rules_path).map_err(|e| e.to_string())?;
    let rules = snapshot::parse_rules(&text)?;
    Ok(gaps(root, units, &rules))
}

/// The repo paths a `scripts/` or `docs/ci/` text names: from each marker to the end of the path word. A marker glued into a
/// longer word (`userscripts/x`) is not a path, and one that ends the text (`docs/ci/` alone) names nothing.
fn named_paths(text: &str) -> Vec<String> {
    const MARKERS: [&str; 2] = ["scripts/", "docs/ci/"];
    let mut out: Vec<String> = Vec::new();
    for m in MARKERS {
        let mut at = 0;
        while let Some(k) = text[at..].find(m) {
            let i = at + k;
            at = i + m.len();
            let glued = text[..i]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
            if glued {
                continue;
            }
            let rest = &text[i..];
            let end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || "_-./".contains(c)))
                .unwrap_or(rest.len());
            let w = &rest[..end];
            if w.len() > m.len() && !out.iter().any(|p| p == w) {
                out.push(w.to_string());
            }
        }
    }
    out
}

/// The token ranges of the bodies of this file's `#[cfg(test)]` items. The lexer reads a string whole, so a `#[cfg(test)]`
/// inside fixture text opens nothing; a `mod t;` declaration has no body here and its file is not opened.
fn cfg_test_regions(t: &[spec::Tok]) -> Vec<(usize, usize)> {
    let mut regions = Vec::new();
    let mut i = 0;
    while i < t.len() {
        let Some(e) = spec::attr_end(t, i) else {
            i += 1;
            continue;
        };
        let name: String = t[i..e]
            .iter()
            .map(|x| x.s.as_str())
            .collect::<Vec<_>>()
            .join("");
        if !name.contains("cfg(test)") {
            i = e;
            continue;
        }
        let mut k = e;
        while let Some(e2) = spec::attr_end(t, k) {
            k = e2;
        }
        let mut d = 0i32;
        let mut j = k;
        while j < t.len() {
            match t[j].s.as_str() {
                "(" | "[" => d += 1,
                ")" | "]" => d -= 1,
                "{" if d <= 0 => {
                    let close = spec::match_brace(t, j);
                    regions.push((j, close + 1));
                    j = close;
                    break;
                }
                ";" if d <= 0 => break,
                _ => {}
            }
            j += 1;
        }
        i = j + 1;
    }
    regions
}

/// A token holding a string literal (a raw one included).
fn is_string(s: &str) -> bool {
    s.starts_with('"') || s.starts_with("r\"") || s.starts_with("r#\"")
}

/// `const NAME: &str = "value"` of this file: name to value. A test names the record paths through them (`CUTOFF`-style
/// constants), so a use of the name counts as naming the value. Cross-file and `concat!` values are not followed.
fn path_consts(t: &[spec::Tok]) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut i = 0;
    while i + 2 < t.len() {
        if t[i].s == "const" && t[i + 1].ident {
            let mut j = i + 2;
            while j < t.len() && !(t[j].s == "=" || t[j].s == ";" || t[j].s == "{") {
                j += 1;
            }
            if j + 1 < t.len() && t[j].s == "=" && !t[j + 1].ident && is_string(&t[j + 1].s) {
                out.insert(t[i + 1].s.clone(), t[j + 1].s.clone());
            }
            i = j + 1;
            continue;
        }
        i += 1;
    }
    out
}

/// The paths a token range names: in its strings, and through the file's `const` values used as names.
fn named_in(
    t: &[spec::Tok],
    a: usize,
    b: usize,
    consts: &BTreeMap<String, String>,
    out: &mut Vec<String>,
) {
    for tok in &t[a.min(t.len())..b.min(t.len())] {
        let found = if tok.ident {
            consts.get(&tok.s).map_or(Vec::new(), |v| named_paths(v))
        } else {
            named_paths(&tok.s)
        };
        for p in found {
            if !out.contains(&p) {
                out.push(p);
            }
        }
    }
}

/// The omissions the rules declare on purpose: a `# private-only <path>` comment line, for a test that carries a private
/// path as fixture text and never reads the real file.
pub fn private_only_markers(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| l.trim().strip_prefix("# private-only "))
        .filter_map(|l| l.split_whitespace().next().map(str::to_string))
        .collect()
}

/// The messages for every `scripts/` or `docs/ci/` path the launcher's own tests name that exists in the private tree but
/// the snapshot neither includes nor declares private. The tests of `tools/ci/lucina-ci/tests` count whole (each is one),
/// in `src` only the `#[cfg(test)]` bodies do: production text is prose, not a test reading.
pub fn test_path_gaps(root: &Path, rules: &Rules, marked: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for dir in ["tools/ci/lucina-ci/tests", "tools/ci/lucina-ci/src"] {
        let Ok(rd) = std::fs::read_dir(root.join(dir)) else {
            continue;
        };
        let mut names: Vec<String> = rd
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.ends_with(".rs"))
            .collect();
        names.sort();
        for name in names {
            let rel = format!("{dir}/{name}");
            let Ok(src) = std::fs::read_to_string(root.join(&rel)) else {
                continue;
            };
            let t = spec::lex(&src, &[], &[]);
            let consts = path_consts(&t);
            let mut paths = Vec::new();
            if dir.ends_with("src") {
                for (a, b) in cfg_test_regions(&t) {
                    named_in(&t, a, b, &consts, &mut paths);
                }
            } else {
                named_in(&t, 0, t.len(), &consts, &mut paths);
            }
            for p in paths {
                if root.join(&p).exists() && !snapshot::included(rules, &p) && !marked.contains(&p)
                {
                    out.push(format!(
                        "{rel} reads {p}, which tools/ci/public-snapshot.rules leaves out (the public copy runs this test)"
                    ));
                }
            }
        }
    }
    out
}

/// Only in the private tree, like the `#[path]` check above: the public copy has neither `crates/` nor the rules file.
/// `rules_path` may name another copy of the rules, to ask whether an older one would have passed.
pub fn check_test_paths(root: &Path, rules_path: &Path) -> Result<Vec<String>, String> {
    if !root.join("crates").is_dir() || !rules_path.is_file() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(rules_path).map_err(|e| e.to_string())?;
    let rules = snapshot::parse_rules(&text)?;
    Ok(test_path_gaps(root, &rules, &private_only_markers(&text)))
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
    fn a_non_mit_crate_in_the_snapshot_must_be_on_the_licence_list() {
        let t = std::env::temp_dir().join(format!("lucina-ci-licence-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        for (name, lic, spdx) in [
            ("zzz-ported", "", "GPL-2.0-only"),
            ("zzz-declared", "GPL-2.0-or-later", ""),
            ("zzz-own", "MIT", "MIT"),
            ("zzz-plain", "", ""),
            ("zzz-listed", "MPL-2.0", ""),
        ] {
            let d = t.join("crates").join(name);
            std::fs::create_dir_all(d.join("src")).unwrap();
            let mut toml = String::from("[package]\nname = \"x\"\n");
            if !lic.is_empty() {
                toml += &format!("license = \"{lic}\"\n");
            }
            std::fs::write(d.join("Cargo.toml"), toml).unwrap();
            let head = if spdx.is_empty() {
                String::new()
            } else {
                format!("// SPDX-License-Identifier: {spdx}\n")
            };
            std::fs::write(d.join("src/lib.rs"), format!("{head}pub fn f() {{}}\n")).unwrap();
        }
        std::fs::write(
            t.join("LICENSE"),
            "MIT ... Code ported from Linux: crates/zzz-listed\n",
        )
        .unwrap();
        let rules = snapshot::parse_rules(
            "group kernel zzz-ported zzz-declared zzz-own zzz-plain zzz-listed\n",
        )
        .unwrap();
        let mut gaps: Vec<String> = licence_gaps(&t, &rules)
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        gaps.sort();
        assert_eq!(gaps, ["zzz-declared", "zzz-ported"]);
        std::fs::write(
            t.join("LICENSE"),
            "crates/zzz-listed crates/zzz-ported crates/zzz-declared\n",
        )
        .unwrap();
        assert!(licence_gaps(&t, &rules).is_empty());
        std::fs::remove_dir_all(&t).ok();
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

    fn zz_file(t: &Path, rel: &str, text: &str) {
        let p = t.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    /// A script of the Brimstone shape: one file per name, the path built from the name itself.
    const NAMED_SH: &str = r#"#!/usr/bin/env bash
# mutants-units: disk_rules wire
name=${1:-disk_rules}
rules=${2:-src/js/runtime/bytecode/fncache/$name.rs}
"$verus" --triggers-mode silent "$work/base-$name.rs" && echo "baseline: verifies"
echo "mutants: 4 refused by verification"
"#;

    /// A script of the LucinaOS shape: no argument at all.
    const PLAIN_SH: &str =
        "#!/usr/bin/env bash\necho 'baseline: verifies'\necho 'refused by verification'\n";

    /// A script that takes a name but builds no file from it: which proof the name is cannot be derived from its text.
    const NOCONV_SH: &str = "#!/bin/sh\nname=${1:-disk_rules}\ncp crates/fncache/$name.src copy\n";

    fn mutants_unit(t: &str, arg: Option<&str>) -> Unit {
        let mut u = unit(
            &format!("mutants/{t}"),
            &format!("verification/{t}/mutants.sh"),
        );
        u.kind = "mutants".into();
        if let Some(a) = arg {
            u.name = format!("mutants/{t}/{a}");
            u.target = format!("verification/{t}/mutants.sh {a}");
        }
        u
    }

    fn shipped_unit(file: &str) -> Unit {
        let stem = file
            .rsplit_once('/')
            .map_or(file, |(_, f)| f)
            .trim_end_matches(".rs");
        let mut u = unit(&format!("shipped/x/{stem}"), file);
        u.kind = "shipped".into();
        u
    }

    #[test]
    fn a_named_script_states_which_file_its_names_are_the_proof_of() {
        assert_eq!(argument_variable(NAMED_SH).as_deref(), Some("name"));
        assert_eq!(
            argument_variable(PLAIN_SH),
            None,
            "a script with no argument names no variable"
        );
        // The repo-relative path the script defaults to, and the copy it writes under its work directory.
        assert_eq!(
            files_for_name(NAMED_SH, "name", "wire"),
            [
                "src/js/runtime/bytecode/fncache/wire.rs".to_string(),
                "$work/base-wire.rs".to_string()
            ]
        );
        // A name with no `.rs` word built from it gives nothing to check.
        assert!(
            files_for_name(NOCONV_SH, "name", "wire").is_empty(),
            "{:?}",
            files_for_name(NOCONV_SH, "name", "wire")
        );
    }

    #[test]
    fn a_name_with_no_shipped_unit_is_refused_and_neither_the_plain_nor_the_unreadable_one_is() {
        let t = std::env::temp_dir().join(format!("lucina-ci-named-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        zz_file(&t, "verification/fncache/mutants.sh", NAMED_SH);
        zz_file(&t, "verification/plain/mutants.sh", PLAIN_SH);
        zz_file(&t, "verification/nocconv/mutants.sh", NOCONV_SH);
        let wire = "src/js/runtime/bytecode/fncache/wire.rs";
        // `wire` is a shipped proof; `disk_rules` is a name whose file CI never verifies. The dir unit of a script with no
        // marker has no name to judge, and a script that states no convention is not judged either.
        let units = [
            mutants_unit("fncache", Some("wire")),
            mutants_unit("fncache", Some("disk_rules")),
            shipped_unit(wire),
            mutants_unit("plain", None),
            mutants_unit("nocconv", Some("wire")),
        ];
        let f = unverified_named_mutants(&t, &units);
        assert_eq!(f.len(), 1, "{f:?}");
        let said = &f[0];
        assert!(said.starts_with("mutants/fncache/disk_rules:"), "{said}");
        assert!(
            said.contains("src/js/runtime/bytecode/fncache/disk_rules.rs"),
            "{said}"
        );
        // Put the shipped file in the list and the same run is clean.
        let units = [
            mutants_unit("fncache", Some("wire")),
            mutants_unit("fncache", Some("disk_rules")),
            shipped_unit(wire),
            shipped_unit("src/js/runtime/bytecode/fncache/disk_rules.rs"),
        ];
        assert!(unverified_named_mutants(&t, &units).is_empty());
        std::fs::remove_dir_all(&t).ok();
    }

    #[test]
    fn a_launcher_test_may_only_name_paths_the_snapshot_holds() {
        let t = std::env::temp_dir().join(format!("lucina-ci-testpaths-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        std::fs::create_dir_all(t.join("crates")).unwrap();
        zz_file(&t, "scripts/tests/zzz-test.sh", "");
        zz_file(
            &t,
            "tools/ci/lucina-ci/tests/zzz_launcher.rs",
            "use std::path::Path;\n#[test]\nfn z() {\n    let s = Path::new(env!(\"CARGO_MANIFEST_DIR\")).join(\"../../../scripts/tests/zzz-test.sh\");\n    let gone = \"scripts/tests/zzz-gone.sh\";\n    assert!(s.exists() || gone.is_empty());\n}\n",
        );
        let rp = t.join("tools/ci/public-snapshot.rules");
        // Included: nothing to say.
        std::fs::write(&rp, "include scripts/\n").unwrap();
        assert!(check_test_paths(&t, &rp).unwrap().is_empty());
        // Left out: refused with that message. The named path that does not exist (zzz-gone.sh) is ignored.
        std::fs::write(&rp, "include scripts/zzz-other.sh\n").unwrap();
        assert_eq!(
            check_test_paths(&t, &rp).unwrap(),
            [
                "tools/ci/lucina-ci/tests/zzz_launcher.rs reads scripts/tests/zzz-test.sh, which tools/ci/public-snapshot.rules leaves out (the public copy runs this test)"
            ]
        );
        // Excluded by an `exclude` line: still an omission the copy runs into, not a declared one.
        std::fs::write(&rp, "include scripts/\nexclude scripts/tests/zzz-test.sh\n").unwrap();
        assert_eq!(check_test_paths(&t, &rp).unwrap().len(), 1);
        // Declared private with a comment marker: the check takes the word and asks no more.
        std::fs::write(
            &rp,
            "# private-only scripts/tests/zzz-test.sh the test carries the name only\ninclude scripts/zzz-other.sh\n",
        )
        .unwrap();
        assert!(check_test_paths(&t, &rp).unwrap().is_empty());
        std::fs::remove_dir_all(&t).ok();
    }

    #[test]
    fn only_the_cfg_test_bodies_of_src_files_count() {
        let t =
            std::env::temp_dir().join(format!("lucina-ci-testpaths-src-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        std::fs::create_dir_all(t.join("crates")).unwrap();
        for f in [
            "scripts/tests/zzz-a.sh",
            "scripts/tests/zzz-b.sh",
            "scripts/tests/zzz-c.sh",
            "scripts/tests/zzz-prod.sh",
        ] {
            zz_file(&t, f, "");
        }
        zz_file(
            &t,
            "tools/ci/lucina-ci/src/zzz.rs",
            r##"pub const SCRIPT: &str = "scripts/tests/zzz-c.sh";
pub fn prod() { let _ = "scripts/tests/zzz-prod.sh"; }
#[cfg(test)]
mod tests {
    fn z() { let _ = super::SCRIPT; let _ = "scripts/tests/zzz-a.sh"; }
}
fn fixture() { let _ = "#[cfg(test)]\nmod fake { let _ = \"scripts/tests/zzz-b.sh\"; }"; }
"##,
        );
        let rp = t.join("tools/ci/public-snapshot.rules");
        std::fs::write(&rp, "include scripts/zzz-other.sh\n").unwrap();
        let g = check_test_paths(&t, &rp).unwrap();
        // The literal of the test mod and the CUTOFF-style constant it uses count; production text and a `#[cfg(test)]`
        // that only lives inside a fixture string do not.
        assert!(
            g.iter()
                .any(|m| m.contains("reads scripts/tests/zzz-a.sh,")),
            "{g:?}"
        );
        assert!(
            g.iter()
                .any(|m| m.contains("reads scripts/tests/zzz-c.sh,")),
            "{g:?}"
        );
        assert_eq!(g.len(), 2, "{g:?}");
        std::fs::remove_dir_all(&t).ok();
    }
}
