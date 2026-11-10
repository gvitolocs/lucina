//! Builds the public copy of the repository: tracked files only, no history, with what must not be published left out and private
//! names replaced, by the rules in `tools/ci/public-snapshot.rules`. Nothing is pushed from here.

use std::fs;
use std::path::Path;
use std::process::Command;

#[derive(Default)]
pub struct Rules {
    pub exclude: Vec<String>,
    pub keep: Vec<String>,
    pub replace: Vec<(String, String)>,
    pub raw: Vec<String>,
    /// The allowlist of paths outside `crates/`.
    pub include: Vec<String>,
    /// (crate, folder): the allowlist of `crates/`, and where each goes.
    pub groups: Vec<(String, String)>,
    /// (from, to): files that move.
    pub moves: Vec<(String, String)>,
}

pub fn parse_rules(text: &str) -> Result<Rules, String> {
    let mut r = Rules::default();
    for line in text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
    {
        let (verb, rest) = line
            .split_once(' ')
            .ok_or_else(|| format!("bad rule: {line}"))?;
        let rest = rest.trim().to_string();
        match verb {
            "exclude" => r.exclude.push(rest),
            "keep" => r.keep.push(rest),
            "raw" => r.raw.push(rest),
            "include" => r.include.push(rest),
            "group" => {
                let mut w = rest.split_whitespace();
                let g = w
                    .next()
                    .ok_or_else(|| format!("bad group: {line}"))?
                    .to_string();
                r.groups.extend(w.map(|c| (c.to_string(), g.clone())));
            }
            "move" => {
                let (a, b) = rest
                    .split_once(" => ")
                    .ok_or_else(|| format!("bad move: {line}"))?;
                r.moves.push((a.to_string(), b.to_string()));
            }
            "replace" => {
                let (a, b) = rest
                    .split_once(" => ")
                    .ok_or_else(|| format!("bad replace: {line}"))?;
                r.replace.push((a.to_string(), b.to_string()));
            }
            _ => return Err(format!("bad rule: {line}")),
        }
    }
    Ok(r)
}

/// `**/x/` is a directory at any depth, `*.ext` a suffix, anything else a prefix of the path.
pub fn matches(path: &str, rule: &str) -> bool {
    if let Some(rest) = rule.strip_prefix("**/") {
        path.starts_with(rest) || path.contains(&format!("/{rest}"))
    } else if let Some(ext) = rule.strip_prefix('*') {
        path.ends_with(ext)
    } else {
        path.starts_with(rule)
    }
}

/// Allowlist: a path is in only if a rule names it. `keep` wins over everything, `exclude` over `include` and `group`.
pub fn included(r: &Rules, path: &str) -> bool {
    if r.keep.iter().any(|k| matches(path, k)) {
        return true;
    }
    if r.exclude.iter().any(|x| matches(path, x)) {
        return false;
    }
    if r.include.is_empty() && r.groups.is_empty() {
        return true;
    }
    if let Some(rest) = path.strip_prefix("crates/") {
        let name = rest.split('/').next().unwrap_or("");
        return r.groups.iter().any(|(c, _)| c == name);
    }
    r.include.iter().any(|i| matches(path, i))
}

/// Where a file goes in the public layout.
pub fn destination(r: &Rules, path: &str) -> String {
    if let Some(rest) = path.strip_prefix("crates/") {
        let name = rest.split('/').next().unwrap_or("");
        if let Some((_, g)) = r.groups.iter().find(|(c, _)| c == name) {
            return format!("{g}/{rest}");
        }
    }
    for (a, b) in &r.moves {
        if let Some(rest) = path.strip_prefix(a.as_str()) {
            return format!("{b}{rest}");
        }
    }
    path.to_string()
}

fn name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_'
}

/// Replaces `from` where it ends at a name boundary, so `crates/lucina-app` never rewrites `crates/lucina-app-chat`.
fn replace_name(text: &str, from: &str, to: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find(from) {
        let after = &rest[i + from.len()..];
        out.push_str(&rest[..i]);
        if after.chars().next().is_some_and(name_char) {
            out.push_str(from);
        } else {
            out.push_str(to);
        }
        rest = after;
    }
    out.push_str(rest);
    out
}

pub fn apply_replacements(r: &Rules, text: &str) -> String {
    let t = r
        .replace
        .iter()
        .fold(text.to_string(), |t, (a, b)| t.replace(a, b));
    // `crates/NAME` from anywhere, `../NAME` between siblings. Both keep the depth of `crates/`.
    r.groups.iter().fold(t, |t, (c, g)| {
        let t = replace_name(&t, &format!("crates/{c}"), &format!("{g}/{c}"));
        replace_name(&t, &format!("../{c}"), &format!("../../{g}/{c}"))
    })
}

