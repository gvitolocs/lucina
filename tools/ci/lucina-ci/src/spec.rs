//! `lucina-ci spec`: freeze the proof SPECIFICATIONS so that a branch cannot silently weaken them.
//!
//!   lucina-ci spec --check [--base REF] [--allow "<why>"] [--merge-gate]
//!   lucina-ci spec --list [--json]
//!
//! The comparison is against the BASE COMMIT's tree (`git show <base>:<path>`, base = merge base of REF and HEAD; REF defaults to
//! `origin/main`, else `main`), never against a lock file the branch could edit itself. A "statement" is a fingerprinted piece of
//! what the proofs promise:
//!   1. every `fn` of a verified file (a unit of kind verus or shipped, plus the files its `#[path]` lines pull in): the HEADER
//!      (attributes, signature, requires/ensures/decreases/recommends), and for `spec fn` also the BODY. Exec and proof bodies are
//!      the proof's business and are not part of it. In shipped files the clauses are `//@` lines.
//!   2. every struct/enum/type/const item of a `verus! { }` block, and in shipped files every `pub const` and every `//@` item.
//!   3. models: each TLA+ `.cfg` (whole, normalised) and, for each INVARIANT/PROPERTY it names, the TRANSITIVE CLOSURE of the operators its
//!      definition references (the module and what it EXTENDS in the tree, plus their ASSUME lines; Spec/Init/Next only if reached).
//!      A name that cannot be resolved (an applied operator that is not defined, declared, LET-local or standard; `A!B`; an EXTENDS
//!      that is neither standard nor in the tree) makes that invariant fall back to the whole normalised module(s). Each Stateright
//!      model file: its `fn properties` plus every fn, method and const of the same file it reaches by simple name; a macro, a
//!      `crate::`/`super::` path or a function brought in by `use` makes it fall back to the whole file's non-test items.
//!      The report line counts closure-covered against fallback and names the fallback modules.
//!   4. mutants: every `try ` line of verification/*/mutants.sh and every entry of `mutations=( ... )` in
//!      scripts/verify-mutations.sh. A changed line is a removed line plus a new one.
//!
//! Present at base and equal: fine. New: free. CHANGED or REMOVED: refused unless an allow reason is given:
//! `LAGENT_ALLOW_SPEC_CHANGE` (set by `lagent merge --allow spec-change=...`), or, for a developer's local run only,
//! `LUCINA_SPEC_CHANGE_ALLOW` / `--allow` (ignored with `--merge-gate`). Spec changes and mutant changes print under separate headings.
//!
//! Heuristic for the end of a header (where the body brace is), documented because it is a heuristic: the file is lexed (comments
//! dropped, `//@` lines turned into code the way scripts/verify-shipped.sh does). After `fn name`, parentheses/brackets are tracked;
//! a `{` at depth 0 opens the body UNLESS it belongs to an expression of a clause: after the keywords `match`, `if` or `else` the
//! next depth-0 `{` (and its matching `}`) is part of the clause, every `{` inside parentheses is, and so is a struct literal
//! (`Name { field: ..` after a clause keyword). In a shipped file whose `fn` line is plain code the rule is line-based instead:
//! the header ends at the first `{` or `;` of a plain (non-`//@`) line; braces on `//@` lines are always clause text.
//!
//! Self check: the `requires/ensures/decreases/recommends` keywords of a verified file are counted; those in extracted headers,
//! and those in bodies that are not part of a statement (`unextracted`), must account for all of them. A keyword outside any
//! function (a function this scan did not recognise) is a miss: exit 2.

use crate::inventory::{self, Unit};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const MERGE_ENV: &str = "LAGENT_ALLOW_SPEC_CHANGE";
pub const LOCAL_ENV: &str = "LUCINA_SPEC_CHANGE_ALLOW";
const MUTATIONS_SH: &str = "scripts/verify-mutations.sh";
const MODELS_DIR: &str = "specs/lucina-models/src";
const FORMAL_DIR: &str = "userspace/formal";
const VERIFICATION_DIR: &str = "verification";

/// Attributes that cannot change what is proved: dropped from the fingerprint.
const IGNORED_ATTRS: &[&str] = &[
    "inline",
    "must_use",
    "allow",
    "warn",
    "deny",
    "expect",
    "cold",
    "track_caller",
    "doc",
];

// ---------------------------------------------------------------------------------------------------------------- hashing

pub fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

