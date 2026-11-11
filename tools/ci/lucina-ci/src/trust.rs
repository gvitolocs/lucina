//! Trust audit (gap B of docs/ci/PROOF-WORKFLOW.md): what each proof unit ASSUMES instead of proving. For every Verus and
//! shipped unit it finds the verified source files (the target and every file it pulls in by `#[path]`), scans them for the
//! markers that make a green proof weaker than it sounds (`assume`, `external_body`, `unsafe`, ...), counts verified against
//! trusted lines, and lists the functions of a shipped file that carry no `//@ requires`/`ensures` at all. The counts are a
//! ratchet: `trust --check` refuses a unit with a nonzero assumption, escape, `unsafe`, loosening-attribute, rlimit or verifier-flag
//! count that `tools/ci/trust-allow.tsv` does not cover (max_count at least the count, with a reason; an rlimit also needs a
//! `max_value` that no value exceeds). The committed `docs/ci/TRUST-REPORT.md` is generated.
//!
//! Attribute table (stage 3b; names checked against the pinned Verus binary, see `docs/ci/TRUST-FLAGS.md`). `verifier::X`,
//! `verifier(X)` and the arguments of `verus_verify(..)`:
//!
//! | Attribute | Class | Why |
//! | --- | --- | --- |
//! | `external_body`, `external`, `exec_allows_no_decreases_clause` | escape | counted above, unchanged |
//! | `external_type_specification`, `external_trait_specification`, `external_trait_extension`, `verus_verify(external)` | escape | declarations the verifier takes from outside |
//! | `external_fn_specification`, `uninterp` | assumption | counted above, unchanged |
//! | `trusted`, `assume_*` | assumption | the item is taken as true |
//! | `nonlinear`, `bit_vector`, `integer_ring` | solver | a different, less transparent decision procedure |
//! | `rlimit(N)`, `--rlimit N` | rlimit | more solver time per function; value recorded, ratcheted |
//! | `loop_isolation(false)`, any `verifier::` name not in this table | loosen | loops see the whole context; unknown means a human decides |
//! | `no_auto_trigger` | info | trigger control, changes completeness not soundness |
//! | `verify`, `spinoff_prover`, `loop_isolation(true)`, `opaque*`, `reveal*`, `inline`, `when_used_as_spec`, `accept_recursive_types`, `reject_recursive_types*`, `memoize`, `truncate`, `ext_equal*`, `auto_ext_equal`, `type_invariant`, `atomic`, `broadcast_forall`, `prophetic`, `reveal_group` | harmless | opt in, performance or sound proof structure |

use crate::coverage;
use crate::inventory::Unit;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub const REPORT: &str = "docs/ci/TRUST-REPORT.md";
pub const BASELINE: &str = "tools/ci/trust-baseline.tsv";
pub const ALLOW: &str = "tools/ci/trust-allow.tsv";

/// How much a marker weakens a proof. All but `Solver` and `Info` are ratcheted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Class {
    Assumption,
    Escape,
    Unsafe,
    /// An attribute that loosens checking and is in no other class.
    Loosen,
    /// `#[verifier::rlimit(N)]` or `--rlimit N`; the value is recorded.
    Rlimit,
    /// A Verus command-line flag that loosens checking (unit `flags/<script>`).
    Flag,
    Solver,
    Info,
}

pub const ALL_CLASSES: [Class; 8] = [
    Class::Assumption,
    Class::Escape,
    Class::Unsafe,
    Class::Loosen,
    Class::Rlimit,
    Class::Flag,
    Class::Solver,
    Class::Info,
];
pub const RATCHET_CLASSES: [Class; 6] = [
    Class::Assumption,
    Class::Escape,
    Class::Unsafe,
    Class::Loosen,
    Class::Rlimit,
    Class::Flag,
];

/// Verus default rlimit is 10; a value of ten times that or more is listed for a human, in thousandths.
pub const RLIMIT_TEN_TIMES_MILLI: u64 = 100_000;

/// An rlimit value in thousandths (`50` is 50000, `2.5` is 2500, `infinity` is `u64::MAX`).
pub fn parse_value(s: &str) -> Option<u64> {
    let s = s.trim();
    if s == "infinity" {
        return Some(u64::MAX);
    }
    let (int, frac) = s.split_once('.').unwrap_or((s, ""));
    if int.is_empty() || !int.bytes().all(|b| b.is_ascii_digit()) || frac.len() > 3 {
        return None;
    }
    if !frac.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let f: u64 = format!("{frac:0<3}").parse().ok()?;
    int.parse::<u64>().ok()?.checked_mul(1000)?.checked_add(f)
}

pub fn fmt_value(v: u64) -> String {
    if v == u64::MAX {
        "infinity".into()
    } else if v.is_multiple_of(1000) {
        (v / 1000).to_string()
    } else {
        format!("{}.{:03}", v / 1000, v % 1000)
            .trim_end_matches('0')
            .to_string()
    }
}