/// Copies the tracked files of `root` into `out`. Submodules are listed in `<out>/.snapshot-gitlinks` for the script to register.
pub fn build(root: &Path, rules: &Rules, out: &Path) -> Result<(usize, usize), String> {
    let o = Command::new("git")
        .args(["ls-files", "-s", "-z"])
        .current_dir(root)
        .output()
        .map_err(|e| e.to_string())?;
    if !o.status.success() {
        return Err("git ls-files failed".into());
    }
    let listing = String::from_utf8_lossy(&o.stdout).into_owned();
    let (mut copied, mut left_out) = (0, 0);
    let mut gitlinks = String::new();
    for entry in listing.split('\0').filter(|e| !e.is_empty()) {
        let (meta, path) = entry.split_once('\t').ok_or("bad ls-files line")?;
        let mut f = meta.split(' ');
        let (mode, sha) = (f.next().unwrap_or(""), f.next().unwrap_or(""));
        if !included(rules, path) {
            left_out += 1;
            continue;
        }
        if mode == "160000" {
            gitlinks.push_str(&format!("{sha} {path}\n"));
            continue;
        }
        let (src, dst) = (root.join(path), out.join(destination(rules, path)));
        if let Some(p) = dst.parent() {
            fs::create_dir_all(p).map_err(|e| e.to_string())?;
        }
        if mode == "120000" {
            let target = fs::read_link(&src).map_err(|e| format!("{path}: {e}"))?;
            std::os::unix::fs::symlink(target, &dst).map_err(|e| format!("{path}: {e}"))?;
            copied += 1;
            continue;
        }
        fs::copy(&src, &dst).map_err(|e| format!("{path}: {e}"))?;
        let raw = rules.raw.iter().any(|x| matches(path, x));
        if !raw
            && let Ok(text) = fs::read_to_string(&dst)
            && !text.contains('\0')
        {
            let new = apply_replacements(rules, &text);
            if new != text {
                fs::write(&dst, new).map_err(|e| format!("{path}: {e}"))?;
            }
        }
        copied += 1;
    }
    fs::write(out.join(".snapshot-gitlinks"), gitlinks).map_err(|e| e.to_string())?;
    Ok((copied, left_out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules() -> Rules {
        parse_rules("# c\nexclude evidence/\nexclude **/target/\nexclude *.dtb\nexclude docs/research/\nkeep docs/research/licenses.md\nreplace abc => user\n").unwrap()
    }

    #[test]
    fn rule_shapes() {
        let r = rules();
        assert!(!included(&r, "evidence/x.log"));
        assert!(!included(&r, "third_party/s/target/debug/a.o"));
        assert!(!included(&r, "target/a"));
        assert!(!included(&r, "userspace/boards/a.dtb"));
        assert!(included(&r, "crates/target-x/src/lib.rs"));
        assert!(included(&r, "evidences.md"));
    }

    fn allowlist() -> Rules {
        parse_rules("include scripts/verify\ninclude verification/\ngroup drivers lucina-ufs lucina-app\nexclude verification/secret/\nkeep docs/x.md\nmove README-public.md => README.md\n").unwrap()
    }

    #[test]
    fn an_allowlist_leaves_out_what_it_does_not_name() {
        let r = allowlist();
        assert!(!included(&r, "drivers/none"));
        assert!(included(&r, "crates/lucina-ufs/src/lib.rs"));
        assert!(!included(&r, "crates/lucina-other/src/lib.rs"));
        assert!(included(&r, "scripts/verify.sh"));
        assert!(!included(&r, "scripts/juno-reload.sh"));
        assert!(!included(&r, "verification/secret/a.rs"));
        assert!(included(&r, "docs/x.md"));
    }

    #[test]
    fn crates_move_to_their_folder() {
        let r = allowlist();
        assert_eq!(
            destination(&r, "crates/lucina-ufs/src/lib.rs"),
            "drivers/lucina-ufs/src/lib.rs"
        );
        assert_eq!(destination(&r, "README-public.md"), "README.md");
        assert_eq!(destination(&r, "verification/a.rs"), "verification/a.rs");
    }

    #[test]
    fn references_follow_the_move_at_name_boundaries() {
        let r = allowlist();
        assert_eq!(
            apply_replacements(&r, "path = \"../../crates/lucina-ufs/src\""),
            "path = \"../../drivers/lucina-ufs/src\""
        );
        assert_eq!(
            apply_replacements(&r, "x = { path = \"../lucina-app\" }"),
            "x = { path = \"../../drivers/lucina-app\" }"
        );
        assert_eq!(
            apply_replacements(&r, "../lucina-app-chat crates/lucina-app-chat"),
            "../lucina-app-chat crates/lucina-app-chat"
        );
    }

    #[test]
    fn keep_wins_over_exclude() {
        let r = rules();
        assert!(included(&r, "docs/research/licenses.md"));
        assert!(!included(&r, "docs/research/other.md"));
    }

    #[test]
    fn replacements_apply() {
        assert_eq!(apply_replacements(&rules(), "/home/abc/x"), "/home/user/x");
    }

    #[test]
    fn bad_rules_are_refused() {
        assert!(parse_rules("nonsense\n").is_err());
        assert!(parse_rules("frobnicate x\n").is_err());
    }
}