pub fn hex(h: u64) -> String {
    format!("{h:016x}")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stmt {
    pub key: String,
    pub kind: &'static str,
    pub text: String,
    pub hash: u64,
}

impl Stmt {
    pub fn new(key: String, kind: &'static str, text: String) -> Stmt {
        let hash = fnv1a(&format!("{kind}\n{text}"));
        Stmt {
            key,
            kind,
            text,
            hash,
        }
    }
    pub fn is_mutant(&self) -> bool {
        matches!(self.kind, "mutant" | "mutation")
    }
}

// ------------------------------------------------------------------------------------------------------------------ lexer

#[derive(Clone, Debug)]
struct Tok {
    s: String,
    line: usize,
    ident: bool,
    /// On a `//@` line.
    tag: bool,
    /// On a `//@` line or a `//@ret` line.
    carry: bool,
}

const OPS: &[&str] = &[
    "<==>", "===", "!==", "==>", "<==", "..=", "...", "&&&", "|||", "==", "!=", "<=", ">=", "&&",
    "||", "=>", "->", "::", "..", "+=", "-=", "*=", "/=", "%=", "^=", "&=", "|=",
];

/// What Verus reads of a shipped file (scripts/verify-shipped.sh): `//@omit` lines gone, `-> T //@ret` as `-> (ret: T)`,
/// `//@ x` as `x`. Returns the text and, per line, "is a proof line" and "carries proof".
fn shipped_transform(src: &str) -> (String, Vec<bool>, Vec<bool>) {
    let mut out = String::new();
    let mut tag = Vec::new();
    let mut carry = Vec::new();
    for line in src.lines() {
        let t = line.trim_end();
        if t.ends_with("//@omit") {
            out.push('\n');
            tag.push(false);
            carry.push(false);
        } else if let Some(body) = t.strip_suffix("//@ret") {
            let body = body.trim_end();
            match body.find("->") {
                Some(i) => {
                    let (pre, ty) = body.split_at(i + 2);
                    out.push_str(&format!("{pre} (ret: {})\n", ty.trim()));
                }
                None => {
                    out.push_str(body);
                    out.push('\n');
                }
            }
            tag.push(false);
            carry.push(true);
        } else if let Some(rest) = t.trim_start().strip_prefix("//@") {
            out.push_str(rest);
            out.push('\n');
            tag.push(true);
            carry.push(true);
        } else {
            out.push_str(line);
            out.push('\n');
            tag.push(false);
            carry.push(false);
        }
    }
    (out, tag, carry)
}

fn lex(src: &str, tag: &[bool], carry: &[bool]) -> Vec<Tok> {
    let c: Vec<char> = src.chars().collect();
    let n = c.len();
    let mut out: Vec<Tok> = Vec::new();
    let mut i = 0usize;
    let mut line = 0usize;
    let push = |out: &mut Vec<Tok>, s: String, line: usize, ident: bool| {
        out.push(Tok {
            s,
            line,
            ident,
            tag: tag.get(line).copied().unwrap_or(false),
            carry: carry.get(line).copied().unwrap_or(false),
        });
    };
    while i < n {
        let ch = c[i];
        if ch == '\n' {
            line += 1;
            i += 1;
            continue;
        }
        if ch.is_whitespace() {
            i += 1;
            continue;
        }
        if ch == '/' && c.get(i + 1) == Some(&'/') {
            while i < n && c[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if ch == '/' && c.get(i + 1) == Some(&'*') {
            let mut d = 1;
            i += 2;
            while i < n && d > 0 {
                if c[i] == '/' && c.get(i + 1) == Some(&'*') {
                    d += 1;
                    i += 2;
                } else if c[i] == '*' && c.get(i + 1) == Some(&'/') {
                    d -= 1;
                    i += 2;
                } else {
                    if c[i] == '\n' {
                        line += 1;
                    }
                    i += 1;
                }
            }
            continue;
        }
        if ch.is_alphabetic() || ch == '_' {
            let st = i;
            while i < n && (c[i].is_alphanumeric() || c[i] == '_') {
                i += 1;
            }
            let word: String = c[st..i].iter().collect();
            if matches!(word.as_str(), "r" | "br" | "b") {
                let mut j = i;
                let mut hashes = 0;
                if word != "b" {
                    while c.get(j) == Some(&'#') {
                        hashes += 1;
                        j += 1;
                    }
                }
                if c.get(j) == Some(&'"') {
                    let start_line = line;
                    j += 1;
                    if word == "b" {
                        while j < n && c[j] != '"' {
                            if c[j] == '\\' && j + 1 < n {
                                if c[j + 1] == '\n' {
                                    line += 1;
                                }
                                j += 2;
                                continue;
                            }
                            if c[j] == '\n' {
                                line += 1;
                            }
                            j += 1;
                        }
                        j = (j + 1).min(n);
                    } else {
                        loop {
                            if j >= n {
                                break;
                            }
                            if c[j] == '"' && (0..hashes).all(|h| c.get(j + 1 + h) == Some(&'#')) {
                                j += 1 + hashes;
                                break;
                            }
                            if c[j] == '\n' {
                                line += 1;
                            }
                            j += 1;
                        }
                    }
                    let s: String = c[st..j.min(n)].iter().collect();
                    push(&mut out, s, start_line, false);
                    i = j.min(n);
                    continue;
                }
            }
            push(&mut out, word, line, true);
            continue;
        }
        if ch.is_ascii_digit() {
            let st = i;
            while i < n && (c[i].is_alphanumeric() || c[i] == '_') {
                i += 1;
            }
            push(&mut out, c[st..i].iter().collect(), line, false);
            continue;
        }
        if ch == '"' {
            let start_line = line;
            let st = i;
            let mut j = i + 1;
            while j < n && c[j] != '"' {
                if c[j] == '\\' && j + 1 < n {
                    if c[j + 1] == '\n' {
                        line += 1;
                    }
                    j += 2;
                    continue;
                }
                if c[j] == '\n' {
                    line += 1;
                }
                j += 1;
            }
            j = (j + 1).min(n);
            push(&mut out, c[st..j].iter().collect(), start_line, false);
            i = j;
            continue;
        }
        if ch == '\'' {
            if c.get(i + 1) == Some(&'\\') {
                let mut j = i + 3;
                while j < n && c[j] != '\'' {
                    j += 1;
                }
                let j = (j + 1).min(n);
                push(&mut out, c[i..j].iter().collect(), line, false);
                i = j;
                continue;
            }
            if c.get(i + 2) == Some(&'\'') {
                push(&mut out, c[i..i + 3].iter().collect(), line, false);
                i += 3;
                continue;
            }
            push(&mut out, "'".into(), line, false);
            i += 1;
            continue;
        }
        let mut matched = false;
        for len in [4usize, 3, 2] {
            if i + len <= n {
                let w: String = c[i..i + len].iter().collect();
                if OPS.contains(&w.as_str()) {
                    push(&mut out, w, line, false);
                    i += len;
                    matched = true;
                    break;
                }
            }
        }
        if matched {
            continue;
        }
        push(&mut out, ch.to_string(), line, false);
        i += 1;
    }
    out
}

fn is_kw(t: &Tok) -> bool {
    t.ident
        && matches!(
            t.s.as_str(),
            "requires" | "ensures" | "decreases" | "recommends"
        )
}

/// A clause keyword that belongs to the verified text (a `verus!` block, or a `//@` line).
fn counts(t: &Tok, verus: bool) -> bool {
    is_kw(t) && (verus || t.tag)
}

fn match_brace(t: &[Tok], open: usize) -> usize {
    let mut d = 0i32;
    let mut k = open;
    while k < t.len() {
        match t[k].s.as_str() {
            "{" => d += 1,
            "}" => {
                d -= 1;
                if d == 0 {
                    return k;
                }
            }
            _ => {}
        }
        k += 1;
    }
    t.len().saturating_sub(1)
}

fn tight(s: &str) -> bool {
    matches!(s, "," | ";" | "(" | ")" | "{" | "}" | "[" | "]")
}

/// Index just past the attribute starting at `i` (`#[..]` or `#![..]`), or None if `i` does not start one.
fn attr_end(t: &[Tok], i: usize) -> Option<usize> {
    if t.get(i)?.s != "#" {
        return None;
    }
    let mut j = i + 1;
    if t.get(j).is_some_and(|x| x.s == "!") {
        j += 1;
    }
    if t.get(j)?.s != "[" {
        return None;
    }
    let mut d = 0;
    let mut k = j;
    while k < t.len() {
        match t[k].s.as_str() {
            "[" => d += 1,
            "]" => {
                d -= 1;
                if d == 0 {
                    return Some(k + 1);
                }
            }
            _ => {}
        }
        k += 1;
    }
    Some(t.len())
}

/// The formatting-free text of tokens: comments gone, one space between tokens except around `,;(){}[]`, trailing commas
/// dropped, attributes that cannot change what is proved dropped.
fn norm_tokens(toks: &[Tok]) -> String {
    let mut v: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        if let Some(e) = attr_end(toks, i) {
            let j = if toks[i + 1].s == "!" { i + 2 } else { i + 1 };
            let name = toks.get(j + 1).map_or("", |x| x.s.as_str());
            if IGNORED_ATTRS.contains(&name) {
                i = e;
                continue;
            }
        }
        if toks[i].s == "," {
            match toks.get(i + 1) {
                None => {
                    i += 1;
                    continue;
                }
                Some(n) if matches!(n.s.as_str(), "}" | ")" | "]") => {
                    i += 1;
                    continue;
                }
                _ => {}
            }
        }
        v.push(&toks[i].s);
        i += 1;
    }
    join_tight(&v)
}

fn join_tight(v: &[&str]) -> String {
    let mut out = String::new();
    for (i, p) in v.iter().enumerate() {
        if i > 0 && !tight(v[i - 1]) && !tight(p) {
            out.push(' ');
        }
        out.push_str(p);
    }
    out
}

// ---------------------------------------------------------------------------------------------------------------- scanning

#[derive(Default)]
struct Found {
    /// name, kind, text
    stmts: Vec<(String, &'static str, String)>,
    header_kw: usize,
    body_kw: usize,
    /// Lines (0-based) of clause keywords outside any recognised function.
    stray: Vec<usize>,
}

fn struct_literal(t: &[Tok], k: usize, clause: bool) -> bool {
    if !clause || k == 0 {
        return false;
    }
    let p = &t[k - 1];
    if !(p.ident && p.s.chars().next().is_some_and(char::is_uppercase)) {
        return false;
    }
    match (t.get(k + 1), t.get(k + 2)) {
        (Some(a), Some(b)) => (a.ident && b.s == ":") || a.s == "..",
        _ => false,
    }
}

fn item_start(t: &[Tok], i: usize, lo: usize) -> usize {
    let mut j = i;
    while j > lo && !matches!(t[j - 1].s.as_str(), ";" | "{" | "}") {
        j -= 1;
    }
    j
}

fn parse_fn(t: &[Tok], i: usize, lo: usize, hi: usize, verus: bool, f: &mut Found) -> usize {
    let j = item_start(t, i, lo);
    let token_mode = verus || t[i].tag;
    let mut k = i + 2;
    let mut depth = 0i32;
    let mut pending = false;
    let mut clause = false;
    let end;
    loop {
        if k >= hi {
            end = hi;
            break;
        }
        let x = &t[k];
        if token_mode {
            match x.s.as_str() {
                "(" | "[" => depth += 1,
                ")" | "]" => depth -= 1,
                "{" => {
                    if depth > 0 || pending || struct_literal(t, k, clause) {
                        k = match_brace(t, k) + 1;
                        pending = false;
                        continue;
                    }
                    end = k;
                    break;
                }
                ";" if depth <= 0 => {
                    end = k;
                    break;
                }
                s if x.ident && depth <= 0 && matches!(s, "match" | "if" | "else") => {
                    pending = true
                }
                _ if is_kw(x) && depth <= 0 => clause = true,
                _ => {}
            }
        } else if !x.tag {
            match x.s.as_str() {
                "(" | "[" => depth += 1,
                ")" | "]" => depth -= 1,
                "{" | ";" if depth <= 0 => {
                    end = k;
                    break;
                }
                _ => {}
            }
        }
        k += 1;
    }
    let has_body = end < hi && t[end].s == "{";
    let body_close = if has_body {
        match_brace(t, end).min(hi.saturating_sub(1))
    } else {
        end
    };
    f.header_kw += t[j..end.min(hi)]
        .iter()
        .filter(|x| counts(x, verus))
        .count();
    if has_body {
        f.body_kw += t[end..=body_close]
            .iter()
            .filter(|x| counts(x, verus))
            .count();
    }
    let is_spec = t[j..i].iter().any(|x| x.ident && x.s == "spec");
    let is_proof = t[j..i].iter().any(|x| x.ident && x.s == "proof");
    let extract = verus || t[i].tag || t[j..end.min(hi)].iter().any(|x| x.carry);
    if extract {
        let stop = if is_spec && has_body {
            body_close + 1
        } else {
            end
        };
        let kind = if is_spec {
            "spec fn"
        } else if is_proof {
            "proof fn"
        } else {
            "fn"
        };
        f.stmts
            .push((t[i + 1].s.clone(), kind, norm_tokens(&t[j..stop.min(hi)])));
    }
    if has_body { body_close + 1 } else { end + 1 }
}

/// struct/enum/type/const: Some(next index) when `t[i]` starts such an item.
fn parse_item(t: &[Tok], i: usize, lo: usize, hi: usize, f: &mut Found) -> Option<usize> {
    let kw = t[i].s.as_str();
    if kw == "const" && t.get(i + 1).is_some_and(|x| x.s == "fn" || x.s == "unsafe") {
        return None;
    }
    if !t.get(i + 1).is_some_and(|x| x.ident || x.s == "_")
        && !t.get(i + 1).is_some_and(|x| x.s == "_")
    {
        return None;
    }
    let j = item_start(t, i, lo);
    // Only attributes and visibility may precede the keyword.
    let mut p = j;
    while p < i {
        if let Some(e) = attr_end(t, p) {
            p = e;
            continue;
        }
        if !matches!(
            t[p].s.as_str(),
            "pub"
                | "("
                | ")"
                | "crate"
                | "super"
                | "self"
                | "in"
                | "::"
                | "open"
                | "closed"
                | "ghost"
                | "tracked"
        ) {
            return None;
        }
        p += 1;
    }
    let mut k = i + 2;
    let mut depth = 0i32;
    let end;
    loop {
        if k >= hi {
            end = hi.saturating_sub(1);
            break;
        }
        let s = t[k].s.as_str();
        if matches!(kw, "struct" | "enum") {
            match s {
                "(" | "[" => depth += 1,
                ")" | "]" => depth -= 1,
                "{" if depth <= 0 => {
                    end = match_brace(t, k).min(hi.saturating_sub(1));
                    break;
                }
                ";" if depth <= 0 => {
                    end = k;
                    break;
                }
                _ => {}
            }
        } else {
            match s {
                "(" | "[" | "{" => depth += 1,
                ")" | "]" | "}" => depth -= 1,
                ";" if depth <= 0 => {
                    end = k;
                    break;
                }
                _ => {}
            }
        }
        k += 1;
    }
    f.stmts
        .push((t[i + 1].s.clone(), "item", norm_tokens(&t[j..=end])));
    Some(end + 1)
}

fn scan(t: &[Tok], lo: usize, hi: usize, verus: bool, shipped_on: bool, f: &mut Found) {
    let mut i = lo;
    while i < hi {
        let tk = &t[i];
        if !verus
            && tk.ident
            && tk.s == "verus"
            && i + 2 < hi
            && t[i + 1].s == "!"
            && t[i + 2].s == "{"
        {
            let close = match_brace(t, i + 2).min(hi);
            scan(t, i + 3, close, true, shipped_on, f);
            i = close + 1;
            continue;
        }
        if counts(tk, verus) {
            f.stray.push(tk.line);
            i += 1;
            continue;
        }
        if (verus || shipped_on) && tk.ident {
            if tk.s == "fn" && i + 1 < hi && t[i + 1].ident {
                i = parse_fn(t, i, lo, hi, verus, f);
                continue;
            }
            let wanted = if verus || tk.tag {
                matches!(tk.s.as_str(), "struct" | "enum" | "type" | "const")
            } else {
                tk.s == "const" && i > 0 && (t[i - 1].s == "pub" || t[i - 1].s == ")")
            };
            if wanted && let Some(n) = parse_item(t, i, lo, hi, f) {
                i = n;
                continue;
            }
        }
        i += 1;
    }
}

pub struct FileScan {
    pub stmts: Vec<Stmt>,
    pub header_kw: usize,
    pub body_kw: usize,
    /// "path:line" of clause keywords no recognised function accounts for.
    pub stray: Vec<String>,
}

pub fn scan_file(path: &str, src: &str) -> FileScan {
    let (text, tag, carry) = shipped_transform(src);
    let shipped_on = carry.iter().any(|&b| b);
    let t = lex(&text, &tag, &carry);
    let mut f = Found::default();
    scan(&t, 0, t.len(), false, shipped_on, &mut f);
    // Independent count of every clause keyword the verifier reads.
    let mut in_verus = vec![false; t.len()];
    let mut i = 0;
    while i < t.len() {
        if t[i].ident
            && t[i].s == "verus"
            && i + 2 < t.len()
            && t[i + 1].s == "!"
            && t[i + 2].s == "{"
        {
            let c = match_brace(&t, i + 2);
            for x in in_verus.iter_mut().take(c).skip(i + 3) {
                *x = true;
            }
            i = c + 1;
        } else {
            i += 1;
        }
    }
    let total = t
        .iter()
        .enumerate()
        .filter(|(n, x)| counts(x, in_verus[*n]))
        .count();
    let mut stray: Vec<String> = f
        .stray
        .iter()
        .map(|l| format!("{path}:{}", l + 1))
        .collect();
    let accounted = f.header_kw + f.body_kw + f.stray.len();
    if total != accounted {
        stray.push(format!(
            "{path}: {} clause keyword(s) in the file but {accounted} accounted for (an item the scan skipped)",
            total
        ));
    }
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut stmts = Vec::new();
    for (name, kind, text) in f.stmts {
        let n = seen.entry(name.clone()).or_insert(0);
        *n += 1;
        let key = if *n == 1 {
            format!("{path}::{name}")
        } else {
            format!("{path}::{name}#{n}")
        };
        stmts.push(Stmt::new(key, kind, text));
    }
    FileScan {
        stmts,
        header_kw: f.header_kw,
        body_kw: f.body_kw,
        stray,
    }
}

// ------------------------------------------------------------------------------------------------------------------ models

fn tla_strip(s: &str) -> String {
    let c: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    let mut depth = 0;
    while i < c.len() {
        if c[i] == '(' && c.get(i + 1) == Some(&'*') {
            depth += 1;
            i += 2;
        } else if depth > 0 && c[i] == '*' && c.get(i + 1) == Some(&')') {
            depth -= 1;
            i += 2;
        } else if depth > 0 {
            if c[i] == '\n' {
                out.push('\n');
            }
            i += 1;
        } else if c[i] == '\\' && c.get(i + 1) == Some(&'*') {
            while i < c.len() && c[i] != '\n' {
                i += 1;
            }
        } else {
            out.push(c[i]);
            i += 1;
        }
    }
    out
}

fn words(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The invariant and property names a TLC config lists.
fn cfg_names(cfg: &str) -> Vec<String> {
    let text = tla_strip(cfg);
    let mut names = Vec::new();
    let mut collecting = false;
    for w in text.split_whitespace() {
        let upper = w.chars().all(|c| c.is_ascii_uppercase() || c == '_') && w.len() > 1;
        if matches!(w, "INVARIANT" | "INVARIANTS" | "PROPERTY" | "PROPERTIES") {
            collecting = true;
        } else if upper
            && matches!(
                w,
                "SPECIFICATION"
                    | "INIT"
                    | "NEXT"
                    | "CONSTANT"
                    | "CONSTANTS"
                    | "CONSTRAINT"
                    | "CONSTRAINTS"
                    | "ACTION_CONSTRAINT"
                    | "ACTION_CONSTRAINTS"
                    | "SYMMETRY"
                    | "VIEW"
                    | "ALIAS"
                    | "CHECK_DEADLOCK"
                    | "POSTCONDITION"
                    | "TERMINAL"
            )
        {
            collecting = false;
        } else if collecting {
            names.push(w.to_string());
        }
    }
    names
}

fn is_def_line(l: &str) -> Option<&str> {
    let first = l.chars().next()?;
    if !(first.is_alphabetic() || first == '_') {
        return None;
    }
    let end = l.find(|c: char| !(c.is_alphanumeric() || c == '_'))?;
    let name = &l[..end];
    let mut rest = l[end..].trim_start();
    if rest.starts_with('(') {
        let mut d = 0;
        let mut cut = None;
        for (i, ch) in rest.char_indices() {
            match ch {
                '(' => d += 1,
                ')' => {
                    d -= 1;
                    if d == 0 {
                        cut = Some(i + 1);
                        break;
                    }
                }
                _ => {}
            }
        }
        rest = rest[cut?..].trim_start();
    }
    rest.starts_with("==").then_some(name)
}

/// Modules that ship with TLC: not in the tree, and their operators are known.
const TLA_STANDARD: &[&str] = &[
    "Naturals",
    "Integers",
    "Reals",
    "Sequences",
    "FiniteSets",
    "Bags",
    "TLC",
    "TLCExt",
    "Randomization",
    "Json",
];

/// Operators of the standard modules that are applied like `Name(args)`.
const TLA_BUILTIN_OPS: &[&str] = &[
    "Len",
    "Append",
    "SubSeq",
    "Head",
    "Tail",
    "Seq",
    "SelectSeq",
    "Cardinality",
    "IsFiniteSet",
    "Print",
    "PrintT",
    "Assert",
    "TLCGet",
    "TLCSet",
    "ToString",
    "RandomElement",
    "SortSeq",
    "Permutations",
    "IsABag",
    "BagIn",
    "CopiesIn",
    "BagCardinality",
    "BagToSet",
    "SetToBag",
    "SubBag",
    "BagUnion",
    "WF_",
    "SF_",
];

/// Words that are syntax, not operators.
const TLA_WORDS: &[&str] = &[
    "LET",
    "IN",
    "IF",
    "THEN",
    "ELSE",
    "CASE",
    "OTHER",
    "CHOOSE",
    "EXCEPT",
    "DOMAIN",
    "SUBSET",
    "UNION",
    "UNCHANGED",
    "ENABLED",
    "TRUE",
    "FALSE",
    "BOOLEAN",
    "STRING",
    "LAMBDA",
    "Nat",
    "Int",
    "Real",
];

#[derive(Default, Clone)]
struct TlaModule {
    path: String,
    /// Name -> the whole definition, normalised.
    defs: BTreeMap<String, String>,
    assumes: Vec<String>,
    extends: Vec<String>,
    declared: BTreeSet<String>,
    /// The whole module, comments dropped, normalised: the fallback fingerprint.
    text: String,
}

fn tla_idents_in(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let c: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < c.len() {
        if c[i].is_alphabetic() || c[i] == '_' {
            let st = i;
            while i < c.len() && (c[i].is_alphanumeric() || c[i] == '_') {
                i += 1;
            }
            out.push(c[st..i].iter().collect());
        } else {
            i += 1;
        }
    }
    out
}

fn parse_tla(path: &str, src: &str) -> TlaModule {
    #[derive(PartialEq)]
    enum K {
        Def,
        Assume,
        Extends,
        Decl,
        Other,
    }
    let stripped = tla_strip(src);
    let mut m = TlaModule {
        path: path.to_string(),
        text: words(&stripped),
        ..TlaModule::default()
    };
    let mut cur: Option<(K, String, String)> = None;
    let flush = |cur: &mut Option<(K, String, String)>, m: &mut TlaModule| {
        let Some((k, name, text)) = cur.take() else {
            return;
        };
        let w = words(&text);
        match k {
            K::Def => {
                m.defs.insert(name, w);
            }
            K::Assume => m.assumes.push(w),
            K::Extends => {
                let rest = w.trim_start_matches("EXTENDS");
                m.extends.extend(
                    rest.split(',')
                        .map(|x| x.trim().to_string())
                        .filter(|x| !x.is_empty()),
                );
            }
            K::Decl => {
                for id in tla_idents_in(&w) {
                    if id != "_"
                        && id != "VARIABLE"
                        && id != "VARIABLES"
                        && id != "CONSTANT"
                        && id != "CONSTANTS"
                    {
                        m.declared.insert(id);
                    }
                }
            }
            K::Other => {}
        }
    };
    for line in stripped.lines() {
        let col0 = line.chars().next().is_some_and(|c| !c.is_whitespace());
        if !col0 {
            if let Some((_, _, t)) = cur.as_mut() {
                t.push('\n');
                t.push_str(line);
            }
            continue;
        }
        if line.starts_with("====") || line.starts_with("----") {
            flush(&mut cur, &mut m);
            continue;
        }
        let starts = |k: &str| line.starts_with(k);
        let new = if starts("EXTENDS") {
            Some((K::Extends, String::new()))
        } else if starts("VARIABLE") || starts("CONSTANT") {
            Some((K::Decl, String::new()))
        } else if starts("ASSUME") {
            Some((K::Assume, String::new()))
        } else if starts("THEOREM") || starts("INSTANCE") || starts("LOCAL INSTANCE") {
            Some((K::Other, String::new()))
        } else {
            let l2 = line.strip_prefix("LOCAL ").unwrap_or(line).trim_start();
            is_def_line(l2).map(|n| (K::Def, n.to_string()))
        };
        match new {
            Some((k, n)) => {
                flush(&mut cur, &mut m);
                cur = Some((k, n, line.to_string()));
            }
            None => {
                if let Some((_, _, t)) = cur.as_mut() {
                    t.push('\n');
                    t.push_str(line);
                }
            }
        }
    }
    flush(&mut cur, &mut m);
    m
}

/// The text of `Name == ...` up to the next top-level definition, `====` or module keyword.
#[cfg(test)]
fn tla_def(tla: &str, name: &str) -> Option<String> {
    parse_tla("", tla).defs.get(name).cloned()
}

/// Identifiers of a definition: (name, applied like `Name(..)`, written `A!B`).
fn tla_refs(text: &str) -> Vec<(String, bool, bool)> {
    let c: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < c.len() {
        let ch = c[i];
        if ch == '"' {
            i += 1;
            while i < c.len() && c[i] != '"' {
                if c[i] == '\\' {
                    i += 1;
                }
                i += 1;
            }
            i += 1;
        } else if ch == '\\' {
            i += 1;
            while i < c.len() && c[i].is_alphabetic() {
                i += 1;
            }
        } else if ch.is_ascii_digit() {
            while i < c.len() && (c[i].is_alphanumeric() || c[i] == '_') {
                i += 1;
            }
        } else if ch.is_alphabetic() || ch == '_' {
            let st = i;
            while i < c.len() && (c[i].is_alphanumeric() || c[i] == '_') {
                i += 1;
            }
            let name: String = c[st..i].iter().collect();
            let mut j = i;
            while j < c.len() && c[j] == ' ' {
                j += 1;
            }
            let applied = c.get(j) == Some(&'(');
            let qualified =
                c.get(i) == Some(&'!') && c.get(i + 1).is_some_and(|x| x.is_alphabetic());
            // `WF_vars(Next)`: the subscript is a reference too.
            if let Some(sub) = name
                .strip_prefix("WF_")
                .or_else(|| name.strip_prefix("SF_"))
            {
                if !sub.is_empty() {
                    out.push((sub.to_string(), false, false));
                }
                continue;
            }
            out.push((name, applied, qualified));
        } else {
            i += 1;
        }
    }
    out
}

/// Names a definition text defines for itself (`LET f(x) == ...`), apart from its own head.
/// Names a definition text defines for itself (`LET f(x) == ...`).
fn tla_locals(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let ids: BTreeSet<String> = tla_idents_in(text).into_iter().collect();
    let word = |c: char| c.is_alphanumeric() || c == '_';
    for w in ids {
        let mut from = 0;
        while let Some(p) = text[from..].find(&w) {
            let at = from + p;
            let end = at + w.len();
            from = end;
            if text[..at].chars().next_back().is_some_and(word)
                || text[end..].chars().next().is_some_and(word)
            {
                continue;
            }
            let mut rest = text[end..].trim_start();
            if rest.starts_with('(') {
                let mut d = 0;
                let mut cut = None;
                for (i, ch) in rest.char_indices() {
                    match ch {
                        '(' => d += 1,
                        ')' => {
                            d -= 1;
                            if d == 0 {
                                cut = Some(i + 1);
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                if let Some(cut) = cut {
                    rest = rest[cut..].trim_start();
                }
            }
            if rest.starts_with("==") {
                out.insert(w.clone());
            }
        }
    }
    out
}

/// The transitive closure of the operators `name` references, over `defs` (the module and what it EXTENDS in the tree).
/// None when a name cannot be resolved: an applied operator that is neither defined, declared, local nor standard, or `A!B`.
fn tla_closure(
    name: &str,
    defs: &BTreeMap<String, String>,
    declared: &BTreeSet<String>,
    assumes: &[String],
) -> Option<String> {
    let mut visited: BTreeSet<String> = BTreeSet::new();
    let mut work = vec![name.to_string()];
    while let Some(n) = work.pop() {
        if !visited.insert(n.clone()) {
            continue;
        }
        let text = defs.get(&n)?;
        let locals = tla_locals(text);
        for (id, applied, qualified) in tla_refs(text) {
            if qualified {
                return None;
            }
            if defs.contains_key(&id) {
                work.push(id);
            } else if applied
                && !declared.contains(&id)
                && !locals.contains(&id)
                && !TLA_BUILTIN_OPS.contains(&id.as_str())
                && !TLA_WORDS.contains(&id.as_str())
            {
                return None;
            }
        }
    }
    let mut out: Vec<String> = visited
        .iter()
        .filter_map(|n| defs.get(n).cloned())
        .collect();
    out.extend(assumes.iter().cloned());
    Some(out.join(" ;; "))
}

#[derive(Default, Debug)]
pub struct ModelStats {
    pub tla_closure: usize,
    pub tla_fallback: usize,
    pub tla_undefined: usize,
    pub tla_fallback_modules: BTreeSet<String>,
    pub sr_closure: usize,
    pub sr_fallback: usize,
    pub sr_fallback_files: BTreeSet<String>,
}

fn model_stmts(tree: &dyn Tree) -> (Vec<Stmt>, ModelStats) {
    let mut out = Vec::new();
    let mut stats = ModelStats::default();
    let files = tree.files_under(FORMAL_DIR);
    let mut cache: BTreeMap<String, TlaModule> = BTreeMap::new();
    for cfg in files.iter().filter(|f| f.ends_with(".cfg")) {
        let Some(text) = tree.read(cfg) else { continue };
        out.push(Stmt::new(cfg.clone(), "tla-cfg", words(&tla_strip(&text))));
        let dir = cfg.rsplit_once('/').map_or("", |x| x.0);
        let stem = cfg.trim_end_matches(".cfg");
        let mut tlas: Vec<&String> = files
            .iter()
            .filter(|f| f.ends_with(".tla") && f.rsplit_once('/').map_or("", |x| x.0) == dir)
            .collect();
        tlas.sort_by_key(|f| (f.trim_end_matches(".tla") != stem, (*f).clone()));
        for name in cfg_names(&text) {
            let mut stmt = None;
            for t in &tlas {
                if !cache.contains_key(*t)
                    && let Some(src) = tree.read(t)
                {
                    cache.insert((*t).clone(), parse_tla(t, &src));
                }
                let Some(primary) = cache.get(*t).cloned() else {
                    continue;
                };
                if !primary.defs.contains_key(&name) {
                    continue;
                }
                // The module and what it EXTENDS in the tree.
                let mut chain: Vec<TlaModule> = vec![primary.clone()];
                let mut resolvable = true;
                let mut queue: Vec<String> = primary.extends.clone();
                let mut seen: BTreeSet<String> = BTreeSet::new();
                while let Some(e) = queue.pop() {
                    if !seen.insert(e.clone()) || TLA_STANDARD.contains(&e.as_str()) {
                        continue;
                    }
                    let found = files
                        .iter()
                        .filter(|f| f.ends_with(&format!("/{e}.tla")))
                        .min_by_key(|f| {
                            (f.rsplit_once('/').map_or("", |x| x.0) != dir, (*f).clone())
                        });
                    match found {
                        Some(p) => {
                            if !cache.contains_key(p)
                                && let Some(src) = tree.read(p)
                            {
                                cache.insert(p.clone(), parse_tla(p, &src));
                            }
                            match cache.get(p) {
                                Some(m) => {
                                    queue.extend(m.extends.clone());
                                    chain.push(m.clone());
                                }
                                None => resolvable = false,
                            }
                        }
                        None => resolvable = false,
                    }
                }
                let mut defs = BTreeMap::new();
                let mut declared = BTreeSet::new();
                let mut assumes = Vec::new();
                for m in &chain {
                    for (k, v) in &m.defs {
                        defs.entry(k.clone()).or_insert_with(|| v.clone());
                    }
                    declared.extend(m.declared.iter().cloned());
                    assumes.extend(m.assumes.iter().cloned());
                }
                let closure = if resolvable {
                    tla_closure(&name, &defs, &declared, &assumes)
                } else {
                    None
                };
                stmt = Some(match closure {
                    Some(c) => {
                        stats.tla_closure += 1;
                        format!("closure: {c}")
                    }
                    None => {
                        stats.tla_fallback += 1;
                        stats.tla_fallback_modules.insert(primary.path.clone());
                        let whole: Vec<&str> = chain.iter().map(|m| m.text.as_str()).collect();
                        format!("whole module: {}", whole.join(" ;; "))
                    }
                });
                break;
            }
            let text = stmt.unwrap_or_else(|| {
                stats.tla_undefined += 1;
                "<no definition found>".to_string()
            });
            out.push(Stmt::new(format!("{cfg}::{name}"), "tla-def", text));
        }
    }
    for rs in tree
        .files_under(MODELS_DIR)
        .iter()
        .filter(|f| f.ends_with(".rs"))
    {
        let Some(src) = tree.read(rs) else { continue };
        if let Some(s) = stateright_stmt(rs, &src, &mut stats) {
            out.push(s);
        }
    }
    (out, stats)
}

/// The tokens without `#[cfg(test)]` items and `#[test]` functions.
fn strip_tests(t: Vec<Tok>) -> Vec<Tok> {
    let mut out = Vec::with_capacity(t.len());
    let mut i = 0;
    while i < t.len() {
        if let Some(e) = attr_end(&t, i) {
            let name: String = t[i..e]
                .iter()
                .map(|x| x.s.as_str())
                .collect::<Vec<_>>()
                .join("");
            if name.contains("cfg(test)") || name == "#[test]" {
                // skip the attribute and the item after it
                let mut k = e;
                while let Some(e2) = attr_end(&t, k) {
                    k = e2;
                }
                let mut d = 0i32;
                while k < t.len() {
                    match t[k].s.as_str() {
                        "(" | "[" => d += 1,
                        ")" | "]" => d -= 1,
                        "{" if d <= 0 => {
                            k = match_brace(&t, k) + 1;
                            break;
                        }
                        ";" if d <= 0 => {
                            k += 1;
                            break;
                        }
                        _ => {}
                    }
                    k += 1;
                }
                i = k;
                continue;
            }
        }
        out.push(t[i].clone());
        i += 1;
    }
    out
}

/// Macros that expand to what they show: they hide no other function.
const SAFE_MACROS: &[&str] = &[
    "vec",
    "matches",
    "assert",
    "assert_eq",
    "assert_ne",
    "debug_assert",
    "format",
    "panic",
    "unreachable",
    "write",
    "writeln",
];

/// The whole `fn properties` of a model file with every function, method and constant of the same file it reaches by simple
/// name (the closure). Cannot be followed reliably (a macro, a `crate::`/`super::` path, a function brought in by `use`): the whole
/// file's non-test items instead.
fn stateright_stmt(path: &str, src: &str, stats: &mut ModelStats) -> Option<Stmt> {
    let t = strip_tests(lex(src, &[], &[]));
    if !t
        .windows(2)
        .any(|w| w[0].s == "fn" && w[1].s == "properties")
    {
        return None;
    }
    // every fn and const/static of the file, by name
    let mut items: BTreeMap<String, Vec<(usize, usize)>> = BTreeMap::new();
    let mut i = 0;
    while i + 1 < t.len() {
        let kw = t[i].s.as_str();
        let is_fn = t[i].ident && kw == "fn" && t[i + 1].ident;
        let is_const =
            t[i].ident && matches!(kw, "const" | "static") && t[i + 1].ident && t[i + 1].s != "fn";
        if is_fn || is_const {
            let mut k = i + 2;
            let mut d = 0i32;
            let mut end = t.len() - 1;
            while k < t.len() {
                match t[k].s.as_str() {
                    "(" | "[" => d += 1,
                    ")" | "]" => d -= 1,
                    "{" if is_fn && d <= 0 => {
                        end = match_brace(&t, k);
                        break;
                    }
                    "{" => d += 1,
                    "}" => d -= 1,
                    ";" if d <= 0 => {
                        end = k;
                        break;
                    }
                    _ => {}
                }
                k += 1;
            }
            items.entry(t[i + 1].s.clone()).or_default().push((i, end));
        }
        i += 1;
    }
    // names brought in by `use`
    let mut imported: BTreeSet<String> = BTreeSet::new();
    let mut i = 0;
    while i < t.len() {
        if t[i].ident
            && t[i].s == "use"
            && (i == 0 || matches!(t[i - 1].s.as_str(), ";" | "}" | "{" | "pub" | ")"))
        {
            let mut k = i + 1;
            let root = t.get(k).map_or("", |x| x.s.as_str());
            let skip = matches!(root, "std" | "core" | "alloc" | "stateright");
            while k < t.len() && t[k].s != ";" {
                if !skip
                    && t[k].ident
                    && t[k].s.chars().next().is_some_and(char::is_lowercase)
                    && !matches!(t[k].s.as_str(), "crate" | "self" | "super" | "as")
                {
                    imported.insert(t[k].s.clone());
                }
                k += 1;
            }
            i = k;
        }
        i += 1;
    }
    let mut visited: BTreeSet<String> = BTreeSet::new();
    let mut work = vec!["properties".to_string()];
    let mut reliable = true;
    while let Some(n) = work.pop() {
        if !visited.insert(n.clone()) {
            continue;
        }
        for &(a, b) in items.get(&n).map_or(&[][..], Vec::as_slice) {
            for k in a..=b.min(t.len() - 1) {
                let x = &t[k];
                if !x.ident {
                    continue;
                }
                if items.contains_key(&x.s) {
                    work.push(x.s.clone());
                    continue;
                }
                let next = t.get(k + 1).map(|y| y.s.as_str());
                let prev = k.checked_sub(1).map(|p| t[p].s.as_str());
                if next == Some("!")
                    && matches!(t.get(k + 2).map(|y| y.s.as_str()), Some("(" | "[" | "{"))
                    && !SAFE_MACROS.contains(&x.s.as_str())
                {
                    reliable = false;
                }
                if matches!(x.s.as_str(), "crate" | "super")
                    || (x.s == "self" && next == Some("::"))
                {
                    reliable = false;
                }
                if next == Some("(") && prev != Some(".") && imported.contains(&x.s) {
                    reliable = false;
                }
            }
        }
    }
    let text = if reliable {
        let mut parts: Vec<String> = Vec::new();
        for n in &visited {
            for &(a, b) in items.get(n).map_or(&[][..], Vec::as_slice) {
                parts.push(norm_tokens(&t[a..=b.min(t.len() - 1)]));
            }
        }
        parts.sort();
        parts.dedup();
        stats.sr_closure += 1;
        format!("closure: {}", parts.join("\n"))
    } else {
        stats.sr_fallback += 1;
        stats.sr_fallback_files.insert(path.to_string());
        format!("whole file: {}", norm_tokens(&t))
    };
    Some(Stmt::new(format!("{path}::properties"), "stateright", text))
}

// ----------------------------------------------------------------------------------------------------------------- mutants

/// Does the quoting of `s` leave a quote open at its end (bash: '..', $'..', "..", `\` continuation)?
fn bash_open(s: &str) -> bool {
    #[derive(PartialEq)]
    enum Q {
        No,
        Single,
        Ansi,
        Double,
    }
    let c: Vec<char> = s.chars().collect();
    let mut q = Q::No;
    let mut i = 0;
    while i < c.len() {
        match q {
            Q::No => match c[i] {
                '\\' => i += 1,
                '\'' => {
                    q = if i > 0 && c[i - 1] == '$' {
                        Q::Ansi
                    } else {
                        Q::Single
                    }
                }
                '"' => q = Q::Double,
                '#' if i == 0 || c[i - 1].is_whitespace() => {
                    while i < c.len() && c[i] != '\n' {
                        i += 1;
                    }
                    continue;
                }
                _ => {}
            },
            Q::Single => {
                if c[i] == '\'' {
                    q = Q::No;
                }
            }
            Q::Ansi => {
                if c[i] == '\\' {
                    i += 1;
                } else if c[i] == '\'' {
                    q = Q::No;
                }
            }
            Q::Double => {
                if c[i] == '\\' {
                    i += 1;
                } else if c[i] == '"' {
                    q = Q::No;
                }
            }
        }
        i += 1;
    }
    q != Q::No || s.trim_end().ends_with('\\')
}

/// Logical `try ...` lines of a mutants script, as written.
fn try_lines(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur: Option<String> = None;
    for l in src.lines() {
        match cur.take() {
            Some(mut c) => {
                c.push('\n');
                c.push_str(l);
                if bash_open(&c) {
                    cur = Some(c);
                } else {
                    out.push(c.trim().to_string());
                }
            }
            None => {
                if l.trim_start().starts_with("try ") {
                    if bash_open(l.trim_start()) {
                        cur = Some(l.trim_start().to_string());
                    } else {
                        out.push(l.trim().to_string());
                    }
                }
            }
        }
    }
    if let Some(c) = cur {
        out.push(c.trim().to_string());
    }
    out
}

fn keyed_lines(path: &str, kind: &'static str, lines: Vec<String>) -> Vec<Stmt> {
    let mut seen: BTreeMap<u64, usize> = BTreeMap::new();
    lines
        .into_iter()
        .map(|l| {
            let h = fnv1a(&l);
            let n = seen.entry(h).or_insert(0);
            *n += 1;
            let key = if *n == 1 {
                format!("{path}::{}", &hex(h)[..8])
            } else {
                format!("{path}::{}#{n}", &hex(h)[..8])
            };
            Stmt::new(key, kind, l)
        })
        .collect()
}

fn mutations_array(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut inside = false;
    for l in src.lines() {
        let t = l.trim();
        if !inside {
            inside = t.starts_with("mutations=(");
            continue;
        }
        if t == ")" {
            break;
        }
        if !t.is_empty() && !t.starts_with('#') {
            out.push(t.to_string());
        }
    }
    out
}

fn mutant_stmts(tree: &dyn Tree, units: &[Unit]) -> Vec<Stmt> {
    let mut scripts: BTreeSet<String> = tree
        .files_under(VERIFICATION_DIR)
        .into_iter()
        .filter(|f| f.ends_with("/mutants.sh"))
        .collect();
    for u in units
        .iter()
        .filter(|u| u.kind == "mutants" && u.target.ends_with(".sh"))
    {
        scripts.insert(u.target.clone());
    }
    let mut out = Vec::new();
    for s in &scripts {
        if let Some(src) = tree.read(s) {
            out.extend(keyed_lines(s, "mutant", try_lines(&src)));
        }
    }
    if let Some(src) = tree.read(MUTATIONS_SH) {
        out.extend(keyed_lines(MUTATIONS_SH, "mutation", mutations_array(&src)));
    }
    out
}

// -------------------------------------------------------------------------------------------------------------------- trees

pub trait Tree {
    fn read(&self, path: &str) -> Option<String>;
    /// Every file below `dir` (repo-relative, `/`-separated), sorted.
    fn files_under(&self, dir: &str) -> Vec<String>;
}

pub struct FsTree(pub PathBuf);

impl Tree for FsTree {
    fn read(&self, path: &str) -> Option<String> {
        fs::read_to_string(self.0.join(path)).ok()
    }
    fn files_under(&self, dir: &str) -> Vec<String> {
        fn walk(root: &Path, rel: &str, out: &mut Vec<String>) {
            let Ok(rd) = fs::read_dir(root.join(rel)) else {
                return;
            };
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if matches!(name.as_str(), "target" | ".git" | "node_modules") {
                    continue;
                }
                let p = format!("{rel}/{name}");
                match e.file_type() {
                    Ok(t) if t.is_dir() => walk(root, &p, out),
                    Ok(t) if t.is_file() => out.push(p),
                    _ => {}
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.0, dir, &mut out);
        out.sort();
        out
    }
}

pub struct GitTree {
    pub root: PathBuf,
    pub rev: String,
}

impl Tree for GitTree {
    fn read(&self, path: &str) -> Option<String> {
        let o = Command::new("git")
            .args(["show", &format!("{}:{path}", self.rev)])
            .current_dir(&self.root)
            .output()
            .ok()?;
        o.status
            .success()
            .then(|| String::from_utf8_lossy(&o.stdout).into_owned())
    }
    fn files_under(&self, dir: &str) -> Vec<String> {
        let o = Command::new("git")
            .args(["ls-tree", "-r", "--name-only", &self.rev, "--", dir])
            .current_dir(&self.root)
            .output();
        let mut v: Vec<String> = match o {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(str::to_string)
                .collect(),
            _ => Vec::new(),
        };
        v.sort();
        v
    }
}

// ---------------------------------------------------------------------------------------------------------------- snapshot

#[derive(Default)]
pub struct Snapshot {
    pub stmts: Vec<Stmt>,
    pub units: usize,
    pub files: usize,
    pub header_kw: usize,
    pub body_kw: usize,
    pub stray: Vec<String>,
    pub models: ModelStats,
}

impl Snapshot {
    pub fn count(&self, f: impl Fn(&Stmt) -> bool) -> usize {
        self.stmts.iter().filter(|s| f(s)).count()
    }
    pub fn summary(&self) -> String {
        let m = &self.models;
        let mut s = format!(
            "{} statements: {} fn, {} items, {} models, {} mutant lines, in {} files of {} units; unextracted: {}",
            self.stmts.len(),
            self.count(|s| s.kind.ends_with("fn")),
            self.count(|s| s.kind == "item"),
            self.count(|s| matches!(s.kind, "tla-cfg" | "tla-def" | "stateright")),
            self.count(Stmt::is_mutant),
            self.files,
            self.units,
            self.body_kw
        );
        s.push_str(&format!(
            "\nspec: models: TLA+ closure-covered {} fallback {} undefined {}; Stateright closure-covered {} fallback {}",
            m.tla_closure, m.tla_fallback, m.tla_undefined, m.sr_closure, m.sr_fallback
        ));
        let fb: Vec<&String> = m
            .tla_fallback_modules
            .iter()
            .chain(m.sr_fallback_files.iter())
            .collect();
        if !fb.is_empty() {
            s.push_str(&format!(
                "\nspec: whole-module fallback used by: {}",
                fb.iter().map(|x| x.as_str()).collect::<Vec<_>>().join(", ")
            ));
        }
        s
    }
}

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

/// Files named by `#[path = "..."]` lines, as repo-relative paths.
fn path_refs(file: &str, src: &str) -> Vec<String> {
    let dir = file.rsplit_once('/').map_or("", |x| x.0);
    let mut v = Vec::new();
    for line in src.lines() {
        let l = line.trim_start();
        let l = l.strip_prefix("//@").map_or(l, str::trim_start);
        if l.starts_with("//") {
            continue;
        }
        if let Some(i) = l.find("#[path") {
            let rest = &l[i..];
            if let Some(a) = rest.find('"')
                && let Some(b) = rest[a + 1..].find('"')
                && let Some(p) = normalize(&format!("{dir}/{}", &rest[a + 1..a + 1 + b]))
            {
                v.push(p);
            }
        }
    }
    v
}

pub fn collect(tree: &dyn Tree, units: &[Unit]) -> Snapshot {
    let mut snap = Snapshot::default();
    let mut map: BTreeMap<String, Stmt> = BTreeMap::new();
    let mut queue: Vec<String> = Vec::new();
    for u in units
        .iter()
        .filter(|u| matches!(u.kind.as_str(), "verus" | "shipped"))
    {
        snap.units += 1;
        if let Some(f) = u.target.split_whitespace().find(|w| w.ends_with(".rs")) {
            queue.push(f.to_string());
        }
    }
    let mut seen: BTreeSet<String> = BTreeSet::new();
    while let Some(p) = queue.pop() {
        if !seen.insert(p.clone()) {
            continue;
        }
        let Some(src) = tree.read(&p) else { continue };
        let fs_ = scan_file(&p, &src);
        snap.files += 1;
        snap.header_kw += fs_.header_kw;
        snap.body_kw += fs_.body_kw;
        snap.stray.extend(fs_.stray);
        for s in fs_.stmts {
            map.insert(s.key.clone(), s);
        }
        queue.extend(path_refs(&p, &src));
    }
    let (model, stats) = model_stmts(tree);
    snap.models = stats;
    for s in model.into_iter().chain(mutant_stmts(tree, units)) {
        map.insert(s.key.clone(), s);
    }
    snap.stmts = map.into_values().collect();
    snap
}

// ----------------------------------------------------------------------------------------------------------------- verdict

pub struct Diff {
    pub changed: Vec<(Stmt, Stmt)>,
    pub removed: Vec<Stmt>,
    pub added: usize,
    pub same: usize,
}

pub fn diff(base: &[Stmt], work: &[Stmt]) -> Diff {
    let w: BTreeMap<&str, &Stmt> = work.iter().map(|s| (s.key.as_str(), s)).collect();
    let b: BTreeSet<&str> = base.iter().map(|s| s.key.as_str()).collect();
    let mut d = Diff {
        changed: Vec::new(),
        removed: Vec::new(),
        added: work.iter().filter(|s| !b.contains(s.key.as_str())).count(),
        same: 0,
    };
    for s in base {
        match w.get(s.key.as_str()) {
            Some(n) if n.hash == s.hash => d.same += 1,
            Some(n) => d.changed.push((s.clone(), (*n).clone())),
            None => d.removed.push(s.clone()),
        }
    }
    d
}

/// Where an allow reason came from.
#[derive(Debug, Clone, Default)]
pub struct Allow {
    /// `LAGENT_ALLOW_SPEC_CHANGE`: set by `lagent merge --allow spec-change=...`.
    pub merge: Option<String>,
    /// `--allow` or `LUCINA_SPEC_CHANGE_ALLOW`: a developer's local run only.
    pub local: Option<(String, &'static str)>,
    /// A local allow that was present but not honoured (`--merge-gate`).
    pub ignored_local: Option<&'static str>,
}

impl Allow {
    pub fn read(
        get: &dyn Fn(&str) -> Option<String>,
        flag: Option<String>,
        merge_gate: bool,
    ) -> Allow {
        let clean = |s: Option<String>| s.map(|x| x.trim().to_string()).filter(|x| !x.is_empty());
        let merge = clean(get(MERGE_ENV));
        let mut local = clean(flag)
            .map(|r| (r, "--allow"))
            .or_else(|| clean(get(LOCAL_ENV)).map(|r| (r, LOCAL_ENV)));
        let mut ignored_local = None;
        if merge_gate {
            ignored_local = local.take().map(|(_, s)| s);
        }
        Allow {
            merge,
            local,
            ignored_local,
        }
    }
    /// The reason that lets the change pass and the name of the source that gave it. The merge-time source wins.
    pub fn effective(&self) -> Option<(&str, &str)> {
        if let Some(r) = &self.merge {
            return Some((r, MERGE_ENV));
        }
        self.local.as_ref().map(|(r, s)| (r.as_str(), *s))
    }
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if let Some(r) = &self.merge {
            parts.push(format!(
                "saw {MERGE_ENV}=\"{r}\" (set by lagent merge --allow spec-change)"
            ));
        }
        if let Some((r, s)) = &self.local {
            parts.push(format!("saw {s}=\"{r}\" (local run)"));
        }
        if let Some(s) = self.ignored_local {
            parts.push(format!(
                "ignored {s}: the merge gate honours only {MERGE_ENV}"
            ));
        }
        if self.merge.is_some() && self.local.is_some() {
            parts.push(format!("both set: {MERGE_ENV} takes precedence"));
        }
        if parts.is_empty() {
            format!("allow: none ({MERGE_ENV}, {LOCAL_ENV} and --allow are all unset or empty)")
        } else {
            format!("allow: {}", parts.join("; "))
        }
    }
}

fn trunc(s: &str) -> String {
    if s.chars().count() <= 600 {
        s.to_string()
    } else {
        format!("{} ...[truncated]", s.chars().take(600).collect::<String>())
    }
}

fn entry(out: &mut String, tag: &str, base: &Stmt, new: Option<&Stmt>) {
    out.push_str(&format!("{tag} {} [{}]\n", base.key, base.kind));
    out.push_str(&format!("    base: {}\n", trunc(&base.text)));
    match new {
        Some(n) => out.push_str(&format!("    new : {}\n", trunc(&n.text))),
        None => out.push_str("    new : (gone)\n"),
    }
}

/// The report and the exit code (0 pass, 1 refused).
pub fn render(d: &Diff, allow: &Allow, summary: &str) -> (String, i32) {
    let spec_changed: Vec<&(Stmt, Stmt)> =
        d.changed.iter().filter(|(b, _)| !b.is_mutant()).collect();
    let spec_removed: Vec<&Stmt> = d.removed.iter().filter(|b| !b.is_mutant()).collect();
    let mut_changed: Vec<&(Stmt, Stmt)> = d.changed.iter().filter(|(b, _)| b.is_mutant()).collect();
    let mut_removed: Vec<&Stmt> = d.removed.iter().filter(|b| b.is_mutant()).collect();
    let spec_n = spec_changed.len() + spec_removed.len();
    let mut_n = mut_changed.len() + mut_removed.len();
    if spec_n + mut_n == 0 {
        return (
            format!(
                "spec: frozen statements intact ({} unchanged, {} new, 0 changed, 0 removed)\nspec: {summary}\n",
                d.same, d.added
            ),
            0,
        );
    }
    let mut out = String::new();
    out.push_str("================ SPEC CHANGE ================\n");
    out.push_str(&format!(
        "{spec_n} specification statement(s) and {mut_n} mutant line(s) changed or removed against the base commit.\n"
    ));
    if spec_n > 0 {
        out.push_str(&format!("\n---- SPEC CHANGES ({spec_n}) ----\n"));
        for (b, n) in &spec_changed {
            entry(&mut out, "CHANGED", b, Some(n));
        }
        for b in &spec_removed {
            entry(&mut out, "REMOVED", b, None);
        }
    }
    if mut_n > 0 {
        out.push_str(&format!("\n---- MUTANT CHANGES ({mut_n}) ----\n"));
        for (b, n) in &mut_changed {
            entry(&mut out, "CHANGED", b, Some(n));
        }
        for b in &mut_removed {
            entry(&mut out, "REMOVED", b, None);
        }
    }
    out.push('\n');
    out.push_str(&allow.describe());
    out.push('\n');
    match allow.effective() {
        Some((reason, src)) => {
            out.push_str(&format!("SPEC CHANGE ALLOWED: {reason}  [{src}]\n"));
            (out, 0)
        }
        None => {
            out.push_str(
                "Merge needs: lagent merge --allow spec-change=\"<why>\" (reviewer must read the statements above)\n",
            );
            (out, 1)
        }
    }
}

// ------------------------------------------------------------------------------------------------------------------ command

fn git_out(root: &Path, args: &[&str]) -> Option<String> {
    let o = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .ok()?;
    o.status
        .success()
        .then(|| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
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

fn val(args: &[String], k: &str) -> Option<String> {
    args.iter()
        .position(|a| a == k)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

pub fn cmd(args: &[String]) -> Result<i32, String> {
    let has = |k: &str| args.iter().any(|a| a == k);
    let top = git_out(Path::new("."), &["rev-parse", "--show-toplevel"])
        .ok_or("not inside a git repository")?;
    let root = PathBuf::from(top);
    let units = inventory::load(&root)?;
    let work = collect(&FsTree(root.clone()), &units);
    if has("--list") {
        for s in &work.stmts {
            if has("--json") {
                println!(
                    "{{\"key\":{},\"kind\":{},\"hash\":\"{}\",\"text\":{}}}",
                    json_str(&s.key),
                    json_str(s.kind),
                    hex(s.hash),
                    json_str(&s.text)
                );
            } else {
                println!(
                    "{}\t{}\t{}\t{}",
                    s.key,
                    s.kind,
                    hex(s.hash),
                    s.text.replace('\n', "\\n").replace('\t', "\\t")
                );
            }
        }
        eprintln!("spec: {}", work.summary());
        return Ok(if work.stray.is_empty() { 0 } else { 2 });
    }
    if !has("--check") {
        return Err(
            "usage: spec --check [--base REF] [--allow WHY] [--merge-gate] | spec --list [--json]"
                .into(),
        );
    }
    if !work.stray.is_empty() {
        for s in &work.stray {
            eprintln!("spec: clause keyword outside any function this scan extracted: {s}");
        }
        eprintln!(
            "spec: the extractor missed a verified function; fix tools/ci/lucina-ci/src/spec.rs before trusting a pass"
        );
        return Ok(2);
    }
    let base_ref = val(args, "--base").unwrap_or_else(|| {
        if git_out(
            &root,
            &["rev-parse", "--verify", "-q", "origin/main^{commit}"],
        )
        .is_some()
        {
            "origin/main".into()
        } else {
            "main".into()
        }
    });
    let resolved = git_out(
        &root,
        &[
            "rev-parse",
            "--verify",
            "-q",
            &format!("{base_ref}^{{commit}}"),
        ],
    )
    .ok_or_else(|| format!("base {base_ref} not found"))?;
    let base = git_out(&root, &["merge-base", &resolved, "HEAD"]).unwrap_or(resolved);
    let gt = GitTree {
        root: root.clone(),
        rev: base.clone(),
    };
    let base_units = match gt.read(inventory::INVENTORY) {
        Some(t) => inventory::from_tsv(&t)?,
        None => units.clone(),
    };
    let b = collect(&gt, &base_units);
    let d = diff(&b.stmts, &work.stmts);
    let allow = Allow::read(
        &|k| std::env::var(k).ok(),
        val(args, "--allow"),
        has("--merge-gate"),
    );
    let short = &base[..base.len().min(10)];
    let summary = format!("against {short}: {}", work.summary());
    let (text, code) = render(&d, &allow, &summary);
    print!("{text}");
    Ok(code)
}

// -------------------------------------------------------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    struct Mem(BTreeMap<String, String>);
    impl Mem {
        fn new(files: &[(&str, &str)]) -> Mem {
            Mem(files
                .iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect())
        }
    }
    impl Tree for Mem {
        fn read(&self, p: &str) -> Option<String> {
            self.0.get(p).cloned()
        }
        fn files_under(&self, dir: &str) -> Vec<String> {
            self.0
                .keys()
                .filter(|k| k.starts_with(&format!("{dir}/")))
                .cloned()
                .collect()
        }
    }

    fn unit(kind: &str, target: &str) -> Unit {
        Unit {
            name: format!("{kind}/{target}"),
            group: "g".into(),
            kind: kind.into(),
            target: target.into(),
            inputs: vec![],
        }
    }

    fn snap_of(files: &[(&str, &str)], units: &[Unit]) -> Snapshot {
        collect(&Mem::new(files), units)
    }

    fn verus_unit() -> Vec<Unit> {
        vec![unit("verus", "verification/x/rules.rs")]
    }

    fn rules(body: &str) -> String {
        format!("use vstd::prelude::*;\nverus! {{\n{body}\n}}\n")
    }

    fn nn(s: &str) -> String {
        norm_tokens(&lex(s, &[], &[]))
    }

    fn find<'a>(s: &'a Snapshot, key: &str) -> &'a Stmt {
        s.stmts.iter().find(|x| x.key == key).unwrap_or_else(|| {
            panic!(
                "no {key} in {:?}",
                s.stmts.iter().map(|x| &x.key).collect::<Vec<_>>()
            )
        })
    }

    const BASE: &str = "/// doc\npub fn dist(a: u32, b: u32) -> (r: u32)\n    requires a >= 1, b <= 100,\n    ensures r < 100 && r >= a,\n{\n    let x = a + b;\n    x\n}\n\npub open spec fn good(x: int) -> bool {\n    x > 0 && x < 10\n}\n";

    fn run(src: &str) -> Snapshot {
        let r = rules(src);
        snap_of(&[("verification/x/rules.rs", &r)], &verus_unit())
    }

    fn verdict(base: &str, work: &str) -> Diff {
        let b = run(base);
        let w = run(work);
        assert!(
            b.stray.is_empty() && w.stray.is_empty(),
            "{:?} {:?}",
            b.stray,
            w.stray
        );
        diff(&b.stmts, &w.stmts)
    }

    #[test]
    fn formatting_and_comments_do_not_matter() {
        let reformatted = "// c\npub fn dist(a:u32,b:u32)->(r:u32)\n   requires\n      a>=1,\n      b<=100\n   ,\n   ensures /* why */ r<100&&r>=a\n{ let y = 1; y }\n\npub open spec fn good(x:int)->bool{\n  x>0 && /* c */ x<10 }\n";
        let d = verdict(BASE, reformatted);
        assert!(
            d.changed.is_empty() && d.removed.is_empty(),
            "{:?}",
            d.changed
        );
        assert_eq!(d.same, 2);
    }

    #[test]
    fn weakened_ensures_is_changed() {
        let d = verdict(BASE, &BASE.replace("r < 100 &&", "r <= 100 &&"));
        assert_eq!(d.changed.len(), 1);
        let d = verdict(BASE, &BASE.replace("r < 100 && r >= a", "r < 100"));
        assert_eq!(d.changed.len(), 1);
        assert!(d.changed[0].0.key.ends_with("::dist"));
    }

    #[test]
    fn removed_requires_is_changed() {
        let d = verdict(BASE, &BASE.replace("requires a >= 1, b <= 100,\n", ""));
        assert_eq!(d.changed.len(), 1);
    }

    #[test]
    fn exec_body_change_is_free_spec_body_change_is_not() {
        let d = verdict(
            BASE,
            &BASE.replace("let x = a + b;\n    x", "let x = a * 2;\n    x + 1"),
        );
        assert!(d.changed.is_empty() && d.removed.is_empty());
        let d = verdict(BASE, &BASE.replace("x > 0 && x < 10", "x > 0 && x < 100"));
        assert_eq!(d.changed.len(), 1);
        assert!(d.changed[0].0.key.ends_with("::good"));
    }

    #[test]
    fn new_fn_is_free_removed_fn_is_removed() {
        let more = format!("{BASE}\npub proof fn extra(a: int)\n    ensures a == a,\n{{\n}}\n");
        let d = verdict(BASE, &more);
        assert!(d.changed.is_empty() && d.removed.is_empty());
        assert_eq!(d.added, 1);
        let d = verdict(&more, BASE);
        assert_eq!(d.removed.len(), 1);
        assert!(d.removed[0].key.ends_with("::extra"));
    }

    #[test]
    fn deleted_unit_is_removed() {
        let r = rules(BASE);
        let b = snap_of(&[("verification/x/rules.rs", &r)], &verus_unit());
        let w = snap_of(&[], &[]);
        assert_eq!(diff(&b.stmts, &w.stmts).removed.len(), 2);
    }

    #[test]
    fn match_in_ensures_header_is_parsed() {
        let src = "/// c\npub const SLOTS: usize = 10;\n\npub fn slot_by_key(active: &[bool; SLOTS], keys: &[i32; SLOTS], used: &[bool; SLOTS], n: usize, key: i32) -> Option<usize> //@ret\n//@ requires n <= SLOTS,\n//@ ensures match ret {\n//@     Some(i) => i < n && ((active[i as int] && keys[i as int] == key) || (!active[i as int] && !used[i as int])),\n//@     None => forall|j: int| 0 <= j < n ==> !(active[j] && keys[j] == key) && (active[j] || used[j]),\n//@ },\n//@     (exists|j: int| 0 <= j < n && active[j] && keys[j] == key) ==> (ret.is_some() && active[ret.unwrap() as int]),\n{\n    let mut i = 0;\n    while i < n\n    //@ invariant n <= SLOTS, i <= n, forall|j: int| 0 <= j < i ==> !(active[j] && keys[j] == key),\n    //@ decreases n - i,\n    {\n        if active[i] && keys[i] == key {\n            return Some(i);\n        }\n        i += 1;\n    }\n    None\n}\n";
        let s = snap_of(
            &[("crates/c/src/tp_rules.rs", src)],
            &[unit("shipped", "crates/c/src/tp_rules.rs")],
        );
        assert!(s.stray.is_empty(), "{:?}", s.stray);
        let h = find(&s, "crates/c/src/tp_rules.rs::slot_by_key");
        assert!(h.text.contains(&nn("(ret: Option<usize>)")), "{}", h.text);
        assert!(
            h.text.contains(&nn("ensures match ret { Some(i) =>")),
            "{}",
            h.text
        );
        assert!(h.text.contains(&nn("None => forall|j: int|")), "{}", h.text);
        assert!(h.text.contains(&nn("==> (ret.is_some()")), "{}", h.text);
        assert!(
            !h.text.contains("invariant") && !h.text.contains("let mut"),
            "{}",
            h.text
        );
        assert!(
            find(&s, "crates/c/src/tp_rules.rs::SLOTS")
                .text
                .contains("10")
        );
        assert_eq!(s.body_kw, 1, "the loop's decreases is unextracted");
        // a weakened ensures in the match arm is a change
        let weak = src.replace(
            "None => forall|j: int| 0 <= j < n",
            "None => forall|j: int| 0 <= j < n - 1",
        );
        let w = snap_of(
            &[("crates/c/src/tp_rules.rs", &weak)],
            &[unit("shipped", "crates/c/src/tp_rules.rs")],
        );
        assert_eq!(diff(&s.stmts, &w.stmts).changed.len(), 1);
        // and a changed exec body is not
        let body = src.replace("i += 1;", "i += 2;");
        let w = snap_of(
            &[("crates/c/src/tp_rules.rs", &body)],
            &[unit("shipped", "crates/c/src/tp_rules.rs")],
        );
        let d = diff(&s.stmts, &w.stmts);
        assert!(d.changed.is_empty() && d.removed.is_empty());
    }

    #[test]
    fn shipped_spec_fn_body_and_one_line_ret() {
        let src = "//@ pub open spec fn used(wr: u32, rd: u32) -> int {\n//@     if wr >= rd { (wr - rd) as int } else { 0 }\n//@ }\n\npub fn fold(b: u8) -> u8 //@ret\n//@ ensures ret == 1,\n{ b }\n";
        let u = [unit("shipped", "a.rs")];
        let s = snap_of(&[("a.rs", src)], &u);
        assert!(s.stray.is_empty(), "{:?}", s.stray);
        assert!(
            find(&s, "a.rs::used").text.contains(&nn("else { 0 }")),
            "{}",
            find(&s, "a.rs::used").text
        );
        assert!(
            find(&s, "a.rs::fold")
                .text
                .ends_with(&nn("ensures ret == 1")),
            "{}",
            find(&s, "a.rs::fold").text
        );
        let w = snap_of(&[("a.rs", &src.replace("{ b }", "{ b + 0 }"))], &u);
        assert!(diff(&s.stmts, &w.stmts).changed.is_empty());
        let w = snap_of(
            &[(
                "a.rs",
                &src.replace("(wr - rd) as int", "(wr - rd + 1) as int"),
            )],
            &u,
        );
        assert_eq!(diff(&s.stmts, &w.stmts).changed.len(), 1);
    }

    #[test]
    fn verus_block_of_juno_ring_rules() {
        let src = "#![feature(proc_macro_hygiene)]\n#[path = \"../../kernel/lucina-ring/src/lib.rs\"]\npub mod ring;\n\nuse ring::*;\nuse vstd::prelude::*;\n\nverus! {\n\n/// Where position `pos` lives.\npub open spec fn slot(pos: int, cap: usize) -> (int, int) {\n    (page_of(pos, cap), pos % PAGE as int)\n}\n\n/// The ring's state.\npub struct Ring {\n    pub head: usize,\n    pub tail: usize,\n    pub contents: Seq<u8>,\n    pub storage: spec_fn((int, int)) -> u8,\n}\n\n/// A write of `data` at `pos`.\npub open spec fn store(s: spec_fn((int, int)) -> u8, pos: usize, data: Seq<u8>, cap: usize) -> spec_fn((int, int)) -> u8\n    decreases data.len(),\n{\n    if data.len() == 0 || !cap_ok(cap) {\n        s\n    } else {\n        let (page, off, n) = piece(pos, data.len() as int, cap);\n        store(put(s, page, off, data.take(n)), at(pos, n) as usize, data.skip(n), cap)\n    }\n}\n\n}\n";
        let lib = "pub const PAGE: usize = 4096;\npub fn cap_ok(cap: usize) -> bool //@ret\n//@ ensures ret == (cap > 0),\n{ cap > 0 }\n";
        let files = [
            ("verification/juno-ring/rules.rs", src),
            ("kernel/lucina-ring/src/lib.rs", lib),
        ];
        let s = snap_of(&files, &[unit("verus", "verification/juno-ring/rules.rs")]);
        assert!(s.stray.is_empty(), "{:?}", s.stray);
        assert_eq!(s.files, 2, "the #[path] file is followed");
        let k = "verification/juno-ring/rules.rs::";
        assert_eq!(find(&s, &format!("{k}slot")).kind, "spec fn");
        assert_eq!(find(&s, &format!("{k}Ring")).kind, "item");
        let st = find(&s, &format!("{k}store"));
        assert!(
            st.text.contains(&nn("decreases data.len()")) && st.text.contains(&nn("data.skip(n)")),
            "{}",
            st.text
        );
        assert!(
            find(&s, "kernel/lucina-ring/src/lib.rs::PAGE")
                .text
                .contains("4096")
        );
        assert!(
            find(&s, "kernel/lucina-ring/src/lib.rs::cap_ok")
                .text
                .contains(&nn("ensures ret == (cap > 0)"))
        );
        // a field of the verified state is part of the statements
        let w = src.replace("pub tail: usize,", "pub tail: u32,");
        let s2 = snap_of(
            &[
                ("verification/juno-ring/rules.rs", &w),
                ("kernel/lucina-ring/src/lib.rs", lib),
            ],
            &[unit("verus", "verification/juno-ring/rules.rs")],
        );
        assert_eq!(diff(&s.stmts, &s2.stmts).changed.len(), 1);
        // a pub const that defines a range is part of them
        let l2 = lib.replace("4096", "8192");
        let s3 = snap_of(
            &[
                ("verification/juno-ring/rules.rs", src),
                ("kernel/lucina-ring/src/lib.rs", &l2),
            ],
            &[unit("verus", "verification/juno-ring/rules.rs")],
        );
        assert_eq!(diff(&s.stmts, &s3.stmts).changed.len(), 1);
    }

    #[test]
    fn missed_function_is_a_stray() {
        // `requires` after a macro-ish construct the scan does not know: outside any fn
        let s = run("proof fn ok()\n    ensures true,\n{\n}\nsome_macro! { requires x }\n");
        assert_eq!(s.stray.len(), 1, "{:?}", s.stray);
    }

    #[test]
    fn struct_literal_in_ensures_is_not_the_body() {
        let src = "pub fn mk(a: u32) -> (r: P)\n    ensures r == P { x: a, y: 0 },\n{\n    P { x: a, y: 0 }\n}\n";
        let s = run(src);
        let t = &find(&s, "verification/x/rules.rs::mk").text;
        assert!(t.ends_with(&nn("ensures r == P { x: a, y: 0 }")), "{t}");
    }

    #[test]
    fn tla_cfg_and_invariants() {
        let cfg = "\\* a comment\nSPECIFICATION Spec\nCONSTANTS\n  MaxResets = 3\nCHECK_DEADLOCK FALSE\nINVARIANT\n  Safety\n  TypeOK\nPROPERTY Live\n";
        let tla = "---- MODULE M ----\nEXTENDS Naturals\nTypeOK == x \\in Nat\n\nSafety ==\n  /\\ x < 3\n  /\\ y = 1\n\nOther == TRUE\n====\n";
        assert_eq!(cfg_names(cfg), vec!["Safety", "TypeOK", "Live"]);
        assert_eq!(
            tla_def(tla, "Safety").unwrap(),
            "Safety == /\\ x < 3 /\\ y = 1"
        );
        let m = Mem::new(&[
            ("userspace/formal/M.cfg", cfg),
            ("userspace/formal/M.tla", tla),
        ]);
        let a = collect(&m, &[]);
        assert_eq!(a.stmts.iter().filter(|s| s.kind == "tla-cfg").count(), 1);
        assert_eq!(
            find(&a, "userspace/formal/M.cfg::Live").text,
            "<no definition found>"
        );
        let weak = tla.replace("/\\ y = 1", "");
        let m2 = Mem::new(&[
            ("userspace/formal/M.cfg", cfg),
            ("userspace/formal/M.tla", &weak),
        ]);
        let b = collect(&m2, &[]);
        let d = diff(&a.stmts, &b.stmts);
        assert_eq!(d.changed.len(), 1);
        assert!(d.changed[0].0.key.ends_with("::Safety"));
        // dropping the invariant from the cfg changes the cfg and removes its definition
        let m3 = Mem::new(&[
            ("userspace/formal/M.cfg", &cfg.replace("  Safety\n", "")),
            ("userspace/formal/M.tla", tla),
        ]);
        let d = diff(&a.stmts, &collect(&m3, &[]).stmts);
        assert_eq!(d.changed.len(), 1);
        assert_eq!(d.removed.len(), 1);
    }

    #[test]
    fn stateright_properties_are_fingerprinted() {
        let src = "impl Model for M {\n    fn properties(&self) -> Vec<Property<Self>> {\n        vec![Property::always(\"safe\", |_, s: &S| s.x < 3), Property::sometimes(\"done\", |_, s: &S| s.done)]\n    }\n}\n";
        let one = |s: &str| {
            let mut st = ModelStats::default();
            stateright_stmt("specs/lucina-models/src/m.rs", s, &mut st).unwrap()
        };
        let a = one(src);
        let formatted = src
            .replace("s.x < 3", "s.x<3")
            .replace("vec![", "vec![ // c\n");
        assert_eq!(one(&formatted).hash, a.hash);
        let weak = src.replace("s.x < 3", "s.x < 4");
        assert_ne!(one(&weak).hash, a.hash);
    }

    const SR: &str = "use stateright::*;\nconst LIMIT: u8 = 3;\nfn bound(s: &S) -> bool { s.x < LIMIT }\nfn unrelated(s: &S) -> bool { s.y > 1 }\nimpl Model for M {\n    fn properties(&self) -> Vec<Property<Self>> {\n        vec![Property::always(\"safe\", |_, s: &S| bound(s))]\n    }\n}\n#[cfg(test)]\nmod tests { fn t() { assert!(true); } }\n";

    fn sr_snap(src: &str) -> Snapshot {
        snap_of(&[("specs/lucina-models/src/m.rs", src)], &[])
    }

    #[test]
    fn stateright_helper_change_is_changed_unreferenced_is_not() {
        let a = sr_snap(SR);
        assert_eq!(a.models.sr_closure, 1);
        assert_eq!(a.models.sr_fallback, 0);
        // a helper the property calls
        let b = sr_snap(&SR.replace("s.x < LIMIT", "s.x <= LIMIT"));
        let d = diff(&a.stmts, &b.stmts);
        assert_eq!(d.changed.len(), 1);
        let (t, code) = render(&d, &Allow::default(), "s");
        assert_eq!(code, 1);
        assert!(t.contains("CHANGED specs/lucina-models/src/m.rs::properties"));
        // a constant the helper reads
        let c = sr_snap(&SR.replace("LIMIT: u8 = 3", "LIMIT: u8 = 30"));
        assert_eq!(diff(&a.stmts, &c.stmts).changed.len(), 1);
        // a function nothing reaches, and the test module
        let e = sr_snap(
            &SR.replace("s.y > 1", "s.y > 2")
                .replace("assert!(true)", "assert!(false)"),
        );
        let d = diff(&a.stmts, &e.stmts);
        assert!(d.changed.is_empty() && d.removed.is_empty());
    }

    #[test]
    fn stateright_fallback_to_the_whole_file() {
        // `bound` is brought in by `use`: its body is in another file, the closure cannot be followed
        let src = "use crate::helpers::bound;\nfn unrelated(s: &S) -> bool { s.y > 1 }\nimpl Model for M {\n    fn properties(&self) -> Vec<Property<Self>> {\n        vec![Property::always(\"safe\", |_, s: &S| bound(s))]\n    }\n}\n#[cfg(test)]\nmod tests { fn t() {} }\n";
        let a = sr_snap(src);
        assert_eq!(a.models.sr_fallback, 1);
        assert!(
            a.models
                .sr_fallback_files
                .contains("specs/lucina-models/src/m.rs")
        );
        assert!(
            a.summary()
                .contains("whole-module fallback used by: specs/lucina-models/src/m.rs")
        );
        // now anything non-test in the file counts
        let b = sr_snap(&src.replace("s.y > 1", "s.y > 2"));
        assert_eq!(diff(&a.stmts, &b.stmts).changed.len(), 1);
        let c = sr_snap(&src.replace("fn t() {}", "fn t() { let _ = 1; }"));
        assert!(diff(&a.stmts, &c.stmts).changed.is_empty());
        // a macro the scan cannot see through is a fallback too
        let m = sr_snap("impl Model for M { fn properties(&self) -> V { custom!(a) } }\n");
        assert_eq!(m.models.sr_fallback, 1);
    }

    const CFG: &str = "SPECIFICATION Spec\nINVARIANT\n  Safety\n";

    fn tla_snap(files: &[(&str, &str)]) -> Snapshot {
        let mut v: Vec<(&str, &str)> = vec![("userspace/formal/M.cfg", CFG)];
        v.extend_from_slice(files);
        snap_of(&v, &[])
    }

    #[test]
    fn tla_helper_change_is_changed_unreferenced_is_not() {
        let tla = "---- MODULE M ----\nEXTENDS Naturals\nVARIABLES x\nBound(n) == n < 3\nDeep == Bound(x)\nSafety == Deep /\\ x \\in Nat\nOther == x = 7\nSpec == Init /\\ [][Next]_x\nInit == x = 0\nNext == x' = x\n====\n";
        let a = tla_snap(&[("userspace/formal/M.tla", tla)]);
        assert_eq!(a.models.tla_closure, 1);
        assert_eq!(a.models.tla_fallback, 0);
        let k = "userspace/formal/M.cfg::Safety";
        assert!(find(&a, k).text.contains("Bound(n) == n < 3"));
        assert!(
            !find(&a, k).text.contains("Other"),
            "Spec/Init/Next/Other are not reached"
        );
        // only a helper two steps down changes
        let b = tla_snap(&[("userspace/formal/M.tla", &tla.replace("n < 3", "n < 30"))]);
        let d = diff(&a.stmts, &b.stmts);
        assert_eq!(d.changed.len(), 1);
        assert!(d.changed[0].0.key.ends_with("::Safety"));
        let (t, code) = render(&d, &Allow::default(), "s");
        assert_eq!(code, 1);
        assert!(t.contains("CHANGED userspace/formal/M.cfg::Safety"));
        // operators the invariant does not reference
        let c = tla_snap(&[(
            "userspace/formal/M.tla",
            &tla.replace("Other == x = 7", "Other == x = 8")
                .replace("x' = x", "x' = x + 1"),
        )]);
        let d = diff(&a.stmts, &c.stmts);
        assert!(d.changed.is_empty() && d.removed.is_empty());
    }

    #[test]
    fn tla_closure_follows_extends_and_assume() {
        let base =
            "---- MODULE Base ----\nEXTENDS Naturals\nASSUME N > 0\nBound(n) == n < 3\n====\n";
        let m = "---- MODULE M ----\nEXTENDS Base\nSafety == Bound(1)\n====\n";
        let a = tla_snap(&[
            ("userspace/formal/Base.tla", base),
            ("userspace/formal/M.tla", m),
        ]);
        assert_eq!(a.models.tla_closure, 1);
        let b = tla_snap(&[
            ("userspace/formal/Base.tla", &base.replace("n < 3", "n < 4")),
            ("userspace/formal/M.tla", m),
        ]);
        assert_eq!(diff(&a.stmts, &b.stmts).changed.len(), 1);
        let c = tla_snap(&[
            (
                "userspace/formal/Base.tla",
                &base.replace("N > 0", "N > -1"),
            ),
            ("userspace/formal/M.tla", m),
        ]);
        assert_eq!(diff(&a.stmts, &c.stmts).changed.len(), 1);
    }

    #[test]
    fn tla_fallback_fingerprints_the_whole_module() {
        // `Mystery(..)` is neither defined, declared, local nor standard: the closure cannot be computed
        let tla = "---- MODULE M ----\nEXTENDS Naturals\nSafety == Mystery(1)\nOther == 1\n====\n";
        let a = tla_snap(&[("userspace/formal/M.tla", tla)]);
        assert_eq!(a.models.tla_closure, 0);
        assert_eq!(a.models.tla_fallback, 1);
        assert!(
            a.models
                .tla_fallback_modules
                .contains("userspace/formal/M.tla")
        );
        assert!(
            a.summary()
                .contains("whole-module fallback used by: userspace/formal/M.tla")
        );
        // an unrelated operator now counts
        let b = tla_snap(&[(
            "userspace/formal/M.tla",
            &tla.replace("Other == 1", "Other == 2"),
        )]);
        assert_eq!(diff(&a.stmts, &b.stmts).changed.len(), 1);
        // comments and layout still do not
        let c = tla_snap(&[(
            "userspace/formal/M.tla",
            &tla.replace("Other == 1", "\\* c\nOther   ==   1"),
        )]);
        assert!(diff(&a.stmts, &c.stmts).changed.is_empty());
        // an EXTENDS that is neither standard nor in the tree is a fallback too, and so is `A!B`
        let d = tla_snap(&[(
            "userspace/formal/M.tla",
            "---- MODULE M ----\nEXTENDS Gone\nSafety == TRUE\n====\n",
        )]);
        assert_eq!(d.models.tla_fallback, 1);
        let e = tla_snap(&[(
            "userspace/formal/M.tla",
            "---- MODULE M ----\nSafety == I!Ok\n====\n",
        )]);
        assert_eq!(e.models.tla_fallback, 1);
        // LET-local operators and standard ones resolve
        let f = tla_snap(&[(
            "userspace/formal/M.tla",
            "---- MODULE M ----\nEXTENDS Sequences\nSafety == LET f(a) == a + 1 IN f(1) > Len(<<>>)\n====\n",
        )]);
        assert_eq!(f.models.tla_closure, 1);
    }

    #[test]
    fn mutants_try_lines() {
        let sh = "#!/usr/bin/env bash\ntry() {\n    local from=$1\n}\ntry 'pos % PAGE)' 'pos % (PAGE - 1))'\n  try $'a\\nb' 'c'\ntry 'x' 'multi\nline'\n# try 'no' 'no'\necho done\n";
        let l = try_lines(sh);
        assert_eq!(l.len(), 3, "{l:?}");
        assert_eq!(l[0], "try 'pos % PAGE)' 'pos % (PAGE - 1))'");
        assert!(l[2].contains("multi\nline"));
        let m = |s: &str| Mem::new(&[("verification/a/mutants.sh", s)]);
        let a = collect(&m(sh), &[]);
        assert_eq!(a.count(Stmt::is_mutant), 3);
        let removed = sh.replace("try 'pos % PAGE)' 'pos % (PAGE - 1))'\n", "");
        let d = diff(&a.stmts, &collect(&m(&removed), &[]).stmts);
        assert_eq!(d.removed.len(), 1);
        assert!(d.removed[0].is_mutant());
        let changed = sh.replace("(PAGE - 1))", "(PAGE - 2))");
        let d = diff(&a.stmts, &collect(&m(&changed), &[]).stmts);
        assert_eq!(
            d.removed.len(),
            1,
            "a changed line is a removed line plus a new one"
        );
        assert_eq!(d.added, 1);
    }

    #[test]
    fn mutations_array() {
        let sh =
            "x=1\nmutations=(\n    'a.rs|s/x/y/|one'\n    # c\n    'b.rs|s/p/q/|two'\n)\nrest\n";
        let m = Mem::new(&[("scripts/verify-mutations.sh", sh)]);
        let a = collect(&m, &[]);
        assert_eq!(a.stmts.len(), 2);
        assert!(a.stmts.iter().all(|s| s.kind == "mutation"));
    }

    // ---- verdicts

    fn some_diff() -> (Diff, Diff) {
        let spec = verdict(BASE, &BASE.replace("r < 100", "r <= 100"));
        let b = Mem::new(&[("verification/a/mutants.sh", "try 'a' 'b'\ntry 'c' 'd'\n")]);
        let w = Mem::new(&[("verification/a/mutants.sh", "try 'a' 'b'\n")]);
        let mutd = diff(&collect(&b, &[]).stmts, &collect(&w, &[]).stmts);
        (spec, mutd)
    }

    fn get_none(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn refused_without_a_reason_and_with_an_empty_one() {
        let (d, _) = some_diff();
        for allow in [
            Allow::read(&get_none, None, false),
            Allow::read(&get_none, Some("   ".into()), false),
            Allow::read(
                &|k| (k == MERGE_ENV || k == LOCAL_ENV).then(|| "".to_string()),
                None,
                false,
            ),
        ] {
            let (t, code) = render(&d, &allow, "s");
            assert_eq!(code, 1);
            assert!(t.contains("SPEC CHANGE"));
            assert!(t.contains("Merge needs: lagent merge --allow spec-change=\"<why>\" (reviewer must read the statements above)"));
            assert!(t.contains("CHANGED verification/x/rules.rs::dist"));
            assert!(t.contains("allow: none"));
        }
    }

    #[test]
    fn merge_env_var_allows() {
        let (d, _) = some_diff();
        let a = Allow::read(
            &|k| (k == MERGE_ENV).then(|| "tightened the bound".to_string()),
            None,
            true,
        );
        let (t, code) = render(&d, &a, "s");
        assert_eq!(code, 0);
        assert!(t.contains("SPEC CHANGE ALLOWED: tightened the bound"));
        assert!(t.contains(&format!("saw {MERGE_ENV}")));
        assert!(
            t.contains("CHANGED verification/x/rules.rs::dist"),
            "the list is printed when allowed too"
        );
    }

    #[test]
    fn local_sources_and_precedence() {
        let (d, _) = some_diff();
        let env = |k: &str| match k {
            MERGE_ENV => Some("from lagent".to_string()),
            LOCAL_ENV => Some("from env".to_string()),
            _ => None,
        };
        // both set: the merge-time one is used and both are reported
        let a = Allow::read(&env, None, false);
        let (t, code) = render(&d, &a, "s");
        assert_eq!(code, 0);
        assert!(t.contains("SPEC CHANGE ALLOWED: from lagent"));
        assert!(t.contains(&format!("saw {MERGE_ENV}")) && t.contains(&format!("saw {LOCAL_ENV}")));
        assert!(t.contains("takes precedence"));
        // local only
        let local = |k: &str| (k == LOCAL_ENV).then(|| "local reason".to_string());
        let (t, code) = render(&d, &Allow::read(&local, None, false), "s");
        assert_eq!(code, 0);
        assert!(
            t.contains("SPEC CHANGE ALLOWED: local reason")
                && t.contains(&format!("[{LOCAL_ENV}]"))
        );
        // --allow beats the local env var
        let (t, _) = render(
            &d,
            &Allow::read(&local, Some("flag reason".into()), false),
            "s",
        );
        assert!(t.contains("SPEC CHANGE ALLOWED: flag reason") && t.contains("[--allow]"));
        // the merge gate ignores local sources and says so
        let (t, code) = render(&d, &Allow::read(&local, Some("flag".into()), true), "s");
        assert_eq!(code, 1);
        assert!(t.contains("ignored --allow") && t.contains("Merge needs: lagent merge --allow"));
    }

    #[test]
    fn spec_and_mutant_changes_have_their_own_headings() {
        let (spec, mutd) = some_diff();
        let merged = Diff {
            changed: spec.changed.into_iter().chain(mutd.changed).collect(),
            removed: spec.removed.into_iter().chain(mutd.removed).collect(),
            added: 0,
            same: 0,
        };
        let (t, code) = render(&merged, &Allow::default(), "s");
        assert_eq!(code, 1);
        let s = t.find("---- SPEC CHANGES (1) ----").expect("spec heading");
        let m = t
            .find("---- MUTANT CHANGES (1) ----")
            .expect("mutant heading");
        assert!(s < m);
        assert!(t[s..m].contains("rules.rs::dist") && !t[s..m].contains("mutants.sh"));
        assert!(t[m..].contains("mutants.sh") && !t[m..].contains("rules.rs::dist"));
        // mutant changes alone: no spec heading, still refused without an allow
        let only = Diff {
            changed: vec![],
            removed: merged.removed.clone(),
            added: 0,
            same: 0,
        };
        let (t, code) = render(&only, &Allow::default(), "s");
        assert_eq!(code, 1);
        assert!(t.contains("MUTANT CHANGES") && !t.contains("SPEC CHANGES"));
    }

    #[test]
    fn pure_addition_passes_quietly() {
        let d = verdict(
            BASE,
            &format!("{BASE}\npub proof fn n() ensures true, {{ }}\n"),
        );
        let (t, code) = render(&d, &Allow::default(), "sum");
        assert_eq!(code, 0);
        assert!(t.contains("1 new") && !t.contains("SPEC CHANGE\n"));
    }

    #[test]
    fn long_text_is_truncated() {
        let long = format!(
            "pub fn f() ensures {} ,\n{{ }}",
            (0..400)
                .map(|i| format!("a{i} == 1"))
                .collect::<Vec<_>>()
                .join(" && ")
        );
        let mut b = Stmt::new("k".into(), "fn", long.clone());
        let n = Stmt::new("k".into(), "fn", "short".into());
        b.text = long;
        let d = Diff {
            changed: vec![(b, n)],
            removed: vec![],
            added: 0,
            same: 0,
        };
        let (t, _) = render(&d, &Allow::default(), "s");
        assert!(t.contains("[truncated]"));
    }
}
