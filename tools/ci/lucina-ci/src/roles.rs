//! `lucina-ci roles`: the model that writes the code must not be the one that proves it or judges the proof (owner rule,
//! docs/ci/PROOF-ROLES.md, docs/ci/PROOF-WORKFLOW.md). Mechanical part: three fingerprints per proof unit, an append-only record log
//! `docs/ci/proof-roles.tsv`, and a merge check that a changed statement or proof carries a record by a STRONG prover that is not the
//! implementer, with (for statements) a reviewer of another model family.
//!
//!   lucina-ci roles --check [--base REF] [--merge-gate]   compare with the base commit's tree; exit 0 ok, 1 refused, 2 usage/io/extractor
//!   lucina-ci roles --list [--json | --summary]            fingerprints and review status per unit; burn-down by group
//!   lucina-ci roles --template statement|proof-edit [--unit U]   a record line to fill
//!   lucina-ci roles --seed [--print]                       append one `seed` record per unit that has no record yet
//!
//! THE THREE FINGERPRINTS (fnv1a-64 like src/spec.rs; hex). Per unit, over its verified files: the target and every file it pulls in by
//! `#[path]` or `mod x;` (exactly the closure src/trust.rs resolves).
//!   statement_fp  the statements `spec --list` produces for those files (spec::scan_file), aggregated: sorted `key<TAB>hash` lines.
//!   proof_fp      normalised text (comments dropped, whitespace collapsed, same normaliser as the freeze) of the PROOF TEXT, per file in
//!                 source order, runs joined by ` ;; `.
//!   exec_fp       the normalised text of everything else in the verified files (plus `//@omit` lines, which Verus never sees).
//! Extraction is token based and reuses the freeze's lexer and fn/item scanner (spec::scan_full gives the token range of every
//! extracted statement). Heuristics, in this order (a token already classified keeps its class):
//!   1. STATEMENT: the token range of every statement the freeze extracts: fn headers (attributes, signature, requires/ensures/
//!      decreases/recommends, `proof fn` headers included), `spec fn` complete with body, struct/enum/type/const items.
//!   2. PROOF, inside the `{ }` body of a `proof fn`: all of it. Inside the body of an exec fn in a `verus!` block: a clause
//!      (`invariant`, `invariant_except_break`, `decreases`, `ensures`, `recommends`, `requires`) from its keyword up to the `{` of the
//!      loop body (parentheses, `if`/`match`/`else` blocks and struct literals tracked as in the freeze); `assert`/`assume` (not
//!      `assert!`) up to its `;`, or after the `by { }` block; `proof { }` blocks and nested `proof fn`; `reveal*(..);`; `let ghost`
//!      and `let tracked` up to `;`; `calc! { }`; `broadcast use ..;`; `#[verifier..]`, `#[trigger]`, `#[auto]` attributes.
//!   3. PROOF, anywhere else: every token on a `//@` line that statement span did not take (shipped files: `//@` lines inside bodies
//!      are the loop invariants and asserts), and a `broadcast use/group` item of a `verus!` block.
//!   4. EXEC: everything else, including the `verus!` wrapper, `use` lines, impl wrappers, exec bodies, plain Rust of shipped files and
//!      their tests.
//!
//! The `/*proof*/` marker of the brief does not occur in the tree and comments are not tokens, so it is not recognised.
//!
//! MODEL units (tla/*, models/*): the files are the unit's directory (TLA+: `.tla .cfg .sh` and extension-less runners; Stateright:
//! `specs/lucina-models` `.rs .toml`), a TLA+ unit owning what no more specific TLA+ unit's directory owns. statement_fp aggregates
//! the freeze's model statements whose key names one of those files. proof_fp is the WHOLE normalised text of all those files (the
//! statement text is inside it: a statement edit moves both fingerprints, and the statement rule asks for the current proof_fp too).
//! exec_fp is `-`. mutants/mutations/deep/full units are not tracked here (the freeze and amendments own their lines): `-` for all three.
//!
//! Self-consistency: every `fn` of a `verus!` block must lie in an extracted statement or its body (else exit 2); the clause keywords
//! of the extracted headers must equal the freeze's own count; any stray clause keyword is exit 2. `roles --list` prints the
//! statement/proof/exec token counts per unit.
//!
//! LIMITS (documented, not hidden): the record is self-declared; the commit-trailer check is the only mechanical link to who really
//! wrote what; it cannot prove the stronger model ran or that the implementer did not see the verdict. Working-tree changes that are
//! not committed have no trailer to check (refused with --merge-gate). Fingerprints are 64-bit FNV like the freeze: against an
//! accidents, not against an adversary (a free literal in the proof text allows a second preimage). Lemma headers are statements, lemma bodies are proof. A unit renamed in the inventory looks new.
//! There is deliberately NO allow flag: a missing reviewer of another family or a missing strong prover blocks the merge.

use crate::amend;
use crate::inventory::{self, Unit};
use crate::spec::{self, FsTree, GitTree, Tok, Tree, fnv1a, hex};
use crate::trust;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;

pub const LOG: &str = "docs/ci/proof-roles.tsv";
const FIELDS: usize = 19;
/// Written by the coordinator when this rule lands on main: the commit that introduced `lucina-ci roles` (line 1), then the units that
/// may get a `pre-rule` record, each with its `started_before` commit. A `pre-rule` record is accepted only while this file exists,
/// lists the unit, and the record's `started_before` equals the listed commit, which must exist in the repository's object database
/// (on a branch or not). It need not be an ancestor of anything.
pub const CUTOFF: &str = "docs/ci/proof-roles-cutoff.txt";
const VERDICT_DIR: &str = "docs/ci/verdicts/";
const MODELS_CRATE: &str = "specs/lucina-models";
pub const KINDS: &[&str] = &["statement", "proof-edit", "recheck", "pre-rule", "seed"];
pub const VERDICTS: &[&str] = &[
    "accepted",
    "rejected",
    "escalated",
    "not-reviewed-under-rule",
];
const NOT_REVIEWED: &str = "not-reviewed-under-rule";

const LOG_HEADER: &str = "\
# Append-only log of who proved what (docs/ci/PROOF-ROLES.md). Never edit or delete a line; add a new one.
# Fields (tab-separated, none empty, no tab or newline inside): id, date, kind, unit, statement_fp, proof_fp, exec_fp, data_fp,
#   implementer_model, implementer_family, prover_model, prover_family, prover_class, reviewer_model, reviewer_family,
#   verdict, verdict_ref, started_before, note
# kind: statement | proof-edit | recheck | pre-rule | seed. started_before: for a pre-rule, a commit before roles landed; else -.
#  verdict: accepted | rejected | escalated | not-reviewed-under-rule.
# prover_class: strong | builder | unknown (derived from prover_model, see roles.rs). verdict_ref: docs/ci/verdicts/<id>.md, or - for a seed.
# fingerprints: 16 hex digits from `lucina-ci roles --list`, or - for a unit that has none.
";

// ------------------------------------------------------------------------------------------------------- names and classes

/// Trailing context-size qualifiers that harnesses add to a model name; only these are stripped, nothing else in brackets.
const QUALIFIERS: &[&str] = &[
    "(1m context)",
    "[1m context]",
    "(1m)",
    "[1m]",
    "(200k context)",
    "[200k]",
    "(200k)",
];

/// A model name as compared everywhere: a trailing context-size qualifier from the `QUALIFIERS` table dropped (`Claude Opus 5.5
/// (1M context)`, `claude-opus-5-5[1m]`; `Claude Opus 5.5 (actually Sonnet)` keeps its words), lower case, every run of
/// non-alphanumeric characters one `-`, none at the ends. A `Co-Authored-By: Claude Opus 5.5 <..>` trailer therefore becomes
/// `claude-opus-5-5`, the same as a record's `claude-opus-5-5`.
pub fn model_norm(s: &str) -> String {
    let mut low = s.trim().to_lowercase();
    loop {
        let before = low.len();
        for q in QUALIFIERS {
            if let Some(h) = low.strip_suffix(q) {
                low = h.trim_end().to_string();
            }
        }
        if low.len() == before {
            break;
        }
    }
    let mut out = String::new();
    let mut dash = false;
    for c in low.chars() {
        if c.is_alphanumeric() {
            if dash && !out.is_empty() {
                out.push('-');
            }
            dash = false;
            out.push(c);
        } else {
            dash = true;
        }
    }
    // Harness spellings without the vendor (`opus 5.5`, `Fable 5.2`) are the same model as the full name.
    if ["opus-", "fable-", "sonnet-", "haiku-"]
        .iter()
        .any(|w| out.starts_with(w))
    {
        out.insert_str(0, "claude-");
    }
    out
}

/// The strong class is an EXPLICIT allowlist of normalised names (Opus 5.5, Fable 5.2 and stronger). A later model is added here
/// when the owner names it; a name that merely contains `opus` or `fable` is not strong.
pub const STRONG: &[&str] = &["claude-opus-5-5", "claude-fable-5-2"];

/// The tier of each strong model: Opus-written code is proved by a Fable-tier model and the other way round (owner rule, 2026-11-11).
/// Every name of `STRONG` has a tier here (a unit test keeps the two tables in step); a later model is added to both.
pub const STRONG_TIERS: &[(&str, &str)] =
    &[("claude-opus-5-5", "opus"), ("claude-fable-5-2", "fable")];

/// The tier (`opus`, `fable`) of a strong model name, `None` for any other model.
pub fn strong_tier(model: &str) -> Option<&'static str> {
    let n = model_norm(model);
    STRONG_TIERS.iter().find(|(m, _)| *m == n).map(|(_, t)| *t)
}

const BUILDER_TOKENS: &[&str] = &[
    "sonnet",
    "haiku",
    "qwen",
    "mistral",
    "vibe",
    "codestral",
    "devstral",
    "qoder",
    "zen",
    "glm",
    "zcode",
    "deepseek-free",
];

/// `strong` (exactly a name of `STRONG` after normalisation), `builder` (a name containing sonnet, haiku, qwen, mistral/vibe, qoder,
/// zen, glm, deepseek-free), else `unknown`.
pub fn class(model: &str) -> &'static str {
    let n = model_norm(model);
    if STRONG.contains(&n.as_str()) {
        "strong"
    } else if BUILDER_TOKENS.iter().any(|w| n.contains(w)) {
        "builder"
    } else {
        "unknown"
    }
}

/// The model family; `unknown` and `-` stay as they are. A strong name is `claude`; for other names the words `opus` and `fable` do
/// not decide (`qwen3-opus-distill` is qwen).
pub fn family(model: &str) -> &'static str {
    match model {
        "-" => "-",
        "unknown" => "unknown",
        m => {
            let n = model_norm(m);
            if STRONG.contains(&n.as_str()) {
                "claude"
            } else {
                amend::family(&n.replace("opus", "-").replace("fable", "-"))
            }
        }
    }
}

/// The model names in the `Co-Authored-By` trailers of the last paragraph of a commit message, normalised. A trailer is typed by
/// whoever commits: a builder can write an Opus trailer. The check ties records to commits; it cannot prove who typed them.
pub fn trailer_models(msg: &str) -> Vec<String> {
    // Only the final paragraph is a trailer block: quoted text and bodies of reverts or squashes above it do not count.
    let msg = msg.replace("\r\n", "\n");
    let block = msg
        .trim_end()
        .rsplit("\n\n")
        .next()
        .unwrap_or("")
        .to_string();
    block
        .lines()
        .filter_map(|l| {
            let t = l.trim();
            let (k, v) = t.split_once(':')?;
            if !k.trim().eq_ignore_ascii_case("co-authored-by") {
                return None;
            }
            let name = v.split('<').next().unwrap_or("").trim();
            let n = model_norm(name);
            (!n.is_empty()).then_some(n)
        })
        .collect()
}

// ------------------------------------------------------------------------------------------------------- classification

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cls {
    Exec,
    Stmt,
    Proof,
}

/// What one file contributes to a unit.
#[derive(Clone, Debug)]
pub struct Item {
    pub name: String,
    pub key: String,
    pub text: String,
    pub hash: u64,
}

pub struct FileClass {
    /// (key, hash) of the freeze's statements of this file.
    pub stmts: Vec<(String, u64)>,
    /// The data definitions among them (struct, enum, const, type): name, key, normalised text, hash.
    pub items: Vec<Item>,
    /// Keys of statements that are bare fn headers: code, not statements.
    pub clauseless: Vec<String>,
    /// The normalised text of every other statement (fn headers, spec fn, proof fn headers).
    pub fn_text: String,
    pub proof_text: String,
    pub exec_text: String,
    /// The code token stream of the file: every executable token, and for each plain `fn` statement its header without clauses and
    /// verifier attributes (`fn_code_header`), in source order. Identical before and after when a bare fn header only gained clauses.
    pub code_toks: Vec<String>,
    pub counts: [usize; 3],
    pub errors: Vec<String>,
}

fn kwlike(t: &Tok, w: &str) -> bool {
    t.ident && t.s == w
}

/// End (exclusive) of a statement that ends at `;` or after a trailing `{ }` block (`assert .. by { }`).
fn stmt_end(t: &[Tok], from: usize, hi: usize) -> usize {
    let mut d = 0i32;
    let mut pending = false;
    let mut k = from;
    while k < hi {
        match t[k].s.as_str() {
            "(" | "[" => d += 1,
            ")" | "]" => {
                d -= 1;
                if d < 0 {
                    return k;
                }
            }
            "{" => {
                let c = spec::match_brace(t, k);
                if d > 0 || pending {
                    pending = false;
                    k = c;
                } else {
                    let mut e = c + 1;
                    if e < hi && t[e].s == ";" {
                        e += 1;
                    }
                    return e.min(hi);
                }
            }
            "}" => return k,
            ";" if d <= 0 => return k + 1,
            s if t[k].ident && d <= 0 && matches!(s, "if" | "match" | "else") => pending = true,
            _ => {}
        }
        k += 1;
    }
    hi
}

/// End (exclusive) of a `..;` item, braces and parentheses nested.
fn semi_end(t: &[Tok], from: usize, hi: usize) -> usize {
    let mut d = 0i32;
    let mut k = from;
    while k < hi {
        match t[k].s.as_str() {
            "(" | "[" | "{" => d += 1,
            ")" | "]" | "}" => {
                d -= 1;
                if d < 0 {
                    return k;
                }
            }
            ";" if d <= 0 => return k + 1,
            _ => {}
        }
        k += 1;
    }
    hi
}

/// A loop or function clause: from its keyword up to (not including) the `{` of the body.
fn clause_end(t: &[Tok], i: usize, hi: usize) -> usize {
    let mut d = 0i32;
    let mut pending = false;
    let mut k = i + 1;
    while k < hi {
        match t[k].s.as_str() {
            "(" | "[" => d += 1,
            ")" | "]" => d -= 1,
            "{" => {
                if d > 0 || pending || spec::struct_literal(t, k, true) {
                    k = spec::match_brace(t, k) + 1;
                    pending = false;
                    continue;
                }
                return k;
            }
            ";" if d <= 0 => return k,
            "}" if d <= 0 => return k,
            s if t[k].ident && d <= 0 && matches!(s, "match" | "if" | "else") => pending = true,
            _ => {}
        }
        k += 1;
    }
    hi
}

/// If the token at `i` starts proof text inside a verified exec body, the end (exclusive) of it.
fn proof_construct(t: &[Tok], i: usize, hi: usize) -> Option<usize> {
    let next = t.get(i + 1).map_or("", |x| x.s.as_str());
    let tk = &t[i];
    if tk.s == "#" {
        let e = spec::attr_end(t, i)?;
        let j = if t.get(i + 1).is_some_and(|x| x.s == "!") {
            i + 3
        } else {
            i + 2
        };
        let name = t.get(j).map_or("", |x| x.s.as_str());
        return matches!(name, "verifier" | "trigger" | "auto").then_some(e.min(hi));
    }
    if !tk.ident {
        return None;
    }
    let s = tk.s.as_str();
    match s {
        "assert" | "assume" if next != "!" => Some(stmt_end(t, i + 1, hi)),
        "proof" if next == "{" => Some((spec::match_brace(t, i + 1) + 1).min(hi)),
        "proof" if next == "fn" => {
            let mut k = i + 2;
            let mut d = 0i32;
            while k < hi {
                match t[k].s.as_str() {
                    "(" | "[" => d += 1,
                    ")" | "]" => d -= 1,
                    "{" if d <= 0 => return Some((spec::match_brace(t, k) + 1).min(hi)),
                    ";" if d <= 0 => return Some(k + 1),
                    _ => {}
                }
                k += 1;
            }
            Some(hi)
        }
        s if s.starts_with("reveal") && next == "(" => Some(semi_end(t, i + 1, hi)),
        "let" if matches!(next, "ghost" | "tracked") => Some(semi_end(t, i + 1, hi)),
        "calc" if next == "!" => {
            let open = i + 2;
            if t.get(open).is_some_and(|x| x.s == "{") {
                let mut e = spec::match_brace(t, open) + 1;
                if e < hi && t[e].s == ";" {
                    e += 1;
                }
                Some(e.min(hi))
            } else {
                None
            }
        }
        "broadcast" if next == "use" => Some(semi_end(t, i + 1, hi)),
        "invariant"
        | "invariant_except_break"
        | "decreases"
        | "ensures"
        | "recommends"
        | "requires" => Some(clause_end(t, i, hi)),
        _ => None,
    }
}

fn exec_body(t: &[Tok], vb: &[bool], cls: &mut [Cls], lo: usize, hi: usize) {
    let mut i = lo;
    while i < hi {
        if cls[i] != Cls::Exec || !vb[i] {
            i += 1;
            continue;
        }
        match proof_construct(t, i, hi) {
            Some(e) if e > i => {
                for c in cls.iter_mut().take(e.min(hi)).skip(i) {
                    if *c == Cls::Exec {
                        *c = Cls::Proof;
                    }
                }
                i = e;
            }
            _ => i += 1,
        }
    }
}

fn runs(t: &[Tok], cls: &[Cls], want: Cls) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut i = 0;
    while i < t.len() {
        if cls[i] == want {
            let a = i;
            while i < t.len() && cls[i] == want {
                i += 1;
            }
            parts.push(spec::norm_tokens(&t[a..i]));
        } else {
            i += 1;
        }
    }
    parts.join(" ;; ")
}

/// `fn name` tokens of a `verus!` block that lie in no extracted statement and in no body (the extractor missed them).
fn find_uncovered(path: &str, t: &[Tok], in_verus: &[bool], covered: &[bool]) -> Vec<String> {
    let mut v = Vec::new();
    for k in 0..t.len() {
        if kwlike(&t[k], "fn")
            && in_verus[k]
            && t.get(k + 1).is_some_and(|x| x.ident)
            && !covered[k]
        {
            v.push(format!(
                "{path}:{}: fn {} in a verus! block is in no extracted statement (unclassified)",
                t[k].line + 1,
                t[k + 1].s
            ));
        }
    }
    v
}

/// The tokens of the item that follows the attribute ending at `after`: further attributes, the signature and the `{ .. }` body, or up
/// to the `;`.
fn gated_extent(t: &[Tok], after: usize) -> (usize, usize) {
    let mut j = after;
    while let Some(e) = spec::attr_end(t, j) {
        j = e;
    }
    let mut d = 0i32;
    let mut k = j;
    while k < t.len() {
        match t[k].s.as_str() {
            "(" | "[" => d += 1,
            ")" | "]" => d -= 1,
            "{" if d <= 0 => return (j, spec::match_brace(t, k) + 1),
            ";" if d <= 0 => return (j, k + 1),
            _ => {}
        }
        k += 1;
    }
    (j, t.len())
}

/// Close of the delimiter opened at `open` (`(`, `[` or `{`).
fn match_delim(t: &[Tok], open: usize) -> usize {
    let (o, c) = match t[open].s.as_str() {
        "(" => ("(", ")"),
        "[" => ("[", "]"),
        _ => ("{", "}"),
    };
    let mut d = 0i32;
    for (k, x) in t.iter().enumerate().skip(open) {
        if x.s == o {
            d += 1;
        } else if x.s == c {
            d -= 1;
            if d == 0 {
                return k;
            }
        }
    }
    t.len().saturating_sub(1)
}

/// Words by which a file says what Verus verifies when it is not written in `verus! { }` or `//@` lines.
const GHOST_WORDS: &[&str] = &["verus_spec", "verus_verify", "verus_keep_ghost"];

/// Does the attribute `t[a..e]` (outer or inner) steer or switch off verification? `verifier::..`, `trigger`, `auto`,
/// `verus_spec`/`verus_verify`, and a `cfg`/`cfg_attr` that names `verus`/`verus_keep_ghost`, is `any()`, `false` or `not(all())`.
/// Plain gates (`cfg(test)`, `cfg(target_os = ..)`) are not.
fn steering_attr(t: &[Tok], a: usize, e: usize) -> bool {
    let inner = &t[a..e];
    let ni = inner.iter().position(|x| x.ident);
    let name = ni.map_or("", |k| inner[k].s.as_str());
    match name {
        "verifier" | "trigger" | "auto" | "verus_spec" | "verus_verify" | "verus" => true,
        "cfg" | "cfg_attr" => {
            let k = ni.unwrap_or(0);
            // The predicate: up to the matching `)` for cfg, up to the first top-level `,` for cfg_attr.
            let mut pred: Vec<&str> = Vec::new();
            let mut d = 0i32;
            for x in inner.iter().skip(k + 1) {
                match x.s.as_str() {
                    "(" => {
                        d += 1;
                        if d == 1 {
                            continue;
                        }
                    }
                    ")" => {
                        d -= 1;
                        if d == 0 {
                            break;
                        }
                    }
                    "," if d == 1 && name == "cfg_attr" => break,
                    _ => {}
                }
                pred.push(x.s.as_str());
            }
            let test_only = !pred.is_empty()
                && pred
                    .iter()
                    .all(|w| matches!(*w, "test" | "not" | "(" | ")"));
            let mentions_verifier = inner.iter().any(|x| {
                x.ident
                    && matches!(
                        x.s.as_str(),
                        "verifier" | "verus_spec" | "verus_verify" | "verus"
                    )
            });
            // A cfg_attr whose payload is itself a gate, a path or a verifier attribute steers even under a test-only condition.
            let payload_steers = name == "cfg_attr" && {
                let mut d2 = 0i32;
                let mut seen_comma = false;
                let mut hit = false;
                for x in inner.iter().skip(k + 1) {
                    match x.s.as_str() {
                        "(" | "[" => d2 += 1,
                        ")" | "]" => d2 -= 1,
                        "," if d2 == 1 => seen_comma = true,
                        w if seen_comma
                            && x.ident
                            && matches!(w, "cfg" | "cfg_attr" | "path" | "verifier") =>
                        {
                            hit = true
                        }
                        _ => {}
                    }
                }
                hit
            };
            // Every other cfg of a verified file can switch the verified code off (`feature = ..`, `target_os = ..`,
            // `debug_assertions`, `any()`, `false`, `verus_keep_ghost`): proof text.
            !test_only || mentions_verifier || payload_steers
        }
        _ => false,
    }
}

/// Attribute-style proofs and gating (fix list 2 and 3 of the review): every steering attribute that no statement span took is PROOF
/// text (item-level `#[cfg(any())]` on a `mod`/`impl`, inner `#![cfg(..)]`, `#[cfg_attr(verus_keep_ghost, verus_spec(invariant ..))]`
/// on a loop), as are `proof! { }` / `proof_with! { }` blocks. Then FAIL CLOSED: a `verus_spec`/`verus_verify`/`verus_keep_ghost`
/// left in executable text is an error naming the place.
fn attr_pass(path: &str, t: &[Tok], cls: &mut [Cls], errors: &mut Vec<String>) {
    let n = t.len();
    let mut k = 0;
    while k < n {
        if cls[k] == Cls::Exec && t[k].s == "#" {
            if let Some(e) = spec::attr_end(t, k) {
                if steering_attr(t, k, e.min(n)) {
                    for c in cls.iter_mut().take(e.min(n)).skip(k) {
                        if *c == Cls::Exec {
                            *c = Cls::Proof;
                        }
                    }
                }
                k = e;
                continue;
            }
        } else if cls[k] == Cls::Exec
            && (kwlike(&t[k], "proof") || kwlike(&t[k], "proof_with"))
            && t.get(k + 1).is_some_and(|x| x.s == "!")
            && t.get(k + 2)
                .is_some_and(|x| matches!(x.s.as_str(), "{" | "(" | "["))
        {
            let mut e = match_delim(t, k + 2) + 1;
            if e < n && t[e].s == ";" {
                e += 1;
            }
            for c in cls.iter_mut().take(e.min(n)).skip(k) {
                *c = Cls::Proof;
            }
            k = e;
            continue;
        }
        k += 1;
    }
    // A test-only cfg (`cfg(test)`) is code only when the item it gates holds no statement or proof token; an inline `mod`/`impl`
    // that holds verified items would otherwise hide them from the verifier with one exec line.
    let mut k = 0;
    while k < n {
        if cls[k] == Cls::Exec
            && t[k].s == "#"
            && let Some(e) = spec::attr_end(t, k)
        {
            let name = t[k..e.min(n)]
                .iter()
                .find(|x| x.ident)
                .map_or("", |x| x.s.as_str());
            let inner_attr = t.get(k + 1).is_some_and(|x| x.s == "!");
            if name == "cfg" {
                // An inner `#![cfg(..)]` gates the whole file; an outer one gates the item that follows.
                let (lo, hi) = if inner_attr {
                    (0, n)
                } else {
                    gated_extent(t, e)
                };
                if cls[lo..hi.min(n)].iter().any(|c| *c != Cls::Exec) {
                    for c in cls.iter_mut().take(e.min(n)).skip(k) {
                        *c = Cls::Proof;
                    }
                }
            }
            k = e;
            continue;
        }
        k += 1;
    }
    for (k, x) in t.iter().enumerate() {
        if cls[k] == Cls::Exec && x.ident && GHOST_WORDS.contains(&x.s.as_str()) {
            errors.push(format!(
                "{path}:{}: `{}` in executable text that no attribute or proof! block accounts for: this scan cannot classify it (fail closed)",
                x.line + 1,
                x.s
            ));
        }
    }
}

/// Does a header carry a steering attribute (`verifier::..`, `verus_spec`, a non-test `cfg`/`cfg_attr`)? Such a header stays a statement:
/// the attribute can hide the fn from Verus.
fn span_has_steering_attr(t: &[Tok]) -> bool {
    let mut p = 0;
    while p < t.len() {
        if let Some(e) = spec::attr_end(t, p) {
            if steering_attr(t, p, e.min(t.len())) {
                return true;
            }
            p = e;
        } else {
            p += 1;
        }
    }
    false
}

/// A plain `fn` header with no clause keyword, no spec attribute and no steering attribute.
fn is_clauseless_fn(st: &spec::Stmt, toks: &[Tok]) -> bool {
    st.kind == "fn"
        && !span_has_steering_attr(toks)
        && !idents_of(&st.text).iter().any(|w| {
            matches!(
                *w,
                "requires"
                    | "ensures"
                    | "decreases"
                    | "recommends"
                    | "verus_spec"
                    | "verus_verify"
                    | "verifier"
                    | "returns"
                    | "opens_invariants"
                    | "no_unwind"
                    | "default_ensures"
            )
        })
}

pub fn classify(path: &str, src: &str) -> FileClass {
    let sc = spec::scan_full(path, src);
    let t = &sc.toks;
    let n = t.len();
    let mut cls = vec![Cls::Exec; n];
    let mut errors: Vec<String> = Vec::new();
    for s in &sc.file.stray {
        errors.push(format!(
            "{s}: clause keyword outside any function the extractor knows"
        ));
    }
    let mut covered = vec![false; n];
    for sp in &sc.spans {
        for k in sp.lo..sp.hi.min(n) {
            cls[k] = Cls::Stmt;
            covered[k] = true;
        }
    }
    for sp in &sc.spans {
        if let Some((b0, b1)) = sp.body {
            let b1 = b1.min(n.saturating_sub(1));
            for c in covered.iter_mut().take(b1 + 1).skip(b0) {
                *c = true;
            }
            if sp.kind == "proof fn" {
                for c in cls.iter_mut().take(b1 + 1).skip(b0) {
                    if *c == Cls::Exec {
                        *c = Cls::Proof;
                    }
                }
            } else {
                exec_body(t, &sc.in_verus, &mut cls, b0 + 1, b1);
            }
        }
    }
    // A `macro_rules!` body that holds verification words (`proof`, `assert`, `requires`, ...) is proof text: what it expands to is not
    // visible to the classifier, so its tokens are not code.
    let mut k = 0;
    while k + 3 < n {
        if t[k].s == "macro_rules"
            && t[k + 1].s == "!"
            && matches!(t[k + 3].s.as_str(), "{" | "(" | "[")
        {
            let (mut depth, mut e) = (0usize, k + 3);
            while e < n {
                match t[e].s.as_str() {
                    "{" | "(" | "[" => depth += 1,
                    "}" | ")" | "]" => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
                e += 1;
            }
            let end = (e + 1).min(n);
            // `assert!`/`assume!` (followed by `!`) are ordinary runtime macros, not verification words
            let verifies = (k + 3..end).any(|j| {
                let w = t[j].s.as_str();
                match w {
                    "assert" | "assume" => t.get(j + 1).is_none_or(|x| x.s != "!"),
                    _ => matches!(
                        w,
                        "proof"
                            | "requires"
                            | "ensures"
                            | "invariant"
                            | "decreases"
                            | "reveal"
                            | "by"
                    ),
                }
            });
            if verifies {
                for c in cls.iter_mut().take(end).skip(k) {
                    if *c == Cls::Exec {
                        *c = Cls::Proof;
                    }
                }
            }
            k = end;
            continue;
        }
        k += 1;
    }
    // `//@` lines no statement took; `broadcast use/group` items of a verus! block.
    let mut k = 0;
    while k < n {
        if cls[k] == Cls::Exec && t[k].tag {
            cls[k] = Cls::Proof;
        } else if cls[k] == Cls::Exec
            && sc.in_verus[k]
            && (kwlike(&t[k], "global")
                || (kwlike(&t[k], "broadcast") && t.get(k + 1).is_some_and(|x| kwlike(x, "use"))))
        {
            let e = semi_end(t, k + 1, n);
            for c in cls.iter_mut().take(e).skip(k) {
                *c = Cls::Proof;
            }
            k = e;
            continue;
        } else if cls[k] == Cls::Exec
            && sc.in_verus[k]
            && kwlike(&t[k], "broadcast")
            && t.get(k + 1).is_some_and(|x| kwlike(x, "group"))
        {
            let mut o = k + 2;
            while o < n && t[o].s != "{" && t[o].s != ";" {
                o += 1;
            }
            let e = if o < n && t[o].s == "{" {
                spec::match_brace(t, o) + 1
            } else {
                o + 1
            };
            for c in cls.iter_mut().take(e.min(n)).skip(k) {
                *c = Cls::Proof;
            }
            k = e;
            continue;
        }
        k += 1;
    }
    attr_pass(path, t, &mut cls, &mut errors);
    // Cross-checks against the freeze's own scan.
    let hk: usize = sc
        .spans
        .iter()
        .map(|sp| {
            t[sp.lo..sp.header_end.min(n).max(sp.lo)]
                .iter()
                .enumerate()
                .filter(|(o, x)| spec::counts(x, sc.in_verus[sp.lo + o]))
                .count()
        })
        .sum();
    if hk != sc.file.header_kw {
        errors.push(format!(
            "{path}: {hk} clause keyword(s) in the headers roles classified as statements, the freeze counts {}",
            sc.file.header_kw
        ));
    }
    errors.extend(find_uncovered(path, t, &sc.in_verus, &covered));
    let mut counts = [0usize; 3];
    for c in &cls {
        counts[match c {
            Cls::Exec => 0,
            Cls::Stmt => 1,
            Cls::Proof => 2,
        }] += 1;
    }
    let mut exec_text = runs(t, &cls, Cls::Exec);
    let code_toks = {
        let by_lo: HashMap<usize, usize> = sc
            .spans
            .iter()
            .enumerate()
            .filter(|(_, sp)| matches!(sp.kind, "fn" | "item"))
            .map(|(i, sp)| (sp.lo, i))
            .collect();
        let mut out: Vec<String> = Vec::new();
        let mut k = 0;
        while k < n {
            if let Some(&i) = by_lo.get(&k) {
                let sp = &sc.spans[i];
                let hi = sp.hi.min(n).max(k + 1);
                if sp.kind == "fn" {
                    out.extend(fn_code_header(&t[k..hi], &sc.in_verus[k..hi]));
                } else {
                    // a data definition: code whether or not a statement refers to it (the file classes it as a statement only
                    // when it holds one), so its tokens are the same either way
                    out.extend(t[k..hi].iter().map(|x| x.s.clone()));
                }
                k = hi;
                continue;
            }
            if cls[k] == Cls::Exec {
                out.push(t[k].s.clone());
            }
            k += 1;
        }
        out
    };
    // `//@omit` lines decide what the verifier sees of the shipped code: they are proof text.
    let mut proof_text = runs(t, &cls, Cls::Proof);
    for l in src.lines().filter(|l| l.trim_end().ends_with("//@omit")) {
        proof_text.push_str(" ;; omit: ");
        proof_text.push_str(&l.split_whitespace().collect::<Vec<_>>().join(" "));
    }
    let mut items = Vec::new();
    let mut fn_text = String::new();
    let mut clauseless: Vec<String> = Vec::new();
    let aligned = sc.file.stmts.len() == sc.spans.len();
    for (si, st) in sc.file.stmts.iter().enumerate() {
        let span_toks: &[Tok] = if aligned {
            &t[sc.spans[si].lo..sc.spans[si].hi.min(n)]
        } else {
            &[]
        };
        if aligned && is_clauseless_fn(st, span_toks) {
            // A bare fn header (no requires/ensures/decreases/recommends, no spec attribute) says nothing that is proved: code.
            clauseless.push(st.key.clone());
            exec_text.push_str(&format!(" ;; fn-header {}: {}", st.key, st.text));
        } else if st.kind == "item" {
            let base = st.key.rsplit("::").next().unwrap_or("");
            let name = base.split('#').next().unwrap_or("").to_string();
            items.push(Item {
                name,
                key: st.key.clone(),
                text: st.text.clone(),
                hash: st.hash,
            });
        } else {
            fn_text.push_str(&st.text);
            fn_text.push('\n');
        }
    }
    FileClass {
        clauseless,
        items,
        fn_text,
        stmts: sc
            .file
            .stmts
            .iter()
            .map(|s| (s.key.clone(), s.hash))
            .collect(),
        proof_text,
        exec_text,
        code_toks,
        counts,
        errors,
    }
}

// ------------------------------------------------------------------------------------------------------- unit fingerprints

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Fp {
    pub stmt: String,
    pub proof: String,
    pub exec: String,
    /// Data definitions (struct, enum, const, type) that a frozen statement refers to; `-` for model units.
    pub data: String,
    /// Attribution detail for the one-model rule. NOT part of the fingerprint and ignored by `==`.
    pub core: Core,
}

/// What `promotion_only` needs to tell a bare fn header that became a statement from any other exec change.
#[derive(Clone, Debug, Default)]
pub struct Core {
    /// Per file, the code token stream (`FileClass::code_toks`) joined by spaces, data definitions included (referred to by a
    /// statement or not). A bare fn header gaining clauses changes none of it.
    pub code: String,
    /// Set for `verus` / `shipped` units only: the three fields below are then meaningful. They tell `change_info` what a commit
    /// that CREATES the unit's file wrote, by content: statements, proof text and code.
    pub classified: bool,
    /// The unit holds at least one frozen statement (a data definition or a bare fn header is not one).
    pub has_stmt: bool,
    /// The unit holds proof text (`FileClass::proof_text`: gated items, `//@omit` lines, proof bodies, ...).
    pub has_proof: bool,
    /// The unit holds any code (executable tokens, data definitions, bare fn headers).
    pub has_code: bool,
}

impl PartialEq for Core {
    fn eq(&self, _: &Core) -> bool {
        true
    }
}
impl Eq for Core {}

#[derive(Clone, Debug)]
pub struct UnitFp {
    pub unit: String,
    pub group: String,
    pub kind: String,
    pub files: Vec<String>,
    /// None for units that are not tracked here.
    pub fp: Option<Fp>,
    /// Tokens: exec, statement, proof (Verus units only).
    pub counts: [usize; 3],
    /// Number of frozen statements of the unit (data definitions and bare fn headers are not statements).
    pub stmt_count: usize,
    /// Keys of those statements, sorted (for the key ratchet).
    pub stmt_keys: Vec<String>,
}

pub fn tracked(kind: &str) -> bool {
    matches!(kind, "verus" | "shipped" | "tla" | "models")
}

fn model_file(f: &str, models: bool) -> bool {
    if f.contains("/target/") || f.contains("/.") {
        return false;
    }
    let ext = f.rsplit_once('.').map(|x| x.1);
    let name = f.rsplit_once('/').map_or(f, |x| x.1);
    if models {
        matches!(ext, Some("rs" | "toml"))
    } else {
        matches!(ext, Some("tla" | "cfg" | "sh")) || !name.contains('.')
    }
}

fn plain_text(src: &str) -> String {
    src.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .flat_map(|l| l.split_whitespace())
        .collect::<Vec<_>>()
        .join(" ")
}

fn model_text(path: &str, src: &str) -> String {
    if path.ends_with(".tla") || path.ends_with(".cfg") {
        spec::words(&spec::tla_strip(src))
    } else if path.ends_with(".rs") {
        let sc = spec::scan_full(path, src);
        spec::norm_tokens(&sc.toks)
    } else {
        plain_text(src)
    }
}

struct Cache<'a> {
    tree: &'a dyn Tree,
    raw: RefCell<HashMap<String, Option<Rc<String>>>>,
    class: RefCell<HashMap<String, Option<Rc<FileClass>>>>,
}

