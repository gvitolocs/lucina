//! Trust audit (gap B of docs/ci/PROOF-WORKFLOW.md): what each proof unit ASSUMES instead of proving. For every Verus and
//! shipped unit it finds the verified source files (the target and every file it pulls in by `#[path]`), scans them for the
//! markers that make a green proof weaker than it sounds (`assume`, `external_body`, `unsafe`, ...), counts verified against
//! trusted lines, and lists the functions of a shipped file that carry no `//@ requires`/`ensures` at all. The counts are a
//! ratchet: `trust --check` refuses a unit with a nonzero assumption, escape or `unsafe` count that `tools/ci/trust-allow.tsv`
//! does not cover (max_count at least the count, with a reason). The committed `docs/ci/TRUST-REPORT.md` is generated.

use crate::coverage;
use crate::inventory::Unit;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub const REPORT: &str = "docs/ci/TRUST-REPORT.md";
pub const BASELINE: &str = "tools/ci/trust-baseline.tsv";
pub const ALLOW: &str = "tools/ci/trust-allow.tsv";

/// How much a marker weakens a proof. The first three are ratcheted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Class {
    Assumption,
    Escape,
    Unsafe,
    Solver,
    Info,
}

pub const ALL_CLASSES: [Class; 5] = [
    Class::Assumption,
    Class::Escape,
    Class::Unsafe,
    Class::Solver,
    Class::Info,
];
pub const RATCHET_CLASSES: [Class; 3] = [Class::Assumption, Class::Escape, Class::Unsafe];