impl Class {
    pub fn name(self) -> &'static str {
        match self {
            Class::Assumption => "assumption",
            Class::Escape => "escape",
            Class::Unsafe => "unsafe",
            Class::Loosen => "loosen",
            Class::Rlimit => "rlimit",
            Class::Flag => "flag",
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
    /// An rlimit's value in thousandths (`u64::MAX` for infinity or a value that is not a literal).
    pub value: Option<u64>,
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

/// Predicates of `#[cfg(..)]`/`cfg_attr` that are no loosening: tests, and the verus forms already counted as escapes or allowed.
fn cfg_benign(pred: &str) -> bool {
    matches!(
        pred,
        "test"
            | "not(test)"
            | "verus"
            | "verus_keep_ghost"
            | "not(verus)"
            | "not(verus_keep_ghost)"
            | "any()"
            | "false"
            | "not(all())"
    )
}

/// How many `#[cfg(..)]`, `#![cfg(..)]`, `#[cfg_attr(..)]`, `#![cfg_attr(..)]` of a whitespace-free line have a predicate that
/// is not benign (a `cfg_attr` predicate is its first argument).
fn cfg_gate_count(ws: &str) -> usize {
    let mut n = 0;
    for pre in ["#[cfg(", "#![cfg(", "#[cfg_attr(", "#![cfg_attr("] {
        let attr = pre.contains("cfg_attr");
        let mut from = 0;
        while let Some(p) = ws[from..].find(pre) {
            let open = from + p + pre.len() - 1;
            if let Some((inner, _)) = balanced(ws, open) {
                let pred = if attr {
                    let mut d = 0i32;
                    let mut cut = inner.len();
                    for (i, c) in inner.char_indices() {
                        match c {
                            '(' | '[' => d += 1,
                            ')' | ']' => d -= 1,
                            ',' if d == 0 => {
                                cut = i;
                                break;
                            }
                            _ => {}
                        }
                    }
                    &inner[..cut]
                } else {
                    inner
                };
                if !cfg_benign(pred) {
                    n += 1;
                }
            }
            from += p + pre.len();
        }
    }
    n
}

/// Does a cleaned line of a `#[cfg(test)]` region hold verified items (a `proof fn`, `spec fn`, `verus!` or a clause keyword)?
fn test_gated_proof(l: &str) -> bool {
    let j = l.split_whitespace().collect::<Vec<_>>().join(" ");
    j.contains("proof fn")
        || j.contains("spec fn")
        || j.contains("verus!")
        || ["requires", "ensures", "decreases", "invariant"]
            .iter()
            .any(|w| !word_at(l, w).is_empty())
}

type Hit = (&'static str, Class, Option<u64>);

fn markers_in(s: &str, ws: &str) -> Vec<Hit> {
    let v: std::cell::RefCell<Vec<Hit>> = std::cell::RefCell::new(Vec::new());
    let add = |n: usize, k: &'static str, c: Class| {
        for _ in 0..n {
            v.borrow_mut().push((k, c, None));
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
    // Gating that switches the verified code or its proof off without saying `verus`: counted like `cfg(not(verus))`.
    for (pat, name) in [
        ("cfg(any())", "cfg(any())"),
        ("cfg(false)", "cfg(false)"),
        ("cfg(not(verus_keep_ghost))", "cfg(not(verus_keep_ghost))"),
        ("cfg(not(all()))", "cfg(not(all()))"),
    ] {
        add(count_sub(ws, pat), name, Class::Escape);
    }
    // Any other cfg gate of a verified file can switch the verified code off (`feature = ..`, `target_os = ..`): a loosening.
    add(cfg_gate_count(ws), "cfg-gate", Class::Loosen);
    add(
        count_sub(ws, "by(integer_ring)"),
        "by(integer_ring)",
        Class::Solver,
    );
    // Other `verifier::` attributes (the table at the top of this file), rlimit included.
    for a in attrs_in(ws) {
        if let Some(h) = classify_attr(&a) {
            v.borrow_mut().push(h);
        }
    }
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
        v.borrow_mut().push(("axiom", Class::Assumption, None));
    }
    v.into_inner()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Via {
    /// `verifier::name`
    Path,
    /// `verifier(name, ..)`
    Paren,
    /// `verus_verify(name, ..)`
    VerusVerify,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Attr {
    name: String,
    /// What is inside the parentheses after the name, if there are any.
    args: Option<String>,
    via: Via,
}

/// The text inside the parenthesis that opens at byte `open` of `s`, and the index after the closing one.
fn balanced(s: &str, open: usize) -> Option<(&str, usize)> {
    let mut depth = 0usize;
    for (i, b) in s.bytes().enumerate().skip(open) {
        match b {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some((&s[open + 1..i], i + 1));
                }
            }
            _ => {}
        }
    }
    None
}

fn split_top(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut depth, mut from) = (0usize, 0usize);
    for (i, b) in s.bytes().enumerate() {
        match b {
            b'(' => depth += 1,
            b')' => depth = depth.saturating_sub(1),
            b',' if depth == 0 => {
                out.push(&s[from..i]);
                from = i + 1;
            }
            _ => {}
        }
    }
    out.push(&s[from..]);
    out.into_iter().filter(|x| !x.is_empty()).collect()
}

/// `name` and its parenthesised arguments at the start of `s`.
fn name_and_args(s: &str) -> Option<(String, Option<String>)> {
    let name: String = s.chars().take_while(|&c| is_ident(c)).collect();
    if name.is_empty() {
        return None;
    }
    let args = if s[name.len()..].starts_with('(') {
        balanced(s, name.len()).map(|(a, _)| a.to_string())
    } else {
        None
    };
    Some((name, args))
}

/// Every `verifier::name`, `verifier(name, ..)` and `verus_verify(name, ..)` attribute on a whitespace-free line.
fn attrs_in(ws: &str) -> Vec<Attr> {
    let mut out = Vec::new();
    let b = ws.as_bytes();
    let mut from = 0;
    while let Some(p) = ws[from..].find("verifier::") {
        let i = from + p;
        from = i + 10;
        if i > 0 && is_ident(b[i - 1] as char) {
            continue;
        }
        if let Some((name, args)) = name_and_args(&ws[i + 10..]) {
            out.push(Attr {
                name,
                args,
                via: Via::Path,
            });
        }
    }
    for (key, via) in [
        ("verifier(", Via::Paren),
        ("verus_verify(", Via::VerusVerify),
    ] {
        let mut from = 0;
        while let Some(p) = ws[from..].find(key) {
            let i = from + p;
            from = i + key.len();
            if i > 0 && is_ident(b[i - 1] as char) {
                continue;
            }
            let Some((inner, _)) = balanced(ws, i + key.len() - 1) else {
                continue;
            };
            for item in split_top(inner) {
                if let Some((name, args)) = name_and_args(item) {
                    out.push(Attr { name, args, via });
                }
            }
        }
    }
    out
}

/// The attribute table. `None` for harmless attributes and for those that the plain-text rules above already count
/// (`external_body`, `external`, `external_fn_specification`, `uninterp`, `exec_allows_no_decreases_clause`).
fn classify_attr(a: &Attr) -> Option<Hit> {
    let n = a.name.as_str();
    let value = || Some(a.args.as_deref().and_then(parse_value).unwrap_or(u64::MAX));
    if a.via == Via::VerusVerify {
        return match n {
            "external" => Some(("verus_verify(external)", Class::Escape, None)),
            "rlimit" => Some(("rlimit", Class::Rlimit, value())),
            _ => None,
        };
    }
    match n {
        "external_body"
        | "external"
        | "external_fn_specification"
        | "uninterp"
        | "exec_allows_no_decreases_clause" => None,
        "external_type_specification" => {
            Some(("verifier::external_type_specification", Class::Escape, None))
        }
        "external_trait_specification" => Some((
            "verifier::external_trait_specification",
            Class::Escape,
            None,
        )),
        "external_trait_extension" => {
            Some(("verifier::external_trait_extension", Class::Escape, None))
        }
        "trusted" => Some(("verifier::trusted", Class::Assumption, None)),
        _ if n.starts_with("assume_") => Some(("verifier::assume_*", Class::Assumption, None)),
        "nonlinear" => Some(("verifier::nonlinear", Class::Solver, None)),
        "bit_vector" => Some(("verifier::bit_vector", Class::Solver, None)),
        "integer_ring" => Some(("verifier::integer_ring", Class::Solver, None)),
        "rlimit" => Some(("rlimit", Class::Rlimit, value())),
        "no_auto_trigger" => Some(("verifier::no_auto_trigger", Class::Info, None)),
        "loop_isolation" => match a.args.as_deref() {
            Some("true") => None,
            Some("false") => Some(("verifier::loop_isolation(false)", Class::Loosen, None)),
            _ => Some(("verifier::loop_isolation(?)", Class::Loosen, None)),
        },
        "verify"
        | "spinoff_prover"
        | "inline"
        | "when_used_as_spec"
        | "accept_recursive_types"
        | "reject_recursive_types"
        | "reject_recursive_types_in_ground_variants"
        | "memoize"
        | "truncate"
        | "ext_equal"
        | "ext_equal_deep"
        | "auto_ext_equal"
        | "type_invariant"
        | "atomic"
        | "broadcast_forall"
        | "prophetic"
        | "reveal_group" => None,
        _ if n.starts_with("opaque") || n.starts_with("reveal") => None,
        _ => Some(("verifier::<unknown>", Class::Loosen, None)),
    }
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
    // An inner `#![cfg(test)]` among the file's LEADING inner attributes (before the first item) gates the whole file; one inside a
    // module, further down, gates only that module and does not blind the scan.
    let mut leading_cfg_test = false;
    for (n, l) in cleaned.iter().enumerate() {
        let w: String = l.split_whitespace().collect();
        if w.is_empty() || proof[n] {
            continue;
        }
        if !w.starts_with("#![") {
            break;
        }
        if w.contains("#![cfg(test)]") {
            leading_cfg_test = true;
        }
    }
    if leading_cfg_test {
        for x in in_test.iter_mut() {
            *x = true;
        }
    }
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
            for (kind, class, value) in markers_in(l, &ws) {
                scan.markers.push(Marker {
                    kind,
                    class,
                    file: path.to_string(),
                    line,
                    value,
                });
            }
        }
        // Verified items inside a `#[cfg(test)]` region are not verified at all: with a test-only gate on an inline mod or impl one exec
        // line switches proofs off. Counted as an escape.
        if in_test[n] && !proof[n] && test_gated_proof(l) {
            scan.markers.push(Marker {
                kind: "cfg(test)-gated-proof",
                class: Class::Escape,
                file: path.to_string(),
                line,
                value: None,
            });
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
// Verifier flags in the scripts and the runner that invoke Verus.

/// Files outside `scripts/verify*.sh` and `verification/*/mutants.sh` that run Verus or pass it arguments.
pub const FLAG_FILES: [&str; 5] = [
    "scripts/verus-one.sh",
    "tools/ci/lucina-ci/src/runner.rs",
    ".github/workflows/verus.yml",
    "tools/ci/install-verus.sh",
    "tools/ci/proof-inventory.tsv",
];
pub const FLAG_UNIT_PREFIX: &str = "flags/";

/// What a Verus command-line flag does to checking. `None` for a token that is not a Verus flag we know.
fn flag_effect(tok: &str, next: Option<&str>) -> Option<Hit> {
    let hit = |k: &'static str, c: Class| Some((k, c, None));
    match tok {
        "--no-verify" => hit("--no-verify", Class::Flag),
        "--no-lifetime" => hit("--no-lifetime", Class::Flag),
        "--no-trait-conflicts" => hit("--no-trait-conflicts", Class::Flag),
        "--no-erasure-check" => hit("--no-erasure-check", Class::Flag),
        "--no-auto-recommends-check" => hit("--no-auto-recommends-check", Class::Flag),
        "--no-vstd" => hit("--no-vstd", Class::Flag),
        "--smt-option" => hit("--smt-option", Class::Flag),
        "--verify-root" | "--verify-module" | "--verify-only-module" | "--verify-function" => {
            hit("--verify-<scope>", Class::Flag)
        }
        t if t.starts_with("--disable-") => hit("--disable-*", Class::Flag),
        "--rlimit" => Some((
            "--rlimit",
            Class::Rlimit,
            Some(next.and_then(parse_value).unwrap_or(u64::MAX)),
        )),
        "-V" => match next.unwrap_or("") {
            "ignore-unexpected-smt" => hit("-V ignore-unexpected-smt", Class::Flag),
            "no-solver-version-check" => hit("-V no-solver-version-check", Class::Flag),
            "allow-inline-air" => hit("-V allow-inline-air", Class::Flag),
            "cvc5" | "cvc5-smtlib" => hit("-V cvc5", Class::Flag),
            "no-bv-simplify" => hit("-V no-bv-simplify", Class::Flag),
            _ => hit("-V (other)", Class::Info),
        },
        "--no-cheating" => hit("--no-cheating (tightens)", Class::Info),
        "--cfg" => hit("--cfg", Class::Info),
        "--features" => hit("--features", Class::Info),
        "--expand-errors" => hit("--expand-errors", Class::Info),
        "--triggers" | "--triggers-mode" => hit("--triggers", Class::Info),
        "--multiple-errors" => hit("--multiple-errors", Class::Info),
        "--num-threads" => hit("--num-threads", Class::Info),
        "--compile" => hit("--compile", Class::Info),
        _ => None,
    }
}

/// The Verus flags in a script, workflow or Rust source. Only logical lines (continuations joined) that mention `verus` are
/// read; `#` and `//` comment lines are skipped. A flag on a line that does not mention Verus, such as an argument array
/// spread over several lines, is not seen (stated in `docs/ci/TRUST-FLAGS.md`). With `record_only` (mutant scripts, which run
/// Verus on planted-bug copies) every hit is class info: recorded, not ratcheted.
pub fn scan_flags(path: &str, text: &str, record_only: bool) -> FileScan {
    let mut scan = FileScan {
        path: path.to_string(),
        ..FileScan::default()
    };
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let start = i;
        let mut logical = lines[i].to_string();
        while logical.trim_end().ends_with('\\') && i + 1 < lines.len() {
            logical = logical.trim_end().trim_end_matches('\\').to_string();
            i += 1;
            logical.push(' ');
            logical.push_str(lines[i]);
        }
        i += 1;
        let t = logical.trim_start();
        if !t.is_empty() {
            scan.code_lines += usize::from(!t.starts_with('#') && !t.starts_with("//"));
        }
        if t.starts_with('#') || t.starts_with("//") || !t.to_ascii_lowercase().contains("verus") {
            continue;
        }
        let toks: Vec<&str> = t
            .split(|c: char| c.is_whitespace() || "\"'(),;[]{}".contains(c))
            .filter(|x| !x.is_empty())
            .collect();
        for (k, tok) in toks.iter().enumerate() {
            let (name, inline) = match tok.split_once('=') {
                Some((a, b)) if a.starts_with('-') => (a, Some(b)),
                _ => (*tok, None),
            };
            let next = inline.or_else(|| toks.get(k + 1).copied());
            if let Some((kind, class, value)) = flag_effect(name, next) {
                scan.markers.push(Marker {
                    kind,
                    class: if record_only { Class::Info } else { class },
                    file: path.to_string(),
                    line: start + 1,
                    value,
                });
            }
        }
    }
    scan
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
    /// The largest rlimit value (thousandths) of the unit, if it has any.
    pub fn rlimit_max(&self) -> Option<u64> {
        self.files
            .iter()
            .flat_map(|f| &f.markers)
            .filter(|m| m.class == Class::Rlimit)
            .map(|m| m.value.unwrap_or(u64::MAX))
            .max()
    }
    pub fn is_flags(&self) -> bool {
        self.kind == "flags"
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
    out.extend(flag_units(root));
    out.sort_by(|a, b| a.unit.cmp(&b.unit));
    out
}

/// The files that invoke Verus, as `(path, record_only)`, sorted.
fn flag_paths(root: &Path) -> Vec<(String, bool)> {
    let mut v: Vec<(String, bool)> = FLAG_FILES.iter().map(|f| (f.to_string(), false)).collect();
    if let Ok(rd) = std::fs::read_dir(root.join("scripts")) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if n.starts_with("verify") && n.ends_with(".sh") {
                v.push((format!("scripts/{n}"), false));
            }
        }
    }
    if let Ok(rd) = std::fs::read_dir(root.join("verification")) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            let rel = format!("verification/{n}/mutants.sh");
            if root.join(&rel).is_file() {
                v.push((rel, true));
            }
        }
    }
    v.sort();
    v.dedup();
    v
}