impl<'a> Cache<'a> {
    fn new(tree: &'a dyn Tree) -> Cache<'a> {
        Cache {
            tree,
            raw: RefCell::new(HashMap::new()),
            class: RefCell::new(HashMap::new()),
        }
    }
    fn read(&self, p: &str) -> Option<Rc<String>> {
        if let Some(v) = self.raw.borrow().get(p) {
            return v.clone();
        }
        let v = self.tree.read(p).map(Rc::new);
        self.raw.borrow_mut().insert(p.to_string(), v.clone());
        v
    }
    fn classed(&self, p: &str) -> Option<Rc<FileClass>> {
        if let Some(v) = self.class.borrow().get(p) {
            return v.clone();
        }
        let v = self.read(p).map(|s| Rc::new(classify(p, &s)));
        self.class.borrow_mut().insert(p.to_string(), v.clone());
        v
    }
}

/// The identifiers of a text.
fn idents_of(text: &str) -> BTreeSet<&str> {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|w| !w.is_empty())
        .collect()
}

/// Keys of the items that a frozen statement refers to: by name in a fn/spec fn/proof fn statement, or (transitively) in the text of
/// an item that is referred to. An item the scan cannot name (`_`, empty) counts as referred to: when in doubt it is data.
pub fn referenced_items(items: &[Item], fn_texts: &str) -> BTreeSet<String> {
    let mut known: BTreeSet<&str> = idents_of(fn_texts);
    let mut out: BTreeSet<String> = BTreeSet::new();
    loop {
        let mut grew = false;
        for it in items {
            if out.contains(&it.key) {
                continue;
            }
            if it.name.is_empty() || it.name == "_" || known.contains(it.name.as_str()) {
                out.insert(it.key.clone());
                known.extend(idents_of(&it.text));
                grew = true;
            }
        }
        if !grew {
            return out;
        }
    }
}

fn agg(lines: &mut [String]) -> String {
    lines.sort();
    hex(fnv1a(&lines.join("\n")))
}

/// Fingerprints of every tracked unit; the second value is the list of extractor problems (exit 2).
pub fn fingerprints(tree: &dyn Tree, units: &[Unit]) -> (Vec<UnitFp>, Vec<String>) {
    let cache = Cache::new(tree);
    let mut errors: Vec<String> = Vec::new();
    let mut out: Vec<UnitFp> = Vec::new();
    let tla: Vec<(&Unit, String)> = units
        .iter()
        .filter(|u| u.kind == "tla")
        .map(|u| {
            (
                u,
                u.inputs
                    .first()
                    .cloned()
                    .unwrap_or_default()
                    .trim_end_matches('/')
                    .to_string(),
            )
        })
        .collect();
    let mut model_stmts: Option<Vec<spec::Stmt>> = None;
    for u in units {
        if !tracked(&u.kind) {
            out.push(UnitFp {
                unit: u.name.clone(),
                group: u.group.clone(),
                kind: u.kind.clone(),
                files: vec![],
                fp: None,
                counts: [0; 3],
                stmt_count: 0,
                stmt_keys: vec![],
            });
            continue;
        }
        if matches!(u.kind.as_str(), "verus" | "shipped") {
            let Some(target) = u.target.split_whitespace().find(|w| w.ends_with(".rs")) else {
                errors.push(format!("{}: no .rs target in `{}`", u.name, u.target));
                continue;
            };
            let read = |f: &str| cache.read(f).map(|s| (*s).clone());
            let mut files = trust::included_files(&read, target);
            files.retain(|f| cache.read(f).is_some());
            files.sort();
            if files.is_empty() {
                errors.push(format!("{}: target {target} cannot be read", u.name));
                continue;
            }
            let (mut st, mut pr, mut ex, mut da) = (vec![], vec![], vec![], vec![]);
            let mut counts = [0usize; 3];
            let mut all_items: Vec<Item> = Vec::new();
            let mut fn_texts = String::new();
            let mut core_parts: Vec<String> = Vec::new();
            let (mut any_proof, mut any_code) = (false, false);
            for f in &files {
                let Some(c) = cache.classed(f) else { continue };
                any_proof |= !c.proof_text.trim().is_empty();
                any_code |= !c.code_toks.is_empty()
                    || !c.exec_text.trim().is_empty()
                    || !c.items.is_empty();
                for e in &c.errors {
                    errors.push(format!("{}: {e}", u.name));
                }
                let item_keys: BTreeSet<&String> = c.items.iter().map(|x| &x.key).collect();
                let bare: BTreeSet<&String> = c.clauseless.iter().collect();
                for (k, h) in &c.stmts {
                    if !item_keys.contains(k) && !bare.contains(k) {
                        st.push(format!("{k}\t{}", hex(*h)));
                    }
                }
                all_items.extend(c.items.iter().cloned());
                fn_texts.push_str(&c.fn_text);
                pr.push(format!("FILE {f}\n{}", c.proof_text));
                ex.push(format!("FILE {f}\n{}", c.exec_text));
                core_parts.push(format!("FILE {f}\n{}", c.code_toks.join(" ")));
                for (n, x) in counts.iter_mut().zip(c.counts) {
                    *n += x;
                }
            }
            // A data definition that no frozen statement refers to (directly or through another referred item, by name) is
            // code; one that a statement refers to is data, rechecked by a strong prover when it changes.
            let referenced = referenced_items(&all_items, &fn_texts);
            let core = Core {
                code: core_parts.join("\n"),
                classified: true,
                has_stmt: !st.is_empty(),
                has_proof: any_proof,
                has_code: any_code,
            };
            for it in &all_items {
                if referenced.contains(&it.key) {
                    da.push(format!("{}\t{}", it.key, hex(it.hash)));
                } else {
                    ex.push(format!("ITEM {}\n{}", it.key, it.text));
                }
            }
            // Per-file order is part of proof/exec text; files are sorted already.
            let stmt_count = st.len();
            let mut stmt_keys: Vec<String> = st
                .iter()
                .map(|l| l.split('\t').next().unwrap_or("").to_string())
                .collect();
            stmt_keys.sort();
            let stmt = agg(&mut st);
            let proof = hex(fnv1a(&pr.join("\n")));
            let exec = hex(fnv1a(&ex.join("\n")));
            let data = agg(&mut da);
            out.push(UnitFp {
                unit: u.name.clone(),
                group: u.group.clone(),
                kind: u.kind.clone(),
                files,
                fp: Some(Fp {
                    stmt,
                    proof,
                    exec,
                    data,
                    core,
                }),
                counts,
                stmt_count,
                stmt_keys,
            });
            continue;
        }
        // Model units.
        let models = u.kind == "models";
        let mut files: Vec<String> = if models {
            tree.files_under(MODELS_CRATE)
        } else {
            let mine = tla
                .iter()
                .find(|(x, _)| x.name == u.name)
                .map(|x| x.1.clone())
                .unwrap_or_default();
            let longer: Vec<&String> = tla
                .iter()
                .map(|x| &x.1)
                .filter(|p| p.len() > mine.len() && p.starts_with(&format!("{mine}/")))
                .collect();
            tree.files_under(&mine)
                .into_iter()
                .filter(|f| !longer.iter().any(|p| f.starts_with(&format!("{p}/"))))
                .collect()
        };
        files.retain(|f| model_file(f, models));
        files.sort();
        if files.is_empty() {
            errors.push(format!("{}: model unit with no files", u.name));
            continue;
        }
        let stmts = model_stmts.get_or_insert_with(|| spec::model_stmts(tree).0);
        let set: BTreeSet<&String> = files.iter().collect();
        let mut st: Vec<String> = stmts
            .iter()
            .filter(|s| {
                let p = s.key.split("::").next().unwrap_or("");
                set.iter().any(|f| f.as_str() == p)
            })
            .map(|s| format!("{}\t{}", s.key, hex(s.hash)))
            .collect();
        let mut pr = Vec::new();
        for f in &files {
            if let Some(src) = cache.read(f) {
                pr.push(format!("FILE {f}\n{}", model_text(f, &src)));
            }
        }
        let st_len = st.len();
        let mut st_keys: Vec<String> = st
            .iter()
            .map(|l| l.split('\t').next().unwrap_or("").to_string())
            .collect();
        st_keys.sort();
        out.push(UnitFp {
            unit: u.name.clone(),
            group: u.group.clone(),
            kind: u.kind.clone(),
            files,
            fp: Some(Fp {
                stmt: agg(&mut st),
                proof: hex(fnv1a(&pr.join("\n"))),
                exec: "-".into(),
                data: "-".into(),
                core: Core::default(),
            }),
            counts: [0; 3],
            stmt_count: st_len,
            stmt_keys: st_keys,
        });
    }
    (out, errors)
}

// ------------------------------------------------------------------------------------------------------- the log

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub id: String,
    pub date: String,
    pub kind: String,
    pub unit: String,
    pub stmt: String,
    pub proof: String,
    pub exec: String,
    pub data: String,
    pub impl_model: String,
    pub impl_family: String,
    pub prover_model: String,
    pub prover_family: String,
    pub prover_class: String,
    pub reviewer_model: String,
    pub reviewer_family: String,
    pub verdict: String,
    pub verdict_ref: String,
    pub started_before: String,
    pub note: String,
}

#[derive(Clone, Debug)]
pub struct Entry {
    /// 1-based line in the file.
    pub line: usize,
    pub raw: String,
    pub id: String,
    pub res: Result<Record, String>,
}

fn id_ok(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn date_ok(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b.iter().enumerate().all(|(i, c)| {
            if i == 4 || i == 7 {
                *c == b'-'
            } else {
                c.is_ascii_digit()
            }
        })
}

fn fp_ok(s: &str) -> bool {
    s == "-" || (s.len() == 16 && s.chars().all(|c| c.is_ascii_hexdigit()))
}

fn parse_fields(f: &[&str]) -> Result<Record, String> {
    if f.len() != FIELDS {
        return Err(format!("{} fields, expected {FIELDS}", f.len()));
    }
    if let Some(i) = f.iter().position(|x| x.trim().is_empty()) {
        return Err(format!("field {} is empty", i + 1));
    }
    let r = Record {
        id: f[0].into(),
        date: f[1].into(),
        kind: f[2].into(),
        unit: f[3].into(),
        stmt: f[4].into(),
        proof: f[5].into(),
        exec: f[6].into(),
        data: f[7].into(),
        impl_model: f[8].into(),
        impl_family: f[9].into(),
        prover_model: f[10].into(),
        prover_family: f[11].into(),
        prover_class: f[12].into(),
        reviewer_model: f[13].into(),
        reviewer_family: f[14].into(),
        verdict: f[15].into(),
        verdict_ref: f[16].into(),
        started_before: f[17].into(),
        note: f[18].into(),
    };
    if !id_ok(&r.id) {
        return Err(format!("id `{}` has characters outside A-Za-z0-9._-", r.id));
    }
    if !date_ok(&r.date) {
        return Err(format!("date `{}` is not YYYY-MM-DD", r.date));
    }
    if !KINDS.contains(&r.kind.as_str()) {
        return Err(format!("kind `{}` is not one of {KINDS:?}", r.kind));
    }
    if !VERDICTS.contains(&r.verdict.as_str()) {
        return Err(format!(
            "verdict `{}` is not one of {VERDICTS:?}",
            r.verdict
        ));
    }
    if (r.kind == "seed") != (r.verdict == NOT_REVIEWED) {
        return Err(format!(
            "kind `{}` with verdict `{}`: only a seed is not-reviewed-under-rule, and a seed is nothing else",
            r.kind, r.verdict
        ));
    }
    for (n, v) in [
        ("statement_fp", &r.stmt),
        ("proof_fp", &r.proof),
        ("exec_fp", &r.exec),
        ("data_fp", &r.data),
    ] {
        if !fp_ok(v) {
            return Err(format!("{n} `{v}` is neither 16 hex digits nor -"));
        }
    }
    if r.kind == "pre-rule" {
        if !full_id(&r.started_before) {
            return Err(format!(
                "started_before `{}` must be a full 40-digit lowercase hex commit id (a pre-rule record names a commit before roles landed; abbreviated names can be shadowed by a branch of the same name)",
                r.started_before
            ));
        }
    } else if r.started_before != "-" {
        return Err("started_before is only for a pre-rule record; use -".into());
    }
    for (n, m, fam) in [
        ("implementer", &r.impl_model, &r.impl_family),
        ("prover", &r.prover_model, &r.prover_family),
        ("reviewer", &r.reviewer_model, &r.reviewer_family),
    ] {
        if family(m) != fam.as_str() {
            return Err(format!(
                "{n}_family `{fam}` does not match the family `{}` derived from `{m}`",
                family(m)
            ));
        }
    }
    let derived = if r.prover_model == "-" {
        "unknown"
    } else {
        class(&r.prover_model)
    };
    if r.prover_class != derived {
        return Err(format!(
            "prover_class `{}` does not match the class `{derived}` derived from `{}`",
            r.prover_class, r.prover_model
        ));
    }
    Ok(r)
}

pub fn parse_log(text: &str) -> Vec<Entry> {
    let mut v = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let t = raw.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let f: Vec<&str> = raw.split('\t').collect();
        let id = f.first().map_or("", |x| *x).to_string();
        v.push(Entry {
            line: i + 1,
            raw: raw.to_string(),
            id,
            res: parse_fields(&f),
        });
    }
    v
}

pub fn append_only(base: &str, work: &str) -> Result<(), String> {
    amend::append_only(base, work).map_err(|e| e.replace(amend::LOG, LOG))
}

pub fn template(kind: &str, unit: Option<(&str, &Fp)>) -> String {
    let (u, s, p, e, d) = match unit {
        Some((u, f)) => (
            u.to_string(),
            f.stmt.clone(),
            f.proof.clone(),
            f.exec.clone(),
            f.data.clone(),
        ),
        None => (
            "<unit>".into(),
            "<statement_fp>".into(),
            "<proof_fp>".into(),
            "<exec_fp>".into(),
            "<data_fp>".into(),
        ),
    };
    let rev = if kind == "statement" {
        "<reviewer-model-of-another-family>\t<reviewer-family>"
    } else {
        "-\t-"
    };
    let sb = if kind == "pre-rule" {
        "<commit before roles landed>"
    } else {
        "-"
    };
    format!(
        "<id>\t<YYYY-MM-DD>\t{kind}\t{u}\t{s}\t{p}\t{e}\t{d}\t<implementer-model>\t<implementer-family>\t<prover-model>\t<prover-family>\tstrong\t{rev}\taccepted\t{VERDICT_DIR}<id>.md\t{sb}\t<note>\n"
    )
}

// ------------------------------------------------------------------------------------------------------- the check

#[derive(Debug, Clone)]
pub struct Refusal {
    pub unit: String,
    pub rule: String,
    pub detail: String,
    pub hint: String,
}

pub enum Added {
    /// Normalised trailer models of the commit that added the line.
    Committed(Vec<String>),
    Uncommitted,
    Unknown,
}

pub struct CheckIn<'a> {
    pub base: &'a BTreeMap<String, Fp>,
    pub work: &'a [UnitFp],
    pub entries: &'a [Entry],
    pub base_log: &'a str,
    pub work_log: &'a str,
    /// Is the verdict file of this record there, non-empty and about this record? (path, record)
    pub verdict_ok: &'a dyn Fn(&str, &Record) -> bool,
    pub added: &'a dyn Fn(&str) -> Added,
    /// Who changed this unit between the base and HEAD, commit by commit.
    pub changes: &'a dyn Fn(&str) -> ChangeInfo,
    /// The same, skipping every commit that is an ancestor of the given commit (a pre-rule unit's `started_before`).
    pub changes_after: &'a dyn Fn(&str, &str) -> ChangeInfo,
    /// Verdict files present at the base that are modified or gone now.
    pub verdict_modified: &'a [String],
    pub merge_gate: bool,
    /// `docs/ci/proof-roles-cutoff.txt` at the base and now (None when absent).
    pub cutoff_work: Option<&'a str>,
    /// Cutoff files (`docs/ci/proof-roles-cutoff*.txt`) created, changed or deleted in the range.
    pub cutoff_changed: &'a [String],
    /// `LAGENT_ALLOW_SPEC_CHANGE` carries the explicit token `cutoff:<why>`.
    pub allow_cutoff: bool,
    /// Does this commit exist in the repository's object database (`git cat-file -e <c>^{commit}`)? A `started_before` that does
    /// not exist cannot be used to exempt anything.
    pub exists: &'a dyn Fn(&str) -> bool,
    /// Messages of `//@omit` derive lines that rustfmt `merge_derives` would merge with a neighbouring derive (see `omit_hazards`).
    pub omit_hazards: &'a [String],
    /// Registration files (`REGISTRATION_FILES`) whose change in the range is only the registration of units whose files are new:
    /// they do not count as an extractor change (`registration_only`).
    pub registration_exempt: &'a [String],
    /// Extractor sources (spec.rs, roles.rs, ...) changed in the range, and files of any unit's closure changed in the range.
    pub extractor_changed: &'a [String],
    pub unit_files_changed: &'a [String],
    /// `LAGENT_ALLOW_SPEC_CHANGE` names `extractor`.
    pub allow_extractor: bool,
}

/// The non-merge commits of base..HEAD that changed one unit's fingerprints, with their normalised `Co-Authored-By` models. A merge
/// commit is compared with the automatic merge of its parents (`git merge-tree`): what it adds by hand is its own authorship.
#[derive(Default, Clone, Debug)]
pub struct ChangeInfo {
    /// Trailers of each commit that changed the executable or the data fingerprint.
    pub exec: Vec<Vec<String>>,
    /// (commit, trailers) of each commit that changed the statement or the proof fingerprint.
    pub proof: Vec<(String, Vec<String>)>,
}

#[derive(Default, Debug)]
pub struct Outcome {
    pub refusals: Vec<Refusal>,
    pub notes: Vec<String>,
    /// Units that changed and are covered.
    pub covered: Vec<String>,
    pub unchanged: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Need {
    Statement,
    Proof,
    /// Only a data definition that a statement refers to changed.
    Data,
    /// A pre-rule record: the independent review replaces the builder/prover split.
    PreRule,
}

fn verdict_path_ok(p: &str) -> bool {
    p.starts_with(VERDICT_DIR)
        && !p.contains("..")
        && !p.contains("//")
        && p.len() > VERDICT_DIR.len()
}

/// The five headings every verdict file carries (docs/ci/PROOF-ROLES.md).
pub const VERDICT_HEADINGS: [&str; 5] = [
    "## Property",
    "## Vacuity",
    "## Shipped code",
    "## Weaknesses",
    "## Recommendation",
];

/// A verdict file is about its record: non-empty, names the record id, the unit and the fingerprints, and has the five headings.
pub fn verdict_text_ok(t: &str, r: &Record) -> bool {
    !t.trim().is_empty()
        && t.contains(&r.id)
        && t.contains(&r.unit)
        && t.contains(&r.proof)
        && (r.kind != "statement" || t.contains(&r.stmt))
        && VERDICT_HEADINGS
            .iter()
            .all(|h| t.lines().any(|l| l.trim_end() == *h))
}

/// The strong-prover rules shared by all kinds; `Err` is the first rule a record breaks: the prover is strong and is not the
/// implementer; a strong implementer's code is proved by a strong model of the other tier; a statement has a reviewer of another
/// family; the verdict file is the record's.
fn validate(
    r: &Record,
    need: Need,
    verdict_ok: &dyn Fn(&str, &Record) -> bool,
) -> Result<(), String> {
    if r.verdict != "accepted" {
        return Err(format!("verdict is `{}`, not accepted", r.verdict));
    }
    if class(&r.prover_model) != "strong" || r.prover_class != "strong" {
        return Err(format!(
            "prover `{}` is class `{}`, not strong (Opus 5.5, Fable 5.2 or stronger)",
            r.prover_model,
            class(&r.prover_model)
        ));
    }
    let (p, i) = (model_norm(&r.prover_model), model_norm(&r.impl_model));
    if i.is_empty() || r.impl_model == "unknown" || r.impl_model == "-" {
        return Err("implementer_model is not named".into());
    }
    if p == i && need != Need::PreRule {
        return Err(format!(
            "prover and implementer are the same model (`{}`)",
            r.prover_model
        ));
    }
    if need != Need::PreRule && class(&r.impl_model) != "builder" {
        // A strong implementer, and one of unknown class (treated as strong of an unknown tier, never skipped), has its code proved
        // by a strong model of the OTHER tier (Opus code by a Fable worker and the reverse); for an unknown tier the prover only
        // has to be strong and a different model, both checked above.
        let it = strong_tier(&i);
        let pt = strong_tier(&p);
        if pt.is_none() || (it.is_some() && it == pt) {
            return Err(format!(
                "implementer `{}` is a strong model ({} tier) or of unknown class: its code must be proved by a strong model of the other tier, not `{}`",
                r.impl_model,
                it.unwrap_or("unknown"),
                r.prover_model
            ));
        }
    }
    if need == Need::Statement {
        if r.reviewer_family == "other" || !amend::FAMILIES.contains(&r.reviewer_family.as_str()) {
            return Err(format!(
                "reviewer_family `{}` is not a known family",
                r.reviewer_family
            ));
        }
        if r.reviewer_family == r.prover_family {
            return Err(format!(
                "reviewer family `{}` is the prover's family",
                r.reviewer_family
            ));
        }
        let rv = model_norm(&r.reviewer_model);
        if rv == p || rv == i {
            return Err(format!(
                "reviewer `{}` is also the prover or the implementer",
                r.reviewer_model
            ));
        }
    }
    if !verdict_path_ok(&r.verdict_ref) || r.verdict_ref != format!("{VERDICT_DIR}{}.md", r.id) {
        return Err(format!(
            "verdict_ref `{}` is not {VERDICT_DIR}{}.md (the verdict file of this record)",
            r.verdict_ref, r.id
        ));
    }
    if !verdict_ok(&r.verdict_ref, r) {
        return Err(format!(
            "verdict file `{}` is missing, empty, or lacks the record id, unit, fingerprints or the five headings",
            r.verdict_ref
        ));
    }
    Ok(())
}

fn need_kind(r: &Record) -> Need {
    match r.kind.as_str() {
        "statement" => Need::Statement,
        "pre-rule" => Need::PreRule,
        _ => Need::Proof,
    }
}

fn candidates<'a>(entries: &'a [Entry], unit: &str, need: Need, fp: &Fp) -> Vec<&'a Record> {
    entries
        .iter()
        .rev()
        .filter_map(|e| e.res.as_ref().ok())
        .filter(|r| r.unit == unit && r.stmt == fp.stmt && r.proof == fp.proof && r.data == fp.data)
        .filter(|r| match need {
            Need::Statement => matches!(r.kind.as_str(), "statement" | "pre-rule"),
            Need::Proof => matches!(r.kind.as_str(), "proof-edit" | "statement" | "pre-rule"),
            Need::Data | Need::PreRule => {
                matches!(
                    r.kind.as_str(),
                    "recheck" | "proof-edit" | "statement" | "pre-rule"
                )
            }
        })
        .collect()
}

/// The coordinator-owned list `docs/ci/proof-roles-cutoff.txt`: line 1 `cutoff <commit>` (the commit that introduced `roles` on main),
/// then one line `unit<TAB>started_before_commit<TAB>reason` per unit that may ever get a `pre-rule` record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cutoff {
    pub commit: String,
    pub units: Vec<(String, String, String)>,
}

pub fn parse_cutoff(text: &str) -> Result<Cutoff, String> {
    let mut lines = text
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'));
    let first = lines.next().ok_or("the file is empty")?;
    let commit = first
        .strip_prefix("cutoff ")
        .map(str::trim)
        .filter(|c| full_id(c))
        .ok_or_else(|| format!("line 1 `{first}` is not `cutoff <full 40-digit lowercase hex commit id>` (the id is informational)"))?;
    let mut units = Vec::new();
    for l in lines {
        let f: Vec<&str> = l.split('\t').collect();
        if f.len() != 3 || f.iter().any(|x| x.trim().is_empty()) {
            return Err(format!(
                "`{l}`: expected unit<TAB>started_before_commit<TAB>reason, none empty"
            ));
        }
        if !full_id(f[1]) {
            return Err(format!(
                "`{l}`: started_before `{}` must be a full 40-digit lowercase hex commit id, not an abbreviation or a name",
                f[1]
            ));
        }
        units.push((f[0].to_string(), f[1].to_string(), f[2].to_string()));
    }
    Ok(Cutoff {
        commit: commit.to_string(),
        units,
    })
}

/// The closing rule for `pre-rule` records. The list is the coordinator's and is protected from any change in a merge: only the units
/// named in it may ever get such a record, each with its fixed `started_before`. That commit may be ANY commit the list names, on a
/// branch or not (a unit's author may have started on a side branch); it only has to exist in the object database at check time, else
/// the record is refused. Commits that are ancestors of it are exempt from the authorship walk, later ones are walked under strict
/// Option A. Absent file: no pre-rule record is accepted.
fn pre_rule_ok(i: &CheckIn, r: &Record) -> Result<(), String> {
    let Some(text) = i.cutoff_work else {
        return Err(format!(
            "{CUTOFF} is absent: no pre-rule record is accepted until the coordinator writes it"
        ));
    };
    let cut = parse_cutoff(text).map_err(|e| format!("{CUTOFF}: {e}"))?;
    let Some((_, sb, _)) = cut.units.iter().find(|(u, _, _)| *u == r.unit) else {
        return Err(format!(
            "`{}` is not listed in {CUTOFF}: only the coordinator's list may get pre-rule records (strict Option A applies)",
            r.unit
        ));
    };
    if *sb != r.started_before {
        return Err(format!(
            "started_before {} differs from the {} that {CUTOFF} fixes for `{}`",
            r.started_before, sb, r.unit
        ));
    }
    if !(i.exists)(sb) {
        return Err(format!(
            "started_before {sb} (listed in {CUTOFF}) does not exist in this repository's object database: fetch the branch that holds it, or ask the coordinator to correct the list"
        ));
    }
    Ok(())
}