impl Class {
    pub fn name(self) -> &'static str {
        match self {
            Class::Assumption => "assumption",
            Class::Escape => "escape",
            Class::Unsafe => "unsafe",
            Class::Solver => "solver",
            Class::Info => "info",
        }
    }
    pub fn parse(s: &str) -> Option<Class> {
        ALL_CLASSES.iter().copied().find(|c| c.name() == s)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Marker {
    pub kind: &'static str,
    pub class: Class,
    pub file: String,
    pub line: usize,
}

#[derive(Debug, Clone, Default)]
pub struct FileScan {
    pub path: String,
    /// Non-blank lines that are not comments (a `//@` line counts: it is proof text).
    pub code_lines: usize,
    /// Lines inside `verus! { }` blocks and `#[verus_spec(..)]` attributes, plus `//@` lines.
    pub verified_lines: usize,
    pub markers: Vec<Marker>,
    /// `(line, name)` of functions outside `verus!` blocks with no `//@ requires`/`ensures`.
    pub unspecified: Vec<(usize, String)>,
    /// `(line, text)` of comment lines saying "Not proved".
    pub not_proved: Vec<(usize, String)>,
}

// ---------------------------------------------------------------------------------------------------------------------
// Lexing: comments and string literals are blanked, `//@` lines are code.

#[derive(Default)]
struct Lex {
    block: usize,
    in_str: bool,
    raw: Option<usize>,
}

fn is_ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// One source line with comments and the inside of string and char literals replaced by spaces.
fn lex_line(st: &mut Lex, line: &str) -> String {
    let c: Vec<char> = line.chars().collect();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < c.len() {
        if st.block > 0 {
            if c[i] == '/' && c.get(i + 1) == Some(&'*') {
                st.block += 1;
                i += 2;
                out.push_str("  ");
            } else if c[i] == '*' && c.get(i + 1) == Some(&'/') {
                st.block -= 1;
                i += 2;
                out.push_str("  ");
            } else {
                out.push(' ');
                i += 1;
            }
            continue;
        }
        if let Some(h) = st.raw {
            if c[i] == '"' && (0..h).all(|k| c.get(i + 1 + k) == Some(&'#')) {
                st.raw = None;
                out.push('"');
                out.extend(std::iter::repeat_n(' ', h));
                i += 1 + h;
            } else {
                out.push(' ');
                i += 1;
            }
            continue;
        }
        if st.in_str {
            if c[i] == '\\' {
                out.push(' ');
                i += 1;
                if i < c.len() {
                    out.push(' ');
                    i += 1;
                }
            } else if c[i] == '"' {
                st.in_str = false;
                out.push('"');
                i += 1;
            } else {
                out.push(' ');
                i += 1;
            }
            continue;
        }
        let next = c.get(i + 1).copied();
        match c[i] {
            '/' if next == Some('/') => break,
            '/' if next == Some('*') => {
                st.block = 1;
                i += 2;
                out.push_str("  ");
            }
            '"' => {
                st.in_str = true;
                out.push('"');
                i += 1;
            }
            'r' if {
                let prev = if i > 0 { Some(c[i - 1]) } else { None };
                let prev2 = if i > 1 { Some(c[i - 2]) } else { None };
                let start_ok = match prev {
                    None => true,
                    Some(p) if !is_ident(p) => true,
                    Some('b') => !prev2.is_some_and(is_ident),
                    _ => false,
                };
                let mut j = i + 1;
                while c.get(j) == Some(&'#') {
                    j += 1;
                }
                start_ok && c.get(j) == Some(&'"')
            } =>
            {
                let mut j = i + 1;
                while c.get(j) == Some(&'#') {
                    j += 1;
                }
                st.raw = Some(j - i - 1);
                out.push('r');
                out.extend(std::iter::repeat_n(' ', j - i));
                i = j + 1;
            }
            '\'' => {
                if next == Some('\\') {
                    let mut j = i + 3;
                    while j < c.len() && c[j] != '\'' {
                        j += 1;
                    }
                    let end = j.min(c.len() - 1);
                    out.extend(std::iter::repeat_n(' ', end - i + 1));
                    i = end + 1;
                } else if c.get(i + 2) == Some(&'\'') {
                    out.push_str("   ");
                    i += 3;
                } else {
                    // A lifetime.
                    out.push('\'');
                    i += 1;
                }
            }
            ch => {
                out.push(ch);
                i += 1;
            }
        }
    }
    out
}

/// Positions of `w` as a whole word.
fn word_at(s: &str, w: &str) -> Vec<usize> {
    let b = s.as_bytes();
    let mut v = Vec::new();
    let mut from = 0;
    while let Some(p) = s[from..].find(w) {
        let i = from + p;
        let before = i == 0 || !is_ident(b[i - 1] as char);
        let after = i + w.len() >= b.len() || !is_ident(b[i + w.len()] as char);
        if before && after {
            v.push(i);
        }
        from = i + w.len();
    }
    v
}

/// `name` as a whole word followed by `(` (spaces allowed), and not a definition (`fn name(`).
fn calls(s: &str, name: &str) -> usize {
    word_at(s, name)
        .into_iter()
        .filter(|&i| {
            s[i + name.len()..].trim_start().starts_with('(') && !s[..i].trim_end().ends_with("fn")
        })
        .count()
}

fn count_sub(s: &str, pat: &str) -> usize {
    s.matches(pat).count()
}

/// The function names declared on a cleaned line.
fn fn_names(s: &str) -> Vec<String> {
    word_at(s, "fn")
        .into_iter()
        .filter_map(|i| {
            let rest = &s[i + 2..];
            let t = rest.trim_start();
            if t.len() == rest.len() {
                return None;
            }
            let name: String = t.chars().take_while(|&c| is_ident(c)).collect();
            if name.is_empty() { None } else { Some(name) }
        })
        .collect()
}

fn markers_in(s: &str, ws: &str) -> Vec<(&'static str, Class)> {
    let mut v: Vec<(&'static str, Class)> = Vec::new();
    let mut add = |n: usize, k: &'static str, c: Class| {
        for _ in 0..n {
            v.push((k, c));
        }
    };
    add(calls(s, "assume"), "assume", Class::Assumption);
    add(calls(s, "admit"), "admit", Class::Assumption);
    add(
        word_at(s, "external_body").len(),
        "external_body",
        Class::Escape,
    );
    let ext = ws
        .match_indices("verifier::external")
        .filter(|(i, m)| !ws[i + m.len()..].chars().next().is_some_and(is_ident))
        .count()
        + count_sub(ws, "verifier(external)");
    add(ext, "external", Class::Escape);
    add(
        word_at(s, "external_fn_specification").len(),
        "external_fn_specification",
        Class::Assumption,
    );
    add(
        word_at(s, "assume_specification").len(),
        "assume_specification",
        Class::Assumption,
    );
    add(word_at(s, "uninterp").len(), "uninterp", Class::Assumption);
    add(
        word_at(s, "proof_from_false").len(),
        "proof_from_false",
        Class::Assumption,
    );
    add(
        word_at(s, "exec_allows_no_decreases_clause").len(),
        "exec_allows_no_decreases_clause",
        Class::Escape,
    );
    add(
        count_sub(ws, "by(nonlinear_arith)"),
        "by(nonlinear_arith)",
        Class::Solver,
    );
    add(
        count_sub(ws, "by(bit_vector)"),
        "by(bit_vector)",
        Class::Solver,
    );
    add(word_at(s, "unsafe").len(), "unsafe", Class::Unsafe);
    add(
        count_sub(ws, "cfg(not(verus))"),
        "cfg(not(verus))",
        Class::Escape,
    );
    add(
        count_sub(ws, "verifier::rlimit(") + count_sub(ws, "verifier(rlimit("),
        "rlimit",
        Class::Info,
    );
    if !s.trim_start().starts_with("use ") {
        add(word_at(s, "transmute").len(), "transmute", Class::Unsafe);
    }
    // An axiom: a proof function (or broadcast item) whose name says so, or `broadcast axiom`.
    let axiom_fn = !word_at(s, "proof").is_empty() || !word_at(s, "broadcast").is_empty();
    let named = axiom_fn
        && fn_names(s)
            .iter()
            .any(|n| n.to_ascii_lowercase().contains("axiom"));
    let spelled = s
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .contains("broadcast axiom");
    if named || spelled {
        v.push(("axiom", Class::Assumption));
    }
    v
}

/// Per line: the source with the `//@` marker removed, the lexed (comment- and string-free) text, and whether it was a proof line.
fn lex_all(text: &str) -> (Vec<String>, Vec<String>, Vec<bool>) {
    let mut st = Lex::default();
    let mut srcs = Vec::new();
    let mut cleaned = Vec::new();
    let mut proof = Vec::new();
    for line in text.lines() {
        let t = line.trim_start();
        let at_top = st.block == 0 && st.raw.is_none() && !st.in_str;
        let is_proof = at_top && t.starts_with("//@");
        let src = if is_proof {
            let indent = &line[..line.len() - t.len()];
            format!("{indent}{}", &t[3..])
        } else {
            line.to_string()
        };
        cleaned.push(lex_line(&mut st, &src));
        srcs.push(src);
        proof.push(is_proof);
    }
    (srcs, cleaned, proof)
}

/// A `mod name;` declaration at the top level of a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModDecl {
    pub name: String,
    pub path: Option<String>,
    pub test: bool,
}

/// The file declarations `mod x;` of a source (not inline modules, and not declarations inside one), with the `#[path]` and
/// `#[cfg(test)]` attributes in front of them.
pub fn mod_decls(text: &str) -> Vec<ModDecl> {
    let (srcs, cleaned, proof) = lex_all(text);
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut pend_path: Option<String> = None;
    let mut pend_test = false;
    for (n, l) in cleaned.iter().enumerate() {
        if proof[n] {
            continue;
        }
        let ws: String = l.split_whitespace().collect();
        if ws.contains("#[cfg(test)]") {
            pend_test = true;
        }
        if let Some(i) = l.find("#[path") {
            let after = &srcs[n][i.min(srcs[n].len())..];
            if let Some(a) = after.find('"')
                && let Some(b) = after[a + 1..].find('"')
            {
                pend_path = Some(after[a + 1..a + 1 + b].to_string());
            }
        }
        let mut found = false;
        {
            for i in word_at(l, "mod") {
                // Braces opened on this line before the keyword (`mod a { mod b; }`) make it a nested declaration.
                let opened = l[..i].matches('{').count();
                let closed = l[..i].matches('}').count();
                if depth + opened > closed {
                    continue;
                }
                let rest = l[i + 3..].trim_start();
                let name: String = rest.chars().take_while(|&c| is_ident(c)).collect();
                if !name.is_empty() && rest[name.len()..].trim_start().starts_with(';') {
                    out.push(ModDecl {
                        name,
                        path: pend_path.take(),
                        test: pend_test,
                    });
                    found = true;
                }
            }
        }
        if found
            || (!l.trim().is_empty() && !l.trim_start().starts_with("#[") && !ws.contains("#["))
        {
            pend_path = None;
            pend_test = false;
        }
        for c in l.chars() {
            if c == '{' {
                depth += 1;
            } else if c == '}' {
                depth = depth.saturating_sub(1);
            }
        }
    }
    out
}