/// One unit `flags/<path>` per file that invokes Verus (kind `flags`), with its flags as markers.
fn flag_units(root: &Path) -> Vec<UnitTrust> {
    flag_paths(root)
        .into_iter()
        .filter_map(|(p, record)| {
            let text = std::fs::read_to_string(root.join(&p)).ok()?;
            Some(UnitTrust {
                unit: format!("{FLAG_UNIT_PREFIX}{p}"),
                kind: "flags".into(),
                target: p.clone(),
                files: vec![scan_flags(&p, &text, record)],
            })
        })
        .collect()
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

/// One verified file of a unit at the base commit and now (`None`: absent there).
#[derive(Debug, Clone)]
pub struct FileDelta {
    pub path: String,
    pub base: Option<String>,
    pub work: Option<String>,
}

/// The lines of `base` that `work` has fewer times (multiset difference), in base order, as one text. A line that merely moved
/// within the file is not removed.
fn lines_removed(base: &str, work: &str) -> String {
    let mut left: BTreeMap<&str, usize> = BTreeMap::new();
    for l in work.lines() {
        *left.entry(l).or_insert(0) += 1;
    }
    let mut out = String::new();
    for l in base.lines() {
        match left.get_mut(l) {
            Some(n) if *n > 0 => *n -= 1,
            _ => {
                out.push_str(l);
                out.push('\n');
            }
        }
    }
    out
}

/// Markers of `class` in the lines of `text`.
fn markers_of(path: &str, text: &str, class: Class) -> usize {
    scan_text(path, text)
        .markers
        .iter()
        .filter(|m| m.class == class)
        .count()
}

/// The NET loss of markers of `class` over the files of a unit: occurrences on removed lines minus occurrences on added lines
/// (multiset line differences per file, summed over the unit, floored at 0). Editing a marker line trivially (a space, a trailing
/// comment) removes the old line and adds the new one: net zero. A marker moved to another file of the unit nets zero as well.
pub fn net_marker_loss(deltas: &[FileDelta], class: Class) -> usize {
    let (mut removed, mut added) = (0usize, 0usize);
    for d in deltas {
        let base = d.base.as_deref().unwrap_or("");
        let work = d.work.as_deref().unwrap_or("");
        removed += markers_of(&d.path, &lines_removed(base, work), class);
        added += markers_of(&d.path, &lines_removed(work, base), class);
    }
    removed.saturating_sub(added)
}

/// The silent-loss ratchet: a count that FELL against the base baseline (per unit and class) is accepted only when the verified
/// files of that unit lost at least that many marker sites as text, net of the sites added on other lines. A scan that stopped seeing sites whose lines are all still
/// in the source (a file the closure no longer reaches, a parse change, a bad checkout) is refused, with the numbers. `units`
/// maps each unit to its files at the base and now; units not in it (the flags units) are not checked.
pub fn silent_drops(
    base: &Counts,
    actual: &Counts,
    units: &BTreeMap<String, Vec<FileDelta>>,
) -> Vec<String> {
    let mut errors = Vec::new();
    for ((unit, class), &b) in base {
        let Some(deltas) = units.get(unit) else {
            continue;
        };
        let Some(c) = Class::parse(class) else {
            continue;
        };
        let a = actual
            .get(&(unit.clone(), class.clone()))
            .copied()
            .unwrap_or(0);
        if a >= b {
            continue;
        }
        let gone = net_marker_loss(deltas, c);
        if gone < b - a {
            errors.push(format!(
                "trust: unit `{unit}` class `{class}` fell from {b} site(s) at the base to {a}, but the unit's verified files ({}) lost only {gone} marker site(s) net of the markers added back: the scan lost sites whose lines are still in the source (a file the closure no longer reaches, an extractor change, a broken checkout). Not accepted; if the sites really went away, remove their lines",
                deltas
                    .iter()
                    .map(|d| d.path.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    errors
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
    /// The optional fifth column, for class `rlimit`: no value may exceed it (thousandths).
    pub max_value: Option<u64>,
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
            bad.push(format!(
                "{at}: class must be assumption, escape, unsafe, loosen, rlimit or flag"
            ));
            continue;
        };
        let Ok(max) = f[2].parse::<usize>() else {
            bad.push(format!("{at}: max_count is not a number"));
            continue;
        };
        let mut max_value = None;
        if let Some(v) = f.get(4).map(|v| v.trim()).filter(|v| !v.is_empty()) {
            match parse_value(v) {
                Some(x) => max_value = Some(x),
                None => {
                    bad.push(format!(
                        "{at}: max_value must be a number (for example 50 or 2.5) or infinity"
                    ));
                    continue;
                }
            }
        }
        ok.push(Allow {
            unit: f[0].to_string(),
            class: class.name().to_string(),
            max,
            reason: f[3].trim().to_string(),
            max_value,
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
#[cfg_attr(not(test), allow(dead_code))]
pub fn ratchet(actual: &Counts, allow: &[Allow], allow_problems: &[String]) -> Outcome {
    ratchet_values(actual, &BTreeMap::new(), allow, allow_problems)
}

/// `ratchet`, and for class `rlimit` also the values: `max_rlimit` is the largest rlimit of each unit (thousandths). A unit with an
/// rlimit needs an allow entry with a `max_value`, and no value may exceed the largest `max_value` of its entries. Raising an
/// existing rlimit therefore needs the allow line to change, with a reason.
pub fn ratchet_values(
    actual: &Counts,
    max_rlimit: &BTreeMap<String, u64>,
    allow: &[Allow],
    allow_problems: &[String],
) -> Outcome {
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
        if class.as_str() == Class::Rlimit.name()
            && let Some(&top) = max_rlimit.get(unit)
        {
            let cap = allow
                .iter()
                .filter(|a| &a.unit == unit && &a.class == class)
                .filter_map(|a| a.max_value)
                .max();
            match cap {
                None => o.errors.push(format!(
                    "{unit}: {n} rlimit but no allow entry has a max_value (fifth column) for it; write the largest value ({}) with a reason in {ALLOW}",
                    fmt_value(top)
                )),
                Some(c) if top > c => o.errors.push(format!(
                    "{unit}: rlimit {} but the allow entry covers {}; lower it, or raise max_value with a reason in {ALLOW}",
                    fmt_value(top),
                    fmt_value(c)
                )),
                Some(_) => {}
            }
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
        if u.is_flags() {
            continue;
        }
        for mk in u.files.iter().flat_map(|f| &f.markers) {
            let (rank, cat) = match mk.class {
                Class::Assumption => (0, "assumption"),
                Class::Escape => (1, "escape"),
                Class::Unsafe => (2, "unsafe"),
                Class::Loosen => (5, "loosen"),
                Class::Rlimit => (6, "rlimit"),
                Class::Solver => (4, "solver"),
                Class::Flag | Class::Info => continue,
            };
            let what = match mk.value {
                Some(v) if mk.class == Class::Rlimit => {
                    format!("{} {}", mk.kind, fmt_value(v))
                }
                _ => mk.kind.to_string(),
            };
            put(rank, cat, what, &mk.file, mk.line, &u.unit);
        }
        for (file, line, name) in u.unspecified() {
            put(3, "unproved fn", format!("fn {name}"), &file, line, &u.unit);
        }
    }
    m.into_values().collect()
}

/// Hand-maintained open trust gaps: `docs/ci/open-trust-gaps.md`. Each gap starts with a `### ` heading; the count goes into the totals.
pub const GAPS: &str = "docs/ci/open-trust-gaps.md";

pub fn count_gaps(text: &str) -> usize {
    text.lines().filter(|l| l.starts_with("### ")).count()
}

#[cfg(test)]
pub fn render_report(units: &[UnitTrust]) -> String {
    render_report_gaps(units, "")
}

pub fn render_report_gaps(units: &[UnitTrust], gaps: &str) -> String {
    let mut units: Vec<&UnitTrust> = units.iter().collect();
    units.sort_by(|a, b| a.unit.cmp(&b.unit));
    let all: Vec<UnitTrust> = units.iter().map(|u| (*u).clone()).collect();
    let owned: Vec<UnitTrust> = all.iter().filter(|u| !u.is_flags()).cloned().collect();
    let flag_units: Vec<&UnitTrust> = all.iter().filter(|u| u.is_flags()).collect();
    let items = ranked_items(&owned);
    let mut s = String::new();
    s.push_str("# Trust report\n\nGenerated by lucina-ci trust --write. Do not edit.\n\n");
    s.push_str("What each Verus and shipped proof unit assumes instead of proving (gap B of `docs/ci/PROOF-WORKFLOW.md`). It scans the\n");
    s.push_str("target file of each unit and every file it pulls in by `#[path]`. Markers in comments and string literals do not count; `//@` lines do. Verified lines are the lines inside `verus! { }` blocks and `#[verus_spec(..)]` attributes, plus `//@` lines.\n");
    s.push_str("Every nonzero assumption, escape, unsafe, loosen, rlimit or flag count of a unit needs a line with a reason in `tools/ci/trust-allow.tsv` (an rlimit also a `max_value`);\n");
    s.push_str("`lucina-ci trust --check` refuses one that has none. `tools/ci/trust-baseline.tsv` records today's counts.\n\n");

    let total = |c: Class| all.iter().map(|u| u.count(c)).sum::<usize>();
    let distinct = |c: Class| {
        all.iter()
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
    s.push_str(&format!("open trust gaps: {}\n\n", count_gaps(gaps)));
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
    s.push_str("unsafe = `unsafe`, `transmute`; loosen = a `verifier::` attribute that loosens checking and fits no other class (`loop_isolation(false)`, an attribute the table in `trust.rs` does not know); rlimit = `#[verifier::rlimit(N)]` and `--rlimit N`, value recorded; flag = a Verus command-line flag that loosens checking (unit `flags/<script>`); solver = `by(nonlinear_arith)`, `by(bit_vector)`, `by(integer_ring)`, `verifier::nonlinear`, `verifier::bit_vector`, `verifier::integer_ring`; info = recorded, not ratcheted (`no_auto_trigger`, neutral flags, flags in mutant scripts). The attribute table and the flag table are in `docs/ci/TRUST-FLAGS.md`.\n\n");

    s.push_str("## Per unit\n\n");
    s.push_str("| Unit | Kind | Files | Verified / code lines | Assumption | Escape | Unsafe | Loosen | Rlimit | Solver | Info | Unspecified fns |\n");
    s.push_str(
        "| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |\n",
    );
    for u in &owned {
        s.push_str(&format!(
            "| `{}` | {} | {} | {} / {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            u.unit,
            u.kind,
            u.files.len(),
            u.verified_lines(),
            u.code_lines(),
            u.count(Class::Assumption),
            u.count(Class::Escape),
            u.count(Class::Unsafe),
            u.count(Class::Loosen),
            u.count(Class::Rlimit),
            u.count(Class::Solver),
            u.count(Class::Info),
            u.unspecified().len()
        ));
    }

    s.push_str("\n## The items most worth a human's look\n\n");
    s.push_str("Ranked: assumption, escape, unsafe, unproved function, solver, loosening attribute, rlimit. The first ten of each are listed; `lucina-ci trust --json` has all.\n\n");
    for (cat, title) in [
        ("assumption", "Assumptions"),
        ("escape", "Escapes"),
        ("unsafe", "Unsafe"),
        (
            "unproved fn",
            "Unproved functions (shipped files, no `//@ requires`/`ensures`)",
        ),
        ("solver", "Solver tactics"),
        ("loosen", "Loosening attributes"),
        (
            "rlimit",
            "Raised rlimits (value in Verus's units; the default is 10)",
        ),
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

    let mut big_set: BTreeSet<(String, usize, String)> = BTreeSet::new();
    for m in all
        .iter()
        .flat_map(|u| u.files.iter().flat_map(|f| &f.markers))
    {
        if m.class == Class::Rlimit && m.value.unwrap_or(u64::MAX) >= RLIMIT_TEN_TIMES_MILLI {
            let v = fmt_value(m.value.unwrap_or(u64::MAX));
            big_set.insert((m.file.clone(), m.line, format!("{} {v}", m.kind)));
        }
    }
    let big: Vec<(String, usize, String)> = big_set.into_iter().collect();
    s.push_str(&format!(
        "### Needs ten times the default rlimit or more (value >= {}) ({})\n\n",
        fmt_value(RLIMIT_TEN_TIMES_MILLI),
        big.len()
    ));
    if big.is_empty() {
        s.push_str("None.\n\n");
    }
    for (file, line, what) in &big {
        s.push_str(&format!("- `{file}:{line}` {what}\n"));
    }
    if !big.is_empty() {
        s.push('\n');
    }

    s.push_str("## Verifier flags\n\n");
    s.push_str("Command-line flags in the files that invoke Verus (`flags/<path>` units; hits are read from logical lines that mention `verus`). ");
    s.push_str("A loosening flag is class `flag` and `--rlimit N` is class `rlimit`, both ratcheted per file through `tools/ci/trust-allow.tsv`; neutral flags and every flag in a mutant script (`verification/*/mutants.sh`, which runs Verus on planted-bug copies) are recorded as info.\n\n");
    s.push_str(
        "| File | Loosening flags | Rlimit | Recorded only |\n| --- | ---: | ---: | ---: |\n",
    );
    let (mutant_units, other_units): (Vec<&UnitTrust>, Vec<&UnitTrust>) = flag_units
        .iter()
        .partition(|u| u.target.starts_with("verification/") && u.target.ends_with("/mutants.sh"));
    for u in other_units.iter().chain(
        mutant_units
            .iter()
            .filter(|u| u.files.iter().any(|f| !f.markers.is_empty())),
    ) {
        s.push_str(&format!(
            "| `{}` | {} | {} | {} |\n",
            u.target,
            u.count(Class::Flag),
            u.count(Class::Rlimit),
            u.count(Class::Info)
        ));
    }
    s.push_str(&format!(
        "\n{} mutant scripts (`verification/*/mutants.sh`) are scanned too and listed above only when they pass a flag.\n\n",
        mutant_units.len()
    ));
    let mut any = false;
    for u in &flag_units {
        for m in u.files.iter().flat_map(|f| &f.markers) {
            any = true;
            let v = m
                .value
                .map(|v| format!(" {}", fmt_value(v)))
                .unwrap_or_default();
            s.push_str(&format!(
                "- `{}:{}` `{}{v}` ({})\n",
                m.file,
                m.line,
                m.kind,
                m.class.name()
            ));
        }
    }
    if !any {
        s.push_str("No flag is used today.\n");
    }
    s.push('\n');

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
    s.push_str("\n## Open trust gaps\n\n");
    if gaps.trim().is_empty() {
        s.push_str("None recorded (`docs/ci/open-trust-gaps.md`).\n");
    } else {
        s.push_str(
            "Hand-maintained in `docs/ci/open-trust-gaps.md`; the count is in the totals.\n\n",
        );
        s.push_str(gaps.trim_end());
        s.push('\n');
    }
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
                let value = m
                    .value
                    .map(|v| format!(",\"value\":{}", json_str(&fmt_value(v))))
                    .unwrap_or_default();
                format!(
                    "{{\"kind\":{},\"class\":\"{}\",\"file\":{},\"line\":{}{value}}}",
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
                ("rlimit", "rlimit"),
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

    // --- stage 3b: loosening attributes, rlimit values, verifier flags -----------------------------------------------

    fn class_of(src: &str) -> Vec<(&'static str, &'static str)> {
        scan_text("f.rs", src)
            .markers
            .iter()
            .map(|m| (m.kind, m.class.name()))
            .collect()
    }

    #[test]
    fn each_new_attribute_class_is_detected() {
        let src = "\
#![verifier::loop_isolation(false)]
#[verifier::external_type_specification] struct A;
#[verifier::external_trait_specification] trait B {}
#[verifier::external_trait_extension(CE via CS)] trait C {}
#[verifier::trusted] fn d() {}
#[verifier::assume_termination] fn e() {}
#[verifier::nonlinear] proof fn f() {}
#[verifier::bit_vector] proof fn g() {}
#[verifier(integer_ring)] proof fn h() {}
#[verifier::no_auto_trigger] spec fn i() {}
#[verifier::some_future_thing] fn j() {}
#[verus_verify(external)] fn k() {}
assert(x) by(integer_ring);
";
        assert_eq!(
            class_of(src),
            [
                ("verifier::loop_isolation(false)", "loosen"),
                ("verifier::external_type_specification", "escape"),
                ("verifier::external_trait_specification", "escape"),
                ("verifier::external_trait_extension", "escape"),
                ("verifier::trusted", "assumption"),
                ("verifier::assume_*", "assumption"),
                ("verifier::nonlinear", "solver"),
                ("verifier::bit_vector", "solver"),
                ("verifier::integer_ring", "solver"),
                ("verifier::no_auto_trigger", "info"),
                ("verifier::<unknown>", "loosen"),
                ("verus_verify(external)", "escape"),
                ("by(integer_ring)", "solver"),
            ]
        );
    }

    #[test]
    fn harmless_attributes_are_not_flagged() {
        let src = "\
#[verifier::verify] fn a() {}
#[verifier::spinoff_prover] proof fn b() {}
#![verifier::loop_isolation(true)]
#[verifier::opaque] spec fn c() -> bool;
#[verifier::opaque_outside_module] spec fn d() -> bool;
#[verifier::inline] spec fn e() -> bool { true }
#[verifier::when_used_as_spec(f)] fn g() {}
#[verifier::accept_recursive_types(T)] struct H<T>(T);
#[verifier::reject_recursive_types(T)] struct I<T>(T);
#[verifier::memoize] spec fn j() -> int { 1 }
#[verifier::reveal_group] broadcast group k {}
#[verus_verify(dual_spec)] fn l() {}
#[verifier(spinoff_prover, opaque)] proof fn m() {}
#![allow(dead_code)]
#[allow(unused)] fn n() {}
";
        assert_eq!(class_of(src), []);
    }

    #[test]
    fn attribute_forms_are_all_read() {
        // Several attributes in one `verifier(..)`, a nested argument list, and a cfg_attr wrapper.
        let src = "\
#[verifier(spinoff_prover, rlimit(40), loop_isolation(false))] proof fn a() {}
#[cfg_attr(verus_keep_ghost, verifier::nonlinear)] proof fn b() {}
#[verus_verify(rlimit(120))] fn c() {}
";
        assert_eq!(
            class_of(src),
            [
                ("rlimit", "rlimit"),
                ("verifier::loop_isolation(false)", "loosen"),
                ("verifier::nonlinear", "solver"),
                ("rlimit", "rlimit"),
            ]
        );
    }

    #[test]
    fn rlimit_values_are_recorded() {
        let src = "\
#[verifier::rlimit(50)] proof fn a() {}
#[verifier(rlimit(2.5))] proof fn b() {}
#[verifier::rlimit(infinity)] proof fn c() {}
#[verifier::rlimit(N)] proof fn d() {}
";
        let v: Vec<Option<u64>> = scan_text("f.rs", src)
            .markers
            .iter()
            .map(|m| m.value)
            .collect();
        // A value that is not a literal counts as the worst case.
        assert_eq!(
            v,
            [Some(50_000), Some(2_500), Some(u64::MAX), Some(u64::MAX)]
        );
        assert_eq!(parse_value("0.5"), Some(500));
        assert_eq!(parse_value("1.2345"), None);
        assert_eq!(parse_value("x"), None);
        assert_eq!(fmt_value(2_500), "2.5");
        assert_eq!(fmt_value(50_000), "50");
        assert_eq!(fmt_value(u64::MAX), "infinity");
        assert_eq!(parse_value(&fmt_value(1_250)), Some(1_250));
    }

    fn rl(unit: &str, n: usize) -> Counts {
        c(&[(unit, "rlimit", n)])
    }

    #[test]
    fn a_higher_rlimit_value_is_refused_unless_the_allow_says_so() {
        let (allow, bad) = parse_allow("u\trlimit\t1\tthe walk lemma needs it\t100\n");
        assert!(bad.is_empty());
        assert_eq!(allow[0].max_value, Some(100_000));
        let at = |top: u64| {
            let m = BTreeMap::from([("u".to_string(), top)]);
            ratchet_values(&rl("u", 1), &m, &allow, &bad)
        };
        assert!(at(100_000).errors.is_empty());
        let o = at(150_000);
        assert_eq!(o.errors.len(), 1);
        assert!(o.errors[0].contains("rlimit 150") && o.errors[0].contains("covers 100"));
        // Infinity is above every number.
        assert_eq!(at(u64::MAX).errors.len(), 1);
        // Raising the allow (with a reason) lets it through.
        let (allow2, _) = parse_allow("u\trlimit\t1\traised for the new lemma\t200\n");
        let m = BTreeMap::from([("u".to_string(), 150_000)]);
        assert!(
            ratchet_values(&rl("u", 1), &m, &allow2, &[])
                .errors
                .is_empty()
        );
    }

    #[test]
    fn an_rlimit_allow_without_a_max_value_is_refused_and_a_new_rlimit_has_no_allow() {
        let (allow, bad) = parse_allow("u\trlimit\t1\told style line\n");
        assert!(bad.is_empty() && allow[0].max_value.is_none());
        let m = BTreeMap::from([("u".to_string(), 20_000)]);
        let o = ratchet_values(&rl("u", 1), &m, &allow, &bad);
        assert_eq!(o.errors.len(), 1);
        assert!(o.errors[0].contains("max_value"));
        // A second rlimit raises the count above the allow.
        let (allow, _) = parse_allow("u\trlimit\t1\tone\t20\n");
        let o = ratchet_values(&rl("u", 2), &m, &allow, &[]);
        assert!(o.errors[0].contains("covers 1"));
        // No allow at all.
        assert!(ratchet_values(&rl("u", 1), &m, &[], &[]).errors[0].contains("no allow entry"));
    }

    #[test]
    fn the_allow_file_gains_a_fifth_column_and_old_lines_still_parse() {
        let old = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../trust-allow.tsv"),
        )
        .unwrap_or_default();
        let (ok, bad) = parse_allow(&old);
        assert!(bad.is_empty(), "{bad:?}");
        assert!(ok.iter().all(|a| a.max_value.is_none()));
        let (ok, bad) = parse_allow("u\tflag\t1\tr\nu\tloosen\t2\tr\t\n");
        assert!(bad.is_empty());
        assert_eq!(ok.len(), 2);
        let (_, bad) = parse_allow("u\trlimit\t1\tr\tabc\n");
        assert_eq!(bad.len(), 1);
        let (_, bad) = parse_allow("u\tinfo\t1\tr\n");
        assert_eq!(bad.len(), 1);
        // The new classes need an allow line like the old ones.
        for cl in ["loosen", "flag"] {
            let o = ratchet(&c(&[("u", cl, 1)]), &[], &[]);
            assert_eq!(o.errors.len(), 1, "{cl}");
        }
    }

    #[test]
    fn verifier_flags_are_found_and_the_harmless_ones_only_recorded() {
        let sh = "\
#!/usr/bin/env bash
# verus --no-verify in a comment
\"$verus\" \"$file\"
\"$verus\" --no-auto-recommends-check --rlimit 50 \"$file\"
\"$verus\" \\
    -V no-solver-version-check \\
    --smt-option smt.random_seed=7 \"$f\"
verus --cfg foo --expand-errors --rlimit=2.5 x.rs
echo --no-verify without the tool
";
        let s = scan_flags("scripts/verify-x.sh", sh, false);
        let got: Vec<(&str, &str, usize, Option<u64>)> = s
            .markers
            .iter()
            .map(|m| (m.kind, m.class.name(), m.line, m.value))
            .collect();
        assert_eq!(
            got,
            [
                ("--no-auto-recommends-check", "flag", 4, None),
                ("--rlimit", "rlimit", 4, Some(50_000)),
                ("-V no-solver-version-check", "flag", 5, None),
                ("--smt-option", "flag", 5, None),
                ("--cfg", "info", 8, None),
                ("--expand-errors", "info", 8, None),
                ("--rlimit", "rlimit", 8, Some(2_500)),
            ]
        );
        // A mutant script only records.
        let m = scan_flags("verification/x/mutants.sh", sh, true);
        assert!(m.markers.iter().all(|k| k.class == Class::Info));
        assert_eq!(m.markers.len(), got.len());
        // No flag, no marker.
        assert!(
            scan_flags("a.sh", "\"$verus\" \"$f\"\n", false)
                .markers
                .is_empty()
        );
    }

    #[test]
    fn a_flag_unit_is_ratcheted_per_script() {
        let s = scan_flags(
            "scripts/verify.sh",
            "\"$verus\" --no-verify \"$f\"\n",
            false,
        );
        let u = UnitTrust {
            unit: "flags/scripts/verify.sh".into(),
            kind: "flags".into(),
            target: "scripts/verify.sh".into(),
            files: vec![s],
        };
        let actual = counts(std::slice::from_ref(&u));
        assert_eq!(
            actual.get(&("flags/scripts/verify.sh".into(), "flag".into())),
            Some(&1)
        );
        assert_eq!(ratchet(&actual, &[], &[]).errors.len(), 1);
        let (allow, bad) =
            parse_allow("flags/scripts/verify.sh\tflag\t1\tcompile-only smoke run\n");
        assert!(ratchet(&actual, &allow, &bad).errors.is_empty());
    }

    #[test]
    fn the_report_lists_big_rlimits_and_flags_and_stays_deterministic() {
        let a = unit_trust(
            "verus/a",
            "verus",
            "verus! {\n#[verifier::rlimit(150)]\nproof fn f() {}\n#[verifier::rlimit(20)]\nproof fn g() {}\n#![verifier::loop_isolation(false)]\n}\n",
        );
        let f = UnitTrust {
            unit: "flags/scripts/verify.sh".into(),
            kind: "flags".into(),
            target: "scripts/verify.sh".into(),
            files: vec![scan_flags(
                "scripts/verify.sh",
                "\"$verus\" --rlimit 200 --no-lifetime x\n",
                false,
            )],
        };
        let one = render_report(&[a.clone(), f.clone()]);
        let two = render_report(&[f, a]);
        assert_eq!(one, two);
        assert!(one.contains("Needs ten times the default rlimit or more (value >= 100) (2)"));
        assert!(one.contains("- `t.rs:2` rlimit 150"));
        assert!(one.contains("- `scripts/verify.sh:1` --rlimit 200"));
        assert!(one.contains("## Verifier flags"));
        assert!(one.contains("- `scripts/verify.sh:1` `--no-lifetime` (flag)"));
        assert!(one.contains("Loosening attributes (1)"));
        // The flags unit is not a proof unit.
        assert!(one.contains("1 units, "));
    }
    fn escape_kinds(src: &str) -> Vec<&'static str> {
        scan_text("f.rs", src)
            .markers
            .iter()
            .filter(|m| m.class.name() == "escape")
            .map(|m| m.kind)
            .collect()
    }

    #[test]
    fn cfg_any_empty_counts_as_an_escape() {
        assert_eq!(
            escape_kinds("#[cfg(any())]\nmod m { fn f() {} }\n"),
            ["cfg(any())"]
        );
        assert_eq!(escape_kinds("#[cfg( any( ) )] mod m {}\n"), ["cfg(any())"]);
    }

    #[test]
    fn cfg_false_counts_as_an_escape() {
        assert_eq!(
            escape_kinds("#[cfg(false)] proof fn p() {}\n"),
            ["cfg(false)"]
        );
    }

    #[test]
    fn cfg_not_verus_keep_ghost_counts_as_an_escape() {
        assert_eq!(
            escape_kinds("#[cfg(not(verus_keep_ghost))] impl X { fn f() {} }\n"),
            ["cfg(not(verus_keep_ghost))"]
        );
    }

    #[test]
    fn cfg_not_all_empty_counts_as_an_escape() {
        assert_eq!(
            escape_kinds("#![cfg(not(all()))]\nfn f() {}\n"),
            ["cfg(not(all()))"]
        );
    }

    #[test]
    fn ordinary_cfg_gates_are_not_escapes() {
        assert!(escape_kinds("#[cfg(test)] mod t {}\n#[cfg(verus_keep_ghost)] use vstd::prelude::*;\n#[cfg(target_os = \"linux\")] fn f() {}\n").is_empty());
    }

    #[test]
    fn cfg_feature_gate_counts_as_a_loosening() {
        let s = scan_text(
            "f.rs",
            "#[cfg(feature = \"x\")]\n#[path = \"a.rs\"]\npub mod a;\n",
        );
        let k: Vec<(&str, &str)> = s.markers.iter().map(|m| (m.kind, m.class.name())).collect();
        assert_eq!(k, [("cfg-gate", "loosen")]);
    }

    #[test]
    fn cfg_target_os_gate_counts_as_a_loosening() {
        let s = scan_text(
            "f.rs",
            "#[cfg(target_os = \"none\")]\nimpl X { fn f(&self) {} }\n#![cfg(debug_assertions)]\n",
        );
        assert_eq!(s.markers.iter().filter(|m| m.kind == "cfg-gate").count(), 2);
    }

    #[test]
    fn benign_cfg_forms_are_not_counted_as_loosenings() {
        let s = scan_text(
            "f.rs",
            "#[cfg(test)] mod t {}\n#[cfg(verus_keep_ghost)] use vstd::prelude::*;\n#[cfg_attr(verus_keep_ghost, verus_verify)] pub struct S;\n#[cfg_attr(test, derive(Debug))] pub struct T;\n",
        );
        assert_eq!(s.markers.iter().filter(|m| m.kind == "cfg-gate").count(), 0);
        let s = scan_text(
            "f.rs",
            "#[cfg_attr(feature = \"x\", derive(Debug))] pub struct T;\n",
        );
        assert_eq!(s.markers.iter().filter(|m| m.kind == "cfg-gate").count(), 1);
    }

    #[test]
    fn open_trust_gaps_are_counted_and_printed_from_the_data_file() {
        let g = "# Open trust gaps\n\n### A builder can type an Opus trailer\nText.\n\n### Second\nMore.\n";
        assert_eq!(count_gaps(g), 2);
        let r = render_report_gaps(&[], g);
        assert!(r.contains("open trust gaps: 2"), "{r}");
        assert!(
            r.contains("## Open trust gaps") && r.contains("A builder can type an Opus trailer")
        );
        let none = render_report_gaps(&[], "");
        assert!(none.contains("open trust gaps: 0") && none.contains("None recorded"));
    }

    #[test]
    fn verified_items_inside_a_cfg_test_region_count_as_an_escape() {
        let src = "#[cfg(test)]\nmod hidden {\n    proof fn lemma() ensures true {}\n}\n";
        assert_eq!(escape_kinds(src), ["cfg(test)-gated-proof"]);
        assert!(
            escape_kinds(
                "#[cfg(test)]\nmod tests {\n    #[test] fn t() { assert_eq!(1, 1); }\n}\n"
            )
            .is_empty()
        );
    }

    #[test]
    fn an_inner_cfg_test_gates_the_whole_file() {
        let src = "#![cfg(test)]\nuse x::y;\npub proof fn lemma() ensures true {}\n";
        assert_eq!(escape_kinds(src), ["cfg(test)-gated-proof"]);
        assert!(escape_kinds("#![cfg(test)]\nuse x::y;\n#[test] fn t() {}\n").is_empty());
    }

    #[test]
    fn an_inner_cfg_test_inside_a_module_does_not_blind_the_scan() {
        let s = scan_text(
            "f.rs",
            "mod t {\n    #![cfg(test)]\n}\nproof fn p() { admit(); }\n",
        );
        assert!(
            s.markers.iter().any(|m| m.kind == "admit"),
            "{:?}",
            s.markers.iter().map(|m| m.kind).collect::<Vec<_>>()
        );
        // but a leading one still gates the file
        let s = scan_text(
            "f.rs",
            "// header\n#![cfg(test)]\nproof fn p() { admit(); }\n",
        );
        assert!(!s.markers.iter().any(|m| m.kind == "admit"));
    }
}

#[cfg(test)]
mod silent_loss_tests {
    use super::*;

    /// The real `drivers/lucina-charge/src/adc.rs` (8 `#[cfg(not(verus_keep_ghost))]` sites, a mid-file `#![allow(..)]` inner attribute)
    /// and the real cfg lines and surroundings of `kernel/lucina-rcu/src/lib.rs` (3 sites), copied 2026-11-11.
    const ADC: &str = include_str!("fixtures/adc.rs.txt");
    const RCU: &str = include_str!("fixtures/rcu_lib.rs.txt");

    fn escapes(path: &str, text: &str) -> usize {
        scan_text(path, text)
            .markers
            .iter()
            .filter(|m| m.class == Class::Escape)
            .count()
    }

    #[test]
    fn the_real_charge_and_rcu_files_count_their_escape_sites() {
        assert_eq!(escapes("drivers/lucina-charge/src/adc.rs", ADC), 8);
        assert_eq!(escapes("kernel/lucina-rcu/src/lib.rs", RCU), 3);
    }

    fn counts(unit: &str, class: &str, n: usize) -> Counts {
        [((unit.to_string(), class.to_string()), n)].into()
    }

    fn deltas(path: &str, base: &str, work: Option<&str>) -> BTreeMap<String, Vec<FileDelta>> {
        [(
            "verus/juno-charge/rules".to_string(),
            vec![FileDelta {
                path: path.into(),
                base: Some(base.into()),
                work: work.map(str::to_string),
            }],
        )]
        .into()
    }

    #[test]
    fn a_count_that_falls_while_the_lines_stay_is_refused() {
        let u = "verus/juno-charge/rules";
        let base = counts(u, "escape", 8);
        // the scan lost all eight sites, the file is unchanged
        let same = deltas("adc.rs", ADC, Some(ADC));
        let e = silent_drops(&base, &BTreeMap::new(), &same);
        assert_eq!(e.len(), 1, "{e:?}");
        assert!(
            e[0].contains("fell from 8") && e[0].contains("lost only 0"),
            "{e:?}"
        );
        // a file the closure no longer reaches (gone from the work side)
        let gone = deltas("adc.rs", ADC, None);
        assert!(
            silent_drops(&base, &counts(u, "escape", 8), &gone).is_empty(),
            "no fall, no refusal"
        );
        // the lines of three sites removed: a fall to five is fine, a fall to four is not
        let mut cut = String::new();
        let mut n = 0;
        for l in ADC.lines() {
            if l.trim() == "#[cfg(not(verus_keep_ghost))]" && n < 3 {
                n += 1;
                continue;
            }
            cut.push_str(l);
            cut.push('\n');
        }
        assert_eq!(escapes("adc.rs", &cut), 5);
        let d = deltas("adc.rs", ADC, Some(&cut));
        assert!(silent_drops(&base, &counts(u, "escape", 5), &d).is_empty());
        assert_eq!(silent_drops(&base, &counts(u, "escape", 4), &d).len(), 1);
        // every marker line edited trivially (a trailing comment): the marker stays on a new line, net loss zero, the drop is refused
        let edited: String = ADC
            .lines()
            .map(|l| {
                if l.trim() == "#[cfg(not(verus_keep_ghost))]" {
                    format!("{l} // edited\n")
                } else {
                    format!("{l}\n")
                }
            })
            .collect();
        let ed = deltas("adc.rs", ADC, Some(&edited));
        assert_eq!(
            net_marker_loss(&ed["verus/juno-charge/rules"], Class::Escape),
            0
        );
        let e = silent_drops(&base, &BTreeMap::new(), &ed);
        assert_eq!(e.len(), 1, "{e:?}");
        // a real deletion of all eight marker lines is accepted
        let nomark: String = ADC
            .lines()
            .filter(|l| l.trim() != "#[cfg(not(verus_keep_ghost))]")
            .map(|l| format!("{l}\n"))
            .collect();
        assert_eq!(escapes("adc.rs", &nomark), 0);
        let del8 = deltas("adc.rs", ADC, Some(&nomark));
        assert_eq!(
            net_marker_loss(&del8["verus/juno-charge/rules"], Class::Escape),
            8
        );
        assert!(silent_drops(&base, &BTreeMap::new(), &del8).is_empty());
        // a marker moved from one file of the unit to another: net zero
        let moved_unit: BTreeMap<String, Vec<FileDelta>> = [(
            "verus/juno-charge/rules".to_string(),
            vec![
                FileDelta {
                    path: "a.rs".into(),
                    base: Some(ADC.into()),
                    work: Some(nomark.clone()),
                },
                FileDelta {
                    path: "b.rs".into(),
                    base: Some("fn b() {}\n".into()),
                    work: Some(format!("fn b() {{}}\n{ADC}")),
                },
            ],
        )]
        .into();
        assert_eq!(
            net_marker_loss(&moved_unit["verus/juno-charge/rules"], Class::Escape),
            0
        );
        // all lines removed (the file deleted): any fall up to eight is accepted
        let del = deltas("adc.rs", ADC, None);
        assert!(silent_drops(&base, &BTreeMap::new(), &del).is_empty());
        // growth, equal counts and unchecked units pass
        assert!(silent_drops(&base, &counts(u, "escape", 9), &same).is_empty());
        assert!(silent_drops(&counts("flags/x", "flag", 3), &BTreeMap::new(), &same).is_empty());
        // a line that merely moves inside the file is not a removal
        let mut lines: Vec<&str> = ADC.lines().collect();
        let first = lines.remove(0);
        lines.push(first);
        let moved = lines.join("\n") + "\n";
        let m = deltas("adc.rs", ADC, Some(&moved));
        assert_eq!(silent_drops(&base, &BTreeMap::new(), &m).len(), 1);
    }

    #[test]
    fn rcu_sites_are_checked_the_same_way() {
        let u = "verus/juno-rcu/rules";
        let units: BTreeMap<String, Vec<FileDelta>> = [(
            u.to_string(),
            vec![FileDelta {
                path: "lib.rs".into(),
                base: Some(RCU.into()),
                work: Some(RCU.into()),
            }],
        )]
        .into();
        assert_eq!(
            silent_drops(&counts(u, "escape", 3), &BTreeMap::new(), &units).len(),
            1
        );
        assert!(silent_drops(&counts(u, "escape", 3), &counts(u, "escape", 3), &units).is_empty());
    }
}