pub fn check(i: &CheckIn) -> Outcome {
    let mut o = Outcome::default();
    let known: BTreeSet<&str> = i.work.iter().map(|u| u.unit.as_str()).collect();
    // Registration lines for units whose files are new in the range are not an extractor change (`registration_only`).
    let extractor_changed: Vec<&String> = i
        .extractor_changed
        .iter()
        .filter(|p| !i.registration_exempt.contains(p))
        .collect();
    for p in i
        .extractor_changed
        .iter()
        .filter(|p| i.registration_exempt.contains(p))
    {
        o.notes.push(format!(
            "{p}: only registers units whose files are new in this range: not an extractor change"
        ));
    }
    if !extractor_changed.is_empty() && !i.unit_files_changed.is_empty() && !i.allow_extractor {
        o.refusals.push(Refusal {
            unit: "-".into(),
            rule: "extractor-changed-with-unit-files".into(),
            detail: format!(
                "the range changes the extractor ({}) and verified unit files ({}): both sides of the freeze would use the new extractor",
                extractor_changed.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", "),
                i.unit_files_changed.iter().take(6).cloned().collect::<Vec<_>>().join(", ")
            ),
            hint: "land the extractor change alone, or merge with LAGENT_ALLOW_SPEC_CHANGE carrying the token `extractor:<why>` (the coordinator's decision)".into(),
        });
    }
    if let Err(e) = append_only(i.base_log, i.work_log) {
        o.refusals.push(Refusal {
            unit: "-".into(),
            rule: "log-append-only".into(),
            detail: e,
            hint: format!("restore {LOG} from the base commit and only add lines at the end"),
        });
    }
    for m in i.omit_hazards {
        o.refusals.push(Refusal {
            unit: "-".into(),
            rule: "omit-derive-merge-hazard".into(),
            detail: m.clone(),
            hint: "rustfmt `merge_derives` merges adjacent `#[derive(..)]` attributes into the first one and keeps the `//@omit` tag, so Verus would stop seeing the other derives. Put every derive of the item on ONE `#[derive(..)] //@omit` line, or on one line without the tag".into(),
        });
    }
    if !i.cutoff_changed.is_empty() && !i.allow_cutoff {
        o.refusals.push(Refusal {
            unit: "-".into(),
            rule: "cutoff-protected".into(),
            detail: format!(
                "the range creates, changes or deletes {} : the list of units that may get pre-rule records belongs to the coordinator",
                i.cutoff_changed.join(", ")
            ),
            hint: "do not touch it in a branch; the coordinator lands it with LAGENT_ALLOW_SPEC_CHANGE naming `cutoff:<why>`".into(),
        });
    }
    // New lines: well formed, a known unit, a unique id, written by the commit of the prover they name.
    let base_n = i.base_log.lines().count();
    let mut ids: BTreeMap<&str, usize> = BTreeMap::new();
    for e in i.entries {
        *ids.entry(e.id.as_str()).or_insert(0) += 1;
    }
    for e in i.entries.iter().filter(|e| e.line > base_n) {
        let r = match &e.res {
            Ok(r) => r,
            Err(m) => {
                o.refusals.push(Refusal {
                    unit: "-".into(),
                    rule: "malformed-record".into(),
                    detail: format!("{LOG} line {} (id `{}`): {m}", e.line, e.id),
                    hint: "fix the new line (it is new in this branch, so it may be rewritten before it is committed)".into(),
                });
                continue;
            }
        };
        if ids.get(r.id.as_str()).copied().unwrap_or(0) > 1 {
            o.refusals.push(Refusal {
                unit: r.unit.clone(),
                rule: "duplicate-id".into(),
                detail: format!("id `{}` appears more than once", r.id),
                hint: "ids are unique".into(),
            });
        }
        if !known.contains(r.unit.as_str()) {
            o.refusals.push(Refusal {
                unit: r.unit.clone(),
                rule: "unknown-unit".into(),
                detail: format!(
                    "record `{}` names a unit that is not in the inventory",
                    r.id
                ),
                hint: "use the unit name from tools/ci/proof-inventory.tsv".into(),
            });
        }
        if r.kind == "seed" {
            continue;
        }
        match (i.added)(&e.raw) {
            Added::Committed(models) => {
                if !models.contains(&model_norm(&r.prover_model)) {
                    o.refusals.push(Refusal {
                        unit: r.unit.clone(),
                        rule: "trailer-mismatch".into(),
                        detail: format!(
                            "record `{}` declares prover `{}`, but the commit that added it carries Co-Authored-By {:?}",
                            r.id, r.prover_model, models
                        ),
                        hint: "the declared prover must be the model named in the trailer of the commit that adds its record line (the prover commits its own record)".into(),
                    });
                }
            }
            Added::Uncommitted => {
                if i.merge_gate {
                    o.refusals.push(Refusal {
                        unit: r.unit.clone(),
                        rule: "trailer-unverifiable".into(),
                        detail: format!("record `{}` is not in any commit of the range", r.id),
                        hint: "commit the record line with the prover's Co-Authored-By trailer"
                            .into(),
                    });
                } else {
                    o.notes.push(format!(
                        "record `{}` is not committed yet: its trailer is checked at the merge gate",
                        r.id
                    ));
                }
            }
            Added::Unknown => o.refusals.push(Refusal {
                unit: r.unit.clone(),
                rule: "trailer-unverifiable".into(),
                detail: format!("cannot find the commit that added record `{}`", r.id),
                hint: "run inside the git worktree with the base commit available".into(),
            }),
        }
    }
    for f in i.verdict_modified {
        o.refusals.push(Refusal {
            unit: "-".into(),
            rule: "verdict-file-modified".into(),
            detail: format!("{f} existed at the base commit and is modified or deleted now (verdict files are append-only)"),
            hint: "restore it from the base commit; write a new record and a new verdict file instead".into(),
        });
    }
    // Units.
    for u in i.work {
        let Some(w) = &u.fp else { continue };
        let b = i.base.get(&u.unit);
        let need = match b {
            None => Some(Need::Statement),
            Some(b) if b.stmt != w.stmt => Some(Need::Statement),
            Some(b) if b.proof != w.proof => Some(Need::Proof),
            Some(b) if b.data != w.data => Some(Need::Data),
            _ => None,
        };
        let Some(need) = need else {
            o.unchanged += 1;
            continue;
        };
        let why = match (b, need) {
            (None, _) => "new unit",
            (_, Need::Statement) => "statement changed",
            (_, Need::Proof) => "proof text changed",
            (_, Need::Data) => "a data definition that a statement refers to changed",
            (_, Need::PreRule) => "pre-rule",
        };
        let cands = candidates(i.entries, &u.unit, need, w);
        let mut errs: Vec<String> = Vec::new();
        let mut used: Option<&Record> = None;
        for r in &cands {
            let res = validate(r, need_kind(r), i.verdict_ok).and_then(|()| {
                if r.kind == "pre-rule" {
                    pre_rule_ok(i, r)
                } else {
                    Ok(())
                }
            });
            match res {
                Ok(()) => {
                    used = Some(r);
                    break;
                }
                Err(e) => errs.push(format!("record `{}`: {e}", r.id)),
            }
        }
        let kind = match need {
            Need::Statement => "statement",
            Need::Proof => "proof-edit",
            Need::Data => "recheck",
            Need::PreRule => "pre-rule",
        };
        let hint = format!(
            "add (as the prover, in your own commit) and write docs/ci/verdicts/<id>.md:\n      {}",
            template(kind, Some((&u.unit, w))).trim_end()
        );
        let Some(rec) = used else {
            let detail = if cands.is_empty() {
                format!(
                    "{why}: no `{kind}` record carries the current fingerprints (statement_fp {}, proof_fp {}, data_fp {})",
                    w.stmt, w.proof, w.data
                )
            } else {
                format!("{why}: {}", errs.join("; "))
            };
            o.refusals.push(Refusal {
                unit: u.unit.clone(),
                rule: match need {
                    Need::Statement => "statement-needs-strong-prover-and-reviewer".into(),
                    Need::Proof => "proof-edit-needs-strong-prover".into(),
                    Need::Data => "recheck-needs-strong-prover".into(),
                    Need::PreRule => "pre-rule".into(),
                },
                detail,
                hint,
            });
            continue;
        };
        if rec.kind == "pre-rule" {
            // Work before `started_before` was written under no rule and is judged by the independent review; every commit after it is
            // strict Option A, so the authorship walk below runs on those commits only.
            o.notes.push(format!(
                "{}: PRE-RULE record `{}`: commits up to {} are exempt, later ones are walked",
                u.unit, rec.id, rec.started_before
            ));
        }
        // A unit with a valid `pre-rule` record keeps that exemption for the commits up to its `started_before`, whichever record
        // covers the current fingerprints: a later `statement` record (a strong prover of the other tier strengthening the
        // pre-rule work) is checked on the commits AFTER it, and the pre-rule record's implementer stands for the exempt code.
        let pre: Option<&Record> = if rec.kind == "pre-rule" {
            None
        } else {
            i.entries
                .iter()
                .filter_map(|e| e.res.as_ref().ok())
                .find(|r| {
                    r.unit == u.unit
                        && r.kind == "pre-rule"
                        && validate(r, need_kind(r), i.verdict_ok)
                            .and_then(|()| pre_rule_ok(i, r))
                            .is_ok()
                })
        };
        let info = if rec.kind == "pre-rule" {
            (i.changes_after)(&u.unit, &rec.started_before)
        } else if let Some(p) = pre {
            o.notes.push(format!(
                "{}: record `{}` covers the current fingerprints; PRE-RULE record `{}` still exempts the commits up to {}",
                u.unit, rec.id, p.id, p.started_before
            ));
            (i.changes_after)(&u.unit, &p.started_before)
        } else {
            (i.changes)(&u.unit)
        };
        // Who wrote the exempt code: the pre-rule record's implementer (the covering record itself, or the earlier one).
        let pre_impl: Option<String> = if rec.kind == "pre-rule" {
            Some(model_norm(&rec.impl_model))
        } else {
            pre.map(|p| model_norm(&p.impl_model))
        };
        // Who wrote the statement or proof text: every commit that changed it must carry a strong trailer.
        let mut bad = false;
        for (c, tr) in &info.proof {
            let why_bad = if tr.is_empty() {
                Some(
                    "carries no Co-Authored-By trailer, so its author cannot be shown to be strong",
                )
            } else if tr.iter().any(|m| class(m) != "strong") {
                Some("carries a builder-class (non-strong) Co-Authored-By trailer")
            } else {
                None
            };
            if let Some(m) = why_bad {
                bad = true;
                o.refusals.push(Refusal {
                    unit: u.unit.clone(),
                    rule: "proof-or-statement-written-by-a-builder".into(),
                    detail: format!(
                        "{why}: commit {c} changed this unit's statement or proof text and {m} ({tr:?}). Builders write code, never statements or proofs"
                    ),
                    hint: "the strong prover makes the statement/proof change in its own commit (split the commit if a builder's one mixes code and proof text)".into(),
                });
            }
        }
        if bad {
            continue;
        }
        // One model must not have written both the code and the statement/proof text (whatever the record says), and the declared
        // prover must have written some of the statement or proof text itself: a signature on another model's proof proves nothing.
        // For a `pre-rule` record the cross-signing rule runs on the commits after `started_before` (the ones it walks); the prover
        // rule does not apply to it.
        let exec_changed = b.is_none_or(|b| b.exec != w.exec || b.data != w.data);
        {
            let mut code_models: BTreeSet<&String> = info.exec.iter().flatten().collect();
            // the exempt code of a pre-rule unit was written by the pre-rule record's implementer
            if let Some(pi) = &pre_impl {
                code_models.insert(pi);
            }
            let both: BTreeSet<&String> = info
                .proof
                .iter()
                .flat_map(|(_, t)| t.iter())
                .filter(|m| code_models.contains(m))
                .collect();
            if (exec_changed || pre_impl.is_some()) && !both.is_empty() {
                o.refusals.push(Refusal {
                    unit: u.unit.clone(),
                    rule: "one-model-wrote-code-and-proof".into(),
                    detail: format!(
                        "{why}: {both:?} appears in the trailers of both a commit that changed this unit's code and a commit that changed its statement or proof text"
                    ),
                    hint: "the model that writes the code must not write the statement or proof: let a builder make the code change and the strong prover the statement/proof change in its own commit".into(),
                });
                continue;
            }
            // a pre-rule prover is the independent reviewer of the exempt work: it need not have written anything
            let pm = model_norm(&rec.prover_model);
            if rec.kind != "pre-rule"
                && matches!(need, Need::Statement | Need::Proof)
                && !info.proof.iter().any(|(_, t)| t.contains(&pm))
            {
                o.refusals.push(Refusal {
                    unit: u.unit.clone(),
                    rule: "prover-wrote-no-statement-or-proof".into(),
                    detail: format!(
                        "{why}: record `{}` names prover `{}`, but no commit that changed this unit's statement or proof text carries that model's trailer",
                        rec.id, rec.prover_model
                    ),
                    hint: "the prover writes the statement or proof change in its own commit; adding the record line is not enough".into(),
                });
                continue;
            }
        }
        // Code commits: each must carry a trailer; the implementer must be among them; the prover must not.
        if exec_changed {
            if info.exec.iter().any(Vec::is_empty) {
                o.refusals.push(Refusal {
                    unit: u.unit.clone(),
                    rule: "code-commit-without-trailer".into(),
                    detail: format!(
                        "{why}: a commit that changed this unit's code carries no Co-Authored-By trailer, so the one-model and implementer checks cannot see its author"
                    ),
                    hint: "every commit must carry the Co-Authored-By trailer of the model that wrote it".into(),
                });
                continue;
            }
            let pm = model_norm(&rec.prover_model);
            let im = model_norm(&rec.impl_model);
            let em: Vec<&String> = info.exec.iter().flatten().collect();
            if !em.is_empty() && !em.iter().any(|m| **m == im) && pre_impl.as_ref() != Some(&im) {
                o.refusals.push(Refusal {
                    unit: u.unit.clone(),
                    rule: "implementer-not-in-trailers".into(),
                    detail: format!(
                        "record `{}` names implementer `{}`, but the commits that changed this unit's code carry {:?}",
                        rec.id, rec.impl_model, em
                    ),
                    hint: "implementer_model must be a model named in the trailer of a commit that changed the code".into(),
                });
                continue;
            }
            if em.iter().any(|m| **m == pm) {
                o.refusals.push(Refusal {
                    unit: u.unit.clone(),
                    rule: "one-model-wrote-code-and-proof".into(),
                    detail: format!(
                        "{why}, and the record's prover `{}` is named in the trailer of a commit that changed this unit's executable code",
                        rec.prover_model
                    ),
                    hint: "the model that edits the code must not be the one that proves it: the prover writes the requirement, a builder makes the code change, the prover rechecks. A unit started before roles landed needs a `pre-rule` record (independent Opus review, docs/ci/PROOF-ROLES.md)".into(),
                });
                continue;
            }
        }
        o.covered
            .push(format!("{} ({why}; record {})", u.unit, rec.id));
    }
    o
}

pub fn render(o: &Outcome, base: &str) -> (String, i32) {
    let mut s = String::new();
    for n in &o.notes {
        s.push_str(&format!("roles: note: {n}\n"));
    }
    if o.refusals.is_empty() {
        s.push_str(&format!(
            "roles: ok against {base}: {} unit(s) unchanged in statement and proof, {} covered by a record\n",
            o.unchanged,
            o.covered.len()
        ));
        for c in &o.covered {
            s.push_str(&format!("roles:   covered: {c}\n"));
        }
        return (s, 0);
    }
    s.push_str(
        "================================================================================\n",
    );
    s.push_str(
        "ROLES REFUSED: the model that writes the code must not prove it or judge the proof\n",
    );
    s.push_str("  (docs/ci/PROOF-ROLES.md). There is no allow flag: a strong prover and, for a statement,\n");
    s.push_str("  a reviewer of another family are required, and a reviewer who is unavailable blocks the merge.\n");
    s.push_str(
        "================================================================================\n",
    );
    for r in &o.refusals {
        s.push_str(&format!("[{}] {}\n    {}\n", r.rule, r.unit, r.detail));
    }
    s.push_str("What to add:\n");
    for r in &o.refusals {
        s.push_str(&format!("  {}: {}\n", r.unit, r.hint));
    }
    (s, 1)
}

// ------------------------------------------------------------------------------------------------------- status and summary

/// `reviewed` when a valid record for the CURRENT statement, proof and data fingerprints exists, `pre-rule` when the record is a
/// pre-rule one (independent review of code and proof written before the rule), else why not.
pub fn status(
    entries: &[Entry],
    u: &UnitFp,
    verdict_ok: &dyn Fn(&str, &Record) -> bool,
) -> &'static str {
    let Some(fp) = &u.fp else { return "n/a" };
    let cs = candidates(entries, &u.unit, Need::Data, fp);
    for r in &cs {
        if validate(r, need_kind(r), verdict_ok).is_ok() {
            return if r.kind == "pre-rule" {
                "pre-rule"
            } else {
                "reviewed"
            };
        }
    }
    let any: Vec<&Record> = entries
        .iter()
        .filter_map(|e| e.res.as_ref().ok())
        .filter(|r| r.unit == u.unit)
        .collect();
    match any.last() {
        None => "no-record",
        Some(r) if r.kind == "seed" => "not-reviewed(seed)",
        Some(_) => "not-reviewed(stale-or-invalid)",
    }
}

/// group -> (reviewed, pre-rule, not reviewed)
pub fn burn_down(
    entries: &[Entry],
    units: &[UnitFp],
    verdict_ok: &dyn Fn(&str, &Record) -> bool,
) -> BTreeMap<String, (usize, usize, usize)> {
    let mut m: BTreeMap<String, (usize, usize, usize)> = BTreeMap::new();
    for u in units.iter().filter(|u| u.fp.is_some()) {
        let e = m.entry(u.group.clone()).or_default();
        match status(entries, u, verdict_ok) {
            "reviewed" => e.0 += 1,
            "pre-rule" => e.1 += 1,
            _ => e.2 += 1,
        }
    }
    m
}

// ------------------------------------------------------------------------------------------------------- git plumbing and command

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let o = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .ok()?;
    o.status
        .success()
        .then(|| String::from_utf8_lossy(&o.stdout).into_owned())
}

fn git1(root: &Path, args: &[&str]) -> Option<String> {
    git(root, args)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn val(args: &[String], k: &str) -> Option<String> {
    args.iter()
        .position(|a| a == k)
        .and_then(|i| args.get(i + 1))
        .cloned()
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

fn today() -> String {
    Command::new("date")
        .arg("+%F")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| date_ok(s))
        .unwrap_or_else(|| "2026-11-11".into())
}

fn commit_trailers(root: &Path, c: &str) -> Vec<String> {
    git(root, &["log", "-1", "--format=%B", c])
        .map(|m| trailer_models(&m))
        .unwrap_or_default()
}

/// raw log line -> trailer models of the oldest non-merge commit of base..HEAD that added it.
fn added_map(root: &Path, base: &str) -> BTreeMap<String, Vec<String>> {
    let mut m = BTreeMap::new();
    let revs = git(
        root,
        &[
            "rev-list",
            "--no-merges",
            "--reverse",
            &format!("{base}..HEAD"),
            "--",
            LOG,
        ],
    )
    .unwrap_or_default();
    for c in revs.lines() {
        let new = git(root, &["show", &format!("{c}:{LOG}")]).unwrap_or_default();
        let old = git(root, &["show", &format!("{c}^:{LOG}")]).unwrap_or_default();
        let old: BTreeSet<&str> = old.lines().collect();
        let tr = commit_trailers(root, c);
        for l in new.lines().filter(|l| !old.contains(l)) {
            m.entry(l.to_string()).or_insert_with(|| tr.clone());
        }
    }
    m
}

/// The verdict file of a record exists in `tree`, is not empty and is about this record (`verdict_text_ok`).
fn verdict_checker(tree: &dyn Tree) -> impl Fn(&str, &Record) -> bool + '_ {
    move |p: &str, r: &Record| {
        verdict_path_ok(p) && tree.read(p).is_some_and(|t| verdict_text_ok(&t, r))
    }
}

/// Fingerprint of one unit at a revision (`None` when it cannot be computed there).
fn rev_fp(root: &Path, rev: &str, name: &str, peers: &[Unit]) -> Option<Fp> {
    let t = GitTree {
        root: root.to_path_buf(),
        rev: rev.to_string(),
    };
    tree_fp(&t, name, peers).0
}

/// Fingerprint of one unit in a tree, and whether the unit is ABSENT there (its target file cannot be read) as opposed to present
/// but not fingerprintable (a parse error, a failing peer).
fn tree_fp(t: &dyn Tree, name: &str, peers: &[Unit]) -> (Option<Fp>, bool) {
    let (v, e) = fingerprints(t, peers);
    let prefix = format!("{name}: target ");
    let absent = e
        .iter()
        .any(|m| m.starts_with(&prefix) && m.ends_with(" cannot be read"));
    if !e.is_empty() {
        return (None, absent);
    }
    (
        v.into_iter().find(|u| u.unit == name).and_then(|u| u.fp),
        false,
    )
}

/// A commit's parent tree with the files the commit RENAMED or COPIED read from their sources: what the new path "was" before.
struct MovedTree {
    inner: GitTree,
    /// new path -> source path
    from: BTreeMap<String, String>,
}

impl Tree for MovedTree {
    fn read(&self, path: &str) -> Option<String> {
        match self.from.get(path) {
            Some(src) => self.inner.read(src),
            None => self.inner.read(path),
        }
    }
    fn files_under(&self, dir: &str) -> Vec<String> {
        let mut v = self.inner.files_under(dir);
        for k in self.from.keys() {
            if k.starts_with(&format!("{dir}/")) && !v.contains(k) {
                v.push(k.clone());
            }
        }
        v.sort();
        v
    }
}

/// Renames and copies (new path -> source path) that `commit` made against `parent` (`git diff-tree -M -C --find-copies-harder`:
/// a copy of a file the commit did not touch counts too).
fn moved_files(root: &Path, parent: &str, commit: &str) -> BTreeMap<String, String> {
    let out = git(
        root,
        &[
            "diff-tree",
            "-r",
            "-M",
            "-C",
            "--find-copies-harder",
            "--name-status",
            parent,
            commit,
        ],
    )
    .unwrap_or_default();
    let mut m = BTreeMap::new();
    for l in out.lines() {
        let mut it = l.split('\t');
        let (Some(st), Some(old), Some(new)) = (it.next(), it.next(), it.next()) else {
            continue;
        };
        if st.starts_with('R') || st.starts_with('C') {
            m.insert(new.to_string(), old.to_string());
        }
    }
    m
}

/// Who changed a unit between `base` and HEAD: for each non-merge commit that touches one of `files`, the fingerprints at the
/// commit and at its first parent are compared; a commit that moves the executable fingerprint and/or the statement or proof
/// fingerprint is listed with its trailers. `peers` is the set of units fingerprinted together (model units claim files against
/// each other).
#[cfg(test)]
pub fn change_info(
    root: &Path,
    base: &str,
    name: &str,
    peers: &[Unit],
    files: &[String],
    skip_ancestors_of: Option<&str>,
) -> ChangeInfo {
    change_info_owned(root, base, name, peers, files, skip_ancestors_of, None)
}

/// `change_info` with the set of files that were inside some unit at the base given (`owned_at_base`): a rename or copy source is
/// trusted only from that set. `None` computes it from `peers` alone.
pub fn change_info_owned(
    root: &Path,
    base: &str,
    name: &str,
    peers: &[Unit],
    files: &[String],
    skip_ancestors_of: Option<&str>,
    owned_at_base: Option<&BTreeSet<String>>,
) -> ChangeInfo {
    let mut info = ChangeInfo::default();
    if files.is_empty() {
        return info;
    }
    let mut args: Vec<String> = vec![
        "rev-list".into(),
        "--full-history".into(),
        "--reverse".into(),
        format!("{base}..HEAD"),
        "--".into(),
    ];
    args.extend(files.iter().cloned());
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    // Non-merge commits that touch the unit's files; merges are enumerated separately below (a path-limited walk drops a merge that
    // is TREESAME to one parent, which is exactly a conflict resolved to one side).
    let mut nm = a.clone();
    nm[1] = "--no-merges";
    let mut revs = git(root, &nm).unwrap_or_default();
    let merges = git(
        root,
        &[
            "rev-list",
            "--merges",
            "--reverse",
            &format!("{base}..HEAD"),
        ],
    )
    .unwrap_or_default();
    revs.push('\n');
    revs.push_str(&merges);
    let mut cache: HashMap<String, Option<Fp>> = HashMap::new();
    let mut at = |rev: &str| -> Option<Fp> {
        cache
            .entry(rev.to_string())
            .or_insert_with(|| rev_fp(root, rev, name, peers))
            .clone()
    };
    let mut base_files: Option<BTreeSet<String>> = None;
    for c in revs.lines().filter(|l| !l.trim().is_empty()) {
        if let Some(sb) = skip_ancestors_of
            && Command::new("git")
                .args(["merge-base", "--is-ancestor", c, sb])
                .current_dir(root)
                .output()
                .is_ok_and(|o| o.status.success())
        {
            continue;
        }
        let parents: Vec<String> = git1(root, &["rev-list", "--parents", "-n", "1", c])
            .map(|l| l.split_whitespace().skip(1).map(str::to_string).collect())
            .unwrap_or_default();
        let after = at(c);
        // What the commit is compared with: its parent; for a merge, the automatic merge of its two parents (what git itself
        // would have made), so only what the merge commit adds by hand counts as its authorship.
        let mut before = match parents.len() {
            0 => None,
            1 => at(&parents[0]),
            2 => merge_tree(root, &parents[0], &parents[1]).and_then(|t| at(&t)),
            _ => None,
        };
        // A commit that CREATES the unit (its target did not exist before) is classified by what the file holds, like any other
        // commit: no statement and no proof text means a code-only commit. A file made by a rename or copy is compared with its
        // source. When the unit merely cannot be fingerprinted before (a parse error), nothing is assumed: both sides count.
        let mut creation = false;
        if before.is_none() && after.as_ref().is_some_and(|a| a.core.classified) {
            let parent_tree = |rev: &str| GitTree {
                root: root.to_path_buf(),
                rev: rev.to_string(),
            };
            match parents.len() {
                0 => creation = true,
                1 => {
                    if tree_fp(&parent_tree(&parents[0]), name, peers).1 {
                        creation = true;
                        // A rename or copy source counts only when it is unchanged since the base and was inside some unit's
                        // file set there: a file staged outside every unit (never walked) could hold statements or proof text
                        // that a move would carry in unseen. Anything else is a plain creation, classified by content.
                        let mut from = moved_files(root, &parents[0], c);
                        if !from.is_empty() {
                            let owned = owned_at_base.unwrap_or_else(|| {
                                base_files.get_or_insert_with(|| {
                                    let bt = GitTree {
                                        root: root.to_path_buf(),
                                        rev: base.to_string(),
                                    };
                                    fingerprints(&bt, peers)
                                        .0
                                        .into_iter()
                                        .flat_map(|u| u.files)
                                        .collect()
                                })
                            });
                            from.retain(|_, src| {
                                owned.contains(src.as_str())
                                    && git(
                                        root,
                                        &["diff", "--quiet", base, &parents[0], "--", src.as_str()],
                                    )
                                    .is_some()
                            });
                        }
                        if !from.is_empty() {
                            let mt = MovedTree {
                                inner: parent_tree(&parents[0]),
                                from,
                            };
                            if let (Some(b), _) = tree_fp(&mt, name, peers) {
                                before = Some(b);
                            }
                        }
                    }
                }
                2 => {
                    creation = merge_tree(root, &parents[0], &parents[1])
                        .is_some_and(|t| tree_fp(&parent_tree(&t), name, peers).1);
                }
                _ => {}
            }
        }
        let (exec, proof) = match (&before, &after) {
            (Some(b), Some(a)) => (
                (b.exec != a.exec || b.data != a.data) && !promotion_only(&b.core, &a.core),
                b.stmt != a.stmt || b.proof != a.proof,
            ),
            (None, Some(a)) if creation => (a.core.has_code, a.core.has_stmt || a.core.has_proof),
            _ => (true, true),
        };
        let tr = commit_trailers(root, c);
        if exec {
            info.exec.push(tr.clone());
        }
        if proof {
            info.proof.push((c[..c.len().min(10)].to_string(), tr));
        }
    }
    info
}

/// The code part of a plain `fn` statement (attributes, signature) as tokens: the clauses (from the first clause keyword on) and the
/// verifier attributes (`#[.. verus_spec / verus_verify ..]`) are verification text, not code; `-> (r: T)` is `-> T`. Any other
/// attribute (`cfg`, `no_mangle`, a steering one) stays: adding it is a code change.
fn fn_code_header(t: &[Tok], in_verus: &[bool]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut k = 0;
    while k < t.len() {
        if let Some(e) = spec::attr_end(t, k) {
            let e = e.min(t.len()).max(k + 1);
            if !t[k..e]
                .iter()
                .any(|x| x.s == "verus_spec" || x.s == "verus_verify")
            {
                out.extend(t[k..e].iter().map(|x| x.s.clone()));
            }
            k = e;
            continue;
        }
        if spec::counts(&t[k], in_verus[k]) {
            break;
        }
        out.push(t[k].s.clone());
        k += 1;
    }
    strip_ret_binder(&out)
}