/// Scan one Rust source. `path` is only recorded.
pub fn scan_text(path: &str, text: &str) -> FileScan {
    let raw: Vec<&str> = text.lines().collect();
    let (_, cleaned, proof) = lex_all(text);

    // verus! { ... } blocks, brace matched.
    let mut in_block = vec![false; raw.len()];
    let mut depth = 0usize;
    let mut pending = false;
    for (n, l) in cleaned.iter().enumerate() {
        if depth > 0 {
            in_block[n] = true;
        }
        let ch: Vec<char> = l.chars().collect();
        let mut i = 0;
        while i < ch.len() {
            if depth == 0 {
                let rest: String = ch[i..].iter().collect();
                if rest.starts_with("verus!") && (i == 0 || !is_ident(ch[i - 1])) {
                    pending = true;
                    i += 6;
                    continue;
                }
                if pending && ch[i] == '{' {
                    depth = 1;
                    pending = false;
                    in_block[n] = true;
                } else if pending && !ch[i].is_whitespace() {
                    pending = false;
                }
            } else {
                in_block[n] = true;
                if ch[i] == '{' {
                    depth += 1;
                } else if ch[i] == '}' {
                    depth -= 1;
                }
            }
            i += 1;
        }
    }

    // `#[verus_spec(...)]` attributes, paren matched: the proof of a file that is not wrapped in `verus!`.
    let mut in_spec = vec![false; raw.len()];
    let mut pdepth = 0usize;
    for (n, l) in cleaned.iter().enumerate() {
        let ch: Vec<char> = l.chars().collect();
        let mut i = 0;
        if pdepth > 0 {
            in_spec[n] = true;
        }
        while i < ch.len() {
            if pdepth == 0 {
                let rest: String = ch[i..].iter().collect();
                if rest.starts_with("verus_spec(") && (i == 0 || !is_ident(ch[i - 1])) {
                    pdepth = 1;
                    in_spec[n] = true;
                    i += 11;
                    continue;
                }
            } else if ch[i] == '(' {
                pdepth += 1;
            } else if ch[i] == ')' {
                pdepth -= 1;
            }
            i += 1;
        }
    }

    // #[cfg(test)] regions: not part of what ships.
    let mut in_test = vec![false; raw.len()];
    let mut pend_test = false;
    let mut tdepth = 0usize;
    for (n, l) in cleaned.iter().enumerate() {
        if proof[n] {
            continue;
        }
        if tdepth > 0 {
            in_test[n] = true;
        }
        if tdepth == 0
            && l.split_whitespace()
                .collect::<String>()
                .contains("#[cfg(test)]")
        {
            pend_test = true;
            in_test[n] = true;
        }
        for c in l.chars() {
            if tdepth == 0 && pend_test {
                if c == '{' {
                    tdepth = 1;
                    in_test[n] = true;
                } else if c == ';' {
                    pend_test = false;
                }
            } else if tdepth > 0 {
                if c == '{' {
                    tdepth += 1;
                } else if c == '}' {
                    tdepth -= 1;
                    if tdepth == 0 {
                        pend_test = false;
                    }
                }
            }
        }
    }

    let mut scan = FileScan {
        path: path.to_string(),
        ..FileScan::default()
    };
    for (n, l) in cleaned.iter().enumerate() {
        let line = n + 1;
        if !l.trim().is_empty() {
            scan.code_lines += 1;
            if in_block[n] || in_spec[n] || proof[n] {
                scan.verified_lines += 1;
            }
        }
        let ws: String = l.split_whitespace().collect();
        // Test code is not what the proof is about: a test calling a function named `admit` is not an `admit()`.
        if !in_test[n] {
            for (kind, class) in markers_in(l, &ws) {
                scan.markers.push(Marker {
                    kind,
                    class,
                    file: path.to_string(),
                    line,
                });
            }
        }
        let t = raw[n].trim();
        if t.starts_with("//")
            && !t.starts_with("//@")
            && (t.contains("Not proved") || t.contains("not proved"))
        {
            scan.not_proved.push((line, t.to_string()));
        }
        if proof[n] || in_block[n] || in_test[n] {
            continue;
        }
        for name in fn_names(l) {
            if !carries_spec(&raw, &cleaned, &proof, n) {
                scan.unspecified.push((line, name));
            }
        }
    }
    scan
}

/// Does the function declared on line `n` have `//@ requires` or `//@ ensures` between its name and its body?
fn carries_spec(raw: &[&str], cleaned: &[String], proof: &[bool], n: usize) -> bool {
    for j in n..raw.len() {
        let has_proof_text = |s: &str| {
            s.find("//@")
                .is_some_and(|p| s[p..].contains("requires") || s[p..].contains("ensures"))
        };
        if has_proof_text(raw[j]) {
            return true;
        }
        // The body starts at `{`; a signature without one ends with `;` (an array type like `[bool; N]` has one in the middle).
        if !proof[j] && (cleaned[j].contains('{') || cleaned[j].trim_end().ends_with(';')) {
            return false;
        }
    }
    false
}

// ---------------------------------------------------------------------------------------------------------------------
// Units.

#[derive(Debug, Clone)]
pub struct UnitTrust {
    pub unit: String,
    pub kind: String,
    pub target: String,
    pub files: Vec<FileScan>,
}

impl UnitTrust {
    pub fn count(&self, class: Class) -> usize {
        self.files
            .iter()
            .flat_map(|f| &f.markers)
            .filter(|m| m.class == class)
            .count()
    }
    pub fn verified_lines(&self) -> usize {
        self.files.iter().map(|f| f.verified_lines).sum()
    }
    pub fn code_lines(&self) -> usize {
        self.files.iter().map(|f| f.code_lines).sum()
    }
    /// Only a shipped unit has them: its target is plain Rust with the proof in comments.
    pub fn unspecified(&self) -> Vec<(String, usize, String)> {
        if self.kind != "shipped" {
            return Vec::new();
        }
        self.files
            .iter()
            .filter(|f| f.path == self.target)
            .flat_map(|f| {
                f.unspecified
                    .iter()
                    .map(|(l, n)| (f.path.clone(), *l, n.clone()))
            })
            .collect()
    }
}

/// The target file and every file it pulls in by `#[path]` or by a `mod x;` declaration (`x.rs` or `x/mod.rs`; a file that is not
/// a `mod.rs`-like root looks in a folder of its own name), followed through the files it pulls in. `#[cfg(test)] mod x;` is
/// skipped. Needs no Verus.
pub fn included_files(read: &dyn Fn(&str) -> Option<String>, target: &str) -> Vec<String> {
    let mut seen = vec![target.to_string()];
    // (file, is a mod.rs-like root: its `mod x;` look next to it)
    let mut todo = vec![(target.to_string(), true)];
    while let Some((file, root)) = todo.pop() {
        let Some(src) = read(&file) else { continue };
        // A `#[path]` in a comment is not one.
        let live: String = src
            .lines()
            .filter(|l| !l.trim_start().starts_with("//") || l.trim_start().starts_with("//@"))
            .map(|l| l.trim_start().strip_prefix("//@").unwrap_or(l))
            .collect::<Vec<_>>()
            .join("\n");
        let (dir, name) = file.rsplit_once('/').unwrap_or(("", &file));
        let stem = name.strip_suffix(".rs").unwrap_or(name);
        let mut found: Vec<(String, bool)> = Vec::new();
        for rel in coverage::path_attributes(&live) {
            if let Some(p) = coverage::resolve(dir, &rel) {
                found.push((p, true));
            }
        }
        let base = if root || stem == "mod" {
            dir.to_string()
        } else if dir.is_empty() {
            stem.to_string()
        } else {
            format!("{dir}/{stem}")
        };
        for d in mod_decls(&src) {
            if d.test || d.path.is_some() {
                continue;
            }
            let flat = coverage::resolve(&base, &format!("{}.rs", d.name));
            let nested = coverage::resolve(&base, &format!("{}/mod.rs", d.name));
            if let Some(f) = flat.filter(|f| read(f).is_some()) {
                found.push((f, false));
            } else if let Some(f) = nested.filter(|f| read(f).is_some()) {
                found.push((f, true));
            }
        }
        for (p, r) in found {
            if !seen.contains(&p) {
                seen.push(p.clone());
                todo.push((p, r));
            }
        }
    }
    seen
}

pub fn compute(root: &Path, units: &[Unit]) -> Vec<UnitTrust> {
    let mut cache: BTreeMap<String, Option<FileScan>> = BTreeMap::new();
    let read = |f: &str| std::fs::read_to_string(root.join(f)).ok();
    let mut out = Vec::new();
    for u in units
        .iter()
        .filter(|u| matches!(u.kind.as_str(), "verus" | "shipped") && u.target.ends_with(".rs"))
    {
        let mut files = Vec::new();
        for f in included_files(&read, &u.target) {
            let s = cache
                .entry(f.clone())
                .or_insert_with(|| read(&f).map(|t| scan_text(&f, &t)));
            if let Some(s) = s {
                files.push(s.clone());
            }
        }
        out.push(UnitTrust {
            unit: u.name.clone(),
            kind: u.kind.clone(),
            target: u.target.clone(),
            files,
        });
    }
    out.sort_by(|a, b| a.unit.cmp(&b.unit));
    out
}

// ---------------------------------------------------------------------------------------------------------------------
// Ratchet.

pub type Counts = BTreeMap<(String, String), usize>;

/// `(unit, class) -> count`, nonzero only, every class.
pub fn counts(units: &[UnitTrust]) -> Counts {
    let mut m = Counts::new();
    for u in units {
        for c in ALL_CLASSES {
            let n = u.count(c);
            if n > 0 {
                m.insert((u.unit.clone(), c.name().to_string()), n);
            }
        }
    }
    m
}

pub fn baseline_tsv(c: &Counts) -> String {
    let mut s = String::from(
        "# Generated: lucina-ci trust --write. Do not edit. unit<TAB>class<TAB>count (zero is not listed)\n",
    );
    for ((u, cl), n) in c {
        s.push_str(&format!("{u}\t{cl}\t{n}\n"));
    }
    s
}