/// `-> (r: T)` (a name for the result, needed once an `ensures` mentions it) is `-> T` as far as the signature goes.
fn strip_ret_binder(t: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut k = 0;
    while k < t.len() {
        if t[k] == "->"
            && t.get(k + 1).map(String::as_str) == Some("(")
            && t.get(k + 2)
                .is_some_and(|x| x.chars().all(|c| c.is_alphanumeric() || c == '_'))
            && t.get(k + 3).map(String::as_str) == Some(":")
        {
            let mut d = 0i32;
            let mut close = None;
            for (m, x) in t.iter().enumerate().skip(k + 1) {
                match x.as_str() {
                    "(" => d += 1,
                    ")" => {
                        d -= 1;
                        if d == 0 {
                            close = Some(m);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            if let Some(c) = close {
                out.push("->".into());
                out.extend(t[k + 4..c].iter().cloned());
                k = c + 1;
                continue;
            }
        }
        out.push(t[k].clone());
        k += 1;
    }
    out
}

/// True when the code token streams of two revisions of a unit are the same: the only exec difference (if any) is that bare fn
/// headers (code) became statements by gaining clauses or verifier attributes, or the reverse. The streams hold every executable
/// token (bodies, impl wrappers), every data definition, and each plain fn's header without its clauses, so a changed body token,
/// name, parameter, return type, `where` bound, added or removed fn or any other attribute breaks the equality. Such a commit is the
/// prover's clauses, not code: it does not count as an exec-changing commit for the one-model rule. (The reverse direction is a
/// removed statement: `statement_fp` moves, and a builder's commit that does it is refused as a builder's statement change.)
pub fn promotion_only(b: &Core, a: &Core) -> bool {
    b.code == a.code
}

/// The tree of the automatic merge of `p1` and `p2` (`git merge-tree --write-tree`); with conflicts it is the tree with conflict
/// markers, which the merge commit's resolution then differs from.
fn merge_tree(root: &Path, p1: &str, p2: &str) -> Option<String> {
    let o = Command::new("git")
        .args(["merge-tree", "--write-tree", p1, p2])
        .current_dir(root)
        .output()
        .ok()?;
    if !matches!(o.status.code(), Some(0) | Some(1)) {
        return None;
    }
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .next()
        .map(|l| l.trim().to_string())
        .filter(|l| l.len() >= 40 && l.chars().all(|c| c.is_ascii_hexdigit()))
}

/// Tracked files whose working-tree or index state differs from HEAD (`git status --porcelain`, untracked files left out).
fn dirty_tracked(root: &Path) -> Vec<String> {
    git(root, &["status", "--porcelain", "--untracked-files=no"])
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

fn top() -> Result<PathBuf, String> {
    git1(Path::new("."), &["rev-parse", "--show-toplevel"])
        .map(PathBuf::from)
        .ok_or_else(|| "not inside a git repository".to_string())
}

type Rooted = (Vec<Unit>, Vec<UnitFp>, Vec<String>);

fn rooted(tree: &dyn Tree) -> Result<Rooted, String> {
    let units = inventory::from_tsv(
        &tree
            .read(inventory::INVENTORY)
            .ok_or_else(|| format!("cannot read {}", inventory::INVENTORY))?,
    )?;
    let (fps, errs) = fingerprints(tree, &units);
    Ok((units, fps, errs))
}

/// Sources of the extractor: all of `tools/ci/lucina-ci/**` (src, Cargo.*) and the unit lists. A change to any of them together with
/// verified unit files is refused at the merge gate. (Structural gap, see PROOF-ROLES.md: this stops the SAME merge; the ratchet file
/// `tools/ci/statement-counts.tsv` stops the two-step attack.)
pub fn is_extractor_path(p: &str) -> bool {
    p.starts_with("tools/ci/lucina-ci/")
        || p == "tools/ci/proof-inventory.tsv"
        || p == "tools/ci/proof-groups.tsv"
}

/// Is `path` one of the protected cutoff files?
pub fn is_cutoff_path(p: &str) -> bool {
    p == CUTOFF || (p.starts_with("docs/ci/proof-roles-cutoff-") && p.ends_with(".txt"))
}

/// Does an allow string (the `LAGENT_ALLOW_SPEC_CHANGE` value) carry the explicit token `name:` followed by a reason? The token starts
/// the string or follows whitespace, `,` or `;`; a mention of the word elsewhere does not count.
pub fn allow_token(env: &str, name: &str) -> bool {
    let pat = format!("{name}:");
    let mut from = 0;
    while let Some(p) = env[from..].find(&pat) {
        let at = from + p;
        let before_ok = at == 0
            || env[..at]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_whitespace() || c == ',' || c == ';');
        let after = env[at + pat.len()..].trim_start();
        if before_ok && !after.is_empty() {
            return true;
        }
        from = at + pat.len();
    }
    false
}

// ------------------------------------------------------------------------------------------------------- commits and hazards

/// A full object id: exactly 40 lowercase hex digits. Abbreviations, upper case and names are refused everywhere a commit is named
/// in the cutoff list or a record: git prefers a ref named like an abbreviated hash (a builder can create one, and worktrees share
/// refs), so only a full id cannot be redirected.
pub fn full_id(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Does this commit exist in the object database of `root`? Only a full 40-digit lowercase hex id is passed to git (as
/// `<id>^{commit}`), so a name from a list file can neither become an option nor resolve to a ref.
pub fn commit_exists(root: &Path, c: &str) -> bool {
    full_id(c)
        && Command::new("git")
            .args(["cat-file", "-e", &format!("{c}^{{commit}}")])
            .current_dir(root)
            .output()
            .is_ok_and(|o| o.status.success())
}

/// `//@omit` derive lines that rustfmt (`merge_derives`, on by default) would merge with a neighbouring `#[derive(..)]`. The merged
/// attribute keeps the `//@omit` tag, so `scripts/verify-shipped.sh` drops ALL of it and Verus stops seeing the other derives: the
/// program Verus reads changes without any proof line changing. Plain `//` comments and blank lines between the two do not
/// protect them. A lone `#[derive(..)] //@omit` line (everything the item derives on that one line) is safe and is not reported.
/// One message per line, `path:line: ...`. `roles --check` refuses a file only when its count rises against the base (main already has
/// 28 such lines in five files, all `#[derive(Clone, ..)]` above `#[derive(Debug)] //@omit`, which a rustfmt run would corrupt).
pub fn omit_hazards_of(path: &str, src: &str) -> Vec<String> {
    fn code(l: &str) -> &str {
        l.split("//").next().unwrap_or("").trim()
    }
    fn derive(l: &str) -> bool {
        code(l).starts_with("#[derive(")
    }
    fn skippable(l: &str) -> bool {
        let t = l.trim();
        t.is_empty() || (t.starts_with("//") && !t.starts_with("//@"))
    }
    let lines: Vec<&str> = src.lines().collect();
    let mut out = Vec::new();
    for (n, l) in lines.iter().enumerate() {
        if !(l.trim_end().ends_with("//@omit") && derive(l)) {
            continue;
        }
        let prev = (0..n).rev().find(|&k| !skippable(lines[k]));
        let next = (n + 1..lines.len()).find(|&k| !skippable(lines[k]));
        let mut near: Vec<usize> = Vec::new();
        near.extend(prev.filter(|&k| derive(lines[k])));
        near.extend(next.filter(|&k| derive(lines[k])));
        if let Some(k) = near.first() {
            out.push(format!(
                "{path}:{}: `//@omit` derive line sits next to the `#[derive(..)]` on line {}: rustfmt merge_derives would merge them and keep the `//@omit` tag",
                n + 1,
                k + 1
            ));
        }
    }
    out
}

// ------------------------------------------------------------------------------------------------------- registration lines

/// Files in which a new proof unit is registered. A range that only ADDS lines for units whose files are new in the same range is not
/// an extractor change (docs/ci/PROOF-ROLES.md): `registration_only`.
pub const REGISTRATION_FILES: &[&str] = &[
    "tools/ci/proof-groups.tsv",
    "tools/ci/proof-inventory.tsv",
    "tools/ci/trust-baseline.tsv",
    "scripts/verify-shipped.sh",
    "docs/ci/statement-counts.tsv",
    "docs/ci/statement-keys.tsv",
    "docs/ci/PROOF-MATRIX.md",
    LOG,
];

/// What a registration line names: a unit, a groups key (a crate or a verification folder, shared by several units), or a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reg {
    Unit(String),
    Key(String),
    File(String),
}

fn line_counts(t: &str) -> BTreeMap<&str, usize> {
    let mut m = BTreeMap::new();
    for l in t.lines().filter(|l| !l.trim().is_empty()) {
        *m.entry(l).or_insert(0) += 1;
    }
    m
}

/// Lines of `a` that `b` has fewer times (multiset difference), in sorted order.
fn lines_beyond<'a>(a: &BTreeMap<&'a str, usize>, b: &BTreeMap<&'a str, usize>) -> Vec<&'a str> {
    let mut v = Vec::new();
    for (l, n) in a {
        for _ in b.get(l).copied().unwrap_or(0)..*n {
            v.push(*l);
        }
    }
    v
}

fn words_of(line: &str) -> Vec<&str> {
    line.split('\t')
        .nth(1)
        .unwrap_or("")
        .split_whitespace()
        .collect()
}

fn matrix_total(l: &str) -> Option<usize> {
    let (n, rest) = l.split_once(' ')?;
    rest.starts_with("units:").then(|| n.parse().ok()).flatten()
}

fn matrix_unit(l: &str) -> Option<String> {
    let cols: Vec<&str> = l.split('|').map(str::trim).collect();
    let u = cols.get(2)?.strip_prefix('`')?.strip_suffix('`')?;
    (cols.first() == Some(&"") && cols.len() >= 5 && !u.is_empty()).then(|| u.to_string())
}

/// The names that the lines added to a registration file register, when the change is ONLY additions of registration lines; an `Err`
/// names the first removed or altered line (or added line that is not a registration line). Pure text: `base` and `work` are the
/// file at the base commit and now ("" when absent). Additions of lines are read as a multiset difference, so a sorted insertion in
/// the middle of a generated list counts as an addition. Exceptions that are still additions of names only: a groups line that gains
/// keys (`proof-groups.tsv`), the `files=( .. )` list of `verify-shipped.sh` that gains paths, and the `N units:` summary line of
/// PROOF-MATRIX.md whose total grows by exactly the number of rows added.
pub fn registration_additions(path: &str, base: &str, work: &str) -> Result<Vec<Reg>, String> {
    let (bc, wc) = (line_counts(base), line_counts(work));
    let (mut removed, mut added) = (lines_beyond(&bc, &wc), lines_beyond(&wc, &bc));
    let altered = |l: &str| {
        format!(
            "{path}: line `{}` is removed or altered, not just added",
            l.chars().take(100).collect::<String>()
        )
    };
    let mut regs: Vec<Reg> = Vec::new();
    match path {
        "tools/ci/proof-groups.tsv" => {
            let base_keys: BTreeSet<&str> = base
                .lines()
                .filter(|l| !l.starts_with('#'))
                .flat_map(words_of)
                .collect();
            let base_groups: BTreeSet<&str> = base
                .lines()
                .filter(|l| !l.starts_with('#'))
                .filter_map(|l| l.split('\t').next())
                .collect();
            // A line that gains keys: removed old line, added line of the same group holding all its keys.
            for r in std::mem::take(&mut removed) {
                if r.starts_with('#') {
                    return Err(altered(r));
                }
                let g = r.split('\t').next().unwrap_or("");
                let ext = added.iter().position(|a| {
                    a.split('\t').next() == Some(g) && {
                        let aw = words_of(a);
                        words_of(r).iter().all(|k| aw.contains(k))
                    }
                });
                let Some(p) = ext else { return Err(altered(r)) };
                let a = added.remove(p);
                let old = words_of(r);
                let new: Vec<&str> = words_of(a);
                if new.len() != old.len() + new.iter().filter(|k| !old.contains(*k)).count() {
                    return Err(altered(r));
                }
                for k in new.iter().filter(|k| !old.contains(*k)) {
                    if base_keys.contains(*k) {
                        return Err(format!("{path}: key `{k}` already has a group at the base"));
                    }
                    regs.push(Reg::Key((*k).to_string()));
                }
            }
            for a in added {
                let g = a.split('\t').next().unwrap_or("");
                if a.starts_with('#') || !a.contains('\t') || base_groups.contains(g) {
                    return Err(format!("{path}: added line `{a}` is not a new group"));
                }
                for k in words_of(a) {
                    if base_keys.contains(k) {
                        return Err(format!("{path}: key `{k}` already has a group at the base"));
                    }
                    regs.push(Reg::Key(k.to_string()));
                }
            }
        }
        "scripts/verify-shipped.sh" => {
            if removed.is_empty() && added.is_empty() {
                return Ok(regs);
            }
            let list = |l: &str| -> Option<Vec<String>> {
                let inner = l.strip_prefix("files=(")?.strip_suffix(')')?;
                Some(inner.split_whitespace().map(str::to_string).collect())
            };
            let (&[r], &[a]) = (removed.as_slice(), added.as_slice()) else {
                return Err(format!("{path}: only the `files=(..)` line may change"));
            };
            let (Some(old), Some(new)) = (list(r), list(a)) else {
                return Err(altered(r));
            };
            let fresh: Vec<&String> = new.iter().filter(|f| !old.contains(f)).collect();
            if !old.iter().all(|f| new.contains(f)) || new.len() != old.len() + fresh.len() {
                return Err(altered(r));
            }
            regs.extend(fresh.into_iter().map(|f| Reg::File(f.clone())));
        }
        "docs/ci/PROOF-MATRIX.md" => {
            let mut rows = 0usize;
            let (mut old_total, mut new_total) = (None, None);
            for r in &removed {
                match matrix_total(r) {
                    Some(n) if old_total.is_none() => old_total = Some(n),
                    _ => return Err(altered(r)),
                }
            }
            for a in &added {
                if let Some(n) = matrix_total(a) {
                    if new_total.replace(n).is_some() {
                        return Err(format!("{path}: two summary lines added"));
                    }
                } else if let Some(u) = matrix_unit(a) {
                    rows += 1;
                    regs.push(Reg::Unit(u));
                } else {
                    return Err(format!("{path}: added line `{a}` is not a matrix row"));
                }
            }
            match (old_total, new_total) {
                (None, None) if rows == 0 => {}
                (Some(o), Some(n)) if n == o + rows => {}
                _ => {
                    return Err(format!(
                        "{path}: the `N units:` summary line must grow by exactly the {rows} row(s) added"
                    ));
                }
            }
        }
        "tools/ci/proof-inventory.tsv"
        | "tools/ci/trust-baseline.tsv"
        | "docs/ci/statement-counts.tsv"
        | "docs/ci/statement-keys.tsv"
        | LOG => {
            if let Some(r) = removed.first() {
                return Err(altered(r));
            }
            let col = if path == LOG { 3 } else { 0 };
            for a in added {
                let f: Vec<&str> = a.split('\t').collect();
                if a.starts_with('#') && path == LOG {
                    continue;
                }
                match f.get(col) {
                    Some(u) if !a.starts_with('#') && f.len() > col + 1 && !u.is_empty() => {
                        regs.push(Reg::Unit((*u).to_string()));
                    }
                    _ => {
                        return Err(format!(
                            "{path}: added line `{a}` is not a registration line"
                        ));
                    }
                }
            }
        }
        _ => return Err(format!("{path} is not a registration file")),
    }
    Ok(regs)
}

/// `registration_additions` plus the proof that every name added belongs to something that is NEW in the range.
pub fn registration_only(
    path: &str,
    base: &str,
    work: &str,
    unit_new: &dyn Fn(&str) -> bool,
    key_new: &dyn Fn(&str) -> bool,
    file_new: &dyn Fn(&str) -> bool,
) -> Result<(), String> {
    for r in registration_additions(path, base, work)? {
        let ok = match &r {
            Reg::Unit(u) => unit_new(u),
            Reg::Key(k) => key_new(k),
            Reg::File(f) => file_new(f),
        };
        if !ok {
            return Err(format!(
                "{path}: {r:?} is not new in this range (its files existed at the base, or it is unknown)"
            ));
        }
    }
    Ok(())
}

fn show_fp(u: &UnitFp) -> (String, String, String, String) {
    match &u.fp {
        Some(f) => (
            f.stmt.clone(),
            f.proof.clone(),
            f.exec.clone(),
            f.data.clone(),
        ),
        None => ("-".into(), "-".into(), "-".into(), "-".into()),
    }
}

pub fn cmd(args: &[String]) -> Result<i32, String> {
    let has = |k: &str| args.iter().any(|a| a == k);
    if has("--template") {
        let kind = val(args, "--template").unwrap_or_default();
        if !matches!(
            kind.as_str(),
            "statement" | "proof-edit" | "recheck" | "pre-rule"
        ) {
            return Err(
                "usage: roles --template statement|proof-edit|recheck|pre-rule [--unit U]".into(),
            );
        }
        let filled = match val(args, "--unit") {
            Some(name) => {
                let root = top()?;
                let (_, fps, _) = rooted(&FsTree(root.clone()))?;
                let u = fps
                    .into_iter()
                    .find(|u| u.unit == name)
                    .ok_or_else(|| format!("no unit {name}"))?;
                u.fp.map(|f| (name, f))
            }
            None => None,
        };
        print!(
            "{}",
            template(&kind, filled.as_ref().map(|(n, f)| (n.as_str(), f)))
        );
        return Ok(0);
    }
    let root = top()?;
    let merge_gate = has("--merge-gate");
    if merge_gate && has("--check") {
        // At the merge gate everything is read from HEAD, and a tree that differs from HEAD is refused.
        let dirty = dirty_tracked(&root);
        if !dirty.is_empty() {
            println!(
                "ROLES REFUSED: the tree differs from HEAD at the merge gate (the check reads HEAD; commit or discard):\n{}",
                dirty
                    .iter()
                    .take(10)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("\n")
            );
            return Ok(1);
        }
    }
    let work_tree: Box<dyn Tree> = if merge_gate && has("--check") {
        Box::new(GitTree {
            root: root.clone(),
            rev: "HEAD".into(),
        })
    } else {
        Box::new(FsTree(root.clone()))
    };
    let (units, fps, errs) = rooted(&*work_tree)?;
    if !errs.is_empty() {
        for e in &errs {
            eprintln!("roles: cannot classify: {e}");
        }
        eprintln!(
            "roles: the extractor missed something; fix tools/ci/lucina-ci/src/roles.rs (or spec.rs) before trusting a pass"
        );
        return Ok(2);
    }
    let log = work_tree.read(LOG).unwrap_or_default();
    let entries = parse_log(&log);
    let vok = verdict_checker(&*work_tree);
    if has("--list") {
        let burn = burn_down(&entries, &fps, &vok);
        let (rv, wv, nr): (usize, usize, usize) = burn
            .values()
            .fold((0, 0, 0), |a, b| (a.0 + b.0, a.1 + b.1, a.2 + b.2));
        if has("--summary") || has("--json") {
            if has("--json") {
                let mut s = String::from("{\"groups\":{");
                s.push_str(
                    &burn
                        .iter()
                        .map(|(g, (a, w, b))| {
                            format!(
                                "{}:{{\"reviewed\":{a},\"pre_rule\":{w},\"not_reviewed\":{b}}}",
                                json_str(g)
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(","),
                );
                s.push_str(&format!(
                    "}},\"reviewed\":{rv},\"pre_rule\":{wv},\"not_reviewed\":{nr},\"units\":["
                ));
                s.push_str(
                    &fps.iter()
                        .map(|u| {
                            let (a, b, c, d) = show_fp(u);
                            format!(
                                "{{\"unit\":{},\"group\":{},\"kind\":{},\"statement_fp\":\"{a}\",\"proof_fp\":\"{b}\",\"exec_fp\":\"{c}\",\"data_fp\":\"{d}\",\"status\":{}}}",
                                json_str(&u.unit),
                                json_str(&u.group),
                                json_str(&u.kind),
                                json_str(status(&entries, u, &vok))
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(","),
                );
                s.push_str("]}");
                println!("{s}");
            } else {
                println!("group\treviewed-under-the-rule\tpre-rule\tnot-yet");
                for (g, (a, w, b)) in &burn {
                    println!("{g}\t{a}\t{w}\t{b}");
                }
                println!("TOTAL\t{rv}\t{wv}\t{nr}");
            }
            return Ok(0);
        }
        for u in &fps {
            let (a, b, c, d) = show_fp(u);
            println!(
                "{}\t{}\t{}\t{a}\t{b}\t{c}\t{d}\t{}\ttokens exec/stmt/proof {}/{}/{}\t{} files",
                u.unit,
                u.group,
                u.kind,
                status(&entries, u, &vok),
                u.counts[0],
                u.counts[1],
                u.counts[2],
                u.files.len()
            );
        }
        let total = fps.iter().fold([0usize; 3], |mut a, u| {
            for (i, x) in a.iter_mut().enumerate() {
                *x += u.counts[i];
            }
            a
        });
        eprintln!(
            "roles: {} units, {} tracked; verus/shipped tokens: exec {} statement {} proof {} (every token in exactly one class); reviewed under the rule {rv}, pre-rule {wv}, not yet {nr}",
            fps.len(),
            fps.iter().filter(|u| u.fp.is_some()).count(),
            total[0],
            total[1],
            total[2]
        );
        return Ok(0);
    }
    if has("--seed") {
        let have: BTreeSet<&str> = entries
            .iter()
            .filter_map(|e| e.res.as_ref().ok())
            .map(|r| r.unit.as_str())
            .collect();
        let date = today();
        let mut out = String::new();
        for u in fps.iter().filter(|u| !have.contains(u.unit.as_str())) {
            let (a, b, c, d) = show_fp(u);
            let id = format!("seed-{}", u.unit.replace('/', "."));
            out.push_str(&format!(
                "{id}\t{date}\tseed\t{}\t{a}\t{b}\t{c}\t{d}\tunknown\tunknown\tunknown\tunknown\tunknown\t-\t-\t{NOT_REVIEWED}\t-\t-\tseeded when the roles rule started: written before the rule, not reviewed under it\n",
                u.unit
            ));
        }
        let n = out.lines().count();
        if has("--print") {
            print!("{out}");
        } else if n > 0 {
            let p = root.join(LOG);
            let mut cur = fs::read_to_string(&p).unwrap_or_default();
            if cur.is_empty() {
                cur.push_str(LOG_HEADER);
            } else if !cur.ends_with('\n') {
                cur.push('\n');
            }
            cur.push_str(&out);
            fs::write(&p, cur).map_err(|e| format!("write {LOG}: {e}"))?;
        }
        eprintln!(
            "roles: {n} seed record(s) {}",
            if has("--print") {
                "printed"
            } else {
                "appended"
            }
        );
        return Ok(0);
    }
    if !has("--check") {
        return Err("usage: roles --check [--base REF] [--merge-gate] | --list [--json|--summary] | --template KIND [--unit U] | --seed [--print]".into());
    }
    let base_ref = val(args, "--base").unwrap_or_else(|| {
        if git1(
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
    let resolved = git1(
        &root,
        &[
            "rev-parse",
            "--verify",
            "-q",
            &format!("{base_ref}^{{commit}}"),
        ],
    )
    .ok_or_else(|| format!("base {base_ref} not found"))?;
    let base = git1(&root, &["merge-base", &resolved, "HEAD"]).unwrap_or(resolved);
    let gt = GitTree {
        root: root.clone(),
        rev: base.clone(),
    };
    let base_units = match gt.read(inventory::INVENTORY) {
        Some(t) => inventory::from_tsv(&t)?,
        None => units.clone(),
    };
    let (bfps, berrs) = fingerprints(&gt, &base_units);
    if !berrs.is_empty() {
        for e in &berrs {
            eprintln!("roles: cannot classify at the base commit: {e}");
        }
        return Ok(2);
    }
    let bfiles: BTreeMap<String, Vec<String>> = bfps
        .iter()
        .map(|u| (u.unit.clone(), u.files.clone()))
        .collect();
    let bmap: BTreeMap<String, Fp> = bfps
        .into_iter()
        .filter_map(|u| u.fp.map(|f| (u.unit, f)))
        .collect();
    let base_log = gt.read(LOG).unwrap_or_default();
    let added = added_map(&root, &base);
    let added_fn = |raw: &str| match added.get(raw) {
        Some(m) => Added::Committed(m.clone()),
        None => {
            // Present in the working tree only? Then it is simply uncommitted.
            if git(&root, &["rev-parse", "--verify", "-q", "HEAD"]).is_some() {
                let head = git(&root, &["show", &format!("HEAD:{LOG}")]).unwrap_or_default();
                if head.lines().any(|l| l == raw) {
                    Added::Unknown
                } else {
                    Added::Uncommitted
                }
            } else {
                Added::Unknown
            }
        }
    };
    let work_units: BTreeMap<&str, &UnitFp> = fps.iter().map(|u| (u.unit.as_str(), u)).collect();
    let inv: BTreeMap<&str, &Unit> = units.iter().map(|u| (u.name.as_str(), u)).collect();
    let models_peers: Vec<Unit> = units
        .iter()
        .filter(|u| matches!(u.kind.as_str(), "tla" | "models"))
        .cloned()
        .collect();
    // Every file that was inside some unit at the base: the only sources a rename or copy may be compared with.
    let owned_at_base: BTreeSet<String> = bfiles.values().flatten().cloned().collect();
    let changes_cache: RefCell<HashMap<(String, String), ChangeInfo>> =
        RefCell::new(HashMap::new());
    let changes_since = |name: &str, since: &str| -> ChangeInfo {
        let key = (name.to_string(), since.to_string());
        if let Some(v) = changes_cache.borrow().get(&key) {
            return v.clone();
        }
        let mut info = ChangeInfo::default();
        if let (Some(w), Some(u)) = (work_units.get(name), inv.get(name)) {
            let mut files = w.files.clone();
            if let Some(bf) = bfiles.get(name) {
                for f in bf {
                    if !files.contains(f) {
                        files.push(f.clone());
                    }
                }
            }
            let peers: Vec<Unit> = if matches!(u.kind.as_str(), "tla" | "models") {
                models_peers.clone()
            } else {
                vec![(*u).clone()]
            };
            info = change_info_owned(
                &root,
                &base,
                name,
                &peers,
                &files,
                (!since.is_empty()).then_some(since),
                Some(&owned_at_base),
            );
        }
        changes_cache.borrow_mut().insert(key, info.clone());
        info
    };
    let changes = |name: &str| changes_since(name, "");
    let verdict_modified: Vec<String> = gt
        .files_under(VERDICT_DIR.trim_end_matches('/'))
        .into_iter()
        .filter(|f| gt.read(f) != work_tree.read(f))
        .collect();
    let cutoff_work = work_tree.read(CUTOFF);
    let exists = |c: &str| commit_exists(&root, c);
    // The extractor and the verified files, as changed in the range (base to HEAD at the gate; base to the working tree otherwise).
    let changed: Vec<String> = if merge_gate {
        git(&root, &["diff", "--name-only", &base, "HEAD"])
    } else {
        git(&root, &["diff", "--name-only", &base])
    }
    .unwrap_or_default()
    .lines()
    .map(str::to_string)
    .collect();
    let extractor_changed: Vec<String> = changed
        .iter()
        .filter(|c| is_extractor_path(c))
        .cloned()
        .collect();
    let cutoff_changed: Vec<String> = changed
        .iter()
        .filter(|c| is_cutoff_path(c))
        .cloned()
        .collect();
    let mut closure: BTreeSet<&String> = BTreeSet::new();
    for u in fps.iter() {
        closure.extend(u.files.iter());
    }
    for fl in bfiles.values() {
        closure.extend(fl.iter());
    }
    let unit_files_changed: Vec<String> = changed
        .iter()
        .filter(|c| closure.contains(c))
        .cloned()
        .collect();
    // Registration files whose change is only the registration of units with NEW files: not an extractor change.
    let file_new = |f: &str| {
        git(&root, &["ls-tree", "-r", "--name-only", &base, "--", f])
            .is_some_and(|s| s.trim().is_empty())
    };
    let base_unit_names: BTreeSet<&str> = base_units.iter().map(|u| u.name.as_str()).collect();
    let unit_new = |name: &str| {
        if base_unit_names.contains(name) {
            return false;
        }
        let files: Vec<&String> = match (work_units.get(name), inv.get(name)) {
            (Some(w), _) if !w.files.is_empty() => w.files.iter().collect(),
            (_, Some(u)) => u.inputs.iter().collect(),
            _ => return false,
        };
        !files.is_empty() && files.iter().all(|f| file_new(f))
    };
    let key_new = |k: &str| {
        let mine: Vec<&str> = units
            .iter()
            .map(|u| u.name.as_str())
            .filter(|n| n.split('/').nth(1) == Some(k))
            .collect();
        !mine.is_empty() && mine.iter().all(|n| unit_new(n))
    };
    let registration_exempt: Vec<String> = changed
        .iter()
        .filter(|p| REGISTRATION_FILES.contains(&p.as_str()))
        .filter(|p| {
            registration_only(
                p,
                &gt.read(p).unwrap_or_default(),
                &work_tree.read(p).unwrap_or_default(),
                &unit_new,
                &key_new,
                &file_new,
            )
            .is_ok()
        })
        .cloned()
        .collect();
    // Ratchet: the hazards that main already has (a `#[derive(Clone, ..)]` line above a `#[derive(Debug)] //@omit` line is the
    // convention of several rules files) are tolerated; a file whose count RISES against the base is refused with all its hazards.
    let mut omit_hazards: Vec<String> = Vec::new();
    for u in fps.iter().filter(|u| u.kind == "shipped") {
        for f in &u.files {
            if let Some(src) = work_tree.read(f) {
                let now = omit_hazards_of(f, &src);
                let before = gt.read(f).map_or(0, |b| omit_hazards_of(f, &b).len());
                if now.len() > before {
                    omit_hazards.extend(now);
                }
            }
        }
    }
    omit_hazards.sort();
    omit_hazards.dedup();
    let allow_env: Vec<String> = [
        Some(spec::MERGE_ENV),
        (!merge_gate).then_some(spec::LOCAL_ENV),
    ]
    .into_iter()
    .flatten()
    .filter_map(|k| std::env::var(k).ok())
    .collect();
    let allow_extractor = allow_env.iter().any(|v| allow_token(v, "extractor"));
    let allow_cutoff = allow_env.iter().any(|v| allow_token(v, "cutoff"));
    let input = CheckIn {
        base: &bmap,
        work: &fps,
        entries: &entries,
        base_log: &base_log,
        work_log: &log,
        verdict_ok: &vok,
        added: &added_fn,
        changes: &changes,
        changes_after: &changes_since,
        verdict_modified: &verdict_modified,
        merge_gate,
        cutoff_work: cutoff_work.as_deref(),
        cutoff_changed: &cutoff_changed,
        allow_cutoff,
        exists: &exists,
        omit_hazards: &omit_hazards,
        registration_exempt: &registration_exempt,
        extractor_changed: &extractor_changed,
        unit_files_changed: &unit_files_changed,
        allow_extractor,
    };
    let o = check(&input);
    let (text, code) = render(&o, &base[..base.len().min(10)]);
    print!("{text}");
    Ok(code)
}

// ------------------------------------------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    const VERUS: &str = r#"
use vstd::prelude::*;
verus! {
pub open spec fn ok(x: u32) -> bool { x < 10 }
proof fn lemma_a(x: u32) requires x < 5 ensures ok(x) { assert(x < 10); }
pub fn f(x: u32) -> (r: u32) requires x < 5 ensures r == x + 1 {
    let mut i = 0;
    while i < x invariant i <= x decreases x - i { i += 1; }
    assert(i == x) by { reveal(ok); };
    proof { lemma_a(x); }
    let ghost g = 5;
    x + 1
}
}
"#;

    const SHIPPED: &str = r#"
pub const SLOTS: usize = 10;
pub fn slot_by_key(active: &[bool; SLOTS], n: usize, key: i32) -> Option<usize> //@ret
//@ requires n <= SLOTS,
//@ ensures match ret { Some(i) => i < n, None => true },
{
    let mut i = 0;
    while i < n
    //@ invariant n <= SLOTS, i <= n,
    //@ decreases n - i,
    {
        if active[i] { return Some(i); }
        i += 1;
    }
    None
}
#[cfg(test)]
mod tests { #[test] fn t() { assert_eq!(1, 1); } }
"#;

    fn fp(s: &str, p: &str, e: &str) -> Fp {
        Fp {
            stmt: s.into(),
            proof: p.into(),
            exec: e.into(),
            data: "eeeeeeeeeeeeeeee".into(),
            core: Core::default(),
        }
    }

    #[test]
    fn verus_block_splits_into_statement_proof_exec() {
        let c = classify("x.rs", VERUS);
        assert!(c.errors.is_empty(), "{:?}", c.errors);
        let p = &c.proof_text;
        for w in [
            "invariant i <= x",
            "decreases x - i",
            "assert(i == x)",
            "reveal(ok)",
            "proof{",
            "let ghost g = 5;",
            "assert(x < 10)",
        ] {
            assert!(p.contains(w), "proof text lacks {w}: {p}");
        }
        let e = &c.exec_text;
        assert!(
            e.contains("i += 1") && e.contains("x + 1") && e.contains("let mut i = 0"),
            "{e}"
        );
        assert!(
            !e.contains("assert") && !e.contains("invariant") && !e.contains("requires"),
            "{e}"
        );
        assert_eq!(c.stmts.len(), 3, "spec fn, proof fn header, fn header");
        assert!(c.counts.iter().all(|&n| n > 0));
    }

    #[test]
    fn verus_edits_move_only_their_own_fingerprint() {
        let base = classify("x.rs", VERUS);
        let key = |c: &FileClass| (c.stmts.clone(), c.proof_text.clone(), c.exec_text.clone());
        let b = key(&base);
        // exec only
        let e = key(&classify(
            "x.rs",
            &VERUS.replace("    x + 1\n}", "    1 + x\n}"),
        ));
        assert_eq!(e.0, b.0);
        assert_eq!(e.1, b.1);
        assert_ne!(e.2, b.2);
        // proof only
        let p = key(&classify(
            "x.rs",
            &VERUS.replace("assert(i == x) by", "assert(i == x + 0) by"),
        ));
        assert_eq!(p.0, b.0);
        assert_ne!(p.1, b.1);
        assert_eq!(p.2, b.2);
        // loop invariant is proof
        let inv = key(&classify(
            "x.rs",
            &VERUS.replace("invariant i <= x", "invariant i <= x + 1"),
        ));
        assert_eq!(inv.0, b.0);
        assert_ne!(inv.1, b.1);
        assert_eq!(inv.2, b.2);
        // statement
        let s = key(&classify(
            "x.rs",
            &VERUS.replace("ensures r == x + 1", "ensures r >= 0"),
        ));
        assert_ne!(s.0, b.0);
        // comments and spacing change nothing
        let c = key(&classify(
            "x.rs",
            &VERUS.replace("let mut i = 0;", "let  mut  i = 0; // note"),
        ));
        assert_eq!(c, b);
    }

    #[test]
    fn shipped_slash_at_lines_split_like_tp_rules() {
        let c = classify("c/src/lib.rs", SHIPPED);
        assert!(c.errors.is_empty(), "{:?}", c.errors);
        let p = &c.proof_text;
        assert!(
            p.contains("invariant n <= SLOTS") && p.contains("decreases n - i"),
            "{p}"
        );
        assert!(
            !p.contains("requires"),
            "header clauses are statements: {p}"
        );
        let e = &c.exec_text;
        assert!(
            e.contains("i += 1") && e.contains("assert_eq") && e.contains("mod tests"),
            "{e}"
        );
        assert!(!e.contains("invariant") && !e.contains("ensures"), "{e}");
        let names: Vec<&str> = c.stmts.iter().map(|s| s.0.as_str()).collect();
        assert!(
            names.contains(&"c/src/lib.rs::SLOTS") && names.contains(&"c/src/lib.rs::slot_by_key")
        );
        // changing an invariant moves proof only; the loop body moves exec only
        let p2 = classify("c/src/lib.rs", &SHIPPED.replace("i <= n,", "i <= n + 1,"));
        assert_eq!(p2.stmts, c.stmts);
        assert_ne!(p2.proof_text, c.proof_text);
        assert_eq!(p2.exec_text, c.exec_text);
        let e2 = classify("c/src/lib.rs", &SHIPPED.replace("i += 1;", "i += 2;"));
        assert_eq!(e2.proof_text, c.proof_text);
        assert_ne!(e2.exec_text, c.exec_text);
    }

    #[test]
    fn juno_ring_style_verus_block_is_classified_and_accounted() {
        let src = r#"
#![feature(proc_macro_hygiene)]
use vstd::prelude::*;
verus! {
/// Where position `pos` lives.
pub open spec fn slot(pos: int, cap: usize) -> (int, int) { (pos, pos % 4096) }
pub struct Ring { pub head: usize, pub contents: Seq<u8> }
pub open spec fn inv(m: Ring) -> bool {
    &&& m.contents.len() <= 4
    &&& forall|i: int| 0 <= i < m.contents.len() ==> #[trigger] m.contents[i] == 0
}
pub proof fn lemma_len(m: Ring) requires inv(m) ensures m.contents.len() <= 4 { }
}
"#;
        let c = classify("x.rs", src);
        assert!(c.errors.is_empty(), "{:?}", c.errors);
        let names: Vec<&str> = c.stmts.iter().map(|s| s.0.as_str()).collect();
        for n in ["x.rs::slot", "x.rs::Ring", "x.rs::inv", "x.rs::lemma_len"] {
            assert!(names.contains(&n), "statement {n} missing: {names:?}");
        }
        assert!(
            !c.proof_text.contains("spec fn") && !c.proof_text.contains("forall"),
            "spec fns are statements: {}",
            c.proof_text
        );
        assert!(!c.exec_text.contains("forall"), "{}", c.exec_text);
        assert!(c.counts[1] > 40, "{:?}", c.counts);
    }

    #[test]
    fn unclassified_verified_fn_is_an_error_via_the_cross_check() {
        // A stray clause keyword outside a function is reported, not swallowed.
        let c = classify("x.rs", "verus! {\n ensures 1 == 1\n}\n");
        assert!(!c.errors.is_empty());
        // The uncovered-fn check itself: with nothing marked as covered, every verus! fn is reported.
        let sc = spec::scan_full("x.rs", VERUS);
        let none = vec![false; sc.toks.len()];
        let v = find_uncovered("x.rs", &sc.toks, &sc.in_verus, &none);
        assert_eq!(v.len(), 3, "{v:?}");
        assert!(v.iter().any(|m| m.contains("fn lemma_a")));
        let c = classify("x.rs", VERUS);
        assert!(c.errors.is_empty(), "{:?}", c.errors);
    }

    #[test]
    fn names_classes_families_and_trailers() {
        assert_eq!(model_norm("Claude Opus 5.5"), "claude-opus-5-5");
        assert_eq!(model_norm("claude-opus-5-5"), "claude-opus-5-5");
        assert_eq!(class("claude-opus-5-5"), "strong");
        assert_eq!(class("claude-fable-5-2"), "strong");
        assert_eq!(
            class("Fable 6"),
            "unknown",
            "strong is an explicit allowlist, not a substring"
        );
        for b in [
            "claude-sonnet-5-5",
            "haiku",
            "qwen3-8-27b",
            "mistral-large",
            "vibe",
            "qoder-efficient",
            "zen-beam",
            "glm-5-3-flash",
            "deepseek-free",
        ] {
            assert_eq!(class(b), "builder", "{b}");
        }
        assert_eq!(class("gpt-6"), "unknown");
        assert_eq!(class("deepseek-paid"), "unknown");
        assert_eq!(family("claude-opus-5-5"), "claude");
        assert_eq!(family("unknown"), "unknown");
        let t = trailer_models(
            "msg\n\nCo-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>\nco-authored-by: Qoder <q@x>\n",
        );
        assert_eq!(t, vec!["claude-sonnet-5-5", "qoder"]);
        assert!(trailer_models("nothing").is_empty());
    }

    // ---- the rules

    const OPUS: &str = "claude-opus-5-5";
    const SON: &str = "claude-sonnet-5-5";

    fn rec(kind: &str, unit: &str, f: &Fp) -> Record {
        Record {
            id: format!("r-{kind}"),
            date: "2026-11-11".into(),
            kind: kind.into(),
            unit: unit.into(),
            stmt: f.stmt.clone(),
            proof: f.proof.clone(),
            exec: f.exec.clone(),
            data: f.data.clone(),
            impl_model: SON.into(),
            impl_family: "claude".into(),
            prover_model: OPUS.into(),
            prover_family: "claude".into(),
            prover_class: "strong".into(),
            reviewer_model: "gpt-6-1-sol".into(),
            reviewer_family: "gpt".into(),
            verdict: "accepted".into(),
            verdict_ref: format!("docs/ci/verdicts/r-{kind}.md"),
            started_before: if kind == "pre-rule" {
                SB.into()
            } else {
                "-".into()
            },
            note: "n".into(),
        }
    }

    fn line(r: &Record) -> String {
        [
            &r.id,
            &r.date,
            &r.kind,
            &r.unit,
            &r.stmt,
            &r.proof,
            &r.exec,
            &r.data,
            &r.impl_model,
            &r.impl_family,
            &r.prover_model,
            &r.prover_family,
            &r.prover_class,
            &r.reviewer_model,
            &r.reviewer_family,
            &r.verdict,
            &r.verdict_ref,
            &r.started_before,
            &r.note,
        ]
        .iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join("\t")
    }

    fn ufp(f: &Fp) -> UnitFp {
        UnitFp {
            unit: "verus/u".into(),
            group: "g".into(),
            kind: "verus".into(),
            files: vec![],
            fp: Some(f.clone()),
            counts: [0; 3],
            stmt_count: 0,
            stmt_keys: vec![],
        }
    }

    struct Scn {
        base: Option<Fp>,
        work: Fp,
        records: Vec<Record>,
        base_log: String,
        verdict: bool,
        trailer: Vec<String>,
        exec_models: Vec<String>,
        proof_commits: Vec<(String, Vec<String>)>,
        extra_log: Option<String>,
        mode: u8,
        merge_gate: bool,
        verdict_modified: Vec<String>,
        cutoff: Option<String>,
        after_proof: Vec<(String, Vec<String>)>,
        after_exec: Option<Vec<Vec<String>>>,
        cutoff_changed: Vec<String>,
        allow_cutoff: bool,
        exec_override: Option<Vec<Vec<String>>>,
        exists_ok: bool,
        extractor_changed: Vec<String>,
        unit_files_changed: Vec<String>,
        allow_extractor: bool,
        omit_hazards: Vec<String>,
        registration_exempt: Vec<String>,
        /// Do not add the default statement/proof commit by the records' provers.
        bare_proof: bool,
    }

    fn scn(base: Option<Fp>, work: Fp, records: Vec<Record>) -> Scn {
        Scn {
            base,
            work,
            records,
            base_log: "# h\n".into(),
            verdict: true,
            trailer: vec![OPUS.into()],
            exec_models: vec![],
            proof_commits: vec![],
            extra_log: None,
            mode: 0,
            merge_gate: true,
            verdict_modified: vec![],
            cutoff: None,
            after_proof: vec![],
            after_exec: None,
            cutoff_changed: vec![],
            allow_cutoff: false,
            exec_override: None,
            exists_ok: true,
            extractor_changed: vec![],
            unit_files_changed: vec![],
            allow_extractor: false,
            omit_hazards: vec![],
            registration_exempt: vec![],
            bare_proof: false,
        }
    }

    fn run(s: &Scn) -> Outcome {
        let mut log = s.base_log.clone();
        for r in &s.records {
            log.push_str(&line(r));
            log.push('\n');
        }
        if let Some(x) = &s.extra_log {
            log = x.clone();
        }
        let entries = parse_log(&log);
        let mut base = BTreeMap::new();
        if let Some(b) = &s.base {
            base.insert("verus/u".to_string(), b.clone());
        }
        let work = vec![ufp(&s.work)];
        let vok = |_: &str, _: &Record| s.verdict;
        let tr = s.trailer.clone();
        let mode = s.mode;
        let added = move |_: &str| match mode {
            0 => Added::Committed(tr.clone()),
            1 => Added::Uncommitted,
            _ => Added::Unknown,
        };
        let info = ChangeInfo {
            exec: s.exec_override.clone().unwrap_or_else(|| {
                if s.exec_models.is_empty() {
                    vec![]
                } else {
                    vec![s.exec_models.clone()]
                }
            }),
            proof: if s.proof_commits.is_empty() && !s.bare_proof {
                // the provers of the records wrote the statement or proof text (a signature alone is refused)
                let mut v: Vec<String> = s
                    .records
                    .iter()
                    .map(|r| model_norm(&r.prover_model))
                    .collect();
                v.sort();
                v.dedup();
                v.into_iter()
                    .map(|m| ("pppppppppp".to_string(), vec![m]))
                    .collect()
            } else {
                s.proof_commits.clone()
            },
        };
        let changes = move |_: &str| info.clone();
        let after = ChangeInfo {
            exec: s.after_exec.clone().unwrap_or_default(),
            proof: s.after_proof.clone(),
        };
        let changes_after = move |_: &str, _: &str| after.clone();
        check(&CheckIn {
            base: &base,
            work: &work,
            entries: &entries,
            base_log: &s.base_log,
            work_log: &log,
            verdict_ok: &vok,
            added: &added,
            changes: &changes,
            changes_after: &changes_after,
            verdict_modified: &s.verdict_modified,
            merge_gate: s.merge_gate,
            cutoff_work: s.cutoff.as_deref(),
            cutoff_changed: &s.cutoff_changed,
            allow_cutoff: s.allow_cutoff,
            exists: &|_: &str| s.exists_ok,
            omit_hazards: &s.omit_hazards,
            registration_exempt: &s.registration_exempt,
            extractor_changed: &s.extractor_changed,
            unit_files_changed: &s.unit_files_changed,
            allow_extractor: s.allow_extractor,
        })
    }

    fn rules(o: &Outcome) -> Vec<&str> {
        o.refusals.iter().map(|r| r.rule.as_str()).collect()
    }

    fn a() -> Fp {
        fp("aaaaaaaaaaaaaaaa", "bbbbbbbbbbbbbbbb", "cccccccccccccccc")
    }

    #[test]
    fn unchanged_unit_needs_nothing() {
        let o = run(&scn(Some(a()), a(), vec![]));
        assert!(o.refusals.is_empty());
        assert_eq!(o.unchanged, 1);
    }

    #[test]
    fn exec_only_change_needs_no_record() {
        let mut w = a();
        w.exec = "dddddddddddddddd".into();
        let o = run(&scn(Some(a()), w, vec![]));
        assert!(o.refusals.is_empty(), "{:?}", o.refusals);
    }

    #[test]
    fn new_unit_and_statement_change_need_a_statement_record() {
        let mut w = a();
        w.stmt = "1111111111111111".into();
        let o = run(&scn(Some(a()), w.clone(), vec![]));
        assert_eq!(rules(&o), ["statement-needs-strong-prover-and-reviewer"]);
        assert!(o.refusals[0].hint.contains("statement"));
        let o = run(&scn(None, a(), vec![]));
        assert_eq!(rules(&o), ["statement-needs-strong-prover-and-reviewer"]);
        // with a valid record
        let o = run(&scn(
            Some(a()),
            w.clone(),
            vec![rec("statement", "verus/u", &w)],
        ));
        assert!(o.refusals.is_empty(), "{:?}", o.refusals);
        assert_eq!(o.covered.len(), 1);
        // a record for other fingerprints does not count
        let o = run(&scn(Some(a()), w, vec![rec("statement", "verus/u", &a())]));
        assert_eq!(rules(&o), ["statement-needs-strong-prover-and-reviewer"]);
        assert!(
            o.refusals[0]
                .detail
                .contains("no `statement` record carries"),
            "{}",
            o.refusals[0].detail
        );
    }

    #[test]
    fn proof_change_needs_a_proof_edit_record() {
        let mut w = a();
        w.proof = "2222222222222222".into();
        let o = run(&scn(Some(a()), w.clone(), vec![]));
        assert_eq!(rules(&o), ["proof-edit-needs-strong-prover"]);
        let mut r = rec("proof-edit", "verus/u", &w);
        r.reviewer_model = "-".into();
        r.reviewer_family = "-".into();
        let o = run(&scn(Some(a()), w.clone(), vec![r.clone()]));
        assert!(o.refusals.is_empty(), "{:?}", o.refusals);
        // builder prover refused
        r.prover_model = SON.into();
        r.prover_class = "builder".into();
        r.impl_model = "qoder".into();
        r.impl_family = "qoder".into();
        let mut sc = scn(Some(a()), w, vec![r]);
        sc.trailer = vec![SON.into()];
        let o = run(&sc);
        assert!(
            o.refusals[0].detail.contains("not strong"),
            "{:?}",
            o.refusals
        );
    }

    #[test]
    fn statement_record_rules() {
        let mut w = a();
        w.stmt = "1111111111111111".into();
        let bad = |f: &dyn Fn(&mut Record), want: &str| {
            let mut r = rec("statement", "verus/u", &w);
            f(&mut r);
            let mut sc = scn(Some(a()), w.clone(), vec![r.clone()]);
            sc.trailer = vec![model_norm(&r.prover_model)];
            let o = run(&sc);
            assert_eq!(o.refusals.len(), 1, "{want}: {:?}", o.refusals);
            assert!(
                o.refusals[0].detail.contains(want),
                "{want}: {}",
                o.refusals[0].detail
            );
        };
        bad(
            &|r| {
                r.prover_model = SON.into();
                r.prover_class = "builder".into();
                r.impl_model = "qoder".into();
                r.impl_family = "qoder".into();
            },
            "not strong",
        );
        bad(&|r| r.impl_model = OPUS.into(), "same model");
        bad(
            &|r| {
                r.reviewer_model = "claude-fable-5-2".into();
                r.reviewer_family = "claude".into();
            },
            "prover's family",
        );
        bad(
            &|r| {
                r.reviewer_model = "gpt-6-1-sol".into();
                r.impl_model = "gpt-6-1-sol".into();
                r.impl_family = "gpt".into();
            },
            "also the prover or the implementer",
        );
        bad(&|r| r.verdict = "rejected".into(), "not accepted");
        bad(&|r| r.verdict = "escalated".into(), "not accepted");
        bad(
            &|r| r.verdict_ref = "docs/other.md".into(),
            "verdict file of this record",
        );
        let mut s = scn(Some(a()), w.clone(), vec![rec("statement", "verus/u", &w)]);
        s.verdict = false;
        assert!(run(&s).refusals[0].detail.contains("lacks"));
        // a kind proof-edit cannot satisfy a statement change
        let o = run(&scn(
            Some(a()),
            w.clone(),
            vec![rec("proof-edit", "verus/u", &w)],
        ));
        assert_eq!(rules(&o), ["statement-needs-strong-prover-and-reviewer"]);
        assert!(
            o.refusals[0]
                .detail
                .contains("no `statement` record carries"),
            "{}",
            o.refusals[0].detail
        );
    }

    #[test]
    fn declared_prover_must_match_the_trailer_of_the_adding_commit() {
        let mut w = a();
        w.stmt = "1111111111111111".into();
        let mut s = scn(Some(a()), w.clone(), vec![rec("statement", "verus/u", &w)]);
        s.trailer = vec![SON.into()];
        let o = run(&s);
        assert!(rules(&o).contains(&"trailer-mismatch"), "{:?}", o.refusals);
        s.trailer = vec![OPUS.into()];
        assert!(run(&s).refusals.is_empty());
    }

    #[test]
    fn one_model_cannot_write_the_code_and_the_proof() {
        let mut w = a();
        w.stmt = "1111111111111111".into();
        w.exec = "dddddddddddddddd".into();
        let mut s = scn(Some(a()), w.clone(), vec![rec("statement", "verus/u", &w)]);
        s.exec_models = vec![SON.into(), OPUS.into()];
        let o = run(&s);
        assert!(
            rules(&o).contains(&"one-model-wrote-code-and-proof"),
            "{:?}",
            o.refusals
        );
        s.exec_models = vec![SON.into()];
        assert!(run(&s).refusals.is_empty());
        // exec unchanged: the exec commits are not consulted
        let mut w2 = a();
        w2.stmt = "1111111111111111".into();
        let mut s2 = scn(
            Some(a()),
            w2.clone(),
            vec![rec("statement", "verus/u", &w2)],
        );
        s2.exec_models = vec![OPUS.into()];
        assert!(run(&s2).refusals.is_empty());
    }

    #[test]
    fn log_must_be_append_only_and_new_lines_well_formed() {
        let mut w = a();
        w.stmt = "1111111111111111".into();
        let r = rec("statement", "verus/u", &w);
        let mut s = scn(Some(a()), w.clone(), vec![r.clone()]);
        s.base_log = format!("# h\n{}\n", line(&rec("seed", "verus/u", &a())));
        s.extra_log = Some(format!("# h\n{}\n", line(&r)));
        let o = run(&s);
        assert!(rules(&o).contains(&"log-append-only"), "{:?}", o.refusals);
        let mut s = scn(Some(a()), w.clone(), vec![r]);
        s.extra_log = Some("# h\nbroken\tline\n".to_string());
        assert!(rules(&run(&s)).contains(&"malformed-record"));
    }

    #[test]
    fn seed_is_exempt_and_never_satisfies_a_change() {
        let seed = Record {
            kind: "seed".into(),
            verdict: NOT_REVIEWED.into(),
            prover_model: "unknown".into(),
            prover_family: "unknown".into(),
            prover_class: "unknown".into(),
            impl_model: "unknown".into(),
            impl_family: "unknown".into(),
            reviewer_model: "-".into(),
            reviewer_family: "-".into(),
            verdict_ref: "-".into(),
            ..rec("seed", "verus/u", &a())
        };
        assert!(parse_fields(&line(&seed).split('\t').collect::<Vec<_>>()).is_ok());
        // unchanged unit with only a seed: nothing needed
        let mut s = scn(Some(a()), a(), vec![seed.clone()]);
        s.base_log = "# h\n".into();
        s.trailer = vec![SON.into()];
        assert!(run(&s).refusals.is_empty());
        // changed statement: the seed does not help
        let mut w = a();
        w.stmt = "1111111111111111".into();
        let mut seed2 = seed;
        seed2.stmt = w.stmt.clone();
        seed2.proof = w.proof.clone();
        let o = run(&scn(Some(a()), w, vec![seed2]));
        assert_eq!(rules(&o), ["statement-needs-strong-prover-and-reviewer"]);
        assert!(
            o.refusals[0]
                .detail
                .contains("no `statement` record carries")
        );
    }

    #[test]
    fn record_fields_are_checked_for_consistency() {
        let f = a();
        let mut r = rec("statement", "verus/u", &f);
        r.prover_class = "strong".into();
        r.prover_model = SON.into();
        let err = parse_fields(&line(&r).split('\t').collect::<Vec<_>>()).unwrap_err();
        assert!(err.contains("prover_class"), "{err}");
        let mut r = rec("statement", "verus/u", &f);
        r.reviewer_family = "claude".into();
        let err = parse_fields(&line(&r).split('\t').collect::<Vec<_>>()).unwrap_err();
        assert!(err.contains("reviewer_family"), "{err}");
        assert!(parse_fields(&["a"]).unwrap_err().contains("fields"));
    }

    #[test]
    fn burn_down_counts_reviewed_units() {
        let mut w = a();
        w.stmt = "1111111111111111".into();
        let r = rec("statement", "verus/u", &w);
        let entries = parse_log(&format!("{}\n", line(&r)));
        let ok = |_: &str, _: &Record| true;
        let u = ufp(&w);
        assert_eq!(status(&entries, &u, &ok), "reviewed");
        assert_eq!(
            status(&entries, &ufp(&a()), &ok),
            "not-reviewed(stale-or-invalid)"
        );
        let m = burn_down(&entries, &[u], &ok);
        assert_eq!(m["g"], (1, 0, 0));
    }

    // ---- review fixes

    const ATTR: &str = r#"
#![allow(unexpected_cfgs)]
#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy)]
pub struct Step { pub khz: u32 }

pub const TOP: usize = 4;

#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires steps.len() > 0,
    ensures r <= cap,
))]
pub fn step_for(steps: &[Step], cap: usize) -> usize {
    let mut i = 0;
    #[cfg_attr(verus_keep_ghost, verus_spec(
        invariant i <= cap,
        decreases cap - i
    ))]
    while i < cap {
        i += 1;
    }
    #[cfg(verus_keep_ghost)]
    proof! {
        assert(i <= cap);
    }
    i
}
"#;

    #[test]
    fn attribute_style_specs_are_statements_and_their_loops_and_proof_blocks_are_proof() {
        let c = classify("c/src/rules.rs", ATTR);
        assert!(c.errors.is_empty(), "{:?}", c.errors);
        let names: Vec<&str> = c.stmts.iter().map(|s| s.0.as_str()).collect();
        for n in [
            "c/src/rules.rs::step_for",
            "c/src/rules.rs::Step",
            "c/src/rules.rs::TOP",
        ] {
            assert!(names.contains(&n), "{n} missing: {names:?}");
        }
        let p = &c.proof_text;
        assert!(
            p.contains("invariant i <= cap") && p.contains("decreases cap - i"),
            "{p}"
        );
        assert!(
            p.contains("proof !{") && p.contains("assert(i <= cap)"),
            "{p}"
        );
        assert!(
            p.contains("#[cfg(verus_keep_ghost)]"),
            "the gate is proof text: {p}"
        );
        let e = &c.exec_text;
        assert!(
            !e.contains("requires") && !e.contains("invariant") && !e.contains("verus_spec"),
            "{e}"
        );
        assert!(e.contains("i += 1") && e.contains("while i < cap"), "{e}");
        // weakening the ensures moves the statement; deleting the invariant or the proof! block moves the proof; exec only moves exec
        let k = |src: &str| {
            let c = classify("c/src/rules.rs", src);
            assert!(c.errors.is_empty(), "{:?}", c.errors);
            (c.stmts, c.proof_text, c.exec_text)
        };
        let b = k(ATTR);
        let w = k(&ATTR.replace("ensures r <= cap", "ensures true"));
        assert_ne!(w.0, b.0);
        let inv = k(&ATTR.replace("invariant i <= cap,", "invariant true,"));
        assert_eq!((&inv.0, &inv.2), (&b.0, &b.2));
        assert_ne!(inv.1, b.1);
        let pb = k(&ATTR.replace("assert(i <= cap);", ""));
        assert_eq!((&pb.0, &pb.2), (&b.0, &b.2));
        assert_ne!(pb.1, b.1);
        let ex = k(&ATTR.replace("i += 1;", "i += 2;"));
        assert_eq!((&ex.0, &ex.1), (&b.0, &b.1));
        assert_ne!(ex.2, b.2);
    }

    #[test]
    fn unclassifiable_ghost_words_fail_closed() {
        let src = "#[cfg_attr(verus_keep_ghost, verus_verify)]\npub struct S;\npub fn f() -> bool { cfg!(verus_keep_ghost) }\n";
        let c = classify("c/src/a.rs", src);
        assert_eq!(c.errors.len(), 1, "{:?}", c.errors);
        assert!(
            c.errors[0].contains("c/src/a.rs:3")
                && c.errors[0].contains("verus_keep_ghost")
                && c.errors[0].contains("fail closed")
        );
        let c = classify("c/src/a.rs", "pub fn f() { let _ = verus_spec; }\n");
        assert_eq!(c.errors.len(), 1, "{:?}", c.errors);
    }

    #[test]
    fn gating_attributes_are_proof_text_for_every_form() {
        let base = classify(
            "c/src/m.rs",
            "#[path = \"x.rs\"]\npub mod walk;\nimpl X { fn f(&self) {} }\npub fn g() {}\n",
        );
        for gate in [
            "#[cfg(any())]",
            "#[cfg(false)]",
            "#[cfg(not(verus))]",
            "#[cfg(not(verus_keep_ghost))]",
            "#[cfg(not(all()))]",
        ] {
            // on a mod declaration
            let m = classify(
                "c/src/m.rs",
                &format!(
                    "{gate}\n#[path = \"x.rs\"]\npub mod walk;\nimpl X {{ fn f(&self) {{}} }}\npub fn g() {{}}\n"
                ),
            );
            assert!(m.errors.is_empty(), "{gate}: {:?}", m.errors);
            assert_eq!(
                m.exec_text, base.exec_text,
                "{gate} on a mod: exec must not move"
            );
            assert_ne!(
                m.proof_text, base.proof_text,
                "{gate} on a mod must move the proof text"
            );
            // on an impl
            let i = classify(
                "c/src/m.rs",
                &format!(
                    "#[path = \"x.rs\"]\npub mod walk;\n{gate}\nimpl X {{ fn f(&self) {{}} }}\npub fn g() {{}}\n"
                ),
            );
            assert_ne!(i.proof_text, base.proof_text, "{gate} on an impl");
            // inner attribute
            let n = classify(
                "c/src/m.rs",
                &format!(
                    "#![{}]\n#[path = \"x.rs\"]\npub mod walk;\nimpl X {{ fn f(&self) {{}} }}\npub fn g() {{}}\n",
                    gate.trim_start_matches("#[").trim_end_matches(']')
                ),
            );
            assert_ne!(
                n.proof_text, base.proof_text,
                "{gate} as an inner attribute"
            );
        }
        // an ordinary gate is exec
        let t = classify(
            "c/src/m.rs",
            "#[cfg(test)]\n#[path = \"x.rs\"]\npub mod walk;\nimpl X { fn f(&self) {} }\npub fn g() {}\n",
        );
        assert_eq!(t.proof_text, base.proof_text);
        assert_ne!(t.exec_text, base.exec_text);
        // inside a verus! body
        let v = classify(
            "x.rs",
            &VERUS.replace(
                "    proof { lemma_a(x); }",
                "    #[cfg(any())]\n    proof { lemma_a(x); }",
            ),
        );
        assert!(v.errors.is_empty(), "{:?}", v.errors);
        assert!(v.proof_text.contains("cfg(any())"), "{}", v.proof_text);
        assert!(!v.exec_text.contains("cfg"), "{}", v.exec_text);
    }

    #[test]
    fn omit_lines_are_proof_text() {
        let src = "pub fn f(n: u32) -> u32 //@ret\n//@ ensures ret == n,\n{\n    n\n}\n";
        let with = "pub fn f(n: u32) -> u32 //@ret\n//@ ensures ret == n,\n{\n    n\n    if n > 3 { return 0; } //@omit\n}\n".replace("    n\n    if n > 3 { return 0; } //@omit\n", "    if n > 3 { return 0; } //@omit\n    n\n");
        let a = classify("c/src/o.rs", src);
        let b = classify("c/src/o.rs", &with);
        assert!(
            a.errors.is_empty() && b.errors.is_empty(),
            "{:?} {:?}",
            a.errors,
            b.errors
        );
        assert_eq!(a.stmts, b.stmts);
        assert_eq!(a.exec_text, b.exec_text, "an omitted line is not exec text");
        assert_ne!(
            a.proof_text, b.proof_text,
            "adding //@omit moves the proof text"
        );
        assert!(b.proof_text.contains("omit: if n > 3"), "{}", b.proof_text);
    }

    #[test]
    fn global_item_in_verus_block_is_proof() {
        let c = classify(
            "x.rs",
            "verus! {\nglobal size_of usize == 8;\npub fn f() {}\n}\n",
        );
        assert!(c.errors.is_empty(), "{:?}", c.errors);
        assert!(c.proof_text.contains("global size_of"), "{}", c.proof_text);
    }

    #[test]
    fn model_aliases_and_the_strong_allowlist() {
        assert_eq!(
            model_norm("Claude Opus 5.5 (1M context)"),
            "claude-opus-5-5"
        );
        assert_eq!(model_norm("claude-opus-5-5[1m]"), "claude-opus-5-5");
        assert_eq!(model_norm("  Claude  OPUS 5.5 "), "claude-opus-5-5");
        assert_eq!(class("Claude Opus 5.5 (1M context)"), "strong");
        assert_eq!(class("qwen3-opus-distill"), "builder");
        assert_eq!(family("qwen3-opus-distill"), "qwen");
        assert_eq!(class("opus-clone"), "unknown");
        assert_eq!(class("claude-opus-4"), "unknown");
        assert_eq!(
            trailer_models("m\n\nCo-Authored-By: Claude Opus 5.5 (1M context) <a@b>"),
            vec!["claude-opus-5-5"]
        );
        // an alias spelling of the implementer is still the implementer
        let mut w = a();
        w.stmt = "1111111111111111".into();
        let mut r = rec("statement", "verus/u", &w);
        r.impl_model = "Claude Opus 5.5 (1M context)".into();
        r.impl_family = "claude".into();
        let o = run(&scn(Some(a()), w.clone(), vec![r]));
        assert!(
            o.refusals[0].detail.contains("same model"),
            "{:?}",
            o.refusals
        );
        // the one-model rule sees through the alias too
        let mut s = scn(
            Some(a()),
            {
                let mut x = w.clone();
                x.exec = "dddddddddddddddd".into();
                x
            },
            vec![],
        );
        let mut x = s.work.clone();
        x.exec = "dddddddddddddddd".into();
        s.records = vec![rec("statement", "verus/u", &x)];
        s.exec_models = vec![SON.into(), model_norm("Claude Opus 5.5 (1M context)")];
        s.work = x;
        assert!(rules(&run(&s)).contains(&"one-model-wrote-code-and-proof"));
    }

    #[test]
    fn trailers_come_from_the_last_paragraph_only() {
        let m = "feat\n\nCo-Authored-By: Claude Opus 5.5 <a@b>\n\nbody text\n\nCo-Authored-By: Claude Sonnet 5.5 <a@b>\n";
        assert_eq!(trailer_models(m), vec!["claude-sonnet-5-5"]);
        assert!(
            trailer_models("subject\n\n> Co-Authored-By: Claude Opus 5.5 <a@b>\n\nmore words\n")
                .is_empty()
        );
    }

    #[test]
    fn a_builder_trailer_on_a_proof_or_statement_commit_is_refused() {
        let mut w = a();
        w.proof = "2222222222222222".into();
        w.exec = "dddddddddddddddd".into();
        let mut r = rec("proof-edit", "verus/u", &w);
        r.reviewer_model = "-".into();
        r.reviewer_family = "-".into();
        let mut s = scn(Some(a()), w.clone(), vec![r.clone()]);
        s.exec_models = vec![SON.into()];
        // Sonnet changed the proof text in commit A; Opus only added the record.
        s.proof_commits = vec![("aaaaaaaaaa".into(), vec![SON.into()])];
        let o = run(&s);
        assert_eq!(
            rules(&o),
            ["proof-or-statement-written-by-a-builder"],
            "{:?}",
            o.refusals
        );
        assert!(o.refusals[0].detail.contains("builder-class"));
        // no trailer at all: unattributed
        s.proof_commits = vec![("aaaaaaaaaa".into(), vec![])];
        let o = run(&s);
        assert_eq!(rules(&o), ["proof-or-statement-written-by-a-builder"]);
        assert!(o.refusals[0].detail.contains("no Co-Authored-By"));
        // a mixed trailer block is still refused
        s.proof_commits = vec![("aaaaaaaaaa".into(), vec![OPUS.into(), SON.into()])];
        assert_eq!(rules(&run(&s)), ["proof-or-statement-written-by-a-builder"]);
        // the strong model wrote it: fine
        s.proof_commits = vec![("aaaaaaaaaa".into(), vec![OPUS.into()])];
        let o = run(&s);
        assert!(o.refusals.is_empty(), "{:?}", o.refusals);
        // a statement change by a builder is refused the same way
        let mut ws = a();
        ws.stmt = "1111111111111111".into();
        let mut s2 = scn(
            Some(a()),
            ws.clone(),
            vec![rec("statement", "verus/u", &ws)],
        );
        s2.proof_commits = vec![("bbbbbbbbbb".into(), vec![SON.into()])];
        assert_eq!(
            rules(&run(&s2)),
            ["proof-or-statement-written-by-a-builder"]
        );
    }

    #[test]
    fn the_declared_implementer_must_be_in_the_code_commits_trailers() {
        let mut w = a();
        w.stmt = "1111111111111111".into();
        w.exec = "dddddddddddddddd".into();
        let mut s = scn(Some(a()), w.clone(), vec![rec("statement", "verus/u", &w)]);
        s.exec_models = vec!["qoder".into()];
        let o = run(&s);
        assert_eq!(
            rules(&o),
            ["implementer-not-in-trailers"],
            "{:?}",
            o.refusals
        );
        s.exec_models = vec![SON.into()];
        assert!(run(&s).refusals.is_empty());
    }

    #[test]
    fn reviewer_must_be_a_known_family_and_the_verdict_file_must_be_this_records() {
        let mut w = a();
        w.stmt = "1111111111111111".into();
        let mut r = rec("statement", "verus/u", &w);
        r.reviewer_model = "some-lane".into();
        r.reviewer_family = "other".into();
        let o = run(&scn(Some(a()), w.clone(), vec![r]));
        assert!(
            o.refusals[0].detail.contains("not a known family"),
            "{:?}",
            o.refusals
        );
        let mut r = rec("statement", "verus/u", &w);
        r.verdict_ref = "docs/ci/verdicts/some-other-record.md".into();
        let o = run(&scn(Some(a()), w.clone(), vec![r]));
        assert!(
            o.refusals[0].detail.contains("verdict file of this record"),
            "{:?}",
            o.refusals
        );
        // a verdict file that existed at the base and changed
        let mut s = scn(Some(a()), w.clone(), vec![rec("statement", "verus/u", &w)]);
        s.verdict_modified = vec!["docs/ci/verdicts/old.md".into()];
        assert_eq!(rules(&run(&s)), ["verdict-file-modified"]);
    }

    #[test]
    fn new_record_lines_unknown_unit_duplicate_id_and_uncommitted_paths() {
        let mut w = a();
        w.stmt = "1111111111111111".into();
        let r = rec("statement", "verus/u", &w);
        // uncommitted: a note without the merge gate, a refusal with it
        let mut s = scn(Some(a()), w.clone(), vec![r.clone()]);
        s.mode = 1;
        s.merge_gate = false;
        let o = run(&s);
        assert!(
            o.refusals.is_empty() && o.notes.len() == 1,
            "{:?} {:?}",
            o.refusals,
            o.notes
        );
        s.merge_gate = true;
        assert_eq!(rules(&run(&s)), ["trailer-unverifiable"]);
        // commit not found
        s.mode = 2;
        assert_eq!(rules(&run(&s)), ["trailer-unverifiable"]);
        // unknown unit
        let mut bad = r.clone();
        bad.unit = "verus/ghost".into();
        let o = run(&scn(Some(a()), w.clone(), vec![bad]));
        assert!(rules(&o).contains(&"unknown-unit"), "{:?}", o.refusals);
        // duplicate id
        let o = run(&scn(Some(a()), w, vec![r.clone(), r]));
        assert!(rules(&o).contains(&"duplicate-id"), "{:?}", o.refusals);
    }

    // ---- git plumbing, against a throwaway repository

    fn tmp_repo(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lucina-roles-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn g(d: &Path, args: &[&str]) -> String {
        let o = Command::new("git")
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(d)
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    }

    fn commit(d: &Path, msg: &str, trailer: Option<&str>) -> String {
        g(d, &["add", "-A"]);
        match trailer {
            Some(t) => g(
                d,
                &[
                    "commit",
                    "-q",
                    "-m",
                    msg,
                    "-m",
                    &format!("Co-Authored-By: {t} <noreply@anthropic.com>"),
                ],
            ),
            None => g(d, &["commit", "-q", "-m", msg]),
        };
        g(d, &["rev-parse", "HEAD"])
    }

    const RULES_RS: &str = "pub fn f(n: usize) -> usize //@ret\n//@ requires n < 10,\n//@ ensures ret == n,\n{\n    let mut i = 0;\n    while i < n\n    //@ invariant i <= n,\n    //@ decreases n - i,\n    {\n        i += 1;\n    }\n    i\n}\n";

    #[test]
    fn git_plumbing_finds_who_changed_what_and_who_added_the_record() {
        let d = tmp_repo("plumb");
        g(&d, &["init", "-q", "-b", "main"]);
        fs::create_dir_all(d.join("crates/x/src")).unwrap();
        fs::create_dir_all(d.join("docs/ci")).unwrap();
        fs::write(d.join("crates/x/src/rules.rs"), RULES_RS).unwrap();
        fs::write(d.join(LOG), LOG_HEADER).unwrap();
        let base = commit(&d, "base", Some("Claude Sonnet 5.5"));
        let unit = Unit {
            name: "shipped/x/rules".into(),
            group: "g".into(),
            kind: "shipped".into(),
            target: "crates/x/src/rules.rs".into(),
            inputs: vec![],
        };
        let files = vec!["crates/x/src/rules.rs".to_string()];
        // commit 1: Sonnet, exec only
        fs::write(
            d.join("crates/x/src/rules.rs"),
            RULES_RS.replace("i += 1;", "i += 1; let _z = 0;"),
        )
        .unwrap();
        commit(&d, "exec", Some("Claude Sonnet 5.5"));
        let i = change_info(
            &d,
            &base,
            &unit.name,
            std::slice::from_ref(&unit),
            &files,
            None,
        );
        assert_eq!(i.exec, vec![vec!["claude-sonnet-5-5".to_string()]]);
        assert!(i.proof.is_empty(), "{:?}", i.proof);
        // commit 2: Sonnet, proof text; commit 3: no trailer, proof text again
        let p1 = RULES_RS
            .replace("i += 1;", "i += 1; let _z = 0;")
            .replace("invariant i <= n", "invariant i <= n + 0");
        fs::write(d.join("crates/x/src/rules.rs"), &p1).unwrap();
        commit(&d, "proof by sonnet", Some("Claude Sonnet 5.5"));
        fs::write(
            d.join("crates/x/src/rules.rs"),
            p1.replace("decreases n - i", "decreases n - i + 0"),
        )
        .unwrap();
        commit(&d, "proof, no trailer", None);
        let i = change_info(
            &d,
            &base,
            &unit.name,
            std::slice::from_ref(&unit),
            &files,
            None,
        );
        assert_eq!(i.exec.len(), 1);
        assert_eq!(i.proof.len(), 2, "{:?}", i.proof);
        assert_eq!(i.proof[0].1, vec!["claude-sonnet-5-5".to_string()]);
        assert!(i.proof[1].1.is_empty());
        // commit 4: Opus adds the record line (touches no unit file); the oldest commit that added a line wins
        let line_text = "rec-1\t2026-11-11\tproof-edit\tshipped/x/rules\t-\t-\t-\t-\tclaude-sonnet-5-5\tclaude\tclaude-opus-5-5\tclaude\tstrong\t-\t-\taccepted\tdocs/ci/verdicts/rec-1.md\t-\tn";
        let mut log = fs::read_to_string(d.join(LOG)).unwrap();
        log.push_str(line_text);
        log.push('\n');
        fs::write(d.join(LOG), log).unwrap();
        commit(&d, "record", Some("Claude Opus 5.5 (1M context)"));
        let m = added_map(&d, &base);
        assert_eq!(
            m.get(line_text),
            Some(&vec!["claude-opus-5-5".to_string()]),
            "{m:?}"
        );
        assert_eq!(m.len(), 1, "only the new line: {m:?}");
        // the check, end to end on these facts: Sonnet wrote the proof text, Opus only signed it
        let entries = parse_log(&fs::read_to_string(d.join(LOG)).unwrap());
        assert!(entries[0].res.is_ok(), "{:?}", entries[0].res);
        let w = Fp {
            stmt: "aaaaaaaaaaaaaaaa".into(),
            proof: "bbbbbbbbbbbbbbbb".into(),
            exec: "cccccccccccccccc".into(),
            data: "eeeeeeeeeeeeeeee".into(),
            core: Core::default(),
        };
        let b = Fp {
            proof: "9999999999999999".into(),
            ..w.clone()
        };
        let mut rec1 = entries[0].res.clone().unwrap();
        rec1.stmt = w.stmt.clone();
        rec1.proof = w.proof.clone();
        rec1.exec = w.exec.clone();
        rec1.data = w.data.clone();
        let entries = vec![Entry {
            line: 8,
            raw: line_text.into(),
            id: "rec-1".into(),
            res: Ok(rec1),
        }];
        let mut basem = BTreeMap::new();
        basem.insert("shipped/x/rules".to_string(), b);
        let mut u = ufp(&w);
        u.unit = "shipped/x/rules".into();
        let work = vec![u];
        let vok = |_: &str, _: &Record| true;
        let added = |raw: &str| match m.get(raw) {
            Some(t) => Added::Committed(t.clone()),
            None => Added::Unknown,
        };
        let changes = |_: &str| i.clone();
        let o = check(&CheckIn {
            base: &basem,
            work: &work,
            entries: &entries,
            base_log: LOG_HEADER,
            work_log: &format!("{LOG_HEADER}{line_text}\n"),
            verdict_ok: &vok,
            added: &added,
            changes: &changes,
            changes_after: &|_: &str, _: &str| ChangeInfo::default(),
            verdict_modified: &[],
            merge_gate: true,
            cutoff_work: None,
            cutoff_changed: &[],
            allow_cutoff: false,
            exists: &|_: &str| false,
            omit_hazards: &[],
            registration_exempt: &[],
            extractor_changed: &[],
            unit_files_changed: &[],
            allow_extractor: false,
        });
        assert!(
            rules(&o).contains(&"proof-or-statement-written-by-a-builder"),
            "{:?}",
            o.refusals
        );
        assert!(
            !rules(&o).contains(&"trailer-mismatch"),
            "the record itself was added by Opus: {:?}",
            o.refusals
        );
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_record_added_by_the_implementers_commit_is_a_trailer_mismatch_in_git() {
        let d = tmp_repo("forge");
        g(&d, &["init", "-q", "-b", "main"]);
        fs::create_dir_all(d.join("docs/ci")).unwrap();
        fs::write(d.join(LOG), LOG_HEADER).unwrap();
        let base = commit(&d, "base", Some("Claude Opus 5.5"));
        let line_text = "rec-2\t2026-11-11\tproof-edit\tshipped/x/rules\t-\t-\t-\t-\tclaude-sonnet-5-5\tclaude\tclaude-opus-5-5\tclaude\tstrong\t-\t-\taccepted\tdocs/ci/verdicts/rec-2.md\t-\tn";
        fs::write(d.join(LOG), format!("{LOG_HEADER}{line_text}\n")).unwrap();
        commit(&d, "record by the builder", Some("Claude Sonnet 5.5"));
        let m = added_map(&d, &base);
        assert_eq!(
            m.get(line_text),
            Some(&vec!["claude-sonnet-5-5".to_string()])
        );
        // a merge of a side branch carries the same commits: the non-merge commit is found
        g(&d, &["checkout", "-q", "-b", "side", &base]);
        let line3 = line_text.replace("rec-2", "rec-3");
        fs::write(d.join(LOG), format!("{LOG_HEADER}{line3}\n")).unwrap();
        commit(&d, "record on a side branch", Some("Claude Opus 5.5"));
        g(&d, &["checkout", "-q", "main"]);
        let mut cur = fs::read_to_string(d.join(LOG)).unwrap();
        cur.push_str(&format!("{line3}\n"));
        fs::write(d.join(LOG), &cur).unwrap();
        commit(&d, "same line on main", Some("Claude Sonnet 5.5"));
        let m = added_map(&d, &base);
        assert!(m.contains_key(&line3), "{m:?}");
        let _ = fs::remove_dir_all(&d);
    }

    // ---- second review round

    struct MemTree(BTreeMap<String, String>);
    impl Tree for MemTree {
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

    const DATA_SRC: &str = r#"
verus! {
pub struct Used { pub a: u32 }
pub struct Unused { pub b: u32 }
pub open spec fn g(u: Used) -> u32 { u.a }
pub fn f(x: u32) -> (r: u32) ensures r == g(Used { a: x }) { x }
}
"#;

    // ---- a bare fn header that becomes a statement is not a code change (Option A order) ----

    fn core_of(src: &str) -> Core {
        fp_of(src).core
    }

    fn vsrc(f: &str) -> String {
        format!("verus! {{\npub struct S {{ pub a: u32 }}\n{f}\n}}\n")
    }

    #[test]
    fn verus_block_promotion_is_not_an_exec_change_but_anything_else_is() {
        let bare = core_of(&vsrc("pub fn f(x: u32) -> u32 { x + 1 }"));
        let with = |sig: &str, body: &str| core_of(&vsrc(&format!("{sig} {{ {body} }}")));
        // clauses added, nothing else
        assert!(promotion_only(
            &bare,
            &with("pub fn f(x: u32) -> u32 requires x < 9", "x + 1")
        ));
        assert!(promotion_only(
            &bare,
            &with(
                "pub fn f(x: u32) -> (r: u32) requires x < 9 ensures r == x + 1",
                "x + 1"
            )
        ));
        // a body token changed in the same commit
        assert!(!promotion_only(
            &bare,
            &with("pub fn f(x: u32) -> u32 requires x < 9", "x + 2")
        ));
        // a parameter renamed, the return type changed, another fn added
        assert!(!promotion_only(
            &bare,
            &with("pub fn f(y: u32) -> u32 requires y < 9", "y + 1")
        ));
        assert!(!promotion_only(
            &bare,
            &with("pub fn f(x: u32) -> u64 requires x < 9", "x + 1")
        ));
        let extra = core_of(&vsrc(
            "pub fn f(x: u32) -> u32 requires x < 9 { x + 1 }\npub fn h() -> u32 { 7 }",
        ));
        assert!(!promotion_only(&bare, &extra));
        // a data definition edited in the same commit
        let d = core_of(
            &vsrc("pub fn f(x: u32) -> u32 requires x < 9 { x + 1 }")
                .replace("pub a: u32", "pub a: u64"),
        );
        assert!(!promotion_only(&bare, &d));
        // the reverse (a builder dropping the clauses) is no code change either, but the statement moved: the builder's commit is
        // then a builder's statement change (the git test below)
        let stmt = fp_of(&vsrc("pub fn f(x: u32) -> u32 requires x < 9 { x + 1 }"));
        let bare_fp = fp_of(&vsrc("pub fn f(x: u32) -> u32 { x + 1 }"));
        assert!(promotion_only(&stmt.core, &bare_fp.core));
        assert_ne!(stmt.stmt, bare_fp.stmt);
        // a body edited by the prover while it adds clauses is caught even when clauses are all it "meant" to add
        assert!(!promotion_only(
            &bare,
            &with("pub fn f(x: u32) -> u32 ensures true", "x")
        ));
    }

    fn shipped_core(src: &str) -> Core {
        let t = MemTree([("crates/x/src/rules.rs".to_string(), src.to_string())].into());
        let (v, e) = fingerprints(&t, &[rules_unit().0]);
        assert!(e.is_empty(), "{e:?}");
        v[0].fp.clone().unwrap().core
    }

    const SBARE: &str = "pub fn f(n: usize) -> usize\n{\n    n + 1\n}\n";
    const SCLAUSES: &str = "pub fn f(n: usize) -> usize //@ret\n//@ requires n < 10,\n//@ ensures ret == n + 1,\n{\n    n + 1\n}\n";

    #[test]
    fn shipped_slash_at_clause_lines_under_a_bare_header_are_a_promotion() {
        let bare = shipped_core(SBARE);
        assert!(promotion_only(&bare, &shipped_core(SCLAUSES)));
        assert!(!promotion_only(
            &bare,
            &shipped_core(&SCLAUSES.replace("n + 1\n}", "n + 2\n}"))
        ));
        assert!(!promotion_only(
            &bare,
            &shipped_core(
                &SCLAUSES
                    .replace("(n: usize)", "(m: usize)")
                    .replace("n <", "m <")
                    .replace("== n", "== m")
                    .replace("n + 1\n}", "m + 1\n}")
            )
        ));
        assert!(!promotion_only(
            &bare,
            &shipped_core(&SCLAUSES.replace("-> usize", "-> u64"))
        ));
        // the attribute style: a verus_spec attribute above a bare header is verification text, a changed signature under it is not
        let attr = "#[cfg_attr(verus_keep_ghost, verus_spec(r => ensures r == n + 1))]\n";
        assert!(promotion_only(
            &bare,
            &shipped_core(&format!("{attr}{SBARE}"))
        ));
        assert!(!promotion_only(
            &bare,
            &shipped_core(&format!("{attr}{}", SBARE.replace("-> usize", "-> u64")))
        ));
        // an added attribute that is not a verifier one is code
        assert!(!promotion_only(
            &bare,
            &shipped_core(&format!("#[no_mangle]\n{SBARE}"))
        ));
    }

    #[test]
    fn the_provers_clauses_on_a_builders_bare_fn_do_not_count_as_a_code_commit() {
        let d = tmp_repo("promote");
        g(&d, &["init", "-q", "-b", "main"]);
        fs::create_dir_all(d.join("crates/x/src")).unwrap();
        fs::write(d.join("crates/x/src/rules.rs"), "pub const K: usize = 1;\n").unwrap();
        let base = commit(&d, "base", Some("Claude Opus 5.5"));
        let (unit, files) = rules_unit();
        let units = std::slice::from_ref(&unit);
        let ci = |d: &Path, base: &str| change_info(d, base, &unit.name, units, &files, None);
        // builder: a bare fn with a body
        fs::write(
            d.join("crates/x/src/rules.rs"),
            format!("pub const K: usize = 1;\n{SBARE}"),
        )
        .unwrap();
        commit(&d, "builder body", Some("Claude Sonnet 5.5"));
        // prover: clauses only
        fs::write(
            d.join("crates/x/src/rules.rs"),
            format!("pub const K: usize = 1;\n{SCLAUSES}"),
        )
        .unwrap();
        commit(&d, "prover clauses", Some("Claude Opus 5.5"));
        let info = ci(&d, &base);
        assert_eq!(
            info.exec,
            vec![vec!["claude-sonnet-5-5".to_string()]],
            "{:?}",
            info.exec
        );
        assert_eq!(info.proof.len(), 1, "{:?}", info.proof);
        assert_eq!(info.proof[0].1, vec!["claude-opus-5-5".to_string()]);
        // the same range with the prover also touching a body token: its commit is an exec change too
        g(&d, &["checkout", "-q", "-b", "bad", "HEAD~1"]);
        fs::write(
            d.join("crates/x/src/rules.rs"),
            format!(
                "pub const K: usize = 1;\n{}",
                SCLAUSES.replace("n + 1\n}", "n + 2\n}")
            ),
        )
        .unwrap();
        commit(&d, "prover clauses and body", Some("Claude Opus 5.5"));
        let bad = ci(&d, &base);
        assert_eq!(bad.exec.len(), 2, "{:?}", bad.exec);
        assert!(
            bad.exec
                .iter()
                .any(|t| t == &vec!["claude-opus-5-5".to_string()])
        );
        // the prover renaming a parameter while it adds the clauses
        g(&d, &["checkout", "-q", "-b", "bad2", "main"]);
        g(&d, &["reset", "-q", "--hard", &base]);
        fs::write(
            d.join("crates/x/src/rules.rs"),
            format!("pub const K: usize = 1;\n{SBARE}"),
        )
        .unwrap();
        commit(&d, "builder body", Some("Claude Sonnet 5.5"));
        let ren = SCLAUSES
            .replace("(n: usize)", "(m: usize)")
            .replace("n <", "m <")
            .replace("== n", "== m")
            .replace("n + 1\n}", "m + 1\n}");
        fs::write(
            d.join("crates/x/src/rules.rs"),
            format!("pub const K: usize = 1;\n{ren}"),
        )
        .unwrap();
        commit(&d, "prover renames", Some("Claude Opus 5.5"));
        let bad2 = ci(&d, &base);
        assert_eq!(bad2.exec.len(), 2, "{:?}", bad2.exec);
        // a builder turning a statement back into a bare header: a statement change by a builder
        g(&d, &["checkout", "-q", "-b", "rev", "main"]);
        g(&d, &["reset", "-q", "--hard", &base]);
        fs::write(
            d.join("crates/x/src/rules.rs"),
            format!("pub const K: usize = 1;\n{SCLAUSES}"),
        )
        .unwrap();
        let mid = commit(&d, "opus adds fn with clauses", Some("Claude Opus 5.5"));
        fs::write(
            d.join("crates/x/src/rules.rs"),
            format!("pub const K: usize = 1;\n{SBARE}"),
        )
        .unwrap();
        commit(&d, "builder drops clauses", Some("Claude Sonnet 5.5"));
        let rev = ci(&d, &mid);
        assert_eq!(rev.proof.len(), 1, "{:?}", rev.proof);
        assert_eq!(rev.proof[0].1, vec!["claude-sonnet-5-5".to_string()]);
        let _ = fs::remove_dir_all(&d);
    }

    fn data_unit() -> Unit {
        Unit {
            name: "verus/x/lib".into(),
            group: "g".into(),
            kind: "verus".into(),
            target: "crates/x/src/lib.rs".into(),
            inputs: vec![],
        }
    }

    fn fp_of(src: &str) -> Fp {
        let t = MemTree([("crates/x/src/lib.rs".to_string(), src.to_string())].into());
        let (v, e) = fingerprints(&t, &[data_unit()]);
        assert!(e.is_empty(), "{e:?}");
        v[0].fp.clone().unwrap()
    }

    #[test]
    fn a_data_definition_no_statement_refers_to_is_code() {
        let base = fp_of(DATA_SRC);
        // Unused is referred to by no statement: its change moves exec only
        let u = fp_of(&DATA_SRC.replace("pub b: u32", "pub b: u64"));
        assert_eq!(
            (&u.stmt, &u.proof, &u.data),
            (&base.stmt, &base.proof, &base.data)
        );
        assert_ne!(u.exec, base.exec);
        // Used is referred to by a spec fn and by an exec header: its change moves data, not statement/proof/exec
        let d = fp_of(&DATA_SRC.replace("pub a: u32", "pub a: u64"));
        assert_eq!((&d.stmt, &d.proof), (&base.stmt, &base.proof));
        assert_ne!(d.data, base.data);
        assert_eq!(d.exec, base.exec);
    }

    #[test]
    fn items_referred_to_through_other_items_are_data_and_unknown_names_are_data() {
        let src = "verus! {\npub struct Inner { pub v: u32 }\npub struct Outer { pub i: Inner }\npub fn f(o: Outer) -> u32 ensures true { 0 }\n}\n";
        let t = MemTree([("crates/x/src/lib.rs".to_string(), src.to_string())].into());
        let (v, _) = fingerprints(&t, &[data_unit()]);
        let base = v[0].fp.clone().unwrap();
        let ch = fp_of(&src.replace("pub v: u32", "pub v: u64"));
        assert_ne!(ch.data, base.data, "Inner is referred to through Outer");
        assert_eq!(ch.exec, base.exec);
        let items = vec![Item {
            name: "_".into(),
            key: "k".into(),
            text: "t".into(),
            hash: 1,
        }];
        assert!(
            referenced_items(&items, "").contains("k"),
            "when in doubt it is data"
        );
    }

    #[test]
    fn a_data_only_change_needs_a_recheck_record_but_a_builder_may_make_it() {
        let mut w = a();
        w.data = "9999999999999999".into();
        w.exec = "dddddddddddddddd".into();
        let o = run(&scn(Some(a()), w.clone(), vec![]));
        assert_eq!(
            rules(&o),
            ["recheck-needs-strong-prover"],
            "{:?}",
            o.refusals
        );
        assert!(o.refusals[0].hint.contains("recheck"));
        let mut r = rec("recheck", "verus/u", &w);
        r.reviewer_model = "-".into();
        r.reviewer_family = "-".into();
        let mut s = scn(Some(a()), w.clone(), vec![r.clone()]);
        // the data definition was changed by Sonnet: allowed (it is code); the record is Opus's
        s.exec_models = vec![SON.into()];
        let o = run(&s);
        assert!(o.refusals.is_empty(), "{:?}", o.refusals);
        // a record for the old data does not count
        let mut old = r.clone();
        old.data = a().data;
        let mut s2 = scn(Some(a()), w, vec![old]);
        s2.exec_models = vec![SON.into()];
        assert_eq!(rules(&run(&s2)), ["recheck-needs-strong-prover"]);
    }

    /// The coordinator's list: cutoff commit, then the only unit that may get a pre-rule record, with its fixed started_before.
    const SB: &str = "abcdef0123456789abcdef0123456789abcdef01";
    const CUT: &str = "cutoff 0123456789abcdef0123456789abcdef01234567\nverus/u\tabcdef0123456789abcdef0123456789abcdef01\tkexec pages: one Opus author before roles\n";

    fn pre(w: &Fp) -> Record {
        let mut r = rec("pre-rule", "verus/u", w);
        r.impl_model = OPUS.into();
        r.impl_family = "claude".into();
        r.reviewer_model = "-".into();
        r.reviewer_family = "-".into();
        r
    }

    #[test]
    fn a_pre_rule_record_with_an_independent_verdict_is_accepted_before_the_cutoff() {
        let mut w = a();
        w.stmt = "1111111111111111".into();
        w.exec = "dddddddddddddddd".into();
        // one Opus session wrote code and proof: builder-free authorship checks do not apply
        let mut s = scn(None, w.clone(), vec![pre(&w)]);
        s.cutoff = Some(CUT.into());
        s.exec_models = vec![OPUS.into()];
        s.proof_commits = vec![("aaaaaaaaaa".into(), vec![OPUS.into(), SON.into()])];
        let o = run(&s);
        assert!(o.refusals.is_empty(), "{:?}", o.refusals);
        assert!(
            o.notes.iter().any(|n| n.contains("PRE-RULE")),
            "{:?}",
            o.notes
        );
    }

    #[test]
    fn a_pre_rule_record_is_refused_when_the_listed_commit_is_missing_or_without_the_list() {
        // This test used to be "refused after the cutoff": the ancestor-of-the-cutoff rule is gone (a listed started_before may be
        // any commit the list names, on a branch or not); what is refused now is a commit missing from the object database.
        let mut w = a();
        w.stmt = "1111111111111111".into();
        let mut s = scn(None, w.clone(), vec![pre(&w)]);
        s.cutoff = Some(CUT.into());
        s.exists_ok = false;
        let o = run(&s);
        assert_eq!(rules(&o), ["statement-needs-strong-prover-and-reviewer"]);
        assert!(
            o.refusals[0]
                .detail
                .contains("does not exist in this repository's object database"),
            "{}",
            o.refusals[0].detail
        );
        // no cutoff file: closed
        let s = scn(None, w.clone(), vec![pre(&w)]);
        let o = run(&s);
        assert!(
            o.refusals[0]
                .detail
                .contains("proof-roles-cutoff.txt is absent"),
            "{}",
            o.refusals[0].detail
        );
    }

    #[test]
    fn a_pre_rule_record_needs_an_independent_verdict_file_and_a_strong_reviewer() {
        let mut w = a();
        w.stmt = "1111111111111111".into();
        let mut s = scn(None, w.clone(), vec![pre(&w)]);
        s.cutoff = Some(CUT.into());
        s.verdict = false;
        let o = run(&s);
        assert!(
            o.refusals[0].detail.contains("lacks"),
            "{}",
            o.refusals[0].detail
        );
        let mut r = pre(&w);
        r.prover_model = SON.into();
        r.prover_class = "builder".into();
        r.prover_family = "claude".into();
        let mut s = scn(None, w.clone(), vec![r]);
        s.cutoff = Some(CUT.into());
        s.trailer = vec![SON.into()];
        assert!(run(&s).refusals[0].detail.contains("not strong"));
        // started_before must be a hash on a pre-rule record and "-" elsewhere
        let mut r = pre(&w);
        r.started_before = "-".into();
        assert!(
            parse_fields(&line(&r).split('\t').collect::<Vec<_>>())
                .unwrap_err()
                .contains("started_before")
        );
        let mut r = rec("statement", "verus/u", &w);
        r.started_before = SB.into();
        assert!(
            parse_fields(&line(&r).split('\t').collect::<Vec<_>>())
                .unwrap_err()
                .contains("started_before")
        );
    }

    #[test]
    fn cutoff_file_is_checked_for_changes() {
        let w = a();
        let entries = parse_log("");
        let base = BTreeMap::new();
        let work = vec![ufp(&w)];
        let vok = |_: &str, _: &Record| true;
        let added = |_: &str| Added::Unknown;
        let changes = |_: &str| ChangeInfo::default();
        let o = check(&CheckIn {
            base: &base,
            work: &work,
            entries: &entries,
            base_log: "",
            work_log: "",
            verdict_ok: &vok,
            added: &added,
            changes: &changes,
            changes_after: &|_: &str, _: &str| ChangeInfo::default(),
            verdict_modified: &[],
            merge_gate: true,
            cutoff_work: Some("aaa"),
            cutoff_changed: &["docs/ci/proof-roles-cutoff.txt".to_string()],
            allow_cutoff: false,
            exists: &|_: &str| true,
            omit_hazards: &[],
            registration_exempt: &[],
            extractor_changed: &[],
            unit_files_changed: &[],
            allow_extractor: false,
        });
        assert!(rules(&o).contains(&"cutoff-protected"), "{:?}", o.refusals);
    }

    #[test]
    fn code_commits_without_a_trailer_are_refused_for_units_with_a_record() {
        let mut w = a();
        w.stmt = "1111111111111111".into();
        w.exec = "dddddddddddddddd".into();
        let mut s = scn(Some(a()), w.clone(), vec![rec("statement", "verus/u", &w)]);
        s.exec_override = Some(vec![vec![SON.into()], vec![]]);
        let o = run(&s);
        assert_eq!(
            rules(&o),
            ["code-commit-without-trailer"],
            "{:?}",
            o.refusals
        );
        s.exec_override = Some(vec![vec![SON.into()]]);
        assert!(run(&s).refusals.is_empty());
    }

    #[test]
    fn extractor_and_unit_files_in_one_merge_are_refused_unless_named() {
        let mut w = a();
        w.exec = "dddddddddddddddd".into();
        let mut s = scn(Some(a()), w, vec![]);
        s.extractor_changed = vec!["tools/ci/lucina-ci/src/spec.rs".into()];
        s.unit_files_changed = vec!["kernel/lucina-ring/src/lib.rs".into()];
        let o = run(&s);
        assert_eq!(
            rules(&o),
            ["extractor-changed-with-unit-files"],
            "{:?}",
            o.refusals
        );
        assert!(
            o.refusals[0].detail.contains("spec.rs")
                && o.refusals[0].detail.contains("lucina-ring")
        );
        s.allow_extractor = true;
        assert!(run(&s).refusals.is_empty());
        // an extractor change alone, or unit files alone, is fine
        s.allow_extractor = false;
        s.unit_files_changed = vec![];
        assert!(run(&s).refusals.is_empty());
        s.unit_files_changed = vec!["kernel/lucina-ring/src/lib.rs".into()];
        s.extractor_changed = vec![];
        assert!(run(&s).refusals.is_empty());
    }

    #[test]
    fn only_listed_context_qualifiers_are_stripped_from_model_names() {
        assert_eq!(
            model_norm("Claude Opus 5.5 (actually Sonnet)"),
            "claude-opus-5-5-actually-sonnet"
        );
        assert_eq!(model_norm("Claude (Qwen) Opus 5.5"), "claude-qwen-opus-5-5");
        assert_ne!(class("Claude Opus 5.5 (actually Sonnet)"), "strong");
        assert_eq!(
            model_norm("Claude Opus 5.5 (1M context)"),
            "claude-opus-5-5"
        );
        assert_eq!(model_norm("claude-opus-5-5[1m]"), "claude-opus-5-5");
    }

    #[test]
    fn verdict_files_need_the_five_headings() {
        let mut w = a();
        w.stmt = "1111111111111111".into();
        let r = rec("statement", "verus/u", &w);
        let good = format!(
            "# Verdict {}: {}\\n{} {}\\n## Property\\nx\\n## Vacuity\\nx\\n## Shipped code\\nx\\n## Weaknesses\\nx\\n## Recommendation\\nx\\n",
            r.id, r.unit, r.stmt, r.proof
        )
        .replace("\\n", "\n");
        assert!(verdict_text_ok(&good, &r));
        assert!(!verdict_text_ok(
            &good.replace("## Vacuity", "## Other"),
            &r
        ));
        assert!(!verdict_text_ok(&good.replace(&r.proof, "x"), &r));
        assert!(!verdict_text_ok(&good.replace(&r.id, "x"), &r));
        assert!(!verdict_text_ok("ok", &r));
    }

    #[test]
    fn plain_looking_cfg_gates_above_a_path_mod_are_proof_text() {
        let base = classify("v/rules.rs", "#[path = \"x.rs\"]\npub mod walk;\n");
        for gate in [
            "#[cfg(feature = \"verify\")]",
            "#[cfg(target_os = \"none\")]",
            "#[cfg(not(unix))]",
            "#[cfg(debug_assertions)]",
            "#[cfg(all(unix, windows))]",
            "#[cfg(target_arch = \"aarch64\")]",
        ] {
            let c = classify(
                "v/rules.rs",
                &format!("{gate}\n#[path = \"x.rs\"]\npub mod walk;\n"),
            );
            assert!(c.errors.is_empty(), "{gate}: {:?}", c.errors);
            assert_ne!(
                c.proof_text, base.proof_text,
                "{gate} must move the proof text"
            );
            assert_eq!(
                c.exec_text, base.exec_text,
                "{gate}: exec text must not hide the gate"
            );
        }
        // only test-only predicates stay exec
        for gate in ["#[cfg(test)]", "#[cfg(not(test))]"] {
            let c = classify(
                "v/rules.rs",
                &format!("{gate}\n#[path = \"x.rs\"]\npub mod walk;\n"),
            );
            assert_eq!(c.proof_text, base.proof_text, "{gate}");
        }
        // a cfg_attr whose first argument is a feature is proof text too
        let c = classify(
            "v/rules.rs",
            "#[cfg_attr(feature = \"x\", derive(Debug))]\npub struct S;\n",
        );
        assert!(c.proof_text.contains("cfg_attr"), "{}", c.proof_text);
    }

    #[test]
    fn the_tree_is_dirty_when_a_tracked_file_differs_from_head_and_head_is_what_is_read() {
        let d = tmp_repo("dirty");
        g(&d, &["init", "-q", "-b", "main"]);
        fs::write(d.join("f.txt"), "one\n").unwrap();
        commit(&d, "base", Some("Claude Sonnet 5.5"));
        assert!(dirty_tracked(&d).is_empty());
        fs::write(d.join("f.txt"), "two\n").unwrap();
        fs::write(d.join("untracked.txt"), "x\n").unwrap();
        let dirty = dirty_tracked(&d);
        assert_eq!(dirty.len(), 1, "{dirty:?}");
        assert!(dirty[0].contains("f.txt"));
        let head = GitTree {
            root: d.clone(),
            rev: "HEAD".into(),
        };
        assert_eq!(
            head.read("f.txt").as_deref(),
            Some("one\n"),
            "HEAD, not the working tree"
        );
        assert_eq!(FsTree(d.clone()).read("f.txt").as_deref(), Some("two\n"));
        let _ = fs::remove_dir_all(&d);
    }

    fn gx(d: &Path, args: &[&str]) -> bool {
        Command::new("git")
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(d)
            .output()
            .unwrap()
            .status
            .success()
    }

    fn rules_unit() -> (Unit, Vec<String>) {
        (
            Unit {
                name: "shipped/x/rules".into(),
                group: "g".into(),
                kind: "shipped".into(),
                target: "crates/x/src/rules.rs".into(),
                inputs: vec![],
            },
            vec!["crates/x/src/rules.rs".to_string()],
        )
    }

    fn merge_repo(tag: &str) -> (PathBuf, String) {
        let d = tmp_repo(tag);
        g(&d, &["init", "-q", "-b", "main"]);
        fs::create_dir_all(d.join("crates/x/src")).unwrap();
        fs::write(d.join("crates/x/src/rules.rs"), RULES_RS).unwrap();
        let base = commit(&d, "base", Some("Claude Opus 5.5"));
        (d, base)
    }

    #[test]
    fn a_builder_rewriting_a_proof_line_in_a_conflict_resolution_is_the_merge_commits_authorship() {
        let (d, base) = merge_repo("evilmerge");
        let (unit, files) = rules_unit();
        g(&d, &["checkout", "-q", "-b", "side"]);
        fs::write(
            d.join("crates/x/src/rules.rs"),
            RULES_RS.replace("invariant i <= n", "invariant i <= n + 1"),
        )
        .unwrap();
        commit(&d, "side proof", Some("Claude Opus 5.5"));
        g(&d, &["checkout", "-q", "main"]);
        fs::write(
            d.join("crates/x/src/rules.rs"),
            RULES_RS.replace("invariant i <= n", "invariant i <= n + 2"),
        )
        .unwrap();
        commit(&d, "main proof", Some("Claude Opus 5.5"));
        assert!(
            !gx(&d, &["merge", "--no-edit", "side"]),
            "the two proof edits conflict"
        );
        // the builder resolves the conflict with a proof line of its own
        fs::write(
            d.join("crates/x/src/rules.rs"),
            RULES_RS.replace("invariant i <= n", "invariant i <= n + 3"),
        )
        .unwrap();
        g(&d, &["add", "-A"]);
        g(
            &d,
            &[
                "commit",
                "-q",
                "-m",
                "Merge side",
                "-m",
                "Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>",
            ],
        );
        let merge = g(&d, &["rev-parse", "HEAD"])[..10].to_string();
        let i = change_info(
            &d,
            &base,
            &unit.name,
            std::slice::from_ref(&unit),
            &files,
            None,
        );
        let by_merge: Vec<&(String, Vec<String>)> =
            i.proof.iter().filter(|(c, _)| *c == merge).collect();
        assert_eq!(
            by_merge.len(),
            1,
            "the merge commit is listed as authoring: {:?}",
            i.proof
        );
        assert_eq!(by_merge[0].1, vec!["claude-sonnet-5-5".to_string()]);
        assert_eq!(
            i.proof.len(),
            3,
            "two Opus commits and the merge: {:?}",
            i.proof
        );
        // and the check refuses it although an Opus record exists
        let mut w = a();
        w.proof = "2222222222222222".into();
        let mut r = rec("proof-edit", "verus/u", &w);
        r.reviewer_model = "-".into();
        r.reviewer_family = "-".into();
        let mut sc = scn(Some(a()), w, vec![r]);
        sc.proof_commits = i.proof.clone();
        let o = run(&sc);
        assert!(
            rules(&o).contains(&"proof-or-statement-written-by-a-builder"),
            "{:?}",
            o.refusals
        );
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn an_evil_merge_without_conflict_and_a_clean_merge_are_told_apart() {
        // clean merge: proof edit on main, exec edit on side; the merge commit adds nothing by hand
        let (d, base) = merge_repo("cleanmerge");
        let (unit, files) = rules_unit();
        g(&d, &["checkout", "-q", "-b", "side"]);
        fs::write(
            d.join("crates/x/src/rules.rs"),
            RULES_RS.replace("i += 1;", "i += 1; let _z = 0;"),
        )
        .unwrap();
        commit(&d, "side exec", Some("Claude Sonnet 5.5"));
        g(&d, &["checkout", "-q", "main"]);
        fs::write(
            d.join("crates/x/src/rules.rs"),
            RULES_RS.replace("invariant i <= n", "invariant i <= n + 1"),
        )
        .unwrap();
        commit(&d, "main proof", Some("Claude Opus 5.5"));
        assert!(gx(&d, &["merge", "--no-edit", "side"]));
        let i = change_info(
            &d,
            &base,
            &unit.name,
            std::slice::from_ref(&unit),
            &files,
            None,
        );
        assert_eq!(i.proof.len(), 1, "only the Opus commit: {:?}", i.proof);
        assert_eq!(i.proof[0].1, vec!["claude-opus-5-5".to_string()]);
        assert_eq!(i.exec.len(), 1, "only the side commit: {:?}", i.exec);
        // evil merge: the builder amends the automatic merge with a proof line
        let cur = fs::read_to_string(d.join("crates/x/src/rules.rs")).unwrap();
        fs::write(
            d.join("crates/x/src/rules.rs"),
            cur.replace("decreases n - i", "decreases n - i + 0"),
        )
        .unwrap();
        g(&d, &["add", "-A"]);
        g(
            &d,
            &[
                "commit",
                "-q",
                "--amend",
                "-m",
                "Merge side",
                "-m",
                "Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>",
            ],
        );
        let i = change_info(
            &d,
            &base,
            &unit.name,
            std::slice::from_ref(&unit),
            &files,
            None,
        );
        assert_eq!(
            i.proof.len(),
            2,
            "the amended merge is authoring: {:?}",
            i.proof
        );
        assert!(
            i.proof
                .iter()
                .any(|(_, t)| t == &vec!["claude-sonnet-5-5".to_string()])
        );
        let _ = fs::remove_dir_all(&d);
    }

    // ---- a commit that creates the unit's file is classified by content

    fn creation_repo(tag: &str) -> (PathBuf, String) {
        let d = tmp_repo(tag);
        g(&d, &["init", "-q", "-b", "main"]);
        fs::create_dir_all(d.join("crates/x/src")).unwrap();
        fs::write(d.join("README"), "x\n").unwrap();
        let base = commit(&d, "base", Some("Claude Opus 5.5"));
        (d, base)
    }

    fn put(d: &Path, src: &str) {
        fs::write(d.join("crates/x/src/rules.rs"), src).unwrap();
    }

    fn info_of(d: &Path, base: &str) -> ChangeInfo {
        let (unit, files) = rules_unit();
        change_info(
            d,
            base,
            &unit.name,
            std::slice::from_ref(&unit),
            &files,
            None,
        )
    }

    const SON_T: &str = "claude-sonnet-5-5";
    const OPUS_T: &str = "claude-opus-5-5";

    #[test]
    fn a_builder_creating_a_file_with_code_only_is_a_code_commit_and_opus_clauses_are_the_proof_side()
     {
        let (d, base) = creation_repo("create-a");
        put(&d, SBARE);
        commit(&d, "builder creates", Some("Claude Sonnet 5.5"));
        put(&d, SCLAUSES);
        commit(&d, "opus clauses", Some("Claude Opus 5.5"));
        let i = info_of(&d, &base);
        assert_eq!(i.exec, vec![vec![SON_T.to_string()]], "{i:?}");
        assert_eq!(i.proof.len(), 1, "{i:?}");
        assert_eq!(i.proof[0].1, vec![OPUS_T.to_string()]);
        // the whole check passes on a new unit: builder code, Opus statement record
        let mut w = a();
        w.stmt = "1111111111111111".into();
        let mut sc = scn(None, w.clone(), vec![rec("statement", "verus/u", &w)]);
        sc.exec_override = Some(i.exec.clone());
        sc.proof_commits = i.proof.clone();
        let o = run(&sc);
        assert!(o.refusals.is_empty(), "{:?}", o.refusals);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_builder_creating_a_file_with_clauses_or_proof_text_is_refused() {
        for (tag, src) in [
            ("create-b1", SCLAUSES.to_string()),
            (
                "create-b2",
                format!("{SBARE}#[cfg(kani)]\nfn harness() {{}}\n"),
            ),
            (
                "create-b3",
                format!("{SBARE}#[cfg(any())]\nfn gated() {{}}\n"),
            ),
        ] {
            let (d, base) = creation_repo(tag);
            put(&d, &src);
            commit(&d, "builder creates", Some("Claude Sonnet 5.5"));
            let i = info_of(&d, &base);
            assert_eq!(i.proof.len(), 1, "{tag}: {i:?}");
            assert_eq!(i.proof[0].1, vec![SON_T.to_string()]);
            let mut w = a();
            w.stmt = "1111111111111111".into();
            let mut sc = scn(None, w.clone(), vec![rec("statement", "verus/u", &w)]);
            sc.exec_override = Some(i.exec.clone());
            sc.proof_commits = i.proof.clone();
            assert!(
                rules(&run(&sc)).contains(&"proof-or-statement-written-by-a-builder"),
                "{tag}"
            );
            let _ = fs::remove_dir_all(&d);
        }
    }

    #[test]
    fn a_test_gated_item_in_a_created_file_is_code_not_proof() {
        let (d, base) = creation_repo("create-test-cfg");
        put(
            &d,
            &format!("{SBARE}#[cfg(test)]\nmod t {{\n    #[test]\n    fn k() {{}}\n}}\n"),
        );
        commit(&d, "builder creates", Some("Claude Sonnet 5.5"));
        let i = info_of(&d, &base);
        assert!(i.proof.is_empty(), "{i:?}");
        assert_eq!(i.exec.len(), 1);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn opus_creating_code_and_later_adding_clauses_is_one_model() {
        let (d, base) = creation_repo("create-c");
        put(&d, SBARE);
        commit(&d, "opus creates", Some("Claude Opus 5.5"));
        put(&d, SCLAUSES);
        commit(&d, "opus clauses", Some("Claude Opus 5.5"));
        let i = info_of(&d, &base);
        assert_eq!(i.exec, vec![vec![OPUS_T.to_string()]], "{i:?}");
        assert_eq!(i.proof.len(), 1);
        let mut w = a();
        w.stmt = "1111111111111111".into();
        // Opus wrote the code and the clauses: a record that names Opus as the implementer fails (prover = implementer) ...
        let mut r = rec("statement", "verus/u", &w);
        r.impl_model = OPUS.into();
        let mut sc = scn(None, w.clone(), vec![r]);
        sc.exec_override = Some(i.exec.clone());
        sc.proof_commits = i.proof.clone();
        let o = run(&sc);
        assert_eq!(rules(&o), ["statement-needs-strong-prover-and-reviewer"]);
        assert!(
            o.refusals[0].detail.contains("same model"),
            "{:?}",
            o.refusals
        );
        // ... and one that names the builder is refused too: Opus is in the trailers of the code AND the clauses
        let mut sc = scn(None, w.clone(), vec![rec("statement", "verus/u", &w)]);
        sc.exec_override = Some(i.exec.clone());
        sc.proof_commits = i.proof.clone();
        assert_eq!(rules(&run(&sc)), ["one-model-wrote-code-and-proof"]);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn an_opus_commit_creating_code_and_clauses_together_counts_both_as_today() {
        let (d, base) = creation_repo("create-d");
        put(&d, SCLAUSES);
        commit(&d, "opus creates all", Some("Claude Opus 5.5"));
        put(&d, &format!("{SCLAUSES}pub fn h() {{}}\n"));
        commit(&d, "builder adds a bare fn", Some("Claude Sonnet 5.5"));
        let i = info_of(&d, &base);
        assert_eq!(
            i.exec,
            vec![vec![OPUS_T.to_string()], vec![SON_T.to_string()]],
            "{i:?}"
        );
        assert_eq!(i.proof.len(), 1);
        assert_eq!(i.proof[0].1, vec![OPUS_T.to_string()]);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_file_created_empty_is_no_authorship_and_the_fill_in_is_classified_normally() {
        let (d, base) = creation_repo("create-empty");
        put(&d, "");
        commit(
            &d,
            "builder creates an empty file",
            Some("Claude Sonnet 5.5"),
        );
        let i = info_of(&d, &base);
        assert!(i.exec.is_empty() && i.proof.is_empty(), "{i:?}");
        put(&d, SBARE);
        commit(&d, "builder fills in", Some("Claude Sonnet 5.5"));
        put(&d, SCLAUSES);
        commit(&d, "opus clauses", Some("Claude Opus 5.5"));
        let i = info_of(&d, &base);
        assert_eq!(i.exec, vec![vec![SON_T.to_string()]], "{i:?}");
        assert_eq!(i.proof.len(), 1);
        assert_eq!(i.proof[0].1, vec![OPUS_T.to_string()]);
        let _ = fs::remove_dir_all(&d);
    }

    /// As production calls it: the unit alone as the peer, the set of files that were inside any unit at the base given.
    fn info_of_two(d: &Path, base: &str) -> ChangeInfo {
        let (unit, files) = rules_unit();
        let owned: BTreeSet<String> = ["crates/x/src/old.rs".to_string()].into();
        change_info_owned(
            d,
            base,
            &unit.name,
            std::slice::from_ref(&unit),
            &files,
            None,
            Some(&owned),
        )
    }

    #[test]
    fn a_file_copied_from_a_unit_file_of_the_base_is_compared_with_its_source() {
        // the source is a file of another unit at the base, unchanged since: a copy of it is no authorship
        let (d, _) = creation_repo("create-copy");
        fs::write(d.join("crates/x/src/old.rs"), SCLAUSES).unwrap();
        let base = commit(&d, "opus source", Some("Claude Opus 5.5"));
        put(&d, SCLAUSES);
        commit(&d, "builder copies", Some("Claude Sonnet 5.5"));
        let i = info_of_two(&d, &base);
        assert!(i.exec.is_empty() && i.proof.is_empty(), "copy: {i:?}");
        // a code change on top is a code change only
        put(&d, &SCLAUSES.replace("n + 1\n}", "n + 2\n}"));
        commit(&d, "builder edits", Some("Claude Sonnet 5.5"));
        let i = info_of_two(&d, &base);
        assert_eq!(i.exec.len(), 1, "{i:?}");
        assert!(i.proof.is_empty(), "{i:?}");
        let _ = fs::remove_dir_all(&d);
        // a copy with a changed clause is statement authoring
        let (d, _) = creation_repo("create-copy2");
        fs::write(d.join("crates/x/src/old.rs"), SCLAUSES).unwrap();
        let base = commit(&d, "opus source", Some("Claude Opus 5.5"));
        put(&d, &SCLAUSES.replace("n < 10", "n < 11"));
        commit(
            &d,
            "builder copies and edits a clause",
            Some("Claude Sonnet 5.5"),
        );
        let i = info_of_two(&d, &base);
        assert_eq!(i.proof.len(), 1, "{i:?}");
        assert_eq!(i.proof[0].1, vec![SON_T.to_string()]);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn staging_clauses_outside_every_unit_and_moving_them_in_is_refused() {
        // the builder stages a clause-bearing file outside every unit, then renames it into the unit's path
        let (d, base) = creation_repo("create-stage");
        fs::create_dir_all(d.join("docs/notes")).unwrap();
        fs::write(d.join("docs/notes/x.rs"), SCLAUSES).unwrap();
        commit(&d, "builder stages", Some("Claude Sonnet 5.5"));
        g(&d, &["mv", "docs/notes/x.rs", "crates/x/src/rules.rs"]);
        commit(&d, "builder moves it in", Some("Claude Sonnet 5.5"));
        let i = info_of(&d, &base);
        assert_eq!(i.proof.len(), 1, "rename: {i:?}");
        assert_eq!(i.proof[0].1, vec![SON_T.to_string()]);
        let _ = fs::remove_dir_all(&d);
        // the same by copy
        let (d, base) = creation_repo("create-stage-copy");
        fs::create_dir_all(d.join("docs/notes")).unwrap();
        fs::write(d.join("docs/notes/x.rs"), SCLAUSES).unwrap();
        commit(&d, "builder stages", Some("Claude Sonnet 5.5"));
        put(&d, SCLAUSES);
        commit(&d, "builder copies it in", Some("Claude Sonnet 5.5"));
        let i = info_of(&d, &base);
        assert_eq!(i.proof.len(), 1, "copy: {i:?}");
        let _ = fs::remove_dir_all(&d);
        // a source that was a unit file at the base but changed since: not trusted either
        let (d, _) = creation_repo("create-stage-changed");
        fs::write(d.join("crates/x/src/old.rs"), SBARE).unwrap();
        let base = commit(&d, "base source", Some("Claude Opus 5.5"));
        fs::write(d.join("crates/x/src/old.rs"), SCLAUSES).unwrap();
        commit(
            &d,
            "builder adds clauses to the other unit",
            Some("Claude Sonnet 5.5"),
        );
        put(&d, SCLAUSES);
        commit(&d, "builder copies it", Some("Claude Sonnet 5.5"));
        let i = info_of_two(&d, &base);
        assert_eq!(i.proof.len(), 1, "changed source: {i:?}");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_merge_that_brings_in_a_created_file_adds_no_authorship_of_its_own() {
        let (d, base) = creation_repo("create-merge");
        g(&d, &["checkout", "-q", "-b", "side"]);
        put(&d, SBARE);
        commit(&d, "side builder creates", Some("Claude Sonnet 5.5"));
        g(&d, &["checkout", "-q", "main"]);
        fs::write(d.join("README"), "y\n").unwrap();
        commit(&d, "main moves on", Some("Claude Opus 5.5"));
        assert!(gx(&d, &["merge", "--no-edit", "side"]));
        let i = info_of(&d, &base);
        assert_eq!(i.exec, vec![vec![SON_T.to_string()]], "{i:?}");
        assert!(i.proof.is_empty(), "{i:?}");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_unit_that_cannot_be_fingerprinted_before_is_not_a_creation() {
        // the file exists before but does not parse: nothing is assumed, both sides count
        let (d, base) = creation_repo("create-broken");
        put(&d, "pub fn f() { let _ = verus_spec; }\n");
        commit(&d, "broken", Some("Claude Opus 5.5"));
        put(&d, SBARE);
        commit(&d, "builder fixes", Some("Claude Sonnet 5.5"));
        let i = info_of(&d, &base);
        assert!(
            i.proof.iter().any(|(_, t)| t == &vec![SON_T.to_string()]),
            "{i:?}"
        );
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn verification_words_in_a_macro_rules_body_are_proof_text() {
        let plain = classify(
            "c/src/m.rs",
            "macro_rules! m { ($x:expr) => { $x + 1 }; }\npub fn g() {}\n",
        );
        let proofy = classify(
            "c/src/m.rs",
            "macro_rules! m { ($x:expr) => { proof { assert($x > 0); } }; }\npub fn g() {}\n",
        );
        assert!(plain.proof_text.is_empty(), "{}", plain.proof_text);
        let rt = classify(
            "c/src/m.rs",
            "macro_rules! m { ($x:expr) => { assert!($x > 0); assert_eq!($x, 1); }; }\npub fn g() {}\n",
        );
        assert!(
            rt.proof_text.is_empty(),
            "runtime assert! is code: {}",
            rt.proof_text
        );
        assert!(
            proofy.proof_text.contains("assert"),
            "{}",
            proofy.proof_text
        );
        assert!(!proofy.exec_text.contains("assert"), "{}", proofy.exec_text);
        assert!(proofy.exec_text.contains("g"), "{}", proofy.exec_text);
    }

    // ---- the prover must have written the proof, and no model writes both sides

    fn fable_record(w: &Fp) -> Record {
        let mut r = rec("statement", "verus/u", w);
        r.impl_model = OPUS.into();
        r.prover_model = "claude-fable-5-2".into();
        r.reviewer_model = "gpt-6-1-sol".into();
        r
    }

    #[test]
    fn a_prover_who_only_signs_another_models_proof_is_refused() {
        let mut w = a();
        w.stmt = "1111111111111111".into();
        w.exec = "dddddddddddddddd".into();
        // Opus wrote the code and the clauses; Fable's commit only adds the record line
        let mut s = scn(None, w.clone(), vec![fable_record(&w)]);
        s.exec_override = Some(vec![vec![OPUS.into()]]);
        s.proof_commits = vec![("cccccccccc".into(), vec![OPUS.into()])];
        s.trailer = vec!["claude-fable-5-2".into()];
        assert_eq!(rules(&run(&s)), ["one-model-wrote-code-and-proof"]);
        // Opus code, but the clauses are by a builder-free split: Fable wrote them: fine
        s.proof_commits = vec![("cccccccccc".into(), vec!["claude-fable-5-2".into()])];
        assert!(run(&s).refusals.is_empty(), "{:?}", run(&s).refusals);
        // a builder wrote the code, Opus the clauses, Fable only signs: the prover wrote nothing
        let mut r = fable_record(&w);
        r.impl_model = SON.into();
        let mut s = scn(None, w.clone(), vec![r]);
        s.exec_override = Some(vec![vec![SON.into()]]);
        s.proof_commits = vec![("cccccccccc".into(), vec![OPUS.into()])];
        s.trailer = vec!["claude-fable-5-2".into()];
        assert_eq!(rules(&run(&s)), ["prover-wrote-no-statement-or-proof"]);
        // the legit cases: builder code and the Opus prover's clauses; an Opus proof edit with no code change
        let mut s = scn(None, w.clone(), vec![rec("statement", "verus/u", &w)]);
        s.exec_override = Some(vec![vec![SON.into()]]);
        s.proof_commits = vec![("cccccccccc".into(), vec![OPUS.into()])];
        assert!(run(&s).refusals.is_empty(), "{:?}", run(&s).refusals);
        let mut w2 = a();
        w2.proof = "2222222222222222".into();
        let mut r = rec("proof-edit", "verus/u", &w2);
        r.reviewer_model = "-".into();
        r.reviewer_family = "-".into();
        let mut s = scn(Some(a()), w2, vec![r]);
        s.proof_commits = vec![("cccccccccc".into(), vec![OPUS.into()])];
        assert!(run(&s).refusals.is_empty(), "{:?}", run(&s).refusals);
        // a pre-rule unit is judged by its review instead
        let mut w3 = a();
        w3.stmt = "1111111111111111".into();
        let mut s = scn(None, w3.clone(), vec![pre(&w3)]);
        s.cutoff = Some(CUT.into());
        s.exec_override = Some(vec![vec![OPUS.into()]]);
        s.bare_proof = true;
        assert!(run(&s).refusals.is_empty(), "{:?}", run(&s).refusals);
    }

    // ---- the lucina-oom shape: a pre-rule unit that a Fable worker then strengthens

    const FABLE: &str = "claude-fable-5-2";

    /// A repository where Opus wrote the code and the clauses up to X, then Fable strengthened the clauses and a builder added code.
    fn oom_repo(
        tag: &str,
        post_clause_trailer: &str,
        post_code_trailer: &str,
    ) -> (PathBuf, String, String) {
        let (d, base) = creation_repo(tag);
        put(&d, SCLAUSES);
        let x = commit(&d, "opus code and clauses (X)", Some("Claude Opus 5.5"));
        put(&d, &SCLAUSES.replace("n < 10", "n < 9"));
        commit(&d, "post-X clauses", Some(post_clause_trailer));
        put(
            &d,
            &format!(
                "{}pub fn extra() {{}}\n",
                SCLAUSES.replace("n < 10", "n < 9")
            ),
        );
        commit(&d, "post-X code", Some(post_code_trailer));
        (d, base, x)
    }

    fn oom_scn(i: &ChangeInfo, post_code_changed: bool) -> Scn {
        let mut w = a();
        w.stmt = "1111111111111111".into();
        let mut stmt = fable_record(&w);
        stmt.id = "r-stmt-fable".into();
        stmt.verdict_ref = "docs/ci/verdicts/r-stmt-fable.md".into();
        let mut old = a();
        old.stmt = "0000000000000000".into();
        let mut p = pre(&old);
        p.id = "r-pre-rule".into();
        p.verdict_ref = "docs/ci/verdicts/r-pre-rule.md".into();
        let mut s = scn(None, w, vec![p, stmt]);
        s.cutoff = Some(CUT.into());
        s.trailer = vec![OPUS.into(), FABLE.into()];
        // the walk from the base would also list the exempt Opus commit: only the commits after X count
        s.exec_override = Some(vec![vec![OPUS.into()]]);
        s.proof_commits = vec![("0000000000".into(), vec![OPUS.into()])];
        s.after_exec = Some(if post_code_changed {
            i.exec.clone()
        } else {
            vec![]
        });
        s.after_proof = i.proof.clone();
        s
    }

    #[test]
    fn a_fable_worker_strengthening_a_pre_rule_unit_passes_and_the_variants_are_refused() {
        let (unit, files) = rules_unit();
        let after = |d: &Path, base: &str, x: &str| {
            change_info(
                d,
                base,
                &unit.name,
                std::slice::from_ref(&unit),
                &files,
                Some(x),
            )
        };
        // legit: Opus (exempt, up to X), Fable clauses, Sonnet code
        let (d, base, x) = oom_repo("oom-ok", "Claude Fable 5.2", "Claude Sonnet 5.5");
        let i = after(&d, &base, &x);
        assert_eq!(i.proof.len(), 1, "{i:?}");
        assert_eq!(i.proof[0].1, vec![FABLE.to_string()]);
        assert_eq!(i.exec, vec![vec![SON_T.to_string()]], "{i:?}");
        let o = run(&oom_scn(&i, true));
        assert!(o.refusals.is_empty(), "{:?}", o.refusals);
        assert!(
            o.notes
                .iter()
                .any(|n| n.contains("PRE-RULE record `r-pre-rule` still exempts")),
            "{:?}",
            o.notes
        );
        let _ = fs::remove_dir_all(&d);

        // a Sonnet trailer on the post-X clause commit
        let (d, base, x) = oom_repo("oom-son", "Claude Sonnet 5.5", "Claude Sonnet 5.5");
        let i = after(&d, &base, &x);
        let o = run(&oom_scn(&i, true));
        assert!(
            rules(&o).contains(&"proof-or-statement-written-by-a-builder"),
            "{:?}",
            o.refusals
        );
        let _ = fs::remove_dir_all(&d);

        // an Opus trailer on the post-X clause commit: Opus proving its own (exempt) code
        let (d, base, x) = oom_repo("oom-opus", "Claude Opus 5.5", "Claude Sonnet 5.5");
        let i = after(&d, &base, &x);
        let o = run(&oom_scn(&i, true));
        assert!(
            rules(&o).contains(&"one-model-wrote-code-and-proof")
                || rules(&o).contains(&"prover-wrote-no-statement-or-proof"),
            "{:?}",
            o.refusals
        );
        assert!(!o.refusals.is_empty());
        // ... even when no code changed after X
        let o = run(&oom_scn(&i, false));
        assert!(
            rules(&o).contains(&"one-model-wrote-code-and-proof"),
            "{:?}",
            o.refusals
        );
        let _ = fs::remove_dir_all(&d);

        // the record names Fable, but only a Sonnet commit changed the clauses
        let mut s = oom_scn(&ChangeInfo::default(), true);
        s.after_proof = vec![("cccccccccc".into(), vec![SON.into()])];
        s.after_exec = Some(vec![]);
        assert!(!run(&s).refusals.is_empty());
        // no post-X statement commit at all: the prover wrote nothing
        let mut s = oom_scn(&ChangeInfo::default(), false);
        s.after_proof = vec![];
        assert!(rules(&run(&s)).contains(&"prover-wrote-no-statement-or-proof"));

        // a post-X Opus code change with Fable clauses, the record naming Fable as implementer (and prover)
        let mut s = oom_scn(&ChangeInfo::default(), true);
        s.after_exec = Some(vec![vec![OPUS.into()]]);
        s.after_proof = vec![("cccccccccc".into(), vec![FABLE.into()])];
        s.records[1].impl_model = FABLE.into();
        assert!(!run(&s).refusals.is_empty(), "implementer = prover");
        // the same code change with the record naming Opus as implementer: Opus code, Fable proof: fine
        let mut s = oom_scn(&ChangeInfo::default(), true);
        s.after_exec = Some(vec![vec![OPUS.into()]]);
        s.after_proof = vec![("cccccccccc".into(), vec![FABLE.into()])];
        let o = run(&s);
        assert!(o.refusals.is_empty(), "{:?}", o.refusals);
        // the implementer must be Opus (the pre-rule record's) or a post-X code author: Sonnet code, record names Qoder
        let mut s = oom_scn(&ChangeInfo::default(), true);
        s.after_exec = Some(vec![vec![SON.into()]]);
        s.after_proof = vec![("cccccccccc".into(), vec![FABLE.into()])];
        s.records[1].impl_model = "qoder".into();
        s.records[1].impl_family = "qoder".into();
        assert!(
            rules(&run(&s)).contains(&"implementer-not-in-trailers"),
            "{:?}",
            run(&s).refusals
        );
    }

    #[test]
    fn a_pre_rule_record_does_not_excuse_one_model_writing_code_and_proof_after_x() {
        let mut w = a();
        w.stmt = "1111111111111111".into();
        // after X: Opus commits code, then Opus commits proof; a fresh pre-rule record names Fable
        let mut r = pre(&w);
        r.prover_model = FABLE.into();
        r.impl_model = OPUS.into();
        let mk = |r: Record| {
            let mut s = scn(None, w.clone(), vec![r]);
            s.cutoff = Some(CUT.into());
            s.trailer = vec![FABLE.into()];
            s
        };
        let mut s = mk(r.clone());
        s.after_exec = Some(vec![vec![OPUS.into()]]);
        s.after_proof = vec![("cccccccccc".into(), vec![OPUS.into()])];
        assert!(
            rules(&run(&s)).contains(&"one-model-wrote-code-and-proof"),
            "{:?}",
            run(&s).refusals
        );
        // the legit shape: everything before X, nothing after
        let s = mk(r.clone());
        assert!(run(&s).refusals.is_empty(), "{:?}", run(&s).refusals);
        // builder-only code after X
        let mut s = mk(r.clone());
        s.after_exec = Some(vec![vec![SON.into()]]);
        assert!(run(&s).refusals.is_empty(), "{:?}", run(&s).refusals);
    }

    // ---- third review round

    #[test]
    fn pre_rule_only_for_listed_units_with_the_fixed_started_before() {
        let mut w = a();
        w.stmt = "1111111111111111".into();
        // listed unit with the listed hash: accepted
        let mut s = scn(None, w.clone(), vec![pre(&w)]);
        s.cutoff = Some(CUT.into());
        assert!(run(&s).refusals.is_empty(), "{:?}", run(&s).refusals);
        // forged started_before (an ancient commit)
        let mut r = pre(&w);
        r.started_before = "0000000000000000000000000000000000000001".into();
        let mut s = scn(None, w.clone(), vec![r]);
        s.cutoff = Some(CUT.into());
        let o = run(&s);
        assert!(
            o.refusals[0].detail.contains("differs from the"),
            "{}",
            o.refusals[0].detail
        );
        // a unit that is not in the list
        let mut s = scn(None, w.clone(), vec![pre(&w)]);
        s.cutoff = Some(
            "cutoff 0123456789abcdef0123456789abcdef01234567\nverus/other\tabcdef0123456789abcdef0123456789abcdef01\tx\n".into(),
        );
        let o = run(&s);
        assert!(
            o.refusals[0].detail.contains("is not listed in"),
            "{}",
            o.refusals[0].detail
        );
        // a listed started_before is NOT required to be an ancestor of the cutoff (the check never asks git for ancestry now); a
        // missing object is refused
        let mut s = scn(None, w.clone(), vec![pre(&w)]);
        s.cutoff = Some(CUT.into());
        s.exists_ok = false;
        assert!(
            run(&s).refusals[0]
                .detail
                .contains("does not exist in this repository's object database")
        );
        // a malformed list closes the kind
        let mut s = scn(None, w.clone(), vec![pre(&w)]);
        s.cutoff = Some("not a cutoff\n".into());
        assert!(
            run(&s).refusals[0]
                .detail
                .contains("not `cutoff <full 40-digit")
        );
    }

    #[test]
    fn a_branch_that_creates_or_changes_the_cutoff_file_is_refused() {
        let mut w = a();
        w.stmt = "1111111111111111".into();
        // the branch creates the list and a pre-rule record for its own unit
        let mut s = scn(None, w.clone(), vec![pre(&w)]);
        s.cutoff = Some(CUT.into());
        s.cutoff_changed = vec!["docs/ci/proof-roles-cutoff.txt".into()];
        let o = run(&s);
        assert!(rules(&o).contains(&"cutoff-protected"), "{:?}", o.refusals);
        // a sibling file of the protected family counts too
        assert!(is_cutoff_path("docs/ci/proof-roles-cutoff-kexec.txt"));
        assert!(is_cutoff_path("docs/ci/proof-roles-cutoff.txt"));
        assert!(!is_cutoff_path("docs/ci/proof-roles.tsv"));
        // the coordinator's allow token lets it land
        s.allow_cutoff = true;
        assert!(!rules(&run(&s)).contains(&"cutoff-protected"));
    }

    #[test]
    fn allow_tokens_are_explicit_not_substrings() {
        assert!(allow_token("extractor: new lexer", "extractor"));
        assert!(allow_token("amend:x extractor: lexer fix", "extractor"));
        assert!(allow_token("cutoff:kexec list;extractor:y", "cutoff"));
        assert!(!allow_token("amend:x (no extractor change)", "extractor"));
        assert!(
            !allow_token("extractor:", "extractor"),
            "a reason is required"
        );
        assert!(!allow_token("my-extractor:why", "extractor"));
        assert!(!allow_token("", "cutoff"));
    }

    #[test]
    fn all_of_the_ci_tool_and_the_unit_lists_are_extractor_sources() {
        for p in [
            "tools/ci/lucina-ci/src/spec.rs",
            "tools/ci/lucina-ci/src/coverage.rs",
            "tools/ci/lucina-ci/src/main.rs",
            "tools/ci/lucina-ci/Cargo.toml",
            "tools/ci/lucina-ci/Cargo.lock",
            "tools/ci/proof-inventory.tsv",
            "tools/ci/proof-groups.tsv",
        ] {
            assert!(is_extractor_path(p), "{p}");
        }
        assert!(!is_extractor_path("tools/ci/statement-counts.tsv"));
        assert!(!is_extractor_path("kernel/lucina-ring/src/lib.rs"));
        let mut w = a();
        w.exec = "dddddddddddddddd".into();
        let mut s = scn(Some(a()), w, vec![]);
        s.extractor_changed = vec!["tools/ci/lucina-ci/src/coverage.rs".into()];
        s.unit_files_changed = vec!["crates/x/src/lib.rs".into()];
        assert_eq!(rules(&run(&s)), ["extractor-changed-with-unit-files"]);
    }

    #[test]
    fn a_test_gate_on_an_inline_mod_or_impl_holding_verified_items_is_proof_text() {
        let hidden = "verus! {\n#[cfg(test)]\nmod hidden {\n    pub proof fn lemma(x: u32) ensures x == x {}\n}\n}\n";
        let plain =
            "verus! {\nmod hidden {\n    pub proof fn lemma(x: u32) ensures x == x {}\n}\n}\n";
        let h = classify("v/walk.rs", hidden);
        let p = classify("v/walk.rs", plain);
        assert!(h.errors.is_empty(), "{:?}", h.errors);
        assert_ne!(
            h.proof_text, p.proof_text,
            "the cfg(test) line must be proof text"
        );
        assert!(h.proof_text.contains("cfg(test)"), "{}", h.proof_text);
        // the same on an impl
        let imp = classify(
            "v/walk.rs",
            "verus! {\n#[cfg(test)]\nimpl X {\n    pub proof fn l(&self) ensures true {}\n}\n}\n",
        );
        assert!(imp.proof_text.contains("cfg(test)"), "{}", imp.proof_text);
        // a test module without verified items stays exec
        let t = classify(
            "v/a.rs",
            "#[cfg(test)]\nmod tests {\n    #[test] fn t() { assert_eq!(1, 1); }\n}\npub fn f() {}\n",
        );
        assert!(!t.proof_text.contains("cfg(test)"), "{}", t.proof_text);
        assert!(t.exec_text.contains("cfg(test)"));
    }

    #[test]
    fn a_merge_resolved_to_the_branch_side_or_with_ours_is_the_merge_commits_authorship() {
        // main strengthens an invariant (Opus); the builder's branch changes exec only, then merges main with `-s ours`
        let (d, base) = merge_repo("ours");
        let (unit, files) = rules_unit();
        g(&d, &["checkout", "-q", "-b", "feat"]);
        fs::write(
            d.join("crates/x/src/rules.rs"),
            RULES_RS.replace("i += 1;", "i += 1; let _z = 0;"),
        )
        .unwrap();
        commit(&d, "feat exec", Some("Claude Sonnet 5.5"));
        g(&d, &["checkout", "-q", "main"]);
        fs::write(
            d.join("crates/x/src/rules.rs"),
            RULES_RS.replace("invariant i <= n", "invariant i <= n + 1"),
        )
        .unwrap();
        commit(&d, "main proof", Some("Claude Opus 5.5"));
        g(&d, &["checkout", "-q", "feat"]);
        g(
            &d,
            &[
                "merge",
                "-q",
                "-s",
                "ours",
                "main",
                "-m",
                "Merge main",
                "-m",
                "Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>",
            ],
        );
        let merge = g(&d, &["rev-parse", "HEAD"])[..10].to_string();
        // the unit's files on HEAD are TREESAME to the first parent: a path-limited walk would not list this merge
        let i = change_info(
            &d,
            &base,
            &unit.name,
            std::slice::from_ref(&unit),
            &files,
            None,
        );
        assert!(
            i.proof
                .iter()
                .any(|(c, t)| *c == merge && t == &vec!["claude-sonnet-5-5".to_string()]),
            "the merge that dropped main's proof change is blamed on its trailer: {:?}",
            i.proof
        );
        let _ = fs::remove_dir_all(&d);

        // a conflict resolved to the branch side
        let (d, base) = merge_repo("theirs");
        g(&d, &["checkout", "-q", "-b", "feat"]);
        fs::write(
            d.join("crates/x/src/rules.rs"),
            RULES_RS.replace("invariant i <= n", "invariant i <= n + 5"),
        )
        .unwrap();
        commit(&d, "feat proof", Some("Claude Opus 5.5"));
        g(&d, &["checkout", "-q", "main"]);
        fs::write(
            d.join("crates/x/src/rules.rs"),
            RULES_RS.replace("invariant i <= n", "invariant i <= n + 6"),
        )
        .unwrap();
        commit(&d, "main proof", Some("Claude Opus 5.5"));
        g(&d, &["checkout", "-q", "feat"]);
        assert!(!gx(&d, &["merge", "--no-edit", "main"]));
        g(&d, &["checkout", "--ours", "crates/x/src/rules.rs"]);
        g(&d, &["add", "-A"]);
        g(
            &d,
            &[
                "commit",
                "-q",
                "-m",
                "Merge main",
                "-m",
                "Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>",
            ],
        );
        let merge = g(&d, &["rev-parse", "HEAD"])[..10].to_string();
        let i = change_info(
            &d,
            &base,
            &unit.name,
            std::slice::from_ref(&unit),
            &files,
            None,
        );
        assert!(i.proof.iter().any(|(c, _)| *c == merge), "{:?}", i.proof);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_bare_new_function_header_is_code_but_clauses_make_a_statement() {
        let base = fp_of(DATA_SRC);
        // a builder adds a bare helper: no statement, no proof, no data; exec moves
        let bare = fp_of(&DATA_SRC.replace(
            "pub open spec fn g",
            "pub fn helper(x: u32) -> u32 { x + 1 }\npub open spec fn g",
        ));
        assert_eq!(
            (&bare.stmt, &bare.proof, &bare.data),
            (&base.stmt, &base.proof, &base.data)
        );
        assert_ne!(bare.exec, base.exec);
        // a header with clauses is a statement
        let with = fp_of(&DATA_SRC.replace(
            "pub open spec fn g",
            "pub fn helper(x: u32) -> u32 ensures true { x + 1 }\npub open spec fn g",
        ));
        assert_ne!(with.stmt, base.stmt);
        // an existing function's header with clauses changing is a statement change
        let weaker = fp_of(&DATA_SRC.replace("ensures r == g(Used { a: x })", "ensures true"));
        assert_ne!(weaker.stmt, base.stmt);
        // dropping the clauses of an existing function turns its statement into code: the statement fingerprint still moves
        let dropped = fp_of(&DATA_SRC.replace("ensures r == g(Used { a: x }) ", ""));
        assert_ne!(dropped.stmt, base.stmt);
        // editing the body of a bare helper moves exec only
        let b1 = fp_of(&DATA_SRC.replace(
            "pub open spec fn g",
            "pub fn helper(x: u32) -> u32 { x + 1 }\npub open spec fn g",
        ));
        let b2 = fp_of(&DATA_SRC.replace(
            "pub open spec fn g",
            "pub fn helper(x: u32) -> u32 { x + 2 }\npub open spec fn g",
        ));
        assert_eq!(
            (&b1.stmt, &b1.proof, &b1.data),
            (&b2.stmt, &b2.proof, &b2.data)
        );
        assert_ne!(b1.exec, b2.exec);
    }

    #[test]
    fn the_count_ratchet_refuses_a_decrease_and_accepts_growth() {
        let mk = |rows: &[(&str, usize, usize)]| -> BTreeMap<String, (usize, usize)> {
            rows.iter()
                .map(|(u, a, b)| (u.to_string(), (*a, *b)))
                .collect()
        };
        let base = mk(&[("verus/a", 10, 500), ("verus/b", 4, 100)]);
        // growth and equal: fine
        assert!(
            spec::count_decreases(&base, &mk(&[("verus/a", 11, 600), ("verus/b", 4, 100)]))
                .is_empty()
        );
        // a statement count drop: the extractor stopped seeing a statement
        let d = spec::count_decreases(&base, &mk(&[("verus/a", 9, 500), ("verus/b", 4, 100)]));
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].key, "counts::verus/a");
        assert!(d[0].text.contains("10 -> 9"));
        // a token count drop alone
        let d = spec::count_decreases(&base, &mk(&[("verus/a", 10, 499), ("verus/b", 4, 100)]));
        assert_eq!(d.len(), 1);
        // a unit that vanished
        let d = spec::count_decreases(&base, &mk(&[("verus/a", 10, 500)]));
        assert_eq!(d[0].key, "counts::verus/b");
        // the file round-trips
        let w = fp_of(DATA_SRC);
        let mut u = ufp(&w);
        u.unit = "verus/x/lib".into();
        u.stmt_count = 3;
        u.counts = [10, 20, 5];
        let tsv = spec::counts_tsv(&[u]);
        assert_eq!(spec::parse_counts(&tsv)["verus/x/lib"], (3, 25));
    }

    // ---- fourth review round

    #[test]
    fn a_listed_unit_still_walks_authorship_for_commits_after_started_before() {
        let mut w = a();
        w.stmt = "1111111111111111".into();
        // the record is valid and listed, but a builder changed the proof text AFTER started_before
        let mut s = scn(None, w.clone(), vec![pre(&w)]);
        s.cutoff = Some(CUT.into());
        s.after_proof = vec![("bbbbbbbbbb".into(), vec![SON.into()])];
        let o = run(&s);
        assert_eq!(
            rules(&o),
            ["proof-or-statement-written-by-a-builder"],
            "{:?}",
            o.refusals
        );
        // commits up to started_before are exempt (the walk handed over nothing)
        s.after_proof = vec![];
        assert!(run(&s).refusals.is_empty(), "{:?}", run(&s).refusals);
        // and a later code commit without a trailer is refused as well
        s.after_exec = Some(vec![vec![]]);
        let mut w2 = w.clone();
        w2.exec = "dddddddddddddddd".into();
        let mut s2 = scn(None, w2.clone(), vec![pre(&w2)]);
        s2.cutoff = Some(CUT.into());
        s2.after_exec = Some(vec![vec![]]);
        assert_eq!(rules(&run(&s2)), ["code-commit-without-trailer"]);
    }

    #[test]
    fn change_info_can_skip_the_commits_up_to_started_before() {
        let (d, base) = merge_repo("skipsb");
        let (unit, files) = rules_unit();
        fs::write(
            d.join("crates/x/src/rules.rs"),
            RULES_RS.replace("invariant i <= n", "invariant i <= n + 1"),
        )
        .unwrap();
        let sb = commit(&d, "proof one", Some("Claude Sonnet 5.5"));
        fs::write(
            d.join("crates/x/src/rules.rs"),
            RULES_RS.replace("invariant i <= n", "invariant i <= n + 2"),
        )
        .unwrap();
        commit(&d, "proof two", Some("Claude Sonnet 5.5"));
        let all = change_info(
            &d,
            &base,
            &unit.name,
            std::slice::from_ref(&unit),
            &files,
            None,
        );
        let after = change_info(
            &d,
            &base,
            &unit.name,
            std::slice::from_ref(&unit),
            &files,
            Some(&sb),
        );
        assert_eq!(all.proof.len(), 2, "{:?}", all.proof);
        assert_eq!(
            after.proof.len(),
            1,
            "only the commit after started_before: {:?}",
            after.proof
        );
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn an_inner_cfg_test_at_the_top_of_a_module_file_gates_the_whole_file() {
        let gated = "#![cfg(test)]\nuse vstd::prelude::*;\nverus! {\npub proof fn lemma(x: u32) ensures x == x {}\n}\n";
        let plain =
            "use vstd::prelude::*;\nverus! {\npub proof fn lemma(x: u32) ensures x == x {}\n}\n";
        let g = classify("v/m.rs", gated);
        let p = classify("v/m.rs", plain);
        assert!(g.errors.is_empty(), "{:?}", g.errors);
        assert!(g.proof_text.contains("cfg(test)"), "{}", g.proof_text);
        assert!(!p.proof_text.contains("cfg(test)"));
        // a plain test file with an inner gate and no verified item stays exec
        let t = classify("v/t.rs", "#![cfg(test)]\n#[test] fn t() {}\n");
        assert!(!t.proof_text.contains("cfg(test)"), "{}", t.proof_text);
    }

    #[test]
    fn a_cfg_attr_whose_payload_is_a_gate_steers_even_under_a_test_only_condition() {
        let base = classify("v/rules.rs", "#[path = \"x.rs\"]\npub mod walk;\n");
        for attr in [
            "#[cfg_attr(not(test), cfg(feature = \"x\"))]",
            "#[cfg_attr(not(test), path = \"other.rs\")]",
            "#[cfg_attr(not(test), cfg(any()))]",
            "#[cfg_attr(not(test), verifier::external)]",
        ] {
            let c = classify(
                "v/rules.rs",
                &format!("{attr}\n#[path = \"x.rs\"]\npub mod walk;\n"),
            );
            assert_ne!(c.proof_text, base.proof_text, "{attr} must be proof text");
        }
        // a test-only cfg_attr with a harmless payload stays exec
        let c = classify(
            "v/rules.rs",
            "#[cfg_attr(test, derive(Debug))]\n#[path = \"x.rs\"]\npub mod walk;\n",
        );
        assert_eq!(c.proof_text, base.proof_text);
    }

    #[test]
    fn a_bare_header_with_a_steering_attribute_or_no_unwind_stays_a_statement() {
        let base = fp_of(DATA_SRC);
        let helper = "pub fn helper(x: u32) -> u32 { x + 1 }\n";
        let with = |attr: &str| {
            fp_of(&DATA_SRC.replace(
                "pub open spec fn g",
                &format!("{attr}\n{helper}pub open spec fn g"),
            ))
        };
        // no attribute: code
        let bare = with("");
        assert_eq!(bare.stmt, base.stmt);
        // a gating attribute hides the fn from Verus: statement
        for attr in [
            "#[cfg(feature = \"x\")]",
            "#[cfg(target_os = \"none\")]",
            "#[cfg(not(verus_keep_ghost))]",
            "#[verifier::external]",
            "#[cfg_attr(verus_keep_ghost, verifier::external_body)]",
        ] {
            assert_ne!(
                with(attr).stmt,
                base.stmt,
                "{attr}: the header must stay a statement"
            );
        }
        // a test-only gate is not steering: still code
        assert_eq!(with("#[cfg(test)]").stmt, base.stmt);
        // no_unwind and default_ensures are clauses
        let nu = fp_of(&DATA_SRC.replace(
            "pub open spec fn g",
            "pub fn helper(x: u32) -> u32 no_unwind { x + 1 }\npub open spec fn g",
        ));
        assert_ne!(nu.stmt, base.stmt);
    }

    #[test]
    fn the_key_ratchet_catches_a_statement_that_disappears_while_another_appears() {
        let set = |rows: &[(&str, &[&str])]| -> BTreeMap<String, BTreeSet<String>> {
            rows.iter()
                .map(|(u, ks)| (u.to_string(), ks.iter().map(|k| k.to_string()).collect()))
                .collect()
        };
        let base = set(&[("verus/a", &["f.rs::x", "f.rs::y"])]);
        // same count, one key replaced: counts alone would not see it
        let work = set(&[("verus/a", &["f.rs::x", "f.rs::z"])]);
        let loss = spec::key_losses(&base, &work);
        assert_eq!(loss.len(), 1);
        assert_eq!(loss[0].key, "keys::verus/a::f.rs::y");
        // growth is fine, a stale (smaller-than-computed) committed file is fine
        assert!(
            spec::key_losses(
                &base,
                &set(&[("verus/a", &["f.rs::x", "f.rs::y", "f.rs::z"])])
            )
            .is_empty()
        );
        // a vanished unit loses all its keys
        assert_eq!(spec::key_losses(&base, &BTreeMap::new()).len(), 2);
        // the file round-trips
        let w = fp_of(DATA_SRC);
        let mut u = ufp(&w);
        u.unit = "verus/x/lib".into();
        u.stmt_keys = vec!["k1".into(), "k2".into()];
        assert_eq!(
            spec::parse_keys(&spec::keys_tsv(&[u]))["verus/x/lib"].len(),
            2
        );
        // equal or higher counts are never a refusal, only decreases
        let b: BTreeMap<String, (usize, usize)> = [("verus/a".to_string(), (5, 50))].into();
        let hi: BTreeMap<String, (usize, usize)> = [("verus/a".to_string(), (6, 60))].into();
        assert!(spec::count_decreases(&b, &hi).is_empty());
    }

    // ---- fifth review round

    type Cm = BTreeMap<String, (usize, usize)>;
    type Km = BTreeMap<String, BTreeSet<String>>;

    fn cm(rows: &[(&str, usize, usize)]) -> Cm {
        rows.iter()
            .map(|(u, a, b)| (u.to_string(), (*a, *b)))
            .collect()
    }

    fn km(rows: &[(&str, &[&str])]) -> Km {
        rows.iter()
            .map(|(u, ks)| (u.to_string(), ks.iter().map(|k| k.to_string()).collect()))
            .collect()
    }

    #[test]
    fn the_branchs_own_ratchet_files_may_not_lower_the_ratchet_nor_claim_what_is_gone() {
        let bc = cm(&[("verus/a", 2, 50)]);
        let bk = km(&[("verus/a", &["f::x", "f::y"])]);
        // unchanged files, extractor reaches everything: nothing lost
        assert!(spec::ratchet_losses(&bc, &bk, &bc, &bk, &bc, &bk).is_empty());
        // the branch deletes a key from the file: refused
        let fk = km(&[("verus/a", &["f::x"])]);
        let l = spec::ratchet_losses(&bc, &bk, &bc, &fk, &bc, &bk);
        assert_eq!(l.len(), 1, "{l:?}");
        assert!(
            l[0].key.starts_with("lowered-keys::verus/a::f::y"),
            "{}",
            l[0].key
        );
        // the branch lowers a count in the file: refused
        let fc = cm(&[("verus/a", 1, 50)]);
        let l = spec::ratchet_losses(&bc, &bk, &fc, &bk, &bc, &bk);
        assert_eq!(l.len(), 1, "{l:?}");
        assert!(l[0].key.starts_with("lowered-counts::verus/a"));
        // the branch deletes the file altogether
        assert!(!spec::ratchet_losses(&bc, &bk, &Cm::new(), &Km::new(), &bc, &bk).is_empty());
        // a statement was added and the file not updated (computed is higher than the file): fine
        let cc = cm(&[("verus/a", 3, 80)]);
        let ck = km(&[("verus/a", &["f::x", "f::y", "f::z"])]);
        assert!(spec::ratchet_losses(&bc, &bk, &bc, &bk, &cc, &ck).is_empty());
        // the branch raised the file and the extractor still reaches it: fine
        assert!(spec::ratchet_losses(&bc, &bk, &cc, &ck, &cc, &ck).is_empty());
        // the file claims a key the extractor no longer reaches (a statement vanished after the file was raised): refused
        let fk2 = km(&[("verus/a", &["f::x", "f::y", "f::w"])]);
        let l = spec::ratchet_losses(&bc, &bk, &bc, &fk2, &bc, &bk);
        assert!(l.iter().any(|s| s.key == "keys::verus/a::f::w"), "{l:?}");
        // and a count the extractor no longer reaches
        let fc2 = cm(&[("verus/a", 5, 90)]);
        assert!(!spec::ratchet_losses(&bc, &bk, &fc2, &bk, &bc, &bk).is_empty());
        // no base files at all (first landing): nothing to compare, but the file-vs-computed rule still runs on its own
        assert!(spec::ratchet_losses(&Cm::new(), &Km::new(), &bc, &bk, &bc, &bk).is_empty());
    }

    // ---- pre-rule started_before: any commit the list names, on a branch or not

    #[test]
    fn commit_exists_asks_the_object_database_and_only_for_hex_names() {
        let (d, base) = merge_repo("exists");
        assert!(commit_exists(&d, &base));
        assert!(
            !commit_exists(&d, &base[..10]),
            "an abbreviation is refused"
        );
        assert!(
            !commit_exists(&d, &base.to_uppercase()),
            "upper case is refused"
        );
        assert!(!commit_exists(
            &d,
            "0123456789abcdef0123456789abcdef01234567"
        ));
        assert!(!commit_exists(&d, "--help"));
        assert!(
            !commit_exists(&d, "HEAD"),
            "a ref name is not a commit hash"
        );
        assert!(!commit_exists(&d, "abc"), "too short");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_listed_started_before_off_every_branch_exempts_its_ancestors_and_later_commits_are_walked()
    {
        let (d, base) = merge_repo("sidebranch");
        let (unit, files) = rules_unit();
        // The author started on a commit that is on no branch and is not an ancestor of main: a detached commit.
        g(&d, &["checkout", "-q", "--detach"]);
        fs::write(
            d.join("crates/x/src/rules.rs"),
            RULES_RS.replace("invariant i <= n", "invariant i <= n + 1"),
        )
        .unwrap();
        let sb = commit(&d, "side work", Some("Claude Sonnet 5.5"));
        g(&d, &["checkout", "-q", "main"]);
        assert!(commit_exists(&d, &sb), "the object is in the database");
        assert_eq!(
            g(&d, &["branch", "--contains", &sb]).trim(),
            "",
            "on no branch"
        );
        // main goes on: a builder changes the proof text AFTER the side work
        fs::write(
            d.join("crates/x/src/rules.rs"),
            RULES_RS.replace("invariant i <= n", "invariant i <= n + 2"),
        )
        .unwrap();
        commit(&d, "later proof by a builder", Some("Claude Sonnet 5.5"));
        let after = change_info(
            &d,
            &base,
            &unit.name,
            std::slice::from_ref(&unit),
            &files,
            Some(&sb),
        );
        assert_eq!(
            after.proof.len(),
            1,
            "the later commit is walked: {:?}",
            after.proof
        );
        assert_eq!(after.proof[0].1, vec!["claude-sonnet-5-5".to_string()]);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_listed_started_before_on_a_side_branch_is_accepted_and_a_missing_one_is_refused() {
        let mut w = a();
        w.stmt = "1111111111111111".into();
        let mut s = scn(None, w.clone(), vec![pre(&w)]);
        s.cutoff = Some(CUT.into());
        // the commit exists (on a side branch, no ancestry is asked): accepted
        assert!(run(&s).refusals.is_empty(), "{:?}", run(&s).refusals);
        // a commit after it is walked under strict Option A
        s.after_proof = vec![("cccccccccc".into(), vec![SON.into()])];
        assert_eq!(rules(&run(&s)), ["proof-or-statement-written-by-a-builder"]);
        s.after_proof = vec![];
        // the object is missing: refused with a clear message
        s.exists_ok = false;
        let o = run(&s);
        assert_eq!(rules(&o), ["statement-needs-strong-prover-and-reviewer"]);
        assert!(
            o.refusals[0]
                .detail
                .contains("does not exist in this repository's object database"),
            "{}",
            o.refusals[0].detail
        );
        // the record's started_before must still equal the list's
        s.exists_ok = true;
        let mut r = pre(&w);
        r.started_before = "0000000000000000000000000000000000000001".into();
        let mut s2 = scn(None, w.clone(), vec![r]);
        s2.cutoff = Some(CUT.into());
        assert!(run(&s2).refusals[0].detail.contains("differs from the"));
    }

    // ---- registration lines are not an extractor change

    fn yes(_: &str) -> bool {
        true
    }
    fn no(_: &str) -> bool {
        false
    }

    #[test]
    fn registration_lines_for_new_units_are_additions_and_anything_else_is_not() {
        let inv = "# Generated\nverus/a/rules\tg\tverus\tverification/a/rules.rs\tx\nverus/c/rules\tg\tverus\tverification/c/rules.rs\tx\n";
        let with_b = "# Generated\nverus/a/rules\tg\tverus\tverification/a/rules.rs\tx\nverus/b/rules\tg\tverus\tverification/b/rules.rs\tx\nverus/c/rules\tg\tverus\tverification/c/rules.rs\tx\n";
        let p = "tools/ci/proof-inventory.tsv";
        // a sorted insertion in the middle is an addition
        assert_eq!(
            registration_additions(p, inv, with_b).unwrap(),
            vec![Reg::Unit("verus/b/rules".into())]
        );
        assert!(registration_only(p, inv, with_b, &yes, &yes, &yes).is_ok());
        // the unit's files existed at the base: not exempt
        let e = registration_only(p, inv, with_b, &no, &yes, &yes).unwrap_err();
        assert!(e.contains("not new in this range"), "{e}");
        // a removed line, an altered line, a new comment, a header edit: all refused
        assert!(registration_additions(p, with_b, inv).is_err());
        let altered = with_b.replace("verification/a/rules.rs\tx", "verification/a/other.rs\tx");
        assert!(registration_additions(p, with_b, &altered).is_err());
        let comment = format!("{with_b}# note\n");
        assert!(registration_additions(p, with_b, &comment).is_err());
        let header = with_b.replace("# Generated", "# Hand made");
        assert!(registration_additions(p, with_b, &header).is_err());
        // every registration file of the table, one added line each
        for (f, line) in [
            (
                "tools/ci/trust-baseline.tsv",
                "shipped/n/rules\tsolver\t3\n",
            ),
            ("docs/ci/statement-counts.tsv", "shipped/n/rules\t4\t100\n"),
            (
                "docs/ci/statement-keys.tsv",
                "shipped/n/rules\tcrates/n/src/rules.rs::f\n",
            ),
        ] {
            let base = "# h\nshipped/m/rules\tx\t1\n";
            let work = format!("{base}{line}");
            assert_eq!(
                registration_additions(f, base, &work).unwrap(),
                vec![Reg::Unit("shipped/n/rules".into())],
                "{f}"
            );
            assert!(
                registration_additions(f, &work, base).is_err(),
                "{f}: removal"
            );
        }
        // the roles log: the unit is field 4; a comment may be added there
        let base = "# h\n";
        let work = "# h\n# a comment\nr-1\t2026-11-11\tstatement\tshipped/n/rules\t-\t-\t-\t-\tx\n";
        assert_eq!(
            registration_additions(LOG, base, work).unwrap(),
            vec![Reg::Unit("shipped/n/rules".into())]
        );
        assert!(registration_additions(LOG, work, base).is_err());
    }

    #[test]
    fn groups_gain_keys_but_never_lose_or_move_one() {
        let p = "tools/ci/proof-groups.tsv";
        let base = "# c\nkernel\tjuno-ring juno-rcu\nboot\tfdt\n";
        let keys = |w: &str| registration_additions(p, base, w);
        assert_eq!(
            keys("# c\nkernel\tjuno-ring juno-rcu juno-new\nboot\tfdt\n").unwrap(),
            vec![Reg::Key("juno-new".into())]
        );
        assert_eq!(
            keys("# c\nkernel\tjuno-ring juno-rcu\nboot\tfdt\nnewgroup\tk1 k2\n").unwrap(),
            vec![Reg::Key("k1".into()), Reg::Key("k2".into())]
        );
        assert!(
            keys("# c\nkernel\tjuno-ring\nboot\tfdt\n").is_err(),
            "a key dropped"
        );
        assert!(
            keys("# c\nkernel\tjuno-ring\nboot\tfdt juno-rcu\n").is_err(),
            "a key moved"
        );
        assert!(
            keys("# c\nkernel\tjuno-ring juno-rcu\nboot\tfdt\nboot\tmore\n").is_err(),
            "a second line of an existing group"
        );
        assert!(
            keys("# c\nkernel\tjuno-ring juno-rcu\nboot\tfdt\nother\tfdt\n").is_err(),
            "an existing key in a new group"
        );
        assert!(
            keys("# c2\nkernel\tjuno-ring juno-rcu\nboot\tfdt\n").is_err(),
            "the comment changed"
        );
        // a key is new only when its units are
        let w = "# c\nkernel\tjuno-ring juno-rcu juno-new\nboot\tfdt\n";
        assert!(registration_only(p, base, w, &yes, &yes, &yes).is_ok());
        assert!(registration_only(p, base, w, &yes, &no, &yes).is_err());
    }

    #[test]
    fn the_verify_shipped_list_and_the_matrix_summary_may_only_grow_with_their_rows() {
        let p = "scripts/verify-shipped.sh";
        let script =
            |files: &str, tail: &str| format!("#!/bin/bash\nroot=x\nfiles=({files})\n{tail}\n");
        let base = script("crates/a/src/r.rs crates/b/src/r.rs", "echo hi");
        assert_eq!(
            registration_additions(
                p,
                &base,
                &script(
                    "crates/a/src/r.rs crates/b/src/r.rs crates/c/src/r.rs",
                    "echo hi"
                )
            )
            .unwrap(),
            vec![Reg::File("crates/c/src/r.rs".into())]
        );
        assert!(
            registration_additions(p, &base, &script("crates/a/src/r.rs", "echo hi")).is_err(),
            "a file dropped"
        );
        assert!(
            registration_additions(
                p,
                &base,
                &script(
                    "crates/a/src/r.rs crates/b/src/r.rs crates/b/src/r.rs",
                    "echo hi"
                )
            )
            .is_err(),
            "a duplicate"
        );
        assert!(
            registration_additions(
                p,
                &base,
                &script(
                    "crates/a/src/r.rs crates/b/src/r.rs crates/c/src/r.rs",
                    "echo changed"
                )
            )
            .is_err(),
            "another line of the script changed"
        );
        assert!(registration_additions(p, &base, &format!("{base}echo more\n")).is_err());
        assert!(
            registration_only(
                p,
                &base,
                &script(
                    "crates/a/src/r.rs crates/b/src/r.rs crates/c/src/r.rs",
                    "echo hi"
                ),
                &yes,
                &yes,
                &no
            )
            .is_err()
        );

        let m = "docs/ci/PROOF-MATRIX.md";
        let mat = |n: usize, rows: &[&str]| {
            let mut s = format!(
                "# Proof matrix\n\n{n} units: x\n\n| Group | Unit | Kind | Target |\n| --- | --- | --- | --- |\n"
            );
            for r in rows {
                s.push_str(&format!("| g | `{r}` | verus | `verification/{r}.rs` |\n"));
            }
            s
        };
        let base = mat(1, &["verus/a/r"]);
        assert_eq!(
            registration_additions(m, &base, &mat(2, &["verus/a/r", "verus/b/r"])).unwrap(),
            vec![Reg::Unit("verus/b/r".into())]
        );
        assert!(
            registration_additions(m, &base, &mat(1, &["verus/a/r", "verus/b/r"])).is_err(),
            "the total must follow the rows"
        );
        assert!(registration_additions(m, &base, &mat(3, &["verus/a/r", "verus/b/r"])).is_err());
        assert!(
            registration_additions(m, &mat(2, &["verus/a/r", "verus/b/r"]), &base).is_err(),
            "a row removed"
        );
        assert!(
            registration_additions(m, &base, &format!("{}text\n", mat(1, &["verus/a/r"]))).is_err()
        );
        assert!(registration_additions("docs/other.md", "a\n", "a\nb\n").is_err());
    }

    #[test]
    fn registration_only_changes_are_not_an_extractor_change_but_anything_else_still_is() {
        let w = a();
        let mut s = scn(Some(a()), w.clone(), vec![]);
        s.extractor_changed = vec!["tools/ci/proof-inventory.tsv".into()];
        s.unit_files_changed = vec!["crates/x/src/rules.rs".into()];
        // not exempt: refused as before
        assert_eq!(rules(&run(&s)), ["extractor-changed-with-unit-files"]);
        // exempt (the registration lines of units with new files): passes, with a note
        s.registration_exempt = vec!["tools/ci/proof-inventory.tsv".into()];
        let o = run(&s);
        assert!(o.refusals.is_empty(), "{:?}", o.refusals);
        assert!(
            o.notes
                .iter()
                .any(|n| n.contains("not an extractor change")),
            "{:?}",
            o.notes
        );
        // an exempt file does not excuse another extractor file
        s.extractor_changed
            .push("tools/ci/lucina-ci/src/roles.rs".into());
        assert_eq!(rules(&run(&s)), ["extractor-changed-with-unit-files"]);
        // and the explicit token still works
        s.allow_extractor = true;
        assert!(run(&s).refusals.is_empty());
    }

    // ---- //@omit derive lines and rustfmt

    #[test]
    fn omit_derive_next_to_another_derive_is_a_merge_hazard() {
        let bad = "#[derive(Clone, Copy)]\n#[derive(Debug)] //@omit\npub struct S;\n";
        let h = omit_hazards_of("c/rules.rs", bad);
        assert_eq!(h.len(), 1, "{h:?}");
        assert!(
            h[0].starts_with("c/rules.rs:2:") && h[0].contains("merge_derives"),
            "{h:?}"
        );
        // after the omit line, a blank line or a plain comment in between, and two omit lines
        assert_eq!(
            omit_hazards_of(
                "f",
                "#[derive(Debug)] //@omit\n#[derive(Clone)]\nstruct S;\n"
            )
            .len(),
            1
        );
        assert_eq!(
            omit_hazards_of(
                "f",
                "#[derive(Clone)]\n\n// c\n#[derive(Debug)] //@omit\nstruct S;\n"
            )
            .len(),
            1
        );
        assert_eq!(
            omit_hazards_of(
                "f",
                "#[derive(Clone)] //@omit\n#[derive(Debug)] //@omit\nstruct S;\n"
            )
            .len(),
            2
        );
        // fine: every derive on the one omit line (as on main), a lone omit derive, an omit that is not a derive, a derive far away
        for ok in [
            "#[derive(Clone, Copy, Debug, PartialEq, Eq)] //@omit\npub struct S;\n",
            "#[derive(Debug)] //@omit\npub struct S;\n",
            "#[allow(clippy::too_many_arguments)] //@omit\npub fn f() {}\n",
            "#[derive(Clone)]\n#[allow(dead_code)]\n#[derive(Debug)] //@omit\npub struct U;\n",
            "#[derive(Clone)]\npub struct A;\n#[derive(Debug)] //@omit\npub struct B;\n",
            "#[derive(Clone)]\n#[derive(Debug)]\npub struct S;\n",
        ] {
            assert!(omit_hazards_of("f", ok).is_empty(), "{ok}");
        }
    }

    #[test]
    fn an_omit_hazard_is_refused_by_the_check() {
        let mut s = scn(Some(a()), a(), vec![]);
        s.omit_hazards = vec!["c/rules.rs:2: `//@omit` derive line sits next to ...".into()];
        let o = run(&s);
        assert_eq!(rules(&o), ["omit-derive-merge-hazard"]);
        assert!(o.refusals[0].detail.contains("c/rules.rs:2"));
    }

    fn rustfmt_available() -> bool {
        Command::new("rustfmt")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
    }

    fn fp_in(d: &Path, kind: &str, target: &str) -> Fp {
        let u = Unit {
            name: format!("{kind}/x/rules"),
            group: "g".into(),
            kind: kind.into(),
            target: target.into(),
            inputs: vec![],
        };
        let (v, e) = fingerprints(&FsTree(d.to_path_buf()), std::slice::from_ref(&u));
        assert!(e.is_empty(), "{e:?}");
        v.into_iter().next().unwrap().fp.expect("a tracked unit")
    }

    // A tp_rules-style shipped file, laid out badly on purpose: long parameter list, one-line blocks, one-line struct, a tuple with a
    // trailing comma, struct literal with no spaces. Everything rustfmt does to it is layout (and trailing commas).
    const FMT_SHIPPED: &str = r#"use std::fmt::Debug;
pub const SLOTS: usize = 10;

#[derive(Clone, Copy)]
pub struct Slot { pub idx: usize, pub live: bool }

#[derive(Clone, Copy, Debug, PartialEq, Eq)] //@omit
pub enum Kind { A, B }

pub fn slot_by_key(active: &[bool; SLOTS], keys: &[i32; SLOTS], used: &[bool; SLOTS], n: usize, key: i32) -> Option<usize> //@ret
//@ requires n <= SLOTS,
//@ ensures match ret { Some(i) => i < n, None => true },
{
    let mut i = 0;
    while i < n
    //@ invariant n <= SLOTS, i <= n,
    //@ decreases n - i,
    {
        if active[i] && keys[i] == key { return Some(i); }
        if !active[i] && !used[i] { return Some(i); }
        i += 1;
    }
    None
}

pub fn pick(a: u32,b: u32,   c: u32) -> u32 //@ret
//@ ensures ret <= 3,
{ let s = Slot{idx:1,live:true}; let _ = (s.idx,s.live,); if a > b {2} else if b > c { 3 } else { 0 } }
"#;

    const FMT_VERUS: &str = r#"use std::fmt::Debug;
use vstd::prelude::*;
verus! {
pub open spec fn ok(x: u32) -> bool { x < 10 }
proof fn lemma_a(x: u32) requires x < 5 ensures ok(x) { assert(x < 10); }
pub fn f(x: u32, y: u32, z: u32, w: u32, v: u32, u: u32, t: u32, s: u32) -> (r: u32) requires x < 5 ensures r == x + 1 {
    let mut i = 0;
    while i < x invariant i <= x decreases x - i { i += 1; }
    assert(i == x) by { reveal(ok); };
    proof { lemma_a(x); }
    x + 1
}
}
pub fn plain(a: u32,b: u32,   c: u32) -> u32 { let _ = (a,b,c,); if a > b {2} else {0} }
"#;

    #[test]
    fn rustfmt_changes_no_fingerprint_of_a_shipped_file_or_a_verus_file() {
        if !rustfmt_available() {
            eprintln!(
                "SKIPPED rustfmt_changes_no_fingerprint_...: rustfmt is not on PATH, the rustfmt layout test did not run"
            );
            return;
        }
        for (kind, rel, src, must_change) in [
            ("shipped", "crates/x/src/rules.rs", FMT_SHIPPED, true),
            ("verus", "verification/x/rules.rs", FMT_VERUS, true),
        ] {
            let d = tmp_repo(&format!("fmt-{kind}"));
            fs::create_dir_all(d.join(rel).parent().unwrap()).unwrap();
            fs::write(d.join(rel), src).unwrap();
            let before = fp_in(&d, kind, rel);
            let o = Command::new("rustfmt")
                .args(["--edition", "2021", rel])
                .current_dir(&d)
                .output()
                .unwrap();
            assert!(
                o.status.success(),
                "rustfmt: {}",
                String::from_utf8_lossy(&o.stderr)
            );
            let formatted = fs::read_to_string(d.join(rel)).unwrap();
            assert_eq!(
                must_change,
                formatted != src,
                "{kind}: rustfmt must have re-laid the sample out"
            );
            let after = fp_in(&d, kind, rel);
            assert_eq!(
                before.stmt, after.stmt,
                "{kind}: statement_fp moved by rustfmt\n{formatted}"
            );
            assert_eq!(
                before.proof, after.proof,
                "{kind}: proof_fp moved by rustfmt\n{formatted}"
            );
            assert_eq!(
                before.exec, after.exec,
                "{kind}: exec_fp moved by rustfmt\n{formatted}"
            );
            assert_eq!(
                before.data, after.data,
                "{kind}: data_fp moved by rustfmt\n{formatted}"
            );
            let _ = fs::remove_dir_all(&d);
        }
    }

    #[test]
    fn rustfmt_merging_derives_keeps_the_omit_tag_so_the_guard_must_refuse_the_source() {
        // The program Verus reads changes (Clone and Copy vanish with the tag) while the proof lines do not: only the guard sees it.
        let src = "#[derive(Clone, Copy)]\n#[derive(Debug)] //@omit\npub struct S {\n    pub a: u32,\n}\n";
        assert_eq!(omit_hazards_of("x.rs", src).len(), 1);
        if !rustfmt_available() {
            eprintln!(
                "SKIPPED the merge half of rustfmt_merging_derives...: rustfmt is not on PATH"
            );
            return;
        }
        let d = tmp_repo("fmt-derive");
        fs::write(d.join("x.rs"), src).unwrap();
        let o = Command::new("rustfmt")
            .args(["--edition", "2021", "x.rs"])
            .current_dir(&d)
            .output()
            .unwrap();
        assert!(o.status.success());
        let out = fs::read_to_string(d.join("x.rs")).unwrap();
        assert!(
            out.starts_with("#[derive(Clone, Copy, Debug)] //@omit\n"),
            "{out}"
        );
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn layout_variants_have_equal_fingerprints() {
        // line breaks, indentation, trailing commas before ) } ], comments, spacing around punctuation: no fingerprint moves.
        let a = "pub struct P { pub x: u32, pub y: u32 }\npub fn g(a: u32, b: u32) -> u32 //@ret\n//@ requires a < 5, b < 5,\n//@ ensures ret == a + b,\n{\n    let p = P { x: a, y: b };\n    let t = (p.x, p.y);\n    t.0 + t.1\n}\n";
        let b = "pub struct P {\n        pub x: u32,\n        pub y: u32,\n}\n\n// a comment that is not code\npub fn g(\n    a: u32,\n    b: u32,\n) -> u32 //@ret\n//@ requires a < 5,\n//@     b < 5,\n//@ ensures\n//@     ret == a + b,\n{\n    /* block */ let p = P {\n        x: a,\n        y: b,\n    };\n    let t = (\n        p.x,\n        p.y,\n    ); // tail\n    t.0 + t.1\n}\n";
        let d = tmp_repo("layout");
        fs::create_dir_all(d.join("c/src")).unwrap();
        fs::write(d.join("c/src/rules.rs"), a).unwrap();
        let fa = fp_in(&d, "shipped", "c/src/rules.rs");
        fs::write(d.join("c/src/rules.rs"), b).unwrap();
        let fb = fp_in(&d, "shipped", "c/src/rules.rs");
        assert_eq!(
            (fa.stmt, fa.proof, fa.exec, fa.data),
            (fb.stmt, fb.proof, fb.exec, fb.data)
        );
        let _ = fs::remove_dir_all(&d);
    }

    // ---- strong tiers, families

    #[test]
    fn strong_code_is_proved_by_a_strong_model_of_the_other_tier() {
        assert_eq!(strong_tier("Claude Opus 5.5"), Some("opus"));
        assert_eq!(strong_tier("claude-fable-5-2"), Some("fable"));
        assert_eq!(strong_tier("claude-sonnet-5-5"), None);
        for m in STRONG {
            assert!(STRONG_TIERS.iter().any(|(n, _)| n == m), "{m} has no tier");
        }
        for (n, _) in STRONG_TIERS {
            assert!(STRONG.contains(n), "{n} is a tier but not strong");
        }
        let mut w = a();
        w.proof = "2222222222222222".into();
        let verdict = |imp: &str, prover: &str| {
            let mut r = rec("proof-edit", "verus/u", &w);
            r.impl_model = imp.into();
            r.prover_model = prover.into();
            let mut s = scn(Some(a()), w.clone(), vec![r]);
            s.trailer = vec![prover.into()];
            s.exec_models = vec![];
            let o = run(&s);
            o.refusals
                .iter()
                .map(|x| x.detail.clone())
                .collect::<Vec<_>>()
        };
        // Opus-written code: not by Opus, by Fable
        let r = verdict(OPUS, OPUS);
        assert!(r.iter().any(|d| d.contains("same model")), "{r:?}");
        assert!(verdict(OPUS, "claude-fable-5-2").is_empty());
        // Fable-written code: by Opus
        assert!(verdict("claude-fable-5-2", OPUS).is_empty());
        let r = verdict("claude-fable-5-2", "claude-fable-5-2");
        assert!(r.iter().any(|d| d.contains("same model")), "{r:?}");
        // a builder's code: either strong model
        assert!(verdict(SON, OPUS).is_empty());
        assert!(verdict(SON, "claude-fable-5-2").is_empty());
    }

    #[test]
    fn a_strong_implementer_needs_the_other_tier_even_when_the_names_differ() {
        // Two names of the same tier (a later Opus) do not satisfy the rule: the tier table decides.
        assert_eq!(
            strong_tier("claude-opus-5-5"),
            strong_tier("Claude Opus 5.5 (1M context)")
        );
        let w = {
            let mut w = a();
            w.proof = "2222222222222222".into();
            w
        };
        let mut r = rec("proof-edit", "verus/u", &w);
        r.impl_model = "claude-fable-5-2".into();
        r.prover_model = OPUS.into();
        let ok = validate(&r, need_kind(&r), &|_: &str, _: &Record| true);
        assert!(ok.is_ok(), "{ok:?}");
        // a pre-rule record keeps the old exemption (the same Opus model wrote the code and reviews it independently)
        let mut p = rec("pre-rule", "verus/u", &w);
        p.impl_model = OPUS.into();
        p.reviewer_model = "-".into();
        p.reviewer_family = "-".into();
        assert!(validate(&p, need_kind(&p), &|_: &str, _: &Record| true).is_ok());
    }

    #[test]
    fn family_names_of_every_lane_normalise_to_their_family() {
        for (m, fam, cls) in [
            ("qwen3.8-27b", "qwen", "builder"),
            ("Qwen3.8:27B", "qwen", "builder"),
            ("glm-5.3-flash", "glm", "builder"),
            ("zen-glm-flash", "glm", "builder"),
            ("zcode", "glm", "builder"),
            ("mistral-large", "mistral", "builder"),
            ("vibe", "mistral", "builder"),
            ("qoder-efficient", "qoder", "builder"),
            ("qodercli", "qoder", "builder"),
            ("zen-deepseek-v4.1-flash-free", "deepseek", "builder"),
            ("deepseek-free", "deepseek", "builder"),
            ("claude-sonnet-5-5", "claude", "builder"),
            ("Claude Opus 5.5 (1M context)", "claude", "strong"),
            ("claude-fable-5-2", "claude", "strong"),
            ("gpt-6.1-sol", "gpt", "unknown"),
            ("codex", "gpt", "unknown"),
            ("deepseek-paid", "deepseek", "unknown"),
            ("something-new", "other", "unknown"),
        ] {
            assert_eq!(family(m), fam, "family of {m}");
            assert_eq!(class(m), cls, "class of {m}");
        }
        for f in [
            "claude", "gpt", "qwen", "mistral", "deepseek", "glm", "qoder",
        ] {
            assert!(amend::FAMILIES.contains(&f), "{f}");
        }
        // a record whose reviewer family is a lane family other than the prover's is accepted for a statement
        let mut w = a();
        w.stmt = "1111111111111111".into();
        for (rm, rf) in [
            ("qoder-efficient", "qoder"),
            ("glm-5.3-flash", "glm"),
            ("zen-deepseek-v4.1-flash-free", "deepseek"),
            ("qwen3.8-27b", "qwen"),
            ("mistral-large", "mistral"),
        ] {
            let mut r = rec("statement", "verus/u", &w);
            r.reviewer_model = rm.into();
            r.reviewer_family = rf.into();
            assert!(
                validate(&r, Need::Statement, &|_: &str, _: &Record| true).is_ok(),
                "{rm}"
            );
        }
    }

    // ---- full object ids only, harness spellings, unknown-class implementers

    #[test]
    fn only_a_full_lowercase_id_names_a_commit_even_when_a_branch_has_its_prefix() {
        let (d, base) = merge_repo("fullid");
        // a builder creates a branch named like the 7-digit prefix of the commit and pointing elsewhere
        let short = &base[..7];
        g(&d, &["branch", short, &base]);
        assert!(
            commit_exists(&d, &base),
            "the full id of a real commit is accepted"
        );
        assert!(
            !commit_exists(&d, short),
            "the abbreviation is refused although a branch of that name exists"
        );
        assert!(!commit_exists(&d, &base[..39]));
        assert!(!commit_exists(&d, &base.to_uppercase()));
        assert!(!commit_exists(&d, &format!("{base}0")), "41 digits");
        assert!(full_id(&base) && !full_id("HEAD") && !full_id(""));
        // the cutoff list and the records refuse abbreviations with a clear message
        let e =
            parse_cutoff("cutoff 0123456789abcdef0123456789abcdef01234567\nverus/u\tabc1234\tr\n")
                .unwrap_err();
        assert!(e.contains("full 40-digit"), "{e}");
        let e =
            parse_cutoff("cutoff abc1234\nverus/u\t0123456789abcdef0123456789abcdef01234567\tr\n")
                .unwrap_err();
        assert!(e.contains("cutoff <full 40-digit"), "{e}");
        assert!(parse_cutoff(CUT).is_ok());
        let mut w = a();
        w.stmt = "1111111111111111".into();
        let mut r = pre(&w);
        r.started_before = "abc1234".into();
        let e = parse_fields(&line(&r).split('\t').collect::<Vec<_>>()).unwrap_err();
        assert!(e.contains("full 40-digit lowercase hex"), "{e}");
        r.started_before = SB.to_uppercase();
        assert!(parse_fields(&line(&r).split('\t').collect::<Vec<_>>()).is_err());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn harness_spellings_of_strong_models_normalise_and_unknown_implementers_are_never_skipped() {
        for n in [
            "opus 5.5",
            "Opus 5.5",
            "Claude Opus 5.5",
            "claude-opus-5-5",
            "Claude Opus 5.5 (1M context)",
        ] {
            assert_eq!(model_norm(n), "claude-opus-5-5", "{n}");
            assert_eq!(class(n), "strong", "{n}");
            assert_eq!(strong_tier(n), Some("opus"), "{n}");
        }
        for n in ["fable 5.2", "Claude Fable 5.2"] {
            assert_eq!(model_norm(n), "claude-fable-5-2", "{n}");
            assert_eq!(strong_tier(n), Some("fable"), "{n}");
        }
        assert_eq!(model_norm("Sonnet 5.5"), "claude-sonnet-5-5");
        assert_eq!(
            class("qwen3-opus-distill"),
            "builder",
            "not a vendor-less spelling"
        );
        assert_eq!(
            trailer_models("m\n\nCo-Authored-By: Opus 5.5 <x@y>\n"),
            vec!["claude-opus-5-5"]
        );
        let mut w = a();
        w.proof = "2222222222222222".into();
        let ok = |imp: &str, prover: &str| {
            let mut r = rec("proof-edit", "verus/u", &w);
            r.impl_model = imp.into();
            r.prover_model = prover.into();
            validate(&r, need_kind(&r), &|_: &str, _: &Record| true)
        };
        // spelled like a harness writes it: the same model / the same tier is still seen
        assert!(ok("opus 5.5", "Claude Opus 5.5").is_err());
        assert!(ok("opus 5.5", "claude-fable-5-2").is_ok());
        assert!(ok("Fable 5.2", "opus 5.5").is_ok());
        // an implementer of unknown class needs a strong prover of a different model; a builder prover never passes
        assert!(ok("gpt-6-1-sol", "claude-opus-5-5").is_ok());
        assert!(ok("gpt-6-1-sol", "gpt-6-1-sol").is_err());
        assert!(ok("gpt-6-1-sol", "claude-sonnet-5-5").is_err());
        assert!(ok("some-new-model", "qoder").is_err());
    }
}