pub fn parse_baseline(text: &str) -> Result<Counts, String> {
    let mut m = Counts::new();
    for (i, l) in text.lines().enumerate() {
        if l.trim().is_empty() || l.starts_with('#') {
            continue;
        }
        let f: Vec<&str> = l.split('\t').collect();
        if f.len() != 3 || Class::parse(f[1]).is_none() {
            return Err(format!(
                "{BASELINE}:{}: expected unit<TAB>class<TAB>count",
                i + 1
            ));
        }
        let n: usize = f[2]
            .parse()
            .map_err(|_| format!("{BASELINE}:{}: count is not a number", i + 1))?;
        m.insert((f[0].to_string(), f[1].to_string()), n);
    }
    Ok(m)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Allow {
    pub unit: String,
    pub class: String,
    pub max: usize,
    pub reason: String,
}

/// Valid entries and the problems found. An entry without a reason is a problem and permits nothing.
pub fn parse_allow(text: &str) -> (Vec<Allow>, Vec<String>) {
    let mut ok = Vec::new();
    let mut bad = Vec::new();
    for (i, l) in text.lines().enumerate() {
        if l.trim().is_empty() || l.starts_with('#') {
            continue;
        }
        let at = format!("{ALLOW}:{}", i + 1);
        let f: Vec<&str> = l.split('\t').collect();
        if f.len() < 4 || f[3].trim().is_empty() {
            bad.push(format!(
                "{at}: the reason is mandatory (unit<TAB>class<TAB>max_count<TAB>reason)"
            ));
            continue;
        }
        let Some(class) = Class::parse(f[1]).filter(|c| RATCHET_CLASSES.contains(c)) else {
            bad.push(format!("{at}: class must be assumption, escape or unsafe"));
            continue;
        };
        let Ok(max) = f[2].parse::<usize>() else {
            bad.push(format!("{at}: max_count is not a number"));
            continue;
        };
        ok.push(Allow {
            unit: f[0].to_string(),
            class: class.name().to_string(),
            max,
            reason: f[3].trim().to_string(),
        });
    }
    (ok, bad)
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

/// Every nonzero count of assumption, escape or unsafe needs an allow entry (`max_count` at least the count, a reason): the
/// baseline alone no longer suffices. An allow above the actual count is stale (warning only). `allow_problems` are the parse
/// errors of the allow file, and an entry without a reason permits nothing.
pub fn ratchet(actual: &Counts, allow: &[Allow], allow_problems: &[String]) -> Outcome {
    let mut o = Outcome::default();
    o.errors.extend(allow_problems.iter().cloned());
    for ((unit, class), &n) in actual {
        if n == 0 || !RATCHET_CLASSES.iter().any(|c| c.name() == class) {
            continue;
        }
        let allowed = allow
            .iter()
            .filter(|a| &a.unit == unit && &a.class == class)
            .map(|a| a.max)
            .max();
        match allowed {
            None => o.errors.push(format!(
                "{unit}: {n} {class} with no allow entry; remove them, or add a line with a reason to {ALLOW}"
            )),
            Some(m) if m < n => o.errors.push(format!(
                "{unit}: {n} {class} but the allow entry covers {m}; remove them, or raise it with a reason in {ALLOW}"
            )),
            Some(_) => {}
        }
    }
    for a in allow {
        let n = actual
            .get(&(a.unit.clone(), a.class.clone()))
            .copied()
            .unwrap_or(0);
        if a.max > n {
            o.warnings.push(format!(
                "stale allow: {} {} allows {} but there are {n} ({})",
                a.unit, a.class, a.max, a.reason
            ));
        }
    }
    o
}

// ---------------------------------------------------------------------------------------------------------------------
// Report.

struct Item {
    category: &'static str,
    what: String,
    file: String,
    line: usize,
    units: BTreeSet<String>,
}

fn ranked_items(units: &[UnitTrust]) -> Vec<Item> {
    let mut m: BTreeMap<(u8, String, usize, String), Item> = BTreeMap::new();
    let mut put =
        |rank: u8, category: &'static str, what: String, file: &str, line: usize, unit: &str| {
            m.entry((rank, file.to_string(), line, what.clone()))
                .or_insert_with(|| Item {
                    category,
                    what,
                    file: file.to_string(),
                    line,
                    units: BTreeSet::new(),
                })
                .units
                .insert(unit.to_string());
        };
    for u in units {
        for mk in u.files.iter().flat_map(|f| &f.markers) {
            let (rank, cat) = match mk.class {
                Class::Assumption => (0, "assumption"),
                Class::Escape => (1, "escape"),
                Class::Unsafe => (2, "unsafe"),
                Class::Solver => (4, "solver"),
                Class::Info => continue,
            };
            put(rank, cat, mk.kind.to_string(), &mk.file, mk.line, &u.unit);
        }
        for (file, line, name) in u.unspecified() {
            put(3, "unproved fn", format!("fn {name}"), &file, line, &u.unit);
        }
    }
    m.into_values().collect()
}

pub fn render_report(units: &[UnitTrust]) -> String {
    let mut units: Vec<&UnitTrust> = units.iter().collect();
    units.sort_by(|a, b| a.unit.cmp(&b.unit));
    let owned: Vec<UnitTrust> = units.iter().map(|u| (*u).clone()).collect();
    let items = ranked_items(&owned);
    let mut s = String::new();
    s.push_str("# Trust report\n\nGenerated by lucina-ci trust --write. Do not edit.\n\n");
    s.push_str("What each Verus and shipped proof unit assumes instead of proving (gap B of `docs/ci/PROOF-WORKFLOW.md`). It scans the\n");
    s.push_str("target file of each unit and every file it pulls in by `#[path]`. Markers in comments and string literals do not count; `//@` lines do. Verified lines are the lines inside `verus! { }` blocks and `#[verus_spec(..)]` attributes, plus `//@` lines.\n");
    s.push_str("Every nonzero assumption, escape or unsafe count of a unit needs a line with a reason in `tools/ci/trust-allow.tsv`;\n");
    s.push_str("`lucina-ci trust --check` refuses one that has none. `tools/ci/trust-baseline.tsv` records today's counts.\n\n");

    let total = |c: Class| owned.iter().map(|u| u.count(c)).sum::<usize>();
    let distinct = |c: Class| {
        owned
            .iter()
            .flat_map(|u| u.files.iter().flat_map(|f| &f.markers))
            .filter(|m| m.class == c)
            .map(|m| (m.file.clone(), m.line, m.kind))
            .collect::<BTreeSet<_>>()
            .len()
    };
    let verified: usize = owned.iter().map(|u| u.verified_lines()).sum();
    let code: usize = owned.iter().map(|u| u.code_lines()).sum();
    let unspec: usize = owned.iter().map(|u| u.unspecified().len()).sum();
    let files: BTreeSet<&str> = owned
        .iter()
        .flat_map(|u| u.files.iter().map(|f| f.path.as_str()))
        .collect();
    s.push_str("## Totals\n\n");
    s.push_str(&format!(
        "{} units, {} distinct source files, {verified} of {code} code lines verified (summed over units), {unspec} unspecified functions.\n\n",
        owned.len(),
        files.len()
    ));
    s.push_str("| Class | Count over units | Distinct sites |\n| --- | ---: | ---: |\n");
    for c in ALL_CLASSES {
        s.push_str(&format!(
            "| {} | {} | {} |\n",
            c.name(),
            total(c),
            distinct(c)
        ));
    }
    s.push_str("\nClasses: assumption = taken as true (`assume`, `admit`, axioms, `assume_specification`, `external_fn_specification`, `uninterp`, `proof_from_false`); ");
    s.push_str("escape = code the verifier does not check (`external_body`, `external`, `cfg(not(verus))`, `exec_allows_no_decreases_clause`); ");
    s.push_str("unsafe = `unsafe`, `transmute`; solver = `by(nonlinear_arith)`, `by(bit_vector)`; info = `rlimit`.\n\n");

    s.push_str("## Per unit\n\n");
    s.push_str("| Unit | Kind | Files | Verified / code lines | Assumption | Escape | Unsafe | Solver | Info | Unspecified fns |\n");
    s.push_str("| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |\n");
    for u in &owned {
        s.push_str(&format!(
            "| `{}` | {} | {} | {} / {} | {} | {} | {} | {} | {} | {} |\n",
            u.unit,
            u.kind,
            u.files.len(),
            u.verified_lines(),
            u.code_lines(),
            u.count(Class::Assumption),
            u.count(Class::Escape),
            u.count(Class::Unsafe),
            u.count(Class::Solver),
            u.count(Class::Info),
            u.unspecified().len()
        ));
    }

    s.push_str("\n## The items most worth a human's look\n\n");
    s.push_str("Ranked: assumption, escape, unsafe, unproved function, solver. The first ten of each are listed; `lucina-ci trust --json` has all.\n\n");
    for (cat, title) in [
        ("assumption", "Assumptions"),
        ("escape", "Escapes"),
        ("unsafe", "Unsafe"),
        (
            "unproved fn",
            "Unproved functions (shipped files, no `//@ requires`/`ensures`)",
        ),
        ("solver", "Solver tactics"),
    ] {
        let list: Vec<&Item> = items.iter().filter(|i| i.category == cat).collect();
        s.push_str(&format!("### {title} ({})\n\n", list.len()));
        if list.is_empty() {
            s.push_str("None.\n\n");
            continue;
        }
        for i in list.iter().take(10) {
            let first = i.units.iter().next().map(String::as_str).unwrap_or("");
            let more = if i.units.len() > 1 {
                format!(" and {} more", i.units.len() - 1)
            } else {
                String::new()
            };
            s.push_str(&format!(
                "- `{}:{}` {} (unit `{first}`{more})\n",
                i.file, i.line, i.what
            ));
        }
        if list.len() > 10 {
            s.push_str(&format!("- ... {} more\n", list.len() - 10));
        }
        s.push('\n');
    }

    s.push_str("## Comments that say \"not proved\"\n\nRead by a human, not by Verus.\n\n");
    let mut np: BTreeMap<(String, usize), String> = BTreeMap::new();
    for u in &owned {
        for f in &u.files {
            for (l, t) in &f.not_proved {
                np.insert((f.path.clone(), *l), t.clone());
            }
        }
    }
    if np.is_empty() {
        s.push_str("None.\n");
    }
    for ((f, l), t) in &np {
        s.push_str(&format!("- `{f}:{l}` {}\n", t.replace('`', "'")));
    }
    s.push_str("\n## Not covered by this scan\n\n");
    s.push_str("- The TLA+ and Stateright models (`specs/juno`, `specs/lucina-models`) are models of the code, not the code; whether they match it is a manual question.\n");
    s.push_str("- Callers: a `requires` is checked only in verified callers (gap C).\n");
    s.push_str("- The Verus and Z3 trusted core, and rustc: the proof is about the source, not the binary.\n");
    s
}

fn json_str(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

pub fn render_json(units: &[UnitTrust], outcome: Option<&Outcome>) -> String {
    let mut s = String::from("{\"units\":[");
    for (i, u) in units.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&format!(
            "{{\"unit\":{},\"kind\":{},\"verified_lines\":{},\"code_lines\":{},\"counts\":{{",
            json_str(&u.unit),
            json_str(&u.kind),
            u.verified_lines(),
            u.code_lines()
        ));
        s.push_str(
            &ALL_CLASSES
                .iter()
                .map(|c| format!("\"{}\":{}", c.name(), u.count(*c)))
                .collect::<Vec<_>>()
                .join(","),
        );
        s.push_str("},\"markers\":[");
        let ms: Vec<String> = u
            .files
            .iter()
            .flat_map(|f| &f.markers)
            .map(|m| {
                format!(
                    "{{\"kind\":{},\"class\":\"{}\",\"file\":{},\"line\":{}}}",
                    json_str(m.kind),
                    m.class.name(),
                    json_str(&m.file),
                    m.line
                )
            })
            .collect();
        s.push_str(&ms.join(","));
        s.push_str("],\"unspecified\":[");
        let us: Vec<String> = u
            .unspecified()
            .iter()
            .map(|(f, l, n)| {
                format!(
                    "{{\"file\":{},\"line\":{l},\"name\":{}}}",
                    json_str(f),
                    json_str(n)
                )
            })
            .collect();
        s.push_str(&us.join(","));
        s.push_str("]}");
    }
    s.push(']');
    if let Some(o) = outcome {
        let arr = |v: &[String]| v.iter().map(|x| json_str(x)).collect::<Vec<_>>().join(",");
        s.push_str(&format!(
            ",\"errors\":[{}],\"warnings\":[{}]",
            arr(&o.errors),
            arr(&o.warnings)
        ));
    }
    s.push_str("}\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(text: &str) -> Vec<(&'static str, usize)> {
        scan_text("f.rs", text)
            .markers
            .iter()
            .map(|m| (m.kind, m.line))
            .collect()
    }

    #[test]
    fn markers_in_code_count_but_not_in_comments_or_strings() {
        let src = "\
// assume(false) in a comment
/// unsafe in a doc comment
fn a() { let s = \"assume(x) external_body unsafe\"; }
/* block
   admit() unsafe */
fn b() { proof { assume(x > 0); } }
unsafe fn c() {}
let r = r#\"unsafe admit()\"#;
let q = '\"'; let l: &'static str = \"\"; unsafe { }
";
        assert_eq!(kinds(src), [("assume", 6), ("unsafe", 7), ("unsafe", 9)]);
    }

    #[test]
    fn proof_lines_of_shipped_files_count() {
        let src = "\
pub fn f(x: u32) -> u32 //@ret
//@ requires x < 10,
{
    //@ assume(x < 5);
    x
}
// //@ assume(y)
//@ #[verifier::external_body]
//@ proof fn lemma_axiom_thing() {}
";
        assert_eq!(
            kinds(src),
            [("assume", 4), ("external_body", 8), ("axiom", 9)]
        );
    }

    #[test]
    fn each_marker_kind_has_its_class() {
        let src = "\
#[verifier::external_body] fn a() {}
#[verifier::external] fn b() {}
#[verifier(external_body)] fn c() {}
#[verifier::external_fn_specification] fn d() {}
assume_specification[ Vec::len ](v: &Vec<u8>);
proof fn x() { admit(); }
#[verifier::uninterp] spec fn u() -> int;
fn y() { proof_from_false(); }
#[verifier::exec_allows_no_decreases_clause] fn z() {}
assert(x) by(nonlinear_arith) requires y;
assert(x) by (bit_vector);
#[cfg(not(verus))] fn w() {}
#[verifier::rlimit(50)] proof fn r() {}
let v = core::mem::transmute::<u32, i32>(1);
broadcast axiom fn ax() {}
";
        let s = scan_text("f.rs", src);
        let got: Vec<(&str, &str)> = s.markers.iter().map(|m| (m.kind, m.class.name())).collect();
        assert_eq!(
            got,
            [
                ("external_body", "escape"),
                ("external", "escape"),
                ("external_body", "escape"),
                ("external_fn_specification", "assumption"),
                ("assume_specification", "assumption"),
                ("admit", "assumption"),
                ("uninterp", "assumption"),
                ("proof_from_false", "assumption"),
                ("exec_allows_no_decreases_clause", "escape"),
                ("by(nonlinear_arith)", "solver"),
                ("by(bit_vector)", "solver"),
                ("cfg(not(verus))", "escape"),
                ("rlimit", "info"),
                ("transmute", "unsafe"),
                ("axiom", "assumption"),
            ]
        );
    }

    #[test]
    fn verus_blocks_are_counted_by_braces() {
        let src = "\
use vstd::prelude::*;
fn outside() {}

verus! {
spec fn f(x: int) -> int {
    if x > 0 { x } else { 0 }
}
proof fn p() {}
} // verus!

fn after() {}
";
        let s = scan_text("f.rs", src);
        // verus! { .. } is lines 4-9 (6 code lines); 2 code lines before, 1 after.
        assert_eq!(s.code_lines, 9);
        assert_eq!(s.verified_lines, 6);
        // Only functions outside the block are unspecified-candidates.
        let names: Vec<&str> = s.unspecified.iter().map(|(_, n)| n.as_str()).collect();
        assert_eq!(names, ["outside", "after"]);
    }

    #[test]
    fn verus_spec_attributes_count_as_verified() {
        let src = "\
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires steps.len() > 0,
    ensures r < 5,
))]
fn f(steps: &[u8]) -> usize { 0 }
";
        let s = scan_text("f.rs", src);
        assert_eq!((s.code_lines, s.verified_lines), (5, 4));
    }

    #[test]
    fn an_array_type_in_a_signature_does_not_end_it() {
        let src = "\
pub fn slot(active: &[bool; 10], n: usize) -> Option<usize> //@ret
//@ requires n <= 10,
{
    None
}
";
        assert!(scan_text("f.rs", src).unspecified.is_empty());
    }

    #[test]
    fn proof_lines_count_as_verified_and_specs_are_found() {
        let src = "\
// Not proved: the floating-point parts.
pub fn spec_ok(a: u32) -> u32 //@ret
//@ ensures ret == a,
{
    a
}
pub fn bare(a: u32) -> u32 {
    a
}
#[cfg(test)]
mod tests {
    fn helper() {}
}
";
        let s = scan_text("f.rs", src);
        assert_eq!(s.verified_lines, 1);
        let names: Vec<&str> = s.unspecified.iter().map(|(_, n)| n.as_str()).collect();
        assert_eq!(names, ["bare"]);
        assert_eq!(
            s.not_proved,
            [(1, "// Not proved: the floating-point parts.".to_string())]
        );
    }

    #[test]
    fn path_attributes_are_followed_relative_to_the_including_file() {
        let files: BTreeMap<&str, &str> = BTreeMap::from([
            (
                "verification/a/t.rs",
                "#[path = \"../../crates/x/src/r.rs\"]\npub mod r;\n// #[path = \"../ignored.rs\"]\n",
            ),
            (
                "crates/x/src/r.rs",
                "#[path = \"sub/q.rs\"]\nmod q;\n//@ #[path = \"p.rs\"]\n",
            ),
            ("crates/x/src/sub/q.rs", "fn q() {}\n"),
        ]);
        let read = |f: &str| files.get(f).map(|s| s.to_string());
        assert_eq!(
            included_files(&read, "verification/a/t.rs"),
            [
                "verification/a/t.rs",
                "crates/x/src/r.rs",
                "crates/x/src/sub/q.rs",
                "crates/x/src/p.rs"
            ]
        );
    }

    fn c(entries: &[(&str, &str, usize)]) -> Counts {
        entries
            .iter()
            .map(|(u, k, n)| ((u.to_string(), k.to_string()), *n))
            .collect()
    }

    #[test]
    fn a_nonzero_count_without_an_allow_is_refused() {
        let o = ratchet(&c(&[("u", "assumption", 1), ("u", "unsafe", 3)]), &[], &[]);
        assert_eq!(o.errors.len(), 2);
        assert!(o.errors[0].contains("no allow entry"));
        // Solver and info counts are not ratcheted.
        assert!(
            ratchet(&c(&[("u", "solver", 9), ("u", "info", 2)]), &[], &[])
                .errors
                .is_empty()
        );
        // The baseline does not matter any more: only the allow.
        assert!(ratchet(&c(&[("v", "escape", 1)]), &[], &[]).errors.len() == 1);
    }

    #[test]
    fn an_allow_with_a_reason_is_accepted_and_one_without_is_rejected() {
        let actual = c(&[("u", "assumption", 2)]);
        let (allow, bad) = parse_allow("u\tassumption\t2\tthe MMIO base is a hardware fact\n");
        assert!(bad.is_empty());
        let o = ratchet(&actual, &allow, &bad);
        assert!(o.errors.is_empty() && o.warnings.is_empty());

        let (allow, bad) = parse_allow("u\tassumption\t2\t\n");
        assert!(allow.is_empty());
        assert_eq!(bad.len(), 1);
        let o = ratchet(&actual, &allow, &bad);
        assert_eq!(o.errors.len(), 2); // the bad line, and the uncovered count
        let (_, bad) = parse_allow("u\tassumption\t2\n");
        assert_eq!(bad.len(), 1);
        // The allow covers its count only.
        let (allow, bad) = parse_allow("u\tassumption\t2\tfine\n");
        let o = ratchet(&c(&[("u", "assumption", 3)]), &allow, &bad);
        assert_eq!(o.errors.len(), 1);
        assert!(o.errors[0].contains("covers 2"));
    }

    #[test]
    fn an_allow_above_the_actual_count_is_stale() {
        let (allow, bad) = parse_allow("u\tescape\t3\told reason\nw\tunsafe\t1\tgone\n");
        let o = ratchet(&c(&[("u", "escape", 1)]), &allow, &bad);
        assert!(o.errors.is_empty());
        assert_eq!(o.warnings.len(), 2);
        assert!(o.warnings[0].starts_with("stale allow"));
    }

    #[test]
    fn markers_in_test_modules_do_not_count() {
        let src = "\
pub fn admit(x: u32) -> u32 { x }
#[cfg(test)]
mod tests {
    fn t() { assert_eq!(admit(1), 1); unsafe { } }
}
fn real() { admit(2); }
";
        assert_eq!(kinds(src), [("admit", 6)]);
    }

    #[test]
    fn mod_declarations_are_found_with_their_attributes() {
        let src = "\
pub mod admit;
#[cfg(test)]
mod t;
#[path = \"other/x.rs\"]
pub mod x;
#[cfg_attr(verus_keep_ghost, verifier::external)]
pub mod teo;
#[cfg(test)]
mod tests {
    mod inner;
}
// mod commented;
mod inline { mod deep; }
fn f() { let m = \"mod s;\"; }
";
        let d = mod_decls(src);
        let got: Vec<(&str, Option<&str>, bool)> = d
            .iter()
            .map(|d| (d.name.as_str(), d.path.as_deref(), d.test))
            .collect();
        assert_eq!(
            got,
            [
                ("admit", None, false),
                ("t", None, true),
                ("x", Some("other/x.rs"), false),
                ("teo", None, false),
            ]
        );
    }

    #[test]
    fn mod_files_are_followed_and_test_modules_skipped() {
        let files: BTreeMap<&str, &str> = BTreeMap::from([
            (
                "verification/i/rules.rs",
                "#[path = \"../../crates/c/src/lib.rs\"]\npub mod c;\n",
            ),
            (
                "crates/c/src/lib.rs",
                "pub mod a;\npub mod b;\n#[cfg(test)]\nmod t;\n",
            ),
            ("crates/c/src/a.rs", "pub mod deep;\n"),
            ("crates/c/src/a/deep.rs", "fn d() {}\n"),
            ("crates/c/src/b/mod.rs", "pub mod leaf;\n"),
            ("crates/c/src/b/leaf.rs", "fn l() {}\n"),
            ("crates/c/src/t.rs", "fn t() {}\n"),
        ]);
        let read = |f: &str| files.get(f).map(|s| s.to_string());
        let mut got = included_files(&read, "verification/i/rules.rs");
        got.sort();
        assert_eq!(
            got,
            [
                "crates/c/src/a.rs",
                "crates/c/src/a/deep.rs",
                "crates/c/src/b/leaf.rs",
                "crates/c/src/b/mod.rs",
                "crates/c/src/lib.rs",
                "verification/i/rules.rs",
            ]
        );
    }

    #[test]
    fn baseline_round_trips() {
        let m = c(&[("a", "assumption", 2), ("b", "unsafe", 1)]);
        assert_eq!(parse_baseline(&baseline_tsv(&m)).unwrap(), m);
        assert!(parse_baseline("a\tnope\t1\n").is_err());
    }

    fn unit_trust(name: &str, kind: &str, src: &str) -> UnitTrust {
        UnitTrust {
            unit: name.into(),
            kind: kind.into(),
            target: "t.rs".into(),
            files: vec![scan_text("t.rs", src)],
        }
    }

    #[test]
    fn the_report_does_not_depend_on_the_order_of_units() {
        let a = unit_trust(
            "verus/a",
            "verus",
            "verus! {\n#[verifier::external_body]\nfn f() {}\n}\n",
        );
        let b = unit_trust("shipped/b", "shipped", "pub fn g() {}\nunsafe fn h() {}\n");
        let one = render_report(&[a.clone(), b.clone()]);
        let two = render_report(&[b, a]);
        assert_eq!(one, two);
        assert!(one.contains("Generated by lucina-ci trust --write. Do not edit."));
        assert!(one.contains("`t.rs:2` external_body"));
        assert!(one.contains("`t.rs:1` fn g"));
    }
}
