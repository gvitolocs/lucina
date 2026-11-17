//! `lucina-ci roles`: the model that writes the code must not be the one that proves it or judges the proof (owner rule,
//! docs/ci/PROOF-ROLES.md, docs/ci/PROOF-WORKFLOW.md). Mechanical part: three fingerprints per proof unit, an append-only record log
//! `docs/ci/proof-roles.tsv`, and a merge check that a changed statement or proof carries a record by a STRONG prover that is not the
//! implementer, with (for statements) a reviewer of another model family.
//!
//!   lucina-ci roles --check [--base REF] [--merge-gate]   compare with the base commit's tree; exit 0 ok, 1 refused, 2 usage/io/extractor
//!   lucina-ci roles --list [--json | --summary]            fingerprints and review status per unit; burn-down by group
//!   lucina-ci roles --template statement|proof-edit [--unit U]   a record line to fill
//!   lucina-ci roles --seed [--print]                       append one `seed` record per unit that has no record yet
//!   lucina-ci roles --audit                                both authorship sets of every unit, one line each (report only, exit 0)
//!   lucina-ci roles --authors UNIT [--base REF]            every commit that shaped one unit, with its model and where that came from
//!
//! THE THREE FINGERPRINTS (fnv1a-64 like src/spec.rs; hex). Per unit, over its verified files: the target and every file it pulls in by
//! `#[path]` or `mod x;` (exactly the closure src/trust.rs resolves).
//!   statement_fp  the statements `spec --list` produces for those files (spec::scan_file), aggregated: sorted `key<TAB>hash` lines.
//!   proof_fp      normalised text (comments dropped, whitespace collapsed, same normaliser as the freeze) of the PROOF TEXT, per file in
//!                 source order, runs joined by ` ;; `, plus one `;; omit: <line>` per `//@omit` line - a tag says what Verus does
//!                 not read, so it is the prover's edit. What the shipped records and `proof-groups.tsv` digest: the authorship
//!                 streams are the stricter pair, and a tag is accepted only on an attribute line (`#[..]`, `#![..]`), anywhere else
//!                 it is refused: `omit-tag-hides-code-from-the-proof`.
//!   exec_fp       the normalised text of everything else in the verified files. It moves when a bare fn header gains clauses, so it is
//!                 not what says "this commit wrote code".
//! Attribution uses two further streams, not fingerprints (`FileClass::author_code` and `author_proof`, src/authorship.rs): the CODE
//! STREAM is the program rustc reads when the verifier is off, and the PROOF STREAM is what only the verifier reads plus the statement
//! text and the tags. Equal across a commit on the code side = nobody wrote code there; equal on both = the commit is neutral.
//! A closed `proof!`/`proof_with!` block with the exact `#[cfg(verus_keep_ghost)]` gate directly in front of it leaves the code stream
//! with its gate (`ghost_gate`, `attr_pass`): rustc reads neither, and the block does not split an exec run either. An ungated block,
//! one behind another attribute, one whose delimiters never close, or one in a file that brought its own `proof` macro, is proof text
//! that still cuts the exec run where it stands; the same gate over an ordinary item keeps that item in the code stream, since rustc
//! compiles it.
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
use crate::authorship;
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
/// The coordinator's correction file for commits ALREADY ON MAIN whose `Co-Authored-By` trailer names the wrong model
/// (`parse_attributions`, docs/ci/PROOF-ROLES.md). The walk reads the listed model instead of the trailer for exactly those commits.
pub const ATTRIB: &str = "docs/ci/proof-roles-attributions.txt";
/// The coordinator's exemption records (`parse_exemptions`, docs/ci/PROOF-ROLES.md): a named approval for one unit's commit range,
/// for a cutoff-listed unit whose past no `pre-rule` record covers. Absent file: no exemptions.
pub const EXEMPT: &str = "docs/ci/proof-roles-exemptions.tsv";
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
/// not decide (`qwen3-opus-distill` is qwen). One family the amendment table (`amend::family`) does not carry is decided here, on the
/// NORMALISED name: `grok` is a word of it (`grok-4-7-high`, `xai-grok-3`) and nothing else a family knows answers. A word inside
/// another word is not the family (`groking` is `other`), and a name a known family already claims keeps that family.
pub fn family(model: &str) -> &'static str {
    match model {
        "-" => "-",
        "unknown" => "unknown",
        m => {
            let n = model_norm(m);
            if STRONG.contains(&n.as_str()) {
                "claude"
            } else {
                let f = amend::family(&n.replace("opus", "-").replace("fable", "-"));
                if f == "other" && n.split('-').any(|w| w == "grok") {
                    "grok"
                } else {
                    f
                }
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
    /// Authorship (docs/ci/PROOF-ROLES.md): the program rustc compiles, verifier off. Every executable token and data definition,
    /// a plain `fn` header up to its first clause (`fn_code_header`), and the content of a `//@omit` line at its position; gate
    /// attributes and the content under a gate nobody compiles are not code, and a `verification/` harness item is in neither stream.
    pub author_code: Vec<String>,
    /// Authorship: the statement text, the proof text, the gate and `//@omit` tags as keyed tokens, and what only the verifier reads.
    pub author_proof: String,
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

/// The `want` tokens in order, one run per stretch of them, `;;` between the runs. `skip` names tokens that are proof text standing
/// inside the code: a `proof!` block in the middle of an executable body says nothing about the code, and where the prover put it
/// says even less, so it neither joins a run nor cuts it.
fn runs(t: &[Tok], cls: &[Cls], want: Cls, skip: &[bool]) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut run: Vec<Tok> = Vec::new();
    for (k, x) in t.iter().enumerate() {
        if cls[k] == want {
            run.push(x.clone());
        } else if !skip.get(k).copied().unwrap_or(false) && !run.is_empty() {
            parts.push(spec::norm_tokens(&run));
            run.clear();
        }
    }
    if !run.is_empty() {
        parts.push(spec::norm_tokens(&run));
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

/// End (exclusive) of the delimited tree opened at `open` (`(`, `[` or `{`), or None when it never closes: an unclosed stretch is
/// not a complete tree, and a token stream that does not close is no one's proof block.
fn delim_end(t: &[Tok], open: usize) -> Option<usize> {
    let (o, c) = match t.get(open)?.s.as_str() {
        "(" => ("(", ")"),
        "[" => ("[", "]"),
        "{" => ("{", "}"),
        _ => return None,
    };
    let mut d = 0i32;
    for (k, x) in t.iter().enumerate().skip(open) {
        if x.s == o {
            d += 1;
        } else if x.s == c {
            d -= 1;
            if d == 0 {
                return Some(k + 1);
            }
        }
    }
    None
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

/// The attribute `t[a..e]` is exactly this token sequence and nothing else.
fn is_gate(t: &[Tok], a: usize, e: usize, want: &[&str]) -> bool {
    e - a == want.len() && t[a..e].iter().zip(want).all(|(x, w)| x.s == *w)
}

/// The gate of a ghost block, and nothing else: `#[cfg(verus_keep_ghost)]`.
fn ghost_gate(t: &[Tok], a: usize, e: usize) -> bool {
    is_gate(
        t,
        a,
        e,
        &["#", "[", "cfg", "(", "verus_keep_ghost", ")", "]"],
    )
}

/// The opposite gate, `#[cfg(not(verus_keep_ghost))]`: what it gates is compiled by the shipped build, so it is code.
fn unghost_gate(t: &[Tok], a: usize, e: usize) -> bool {
    is_gate(
        t,
        a,
        e,
        &[
            "#",
            "[",
            "cfg",
            "(",
            "not",
            "(",
            "verus_keep_ghost",
            ")",
            ")",
            "]",
        ],
    )
}

/// Is `proof` / `proof_with` here the bare name of a macro call, not the last segment of a path (`foo::proof!`) or a method
/// (`x.proof!`)? Only the bare name is Verus's ghost block.
fn bare_proof_name(t: &[Tok], k: usize) -> bool {
    k == 0 || !matches!(t[k - 1].s.as_str(), "::" | ".")
}

/// Does this file bring a macro of its own in under the name `proof` or `proof_with`? Then `proof!` is that macro and not Verus's
/// ghost block, and the file gets no special treatment for such calls. Seen here: a `macro_rules!` of that name, a rename to it
/// (`use m as proof`) and a plain import of it (`use foo::proof;`, `use foo::{proof, ..};`). A macro defined in another file that
/// reaches this one only by a glob (`use foo::*;`) cannot be told from Verus's own, so it is not treated as an own macro.
fn own_proof_macro(t: &[Tok]) -> bool {
    let n = t.len();
    let name = |x: &Tok| x.ident && matches!(x.s.as_str(), "proof" | "proof_with");
    for k in 0..n.saturating_sub(2) {
        if t[k].s == "macro_rules" && t[k + 1].s == "!" && name(&t[k + 2]) {
            return true;
        }
    }
    for k in 0..n {
        if !kwlike(&t[k], "use") {
            continue;
        }
        let mut j = k + 1;
        while j < n && t[j].s != ";" {
            // The name is bound here when it ends a path or a group element (`use foo::proof;`, `use foo::{proof, x};`), or when
            // something is renamed to it (`use foo::m as proof;`). `use proof::x;` binds `x`, and `use foo::proof as p;` binds `p`.
            if t[j].s == "as" && t.get(j + 1).is_some_and(name)
                || name(&t[j])
                    && matches!(t.get(j + 1).map_or("", |x| x.s.as_str()), ";" | "," | "}")
            {
                return true;
            }
            j += 1;
        }
    }
    false
}

/// Attribute-style proofs and gating (fix list 2 and 3 of the review): every steering attribute that no statement span took is PROOF
/// text (item-level `#[cfg(any())]` on a `mod`/`impl`, inner `#![cfg(..)]`, `#[cfg_attr(verus_keep_ghost, verus_spec(invariant ..))]`
/// on a loop), as are `proof! { }` / `proof_with! { }` blocks. Then FAIL CLOSED: a `verus_spec`/`verus_verify`/`verus_keep_ghost`
/// left in executable text is an error naming the place.
///
/// `ghost[k]` is set only where `t[k]` belongs to a closed `proof!`/`proof_with!` block with the exact `#[cfg(verus_keep_ghost)]`
/// gate directly in front of it: such a block is proof text whose PLACE in the code says nothing about the code, so it may not cut an
/// exec run either. An ungated block, or one behind another attribute, is classified as before: proof text that cuts the exec run.
fn attr_pass(path: &str, t: &[Tok], cls: &mut [Cls], ghost: &mut [bool], errors: &mut Vec<String>) {
    let n = t.len();
    let own = own_proof_macro(t);
    let mut k = 0;
    let mut last_attr: Option<(usize, usize)> = None;
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
                last_attr = Some((k, e.min(n)));
                k = e;
                continue;
            }
        } else if !own
            && cls[k] == Cls::Exec
            && (kwlike(&t[k], "proof") || kwlike(&t[k], "proof_with"))
            && bare_proof_name(t, k)
            && t.get(k + 1).is_some_and(|x| x.s == "!")
            && t.get(k + 2)
                .is_some_and(|x| matches!(x.s.as_str(), "{" | "(" | "["))
        {
            let end = delim_end(t, k + 2);
            let mut e = end.unwrap_or(n).min(n);
            if e < n && t[e].s == ";" {
                e += 1;
            }
            let e = e.min(n);
            // The attribute directly in front of a block that closes, if there is one. The two gates point opposite ways:
            // `#[cfg(verus_keep_ghost)]` makes the block ghost text, `#[cfg(not(verus_keep_ghost))]` puts it in the shipped build.
            let gate = match last_attr {
                Some((a, e0)) if end.is_some() && e0 == k => {
                    Some((a, ghost_gate(t, a, e0), unghost_gate(t, a, e0)))
                }
                _ => None,
            };
            match gate {
                // Ghost gate: the pair is proof text and invisible to the code stream, so adding, removing or moving it leaves
                // exec_fp alone. Only a block that closes is exempt: an unclosed one still swallows the rest of the file into
                // proof text as before, but exempts nothing from the code — what it hides there, it hides for good.
                Some((a, true, _)) => {
                    for c in cls.iter_mut().take(e).skip(k) {
                        *c = Cls::Proof;
                    }
                    for g in ghost.iter_mut().take(e).skip(a) {
                        *g = true;
                    }
                }
                // The not-gate: the block is compiled by the normal build, so it stays code through and through and an edit inside
                // it is a code change. The gate itself is proof text, being what switches Verus off.
                Some((_, false, true)) => {}
                // A bare block, or one behind another attribute: proof text whose place in the code still shows, as before.
                _ => {
                    for c in cls.iter_mut().take(e).skip(k) {
                        *c = Cls::Proof;
                    }
                }
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
    let mut ghost = vec![false; n];
    attr_pass(path, t, &mut cls, &mut ghost, &mut errors);
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
    let mut exec_text = runs(t, &cls, Cls::Exec, &ghost);
    // The lines Verus is told to skip: a `//@omit` tag at the end of a line. The tag decides what the verifier reads, so it is the
    // prover's choice and proof text (below, as a count and nothing else); what the tagged line says is shipped code that rustc does
    // read, so its tokens go back into the code stream at their source position (`author_streams`).
    let omit: Vec<(usize, Vec<Tok>)> = src
        .lines()
        .enumerate()
        .filter(|(_, l)| l.trim_end().ends_with("//@omit"))
        .map(|(ln, l)| (ln, spec::lex_omit_line(l, ln)))
        .collect();
    // Proof text is what the freeze has always frozen, and the shipped records and `proof-groups.tsv` baseline are digests of it:
    // every `//@omit` line goes in whole, `;; omit: ` and its normalised text, because a tag decides what Verus reads. The
    // authorship streams are the stricter pair (spec 39): the proof STREAM keys the tag alone, never the tagged content.
    let mut proof_text = runs(t, &cls, Cls::Proof, &[]);
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
    let (author_code, author_proof) = author_streams(path, t, &sc, &cls, &omit, &fn_text);
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
        author_code,
        author_proof,
        counts,
        errors,
    }
}

/// The predicate of a `cfg` or `cfg_attr` attribute: the words of its condition, without the outer brackets.
fn cfg_predicate(inner: &[Tok], name: &str) -> Vec<String> {
    let ni = inner.iter().position(|x| x.ident && x.s == name);
    let Some(mut k) = ni.map(|n| n + 1) else {
        return Vec::new();
    };
    while k < inner.len() && inner[k].s != "(" {
        k += 1;
    }
    let mut d = 0i32;
    let mut out: Vec<String> = Vec::new();
    for x in inner.iter().skip(k) {
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
        out.push(x.s.clone());
    }
    out
}

/// Rust's own test build: `cfg(test)`. Neither a choice of what the verifier reads nor a feature gate: attribute and item are code.
fn gate_is_rust_test(inner: &[Tok], name: &str) -> bool {
    name == "cfg" && cfg_predicate(inner, name) == ["test".to_string()]
}

/// A gate nobody ever compiles: `cfg(any())`, `cfg(false)`, `cfg(not(all()))`. What it gates is not code and only the verifier reads it.
fn gate_is_dead(inner: &[Tok], name: &str) -> bool {
    let words = cfg_predicate(inner, name);
    let p: Vec<&str> = words.iter().map(String::as_str).collect();
    p == ["false"] || p == ["any", "(", ")"] || p == ["not", "(", "all", "(", ")", ")"]
}

/// The cfg names Verus switches on and plain rustc never defines.
const VERUS_CFGS: &[&str] = &["verus_keep_ghost", "verus"];

/// Can a `cfg` predicate hold for plain rustc, with the Verus cfgs off? Every other term (`test`, `feature = ".."`, `target_os = ".."`,
/// `debug_assertions`) may hold in some build, so only a predicate that needs a Verus cfg is never compiled. A predicate that does not
/// parse is not compiled either: the stream fails closed on the safe side.
fn cfg_holds_for_rustc(p: &[&str]) -> bool {
    fn expect_close(p: &[&str], i: &mut usize) {
        if p.get(*i) == Some(&")") {
            *i += 1;
        }
    }
    fn list(p: &[&str], i: &mut usize, conn: &str) -> bool {
        let or = conn == "any";
        let mut acc = !or;
        while *i < p.len() && p[*i] != ")" {
            if p[*i] == "," {
                *i += 1;
                continue;
            }
            let v = atom(p, i);
            acc = if or { acc || v } else { acc && v };
        }
        if conn == "not" { !acc } else { acc }
    }
    fn atom(p: &[&str], i: &mut usize) -> bool {
        let Some(w) = p.get(*i).copied() else {
            return false;
        };
        *i += 1;
        match w {
            "(" => {
                let v = list(p, i, "all");
                expect_close(p, i);
                v
            }
            "not" | "all" | "any" => {
                if p.get(*i) != Some(&"(") {
                    return false;
                }
                *i += 1;
                let v = list(p, i, w);
                expect_close(p, i);
                v
            }
            "true" => true,
            "false" => false,
            _ => {
                if p.get(*i) == Some(&"=") {
                    *i += 2;
                }
                !VERUS_CFGS.contains(&w)
            }
        }
    }
    let mut i = 0usize;
    let mut acc = true;
    while i < p.len() {
        if p[i] == "," {
            i += 1;
            continue;
        }
        acc &= atom(p, &mut i);
    }
    acc
}

/// The gate of roles 1.1.3: an item or block that only `cfg(verus_keep_ghost)` or `cfg(verus)` compiles. rustc drops it, so what it
/// gates is not part of the program the code stream holds: moving shipped code under such a gate removes its code tokens.
fn gate_is_verus_only(inner: &[Tok], name: &str) -> bool {
    if name != "cfg" {
        return false;
    }
    let words = cfg_predicate(inner, name);
    let p: Vec<&str> = words.iter().map(String::as_str).collect();
    !p.is_empty() && !cfg_holds_for_rustc(&p)
}

/// The name of the item an attribute attaches to: the first identifier after the item keyword (`#[cfg(test)] mod t {` -> `t`).
fn attributed_item_name(t: &[Tok], after: usize) -> Option<String> {
    const MODIFIERS: [&str; 9] = [
        "pub", "crate", "in", "mut", "unsafe", "async", "const", "extern", "default",
    ];
    const ITEMS: [&str; 13] = [
        "fn",
        "struct",
        "enum",
        "union",
        "trait",
        "impl",
        "type",
        "mod",
        "macro",
        "macro_rules",
        "static",
        "use",
        "let",
    ];
    let mut k = after;
    loop {
        while k < t.len() && t[k].ident && MODIFIERS.contains(&t[k].s.as_str()) {
            k += 1;
        }
        if k < t.len() && t[k].s == "(" {
            k = delim_end(t, k).unwrap_or(t.len());
            continue;
        }
        break;
    }
    if k >= t.len() {
        return None;
    }
    if t[k].ident && ITEMS.contains(&t[k].s.as_str()) {
        k += 1;
        if k < t.len() && t[k].s == "<" {
            let mut d = 0i32;
            while k < t.len() {
                match t[k].s.as_str() {
                    "<" | "<<" => d += 1,
                    ">>" => {
                        d -= 2;
                        if d <= 0 {
                            k += 1;
                            break;
                        }
                    }
                    ">" => {
                        d -= 1;
                        if d <= 0 {
                            k += 1;
                            break;
                        }
                    }
                    _ => {}
                }
                k += 1;
            }
        }
        if k < t.len() && t[k].ident {
            return Some(t[k].s.clone());
        }
        return None;
    }
    t.get(k).filter(|x| x.ident).map(|x| x.s.clone())
}

/// The harness items of a `verification/` file: inner attributes, `#[path = ..] mod x;`, the `use` of `vstd`/`builtin`/
/// `builtin_macros` and an empty `fn main() {}`. They compute nothing, so they are in neither stream (PROOF-ROLES.md).
fn harness_veto(t: &[Tok], veto: &mut [bool]) {
    let n = t.len();
    let mut k = 0;
    let mut depth = 0i32;
    while k < n {
        match t[k].s.as_str() {
            "(" | "[" => depth += 1,
            ")" | "]" => depth -= 1,
            "{" => {
                depth += 1;
                k += 1;
                continue;
            }
            "}" => depth -= 1,
            _ => {}
        }
        if depth != 0 {
            k += 1;
            continue;
        }
        if t[k].s == "#" {
            if let Some(e) = spec::attr_end(t, k) {
                let e = e.min(n).max(k + 1);
                let inner = &t[k..e];
                let inner_attr = t.get(k + 1).is_some_and(|x| x.s == "!");
                let name = inner.iter().find(|x| x.ident).map_or("", |x| x.s.as_str());
                if inner_attr || name == "path" {
                    for x in veto.iter_mut().take(e) {
                        *x = true;
                    }
                    if name == "path" {
                        k = semi_end(t, e, n);
                        for x in veto.iter_mut().take(k.min(n)).skip(e) {
                            *x = true;
                        }
                        continue;
                    }
                }
                k = e;
                continue;
            }
        } else if t[k].ident && t[k].s == "use" {
            let root = t
                .get(k + 1)
                .filter(|x| x.ident)
                .map_or("", |x| x.s.as_str());
            if matches!(root, "vstd" | "builtin" | "builtin_macros") {
                let e = semi_end(t, k + 1, n);
                for x in veto.iter_mut().take(e.min(n)).skip(k) {
                    *x = true;
                }
                k = e;
                continue;
            }
        } else if t[k].ident
            && t[k].s == "fn"
            && t.get(k + 1).is_some_and(|x| x.s == "main")
            && t.get(k + 2).is_some_and(|x| x.s == "(")
            && t.get(k + 3).is_some_and(|x| x.s == ")")
            && t.get(k + 4).is_some_and(|x| x.s == "{")
            && t.get(k + 5).is_some_and(|x| x.s == "}")
        {
            for x in veto.iter_mut().take(k + 6).skip(k) {
                *x = true;
            }
            k += 6;
            continue;
        }
        k += 1;
    }
}

/// The two authorship streams of one file version (docs/ci/PROOF-ROLES.md): the code rustc compiles with the verifier off, and the
/// text only the verifier reads, with each `//@omit` tag and each gate keyed by the item it attaches to.
fn author_streams(
    path: &str,
    t: &[Tok],
    sc: &spec::Scanned,
    cls: &[Cls],
    omit: &[(usize, Vec<Tok>)],
    fn_text: &str,
) -> (Vec<String>, String) {
    let n = t.len();
    let mut not_code = vec![false; n];
    let mut only_verifier = vec![false; n];
    let mut neither = vec![false; n];
    let mut keys: Vec<String> = Vec::new();
    // Gates: the attribute is the prover's choice (proof), the item under it is code, unless nobody compiles the gate at all.
    let mut k = 0;
    while k < n {
        if t[k].s == "#"
            && let Some(e) = spec::attr_end(t, k)
        {
            let e = e.min(n).max(k + 1);
            let inner = &t[k..e];
            let name = inner.iter().find(|x| x.ident).map_or("", |x| x.s.as_str());
            if matches!(name, "cfg" | "cfg_attr")
                && !(gate_is_rust_test(inner, name) && cls[k] != Cls::Proof)
            {
                let dead = gate_is_dead(inner, name) || gate_is_verus_only(inner, name);
                let (lo, hi) = if t.get(k + 1).is_some_and(|x| x.s == "!") {
                    (0, n)
                } else {
                    gated_extent(t, e)
                };
                for j in k..e {
                    not_code[j] = true;
                    only_verifier[j] = true;
                }
                if dead {
                    for j in lo..hi.min(n) {
                        not_code[j] = true;
                        only_verifier[j] = true;
                    }
                }
                let item =
                    attributed_item_name(t, lo).unwrap_or_else(|| format!("line{}", t[k].line + 1));
                keys.push(format!("gate@{name} {item}"));
            }
            k = e;
            continue;
        }
        k += 1;
    }
    // A ghost, spec or proof item inside a `verus!` block: the verifier reads it, rustc never does.
    for sp in &sc.spans {
        let hi = sp.hi.min(n);
        if sp.kind == "item"
            && sp.lo < hi
            && sc.in_verus.get(sp.lo).is_some_and(|b| *b)
            && (sp.lo..hi.min(sp.lo + 5)).any(|j| {
                t[j].ident && matches!(t[j].s.as_str(), "ghost" | "spec" | "proof" | "tracked")
            })
        {
            for j in sp.lo..hi {
                not_code[j] = true;
                only_verifier[j] = true;
            }
        }
    }
    // `//@omit` tags, keyed by the item they attach to: the tag is the prover's choice, the tagged line is shipped code.
    for (ln, toks) in omit {
        // the item the tagged attribute attaches to: the first item keyword on a later line, then its name. An inner attribute
        // hides no item, and a line that is no attribute attaches to none: the key is then `omit@? <line>`.
        let inner =
            toks.first().is_some_and(|x| x.s == "#") && toks.get(1).is_some_and(|x| x.s == "!");
        let kind = toks.iter().find(|x| x.ident).map_or("?", |x| x.s.as_str());
        let mut name = String::new();
        if !inner {
            for j in 0..n {
                if t[j].line > *ln
                    && t[j].ident
                    && matches!(
                        t[j].s.as_str(),
                        "fn" | "struct"
                            | "enum"
                            | "union"
                            | "trait"
                            | "impl"
                            | "mod"
                            | "const"
                            | "static"
                            | "type"
                            | "macro"
                            | "macro_rules"
                    )
                {
                    name = (j + 1..n)
                        .find(|m| t[*m].ident)
                        .map_or(String::new(), |m| t[m].s.clone());
                    break;
                }
            }
        }
        if name.is_empty() {
            keys.push(format!("omit@? {}", ln + 1));
        } else {
            keys.push(format!("omit@{kind} {name}"));
        }
    }
    if path.starts_with("verification/") {
        harness_veto(t, &mut neither);
    }
    // The code stream: Exec tokens and data definitions with the vetoes applied, a fn header without its clauses or gate
    // attribute, and the `//@omit` content at its position.
    let by_lo: HashMap<usize, usize> = sc
        .spans
        .iter()
        .enumerate()
        .filter(|(_, sp)| matches!(sp.kind, "fn" | "item"))
        .map(|(i, sp)| (sp.lo, i))
        .collect();
    let mut oi = 0usize;
    let mut out: Vec<String> = Vec::new();
    let mut k = 0;
    while k < n {
        if let Some(&i) = by_lo.get(&k) {
            let sp = &sc.spans[i];
            let hi = sp.hi.min(n).max(k + 1);
            if sp.kind == "fn" {
                let mut merged: Vec<Tok> = Vec::new();
                let mut in_v: Vec<bool> = Vec::new();
                for (x, &iv) in t[k..hi].iter().zip(&sc.in_verus[k..hi]) {
                    while oi < omit.len() && omit[oi].0 < x.line {
                        for o in &omit[oi].1 {
                            merged.push(o.clone());
                            in_v.push(false);
                        }
                        oi += 1;
                    }
                    merged.push(x.clone());
                    in_v.push(iv);
                }
                out.extend(fn_code_header(&merged, &in_v));
            } else {
                for j in k..hi {
                    while oi < omit.len() && omit[oi].0 < t[j].line {
                        out.extend(omit[oi].1.iter().map(|x| x.s.clone()));
                        oi += 1;
                    }
                    if !not_code[j] && !neither[j] {
                        out.push(t[j].s.clone());
                    }
                }
            }
            k = hi;
            continue;
        }
        if cls[k] == Cls::Exec && !not_code[k] && !neither[k] {
            while oi < omit.len() && omit[oi].0 < t[k].line {
                out.extend(omit[oi].1.iter().map(|x| x.s.clone()));
                oi += 1;
            }
            out.push(t[k].s.clone());
        }
        k += 1;
    }
    for (_, v) in &omit[oi..] {
        out.extend(v.iter().map(|x| x.s.clone()));
    }
    // The proof stream: what the freeze classes as proof plus what only the verifier reads, in source order as ONE token list, then
    // the statement texts and the keys. Where a run stops is layout, not authorship: splitting the list per run would let a commit
    // that only adds code between two proof regions be credited with writing proof text (the errseq case (a)).
    let mut pt: Vec<Tok> = Vec::new();
    let mut k = 0;
    while k < n {
        if (cls[k] == Cls::Proof || only_verifier[k]) && !neither[k] {
            let a = k;
            while k < n && (cls[k] == Cls::Proof || only_verifier[k]) && !neither[k] {
                k += 1;
            }
            pt.extend(t[a..k].iter().cloned());
        } else {
            k += 1;
        }
    }
    let mut proof = spec::norm_tokens(&pt);
    proof.push('\n');
    proof.push_str(fn_text);
    if !keys.is_empty() {
        proof.push_str(&keys.join("\n"));
        proof.push('\n');
    }
    (norm_stream(out), proof)
}

/// The stream normalisation of the freeze lexer on a token list (docs/ci/PROOF-ROLES.md): a comma before a closing bracket is
/// layout, and the `doc`/`allow`/`inline` style attributes are not part of what a model wrote.
fn norm_stream(v: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(v.len());
    let mut k = 0usize;
    while k < v.len() {
        if v[k] == "#" {
            let mut j = k + 1;
            if v.get(j).map(String::as_str) == Some("!") {
                j += 1;
            }
            if v.get(j).map(String::as_str) == Some("[")
                && spec::is_ignored_attr(v.get(j + 1).map(String::as_str).unwrap_or(""))
            {
                let mut d = 0i32;
                let mut m = j;
                while m < v.len() {
                    match v[m].as_str() {
                        "[" => d += 1,
                        "]" => {
                            d -= 1;
                            if d == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    m += 1;
                }
                k = m + 1;
                continue;
            }
        }
        if v[k] == ","
            && v.get(k + 1)
                .is_none_or(|x| matches!(x.as_str(), "}" | ")" | "]"))
        {
            k += 1;
            continue;
        }
        out.push(v[k].clone());
        k += 1;
    }
    out
}

// ------------------------------------------------------------------------------------------------------- unit fingerprints

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Fp {
    pub stmt: String,
    pub proof: String,
    pub exec: String,
    /// Data definitions (struct, enum, const, type) that a frozen statement refers to; `-` for model units.
    pub data: String,
}

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

pub(crate) fn model_text(path: &str, src: &str) -> String {
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
            for f in &files {
                let Some(c) = cache.classed(f) else { continue };
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
                for (n, x) in counts.iter_mut().zip(c.counts) {
                    *n += x;
                }
            }
            // A data definition that no frozen statement refers to (directly or through another referred item, by name) is
            // code; one that a statement refers to is data, rechecked by a strong prover when it changes.
            let referenced = referenced_items(&all_items, &fn_texts);
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
    /// The authorship walk of this run: who shaped the code and who shaped the statement and proof text of every unit over
    /// `LB..HEAD` by content (`authorship.rs`, docs/ci/PROOF-ROLES.md).
    pub authors: &'a authorship::Walk,
    /// Verdict files present at the base that are modified or gone now.
    pub verdict_modified: &'a [String],
    pub merge_gate: bool,
    /// `docs/ci/proof-roles-cutoff.txt` at the base and now (None when absent).
    pub cutoff_work: Option<&'a str>,
    /// Cutoff and exemption files (`docs/ci/proof-roles-cutoff*.txt`, `docs/ci/proof-roles-exemptions*.tsv`) created, changed or
    /// deleted in the range: both belong to the coordinator.
    pub cutoff_changed: &'a [String],
    /// `docs/ci/proof-roles-exemptions.tsv` now (None when absent, which is an empty file).
    pub exempt_work: Option<&'a str>,
    /// Messages of `cutoff_gaps`: a listed unit whose exempt past reaches the walk with neither a record nor an exemption.
    pub cutoff_gaps: &'a [String],
    /// Messages of `cutoff_acceptances`: a listed unit whose walked past is dropped by an exemption line and NOT by a `pre-rule`
    /// record. The check prints these as an acceptance, never as a clean rule (review item 5).
    pub cutoff_accepts: &'a [String],
    /// Messages of `revived_exemptions`: a listing or line whose commit was out of the range base's history and is in HEAD's, so
    /// this range's merges woke an approval the coordinator wrote for another branch (review 4, weakness 2).
    pub exemption_revived: &'a [String],
    /// `LAGENT_ALLOW_SPEC_CHANGE` carries the explicit token `cutoff:<why>`.
    pub allow_cutoff: bool,
    /// Does this commit exist in the repository's object database (`git cat-file -e <c>^{commit}`)? A `started_before` that does
    /// not exist cannot be used to exempt anything.
    pub exists: &'a dyn Fn(&str) -> bool,
    /// Is the first commit an ancestor of the second (`git merge-base --is-ancestor`)? An exemption line whose `from_commit` is
    /// not an ancestor of its `to_commit` names no stretch of history and exempts the whole past instead (review 2, New 3).
    pub ancestor: &'a dyn Fn(&str, &str) -> bool,
    /// Messages of `//@omit` derive lines that rustfmt `merge_derives` would merge with a neighbouring derive (see `omit_hazards`).
    pub omit_hazards: &'a [String],
    /// Messages of `//@omit` tags on a line that is not an attribute: the tag may only hide an attribute, never executable code.
    pub omit_bad_tags: &'a [String],
    /// Registration files (`REGISTRATION_FILES`) whose change in the range is only the registration of units whose files are new:
    /// they do not count as an extractor change (`registration_only`).
    pub registration_exempt: &'a [String],
    /// Extractor sources (spec.rs, roles.rs, ...) changed in the range, and files of any unit's closure changed in the range.
    pub extractor_changed: &'a [String],
    pub unit_files_changed: &'a [String],
    /// `LAGENT_ALLOW_SPEC_CHANGE` names `extractor`.
    pub allow_extractor: bool,
    /// The attribution-correction file and what the check needs to validate it (`None`: not checked at all).
    pub attrib: Option<&'a AttribCtx<'a>>,
}

/// The commit the history-aware cross-model rule walks from: the id on line 1 of the cutoff list (`cutoff <commit>`, the commit that
/// introduced `roles` on main) when the list parses, else this constant.
pub const HISTORY_LOWER_BOUND: &str = "ebc0a4efd5371645bad2cae439655c1223d7fd56";

pub fn history_lower_bound(cutoff_text: Option<&str>) -> String {
    cutoff_text
        .and_then(|t| parse_cutoff(t).ok())
        .map_or_else(|| HISTORY_LOWER_BOUND.to_string(), |c| c.commit)
}

/// Everything `check_attributions` needs about `docs/ci/proof-roles-attributions.txt`.
pub struct AttribCtx<'a> {
    /// The file now (HEAD at the merge gate) and at the base; `None` when absent.
    pub work: Option<&'a str>,
    pub base: Option<&'a str>,
    /// Attribution files (`docs/ci/proof-roles-attributions*.txt`) created, changed or deleted in the range.
    pub changed: &'a [String],
    /// `LAGENT_ALLOW_SPEC_CHANGE` carries `attribution:<reason>` / `attribution-strong:<reason>`.
    pub allow: bool,
    pub allow_strong: bool,
    /// Is this commit an ancestor of the base commit (already on main before this merge)? False for a missing commit.
    pub ancestor_of_base: &'a dyn Fn(&str) -> bool,
    /// Is this commit a merge commit?
    pub is_merge: &'a dyn Fn(&str) -> bool,
    /// The trailer models of the commit as its message carries them, without any correction.
    pub raw_models: &'a dyn Fn(&str) -> Vec<String>,
}

/// One line of the attribution file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attribution {
    pub commit: String,
    /// Normalised (`model_norm`).
    pub model: String,
    pub reason: String,
}

/// The coordinator-owned correction file: lines `<full 40 lowercase hex commit><TAB><model><TAB><reason>`, `#` comments and blank
/// lines allowed. Any malformed line, short or upper-case id, empty model or reason, model of unknown class (neither strong nor
/// builder) or a commit named twice (a commit is corrected ONCE) makes the whole file invalid.
pub fn parse_attributions(text: &str) -> Result<Vec<Attribution>, String> {
    let mut out: Vec<Attribution> = Vec::new();
    for (n, l) in text.lines().enumerate() {
        if l.trim().is_empty() || l.trim_start().starts_with('#') {
            continue;
        }
        let f: Vec<&str> = l.splitn(3, '\t').collect();
        if f.len() != 3 || f.iter().any(|x| x.trim().is_empty()) {
            return Err(format!(
                "line {}: `{l}`: expected commit<TAB>model<TAB>reason, none empty",
                n + 1
            ));
        }
        if !full_id(f[0]) {
            return Err(format!(
                "line {}: commit `{}` must be a full 40-digit lowercase hex id, not an abbreviation, upper case or a name",
                n + 1,
                f[0]
            ));
        }
        let model = model_norm(f[1]);
        if class(&model) == "unknown" {
            return Err(format!(
                "line {}: model `{}` is of unknown class (neither a strong model nor a builder)",
                n + 1,
                f[1]
            ));
        }
        if out.iter().any(|a| a.commit == f[0]) {
            return Err(format!(
                "line {}: commit {} is corrected twice: a commit may be corrected ONCE",
                n + 1,
                f[0]
            ));
        }
        out.push(Attribution {
            commit: f[0].to_string(),
            model,
            reason: f[2].trim().to_string(),
        });
    }
    Ok(out)
}

/// Is `c` (a full id) an existing commit that is an ancestor of `base`?
pub fn is_ancestor_of(root: &Path, c: &str, base: &str) -> bool {
    commit_exists(root, c)
        && Command::new("git")
            .args(["merge-base", "--is-ancestor", c, base])
            .current_dir(root)
            .output()
            .is_ok_and(|o| o.status.success())
}

/// Does `c` have more than one parent?
pub fn is_merge_commit(root: &Path, c: &str) -> bool {
    git1(root, &["rev-list", "--parents", "-n", "1", c])
        .is_some_and(|l| l.split_whitespace().count() > 2)
}

/// Is `path` one of the protected attribution files?
pub fn is_attrib_path(p: &str) -> bool {
    p == ATTRIB || (p.starts_with("docs/ci/proof-roles-attributions-") && p.ends_with(".txt"))
}

/// The commit -> model map the walk applies, for a file that parses; empty for an absent or invalid file (the check refuses the latter).
pub fn attribution_map(text: Option<&str>) -> BTreeMap<String, String> {
    text.and_then(|t| parse_attributions(t).ok())
        .map(|v| v.into_iter().map(|a| (a.commit, a.model)).collect())
        .unwrap_or_default()
}

/// Validate the attribution file against the repository and the allow tokens; pushes refusals and the `used` note.
fn check_attributions(a: &AttribCtx, o: &mut Outcome) {
    let refuse = |o: &mut Outcome, rule: &str, detail: String, hint: &str| {
        o.refusals.push(Refusal {
            unit: "-".into(),
            rule: rule.into(),
            detail,
            hint: hint.into(),
        });
    };
    if !a.changed.is_empty() && !a.allow {
        refuse(
            o,
            "attribution-protected",
            format!(
                "the range creates, changes or deletes {}: the corrections of wrongly stamped commits belong to the coordinator",
                a.changed.join(", ")
            ),
            "do not touch it in a branch; the coordinator lands it with LAGENT_ALLOW_SPEC_CHANGE naming `attribution:<why>`",
        );
    }
    let mut used = 0usize;
    if let Some(text) = a.work {
        match parse_attributions(text) {
            Err(e) => refuse(
                o,
                "attribution-malformed",
                format!("{ATTRIB}: {e}"),
                "fix the file (the whole file is invalid until every line parses)",
            ),
            Ok(list) => {
                used = list.len();
                let base_lines: Vec<&str> = a.base.map(|b| b.lines().collect()).unwrap_or_default();
                let work_lines: Vec<&str> = text.lines().collect();
                let prefix_ok = base_lines.len() <= work_lines.len()
                    && base_lines.iter().zip(&work_lines).all(|(b, w)| b == w);
                if !prefix_ok {
                    refuse(
                        o,
                        "attribution-not-append-only",
                        format!(
                            "{ATTRIB} does not keep every line of the base copy byte-identical and in order"
                        ),
                        "only append new lines at the end; a correction is never edited or removed",
                    );
                }
                let base_set: BTreeSet<&str> = base_lines.iter().copied().collect();
                for at in &list {
                    if !(a.ancestor_of_base)(&at.commit) {
                        refuse(
                            o,
                            "attribution-not-on-base",
                            format!(
                                "{}: not an ancestor of the base commit (a commit of this range, a commit that does not exist, or one not on main yet): only commits already on main can be corrected",
                                at.commit
                            ),
                            "a branch cannot re-attribute its own commits; correct a commit only after it landed on main",
                        );
                    } else if (a.is_merge)(&at.commit) {
                        // a merge commit carries no trailer of a model that shaped content, so a correction of one is allowed and
                        // matters only where the merge itself authored a stream: it resolved a conflict, or it dropped a side
                        o.notes.push(format!(
                            "{}: a merge commit is corrected: it names who authored the merge's own resolution",
                            at.commit
                        ));
                    }
                    let is_new = !text
                        .lines()
                        .find(|l| l.starts_with(&at.commit))
                        .is_some_and(|l| base_set.contains(l));
                    let raw = (a.raw_models)(&at.commit);
                    if is_new
                        && class(&at.model) == "strong"
                        && !raw.iter().any(|m| class(m) == "strong")
                    {
                        if a.allow_strong {
                            o.notes.push(format!(
                                "ATTRIBUTION-STRONG: {} is re-attributed to the STRONG model {} although its trailer names {:?} (allowed by `attribution-strong:`): {}",
                                at.commit, at.model, raw, at.reason
                            ));
                        } else {
                            refuse(
                                o,
                                "attribution-strong",
                                format!(
                                    "{}: re-attributed to the STRONG model {} although its trailer names {:?}: a builder's commit would count as strong work",
                                    at.commit, at.model, raw
                                ),
                                "adding such a line needs LAGENT_ALLOW_SPEC_CHANGE naming `attribution-strong:<why>` (the coordinator's decision)",
                            );
                        }
                    }
                }
            }
        }
    } else if a.base.is_some() {
        refuse(
            o,
            "attribution-not-append-only",
            format!("{ATTRIB} existed at the base and is gone now"),
            "restore the file from the base commit",
        );
    }
    o.notes
        .push(format!("attribution corrections used: {used}"));
}

/// The non-merge commits of base..HEAD that changed one unit's fingerprints, with their normalised `Co-Authored-By` models. A merge
/// commit is compared with the automatic merge of its parents (`git merge-tree`): what it adds by hand is its own authorship.
#[derive(Default, Clone, Debug)]
pub struct ChangeInfo {
    /// Trailers of each commit whose CODE STREAM (the program rustc reads) moved, i.e. that wrote code.
    pub exec: Vec<Vec<String>>,
    /// (commit, models) of each commit in the range whose PROOF STREAM (statement and proof text, tags and gates) moved. This is
    /// what says "someone wrote proof text here", not the fingerprints: a proof edit that moves neither statement_fp nor proof_fp
    /// is still an edit of the proof, and the builder rule reads it either way.
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
/// then one line `unit<TAB>started_before_commit<TAB>reason` per unit that may ever get a `pre-rule` record. `exempt` is the
/// coordinator's other file (`docs/ci/proof-roles-exemptions.tsv`), the named approvals for the listed units whose past no record
/// covers; it travels with the list because a line there is only meaningful together with its listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cutoff {
    pub commit: String,
    pub units: Vec<(String, String, String)>,
    pub exempt: Vec<Exemption>,
}

/// One line of the coordinator's exemption file: a named approval that covers the past of one unit up to and including `to`,
/// which is the commit the cutoff list fixes for it. `from` names the unit's first commit, the origin of the range. The two author
/// fields are what the trailers and the ledger show for the covered range, or `not established` where history shows nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exemption {
    pub unit: String,
    pub from: String,
    pub to: String,
    pub code_author: String,
    pub statement_author: String,
    pub approver: String,
    pub approved_on: String,
    pub note: String,
}

pub const NOT_ESTABLISHED: &str = "not established";
const EXEMPT_FIELDS: usize = 8;

/// The exemption file of the coordinator: eight tab-separated fields, the two commit ids whole hex, the date `YYYY-MM-DD`.
/// A malformed line is refused, not skipped: a line nobody can read exempts nobody. `listed` is the cutoff list's unit and
/// `started_before` (from `parse_cutoff`): a line that names no listed unit, or names one at a commit other than the commit the
/// list fixes for it, exempts nobody either and is refused. The list is the permission; without it an approval has nothing to
/// stand on.
///
/// The range is read as history too (`review 2, New 3`): `exists` says whether a commit is an object of this repository,
/// `ancestor(from, to)` whether `from` is an ancestor of `to`. A line whose `from_commit` is a sibling of `to_commit` names no
/// stretch of history at all, and the walk would drop every ancestor of `to_commit` back to the lower bound: an approval of three
/// commits would exempt the whole life of the unit. Such a line is refused exactly like a malformed one, with its line number.
pub fn parse_exemptions(
    text: &str,
    listed: &BTreeMap<String, String>,
    exists: &dyn Fn(&str) -> bool,
    ancestor: &dyn Fn(&str, &str) -> bool,
) -> Result<Vec<Exemption>, String> {
    let mut out: Vec<Exemption> = Vec::new();
    // The number in every message below is the LINE OF THE FILE, comments and blank lines counted: a coordinator who is told
    // "line 17" finds it with `grep -n` (review 3, weakness 5).
    for (n, l) in text.lines().enumerate() {
        if l.trim().is_empty() || l.trim_start().starts_with('#') {
            continue;
        }
        let f: Vec<&str> = l.split('\t').collect();
        if f.len() != EXEMPT_FIELDS || f.iter().any(|x| x.trim().is_empty()) {
            return Err(format!(
                "line {}: `{l}`: expected unit<TAB>from_commit<TAB>to_commit<TAB>code_author<TAB>statement_author<TAB>approver<TAB>approved_on<TAB>note, {EXEMPT_FIELDS} fields, none empty",
                n + 1
            ));
        }
        for (name, v) in [("from_commit", f[1]), ("to_commit", f[2])] {
            if !full_id(v) {
                return Err(format!(
                    "line {}: {name} `{v}` must be a full 40-digit lowercase hex commit id, not an abbreviation or a name",
                    n + 1
                ));
            }
        }
        if !date_ok(f[6]) {
            return Err(format!(
                "line {}: approved_on `{}` is not YYYY-MM-DD",
                n + 1,
                f[6]
            ));
        }
        // Both commits must be objects here, and `from_commit` must lie in the past of `to_commit`: otherwise the line names no
        // range, and what it exempts is everything up to `to_commit`.
        for (name, v) in [("from_commit", f[1]), ("to_commit", f[2])] {
            if !exists(v) {
                return Err(format!(
                    "line {}: {name} `{v}` is not a commit in this repository: fetch the branch that holds it, or ask the coordinator to correct the line",
                    n + 1
                ));
            }
        }
        if !ancestor(f[1], f[2]) {
            return Err(format!(
                "line {}: from_commit {} is not an ancestor of to_commit {}: an exemption covers one stretch of history, from the unit's first commit up to the commit the list fixes for it, and a line that names no such stretch exempts nothing",
                n + 1,
                authorship::short(f[1]),
                authorship::short(f[2]),
            ));
        }
        // The line stands on the coordinator's list: it names a unit the list carries, at the very commit the list fixes for it.
        // Without that the approval exempts nothing, so it is refused rather than ignored (review 5).
        if listed.get(f[0]).map(String::as_str) != Some(f[2]) {
            return Err(format!(
                "line {}: `{}` exempts no cutoff listing - {CUTOFF} names no unit `{}` with started_before `{}`; only the past of a listed unit may be approved away, and only at the commit the list fixes for it",
                n + 1,
                f[0],
                f[0],
                f[2]
            ));
        }
        if out
            .iter()
            .any(|e| e.unit == f[0] && e.to == f[2] && e.from == f[1])
        {
            return Err(format!(
                "line {}: `{}` names the same unit twice for the range ending at {}",
                n + 1,
                f[0],
                f[2]
            ));
        }
        out.push(Exemption {
            unit: f[0].to_string(),
            from: f[1].to_string(),
            to: f[2].to_string(),
            code_author: f[3].to_string(),
            statement_author: f[4].to_string(),
            approver: f[5].to_string(),
            approved_on: f[6].to_string(),
            note: f[7].to_string(),
        });
    }
    Ok(out)
}

/// What the walk drops as the unit's exempt past: the `started_before` of its valid `pre-rule` record, else the `to_commit` of the
/// exemption the coordinator named for the cutoff list's own line, else nothing. Either line withdrawn revokes the exemption:
/// a cutoff listing alone, with no record and no matching exemption, exempts nobody (review 5, scenario N). The exemption is
/// anchored at `to_commit`, so the unit's creating commit `from` lies inside the covered past, and the first commit AFTER `to`
/// that changes the unit's code or statements falls under the full rule.
pub fn exempt_past<'a>(
    unit: &str,
    pre: &'a BTreeMap<String, String>,
    listed: &'a BTreeMap<String, String>,
    exempt: &'a [Exemption],
) -> Option<(&'a str, Option<&'a str>)> {
    if let Some(s) = pre.get(unit) {
        return Some((s.as_str(), None));
    }
    let sb = listed.get(unit)?;
    exempt
        .iter()
        .find(|e| e.unit == unit && &e.to == sb)
        .map(|e| (sb.as_str(), Some(e.from.as_str())))
}

/// The units the coordinator's list carries, each with the commit it fixes for it: what an exemption line must name to have any
/// force. No list, nothing to exempt.
pub fn listed_starts(cut: Option<&Cutoff>) -> BTreeMap<String, String> {
    cut.map(|c| {
        c.units
            .iter()
            .map(|(u, sb, _)| (u.clone(), sb.clone()))
            .collect()
    })
    .unwrap_or_default()
}

/// Review 5, scenario N: the coordinator's list is a permission to write a record, not an exemption of its own. A listed unit
/// whose past reaches the walk (`reaches_walk` judges the commit: it exists here and is inside `LB..HEAD`) needs either a valid
/// `pre-rule` record or an exemption line naming the very commit the list fixes for it. Absent the exemption file, nothing exempts.
pub fn cutoff_gaps(
    cut: &Cutoff,
    pre: &BTreeMap<String, String>,
    reaches_walk: &dyn Fn(&str) -> bool,
) -> Vec<String> {
    cut.units
        .iter()
        .filter(|(u, _, _)| !pre.contains_key(u))
        .filter(|(_, sb, _)| reaches_walk(sb))
        .filter(|(u, sb, _)| !cut.exempt.iter().any(|e| &e.unit == u && &e.to == sb))
        .map(|(u, sb, _)| {
            format!(
                "`{u}` is listed in {CUTOFF} with started_before {}, inside the walked range, and no pre-rule record or exemption \
                 line of {EXEMPT} covers it: the listing alone exempts nothing, so the past of the unit is walked and judged by \
                 the full rule",
                authorship::short(sb)
            )
        })
        .collect()
}

/// What an exemption line, rather than a `pre-rule` record, is doing for a listed unit: the past is ACCEPTED by a name, it is not
/// shown to satisfy the rule. `roles --check` says so for every unit whose walked past reaches the walk and is dropped this way, so
/// an exemption can never read as a clean pass (review item 5). The two author fields are quoted as the file records them; where
/// history shows nothing they say `not established`, and no document may read role separation into either.
pub fn cutoff_acceptances(
    cut: &Cutoff,
    pre: &BTreeMap<String, String>,
    reaches_walk: &dyn Fn(&str) -> bool,
) -> Vec<String> {
    cut.units
        .iter()
        .filter(|(u, _, _)| !pre.contains_key(u))
        .filter(|(_, sb, _)| reaches_walk(sb))
        .filter_map(|(u, sb, _)| {
            let e = cut.exempt.iter().find(|e| e.unit == *u && e.to == *sb)?;
            Some(format!(
                "`{u}`: the past up to {} is exempted by a named line of {EXEMPT} (approver `{}`, {}), NOT by a `pre-rule` \
                 record: code author `{}`, statement or proof author `{}` - the line ACCEPTS that history, it does not show the \
                 two sides were written by different models, so the rule is not satisfied for it",
                authorship::short(sb),
                e.approver,
                e.approved_on,
                e.code_author,
                e.statement_author,
            ))
        })
        .collect()
}

/// The two views the `pre-rule` records give the walk, from ONE filter: the exempt past of each unit (its `started_before`) and
/// the implementer who stands for that exempt code (roles 1.5). `authorship::walk` reads commits, not
/// `docs/ci/proof-roles.tsv`, so both have to be handed to it; a record that does not validate exempts nothing and names nobody.
pub fn pre_rule_maps(
    entries: &[Entry],
    cut: &Cutoff,
    exists: &dyn Fn(&str) -> bool,
    ok: &dyn Fn(&Record) -> bool,
) -> (BTreeMap<String, String>, BTreeMap<String, String>) {
    let mut starts: BTreeMap<String, String> = BTreeMap::new();
    let mut impls: BTreeMap<String, String> = BTreeMap::new();
    for r in entries.iter().filter_map(|e| e.res.as_ref().ok()) {
        if r.kind != "pre-rule" || !ok(r) || pre_rule_listed(cut, exists, r).is_err() {
            continue;
        }
        starts.insert(r.unit.clone(), r.started_before.clone());
        impls.insert(r.unit.clone(), model_norm(&r.impl_model));
    }
    (starts, impls)
}

/// Exemption lines whose `to_commit` is not in HEAD's history (review 3, weaknesses 3 and 4): they approve a past this repository
/// does not contain, so they exempt nothing and their author fields must name nobody in the rule. The coordinator's file can
/// outlive the branch it was written for, and a refusal would punish every clone that cannot see that branch, which is every
/// `--single-branch` and every shallow one. The line is dropped and REPORTED, never silently kept or silently dropped.
pub fn drop_inert_exempt(exempt: &mut Vec<Exemption>, live: &dyn Fn(&str) -> bool) -> Vec<String> {
    let mut notes: Vec<String> = Vec::new();
    exempt.retain(|e| {
        let keeps = live(&e.to);
        if !keeps {
            notes.push(format!(
                "`{}`: the line of {EXEMPT} ending at {} is inert - that commit is not in this history, so it exempts nothing and its authors name nobody",
                e.unit,
                authorship::short(&e.to)
            ));
        }
        keeps
    });
    notes
}

/// The coordinator's approvals that the JUDGED RANGE brings to life (review 4, weakness 2): the commit a cutoff listing or an
/// exemption line names is an ancestor of HEAD but not of the range base, so the line was inert where the branch started and this
/// range's merges fetched the very history it approves. An inert line is harmless while it sleeps; woken by a merge it drops that
/// history from BOTH authorship sets and its `code_author` enters the rule as a phantom, which is a coordinator decision nobody
/// made. The base is what the range judged against, so a stale line costs the branch nothing but a named refusal: delete the line
/// or the listing, or land it over with the coordinator's `cutoff:<why>` allow. `live` is HEAD's history, `at_base` the range base's.
pub fn revived_exemptions(
    cut: &Cutoff,
    live: &dyn Fn(&str) -> bool,
    at_base: &dyn Fn(&str) -> bool,
) -> Vec<String> {
    let revived = |c: &str| live(c) && !at_base(c);
    let mut out: Vec<String> = Vec::new();
    for e in &cut.exempt {
        if revived(&e.to) {
            out.push(format!(
                "`{EXEMPT}`, the line of `{}` ending at {}: that commit is not an ancestor of the range base, so the line exempted \
                 nothing when this range started and the branch it names came in through a merge this range contains. The approval is \
                 now LIVE and drops that history from both authorship sets: the coordinator has to re-read it, not a merge",
                e.unit,
                authorship::short(&e.to)
            ));
        }
    }
    for (u, sb, _) in &cut.units {
        if !revived(sb) || cut.exempt.iter().any(|e| &e.unit == u && &e.to == sb) {
            // the line above names this listing and its commit already
            continue;
        }
        out.push(format!(
            "`{CUTOFF}`, the listing of `{u}` at started_before {}: that commit is not an ancestor of the range base, so the listing \
             covered nothing when this range started and the branch it names came in through a merge this range contains. A revived \
             listing is the ground a `pre-rule` record or an exemption line stands on, so the coordinator has to re-read it, not a merge",
            authorship::short(sb)
        ));
    }
    out
}

/// Whether the walked range can reach a commit at all: the walk reads `LB..HEAD` and nothing else, so a commit that is not in
/// HEAD's history is in no range, and a listing that names one neither exempts anything nor has anything to explain (review 3,
/// the landing blocker). Refusing it would punish every `--single-branch` and every shallow clone, which cannot see the branch the
/// coordinator wrote the line for.
pub fn reaches_walk(root: &Path, lb: &str, c: &str) -> bool {
    is_ancestor_of(root, c, "HEAD") && !is_ancestor_of(root, c, lb)
}

/// The walk, and the records' contribution to it, in ONE call, so that every view of authorship (`--check`, `--authors`,
/// `--audit`) asks the same question: with the implementer of a covering `pre-rule` record missing from the code side,
/// `--authors` printed `Clash none` for a unit `--check` refuses (review 3, cosmetic 6).
#[allow(clippy::too_many_arguments)] // the walk and the two record maps, exactly as `authorship::walk` takes them
pub fn walk_with_pre_rules(
    root: &Path,
    base: &str,
    units: &[UnitFp],
    cut: &Cutoff,
    pre: &BTreeMap<String, String>,
    pre_impl: &BTreeMap<String, String>,
    r: &authorship::ModelResolver,
    cache: Option<&Path>,
) -> Result<authorship::Walk, String> {
    let mut w = authorship::walk(root, base, units, cut, pre, r, cache)?;
    for (u, m) in pre_impl {
        if let Some(e) = w.units.get_mut(u) {
            e.pre_rule_impl = Some(m.clone());
            e.pre_rule_start = pre.get(u).cloned();
        }
    }
    Ok(w)
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
        exempt: Vec::new(),
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
    pre_rule_listed(&cut, i.exists, r)
}

/// The coordinator's list alone decides: only a listed unit may get the record, only with the `started_before` the list fixes, and
/// only when that commit exists. Used by the walk as well, so the exempt commits are the same ones `check` exempts.
fn pre_rule_listed(cut: &Cutoff, exists: &dyn Fn(&str) -> bool, r: &Record) -> Result<(), String> {
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
    if !exists(sb) {
        return Err(format!(
            "started_before {sb} (listed in {CUTOFF}) does not exist in this repository's object database: fetch the branch that holds it, or ask the coordinator to correct the list"
        ));
    }
    Ok(())
}

/// The unit's valid `pre-rule` record, if any.
fn valid_pre_rule<'a>(i: &'a CheckIn, unit: &str) -> Option<&'a Record> {
    i.entries
        .iter()
        .filter_map(|e| e.res.as_ref().ok())
        .find(|r| {
            r.unit == unit
                && r.kind == "pre-rule"
                && validate(r, need_kind(r), i.verdict_ok)
                    .and_then(|()| pre_rule_ok(i, r))
                    .is_ok()
        })
}

/// The builder rule of one unit (spec 18): every commit of the range that wrote statement or proof text is a strong model's,
/// whether or not a fingerprint moved. `true` when a refusal was added, i.e. the unit's other rules are not worth reading.
fn builder_proof_text(
    o: &mut Outcome,
    unit: &str,
    why: &str,
    proof: &[(String, Vec<String>)],
) -> bool {
    let mut bad = false;
    for (c, tr) in proof {
        let why_bad = if tr.is_empty() {
            Some("carries no Co-Authored-By trailer, so its author cannot be shown to be strong")
        } else if tr.iter().any(|m| class(m) != "strong") {
            Some("carries a builder-class (non-strong) Co-Authored-By trailer")
        } else {
            None
        };
        if let Some(m) = why_bad {
            bad = true;
            o.refusals.push(Refusal {
                unit: unit.into(),
                rule: "proof-or-statement-written-by-a-builder".into(),
                detail: format!(
                    "{why}: commit {c} changed this unit's statement or proof text and {m} ({tr:?}). Builders write code, never statements or proofs"
                ),
                hint: "the strong prover makes the statement/proof change in its own commit (split the commit if a builder's one mixes code and proof text)".into(),
            });
        }
    }
    bad
}

/// The authorship rules of one unit, read off the walk: the one rule (`CodeAuthors(U) ∩ ProofAuthors(U) = ∅`, roles 1.1), the
/// commits of the history part that neither the ledger, the attribution file nor a trailer names, and once
/// `LEDGER_REQUIRED_FROM` is set, the range commits with no ledger entry. Returns the refusals added.
fn authorship_rules(
    i: &CheckIn,
    o: &mut Outcome,
    unit: &str,
    ua: &authorship::UnitAuthors,
    pre_impl: Option<&String>,
    why: &str,
) -> usize {
    let n0 = o.refusals.len();
    let lbn = &i.authors.lb[..i.authors.lb.len().min(10)];
    let commits = |v: &[String]| -> String {
        if v.is_empty() {
            "the exempt work before `started_before`".to_string()
        } else {
            v.join(", ")
        }
    };
    let source = |m: &str| -> String {
        let mut v: BTreeSet<&str> = BTreeSet::new();
        for a in &ua.commits {
            if (a.code || a.proof) && a.models.iter().any(|x| x == m) {
                v.insert(a.source.label());
            }
        }
        if v.is_empty() {
            "Pre-rule record".into()
        } else {
            v.into_iter().collect::<Vec<_>>().join("+")
        }
    };
    let mut code = authorship::code_authors(ua);
    if let Some(p) = pre_impl {
        code.entry(p.clone()).or_default();
    }
    let proof = authorship::proof_authors(ua);
    let both: Vec<String> = code
        .keys()
        .filter(|m| !m.is_empty() && proof.contains_key(m.as_str()))
        .cloned()
        .collect();
    if !both.is_empty() {
        let get = |m: &BTreeMap<String, Vec<String>>, k: &str| -> Vec<String> {
            m.get(k).cloned().unwrap_or_default()
        };
        let what: Vec<String> = both
            .iter()
            .map(|m| {
                format!(
                    "{m} shaped the code in {} and the statement or proof text in {} (Source: {})",
                    commits(&get(&code, m)),
                    commits(&get(&proof, m)),
                    source(m)
                )
            })
            .collect();
        o.refusals.push(Refusal {
            unit: unit.into(),
            rule: "one-model-wrote-code-and-proof".into(),
            detail: format!(
                "{why}: {}; the two sides of this unit were shaped by the same model since {lbn} (every commit of the walk counts, not only this range's)",
                what.join("; ")
            ),
            hint: "the model that writes the code must not write the statement or proof: let a builder make the code change and the strong prover the statement/proof change in its own commit. If a trailer names the wrong model, the coordinator corrects it in docs/ci/proof-roles-attributions.txt".into(),
        });
    }
    // A commit of the history whose author nothing names: the rule cannot be checked for it, and guessing is not allowed.
    for a in ua
        .commits
        .iter()
        .filter(|a| !a.in_range && a.models.is_empty())
    {
        let side = match (a.code, a.proof) {
            (true, true) => "code and statement or proof text",
            (true, false) => "code",
            _ => "statement or proof text",
        };
        o.refusals.push(Refusal {
            unit: unit.into(),
            rule: "author-unknown".into(),
            detail: format!(
                "{}: commit {} changed this unit's {side} ({}) and neither the ledger, the attribution file nor a Co-Authored-By trailer names who wrote it",
                why,
                a.commit,
                a.paths.join(", ")
            ),
            hint: "run `lagent attest --model <id>` for that commit once it is on main, or ask the coordinator to correct it in docs/ci/proof-roles-attributions.txt".into(),
        });
    }
    // A code commit of the RANGE that names no model is refused here, not on the next branch that happens to read the same
    // history (review 5, scenarios D and E): the range keeps its own rule name (spec 1.3). The proof side of the same gap is
    // `builder_proof_text`, which reads the proof stream and so catches an untrailered commit whatever the fingerprints do.
    for a in ua
        .commits
        .iter()
        .filter(|a| a.in_range && a.code && a.models.is_empty())
    {
        o.refusals.push(Refusal {
            unit: unit.into(),
            rule: "code-commit-without-trailer".into(),
            detail: format!(
                "{why}: commit {} changed this unit's code ({}) and carries no Co-Authored-By trailer, so the one-model and implementer checks cannot see its author",
                a.commit,
                a.paths.join(", ")
            ),
            hint: "every commit must carry the Co-Authored-By trailer of the model that wrote it".into(),
        });
    }
    // The open gap closes here: once every commit of the range needs a ledger entry, a trailer the committing session wrote for
    // itself no longer decides (roles 5). The commit id the constant names is always at or before the base by then.
    if authorship::LEDGER_REQUIRED_FROM.is_some() && i.authors.ledger_broken.is_none() {
        for a in ua
            .commits
            .iter()
            .filter(|c| c.in_range && (c.code || c.proof))
        {
            if a.source != authorship::Source::Ledger {
                o.refusals.push(Refusal {
                    unit: unit.into(),
                    rule: "ledger-entry-missing".into(),
                    detail: format!(
                        "{}: commit {} shaped this unit but has no entry in the commit ledger",
                        why, a.commit
                    ),
                    hint: format!(
                        "run `lagent attest --model <id>` for that commit (the ledger is {})",
                        authorship::LEDGER_ENV
                    ),
                });
            }
        }
    }
    o.refusals.len() - n0
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
    for m in i.omit_bad_tags {
        o.refusals.push(Refusal {
            unit: "-".into(),
            rule: "omit-tag-hides-code-from-the-proof".into(),
            detail: m.clone(),
            hint: "omit may only hide attributes: a `//@omit` tag belongs at the end of an attribute line (`#[..]` or `#![..]`). On any other line it hides executable code from Verus, which is what this rule is for; drop the tag, or say in the commit why the line is not code".into(),
        });
    }
    if !i.cutoff_changed.is_empty() && !i.allow_cutoff {
        o.refusals.push(Refusal {
            unit: "-".into(),
            rule: "cutoff-protected".into(),
            detail: format!(
                "the range creates, changes or deletes {} : the list of units that may get pre-rule records, and the exemption file that names who approved a listed past, belong to the coordinator",
                i.cutoff_changed.join(", ")
            ),
            hint: "do not touch it in a branch; the coordinator lands it with LAGENT_ALLOW_SPEC_CHANGE naming `cutoff:<why>`".into(),
        });
    }
    if let Some(t) = i.exempt_work {
        let cut = i.cutoff_work.and_then(|c| parse_cutoff(c).ok());
        let listed = listed_starts(cut.as_ref());
        if let Err(e) = parse_exemptions(t, &listed, i.exists, i.ancestor) {
            o.refusals.push(Refusal {
                unit: "-".into(),
                rule: "exemptions-malformed".into(),
                detail: format!("{EXEMPT}: {e}"),
                hint: format!(
                    "an exemption line nobody can read exempts nobody: {EXEMPT_FIELDS} tab-separated fields, none empty, the two commits \
                     as full 40-digit hex ids, the date YYYY-MM-DD, the unit and its `to_commit` as {CUTOFF} carries them, and \
                     `{NOT_ESTABLISHED}` in an author field where history shows nothing"
                ),
            });
        }
    }
    for m in i.cutoff_gaps {
        o.refusals.push(Refusal {
            unit: "-".into(),
            rule: "cutoff-listing-without-exemption".into(),
            detail: m.clone(),
            hint: "the coordinator's list names who MAY get a pre-rule record, it exempts no history by itself: append the `pre-rule` record, or ask the coordinator for a line in docs/ci/proof-roles-exemptions.tsv naming the range and the person who approved it".into(),
        });
    }
    // A line that was inert at the range base and is live at HEAD changed status through a merge of THIS range, and it drops real
    // history from both authorship sets. Re-reading it is the coordinator's call, not a side effect of a merge (review 4).
    if !i.exemption_revived.is_empty() && !i.allow_cutoff {
        for m in i.exemption_revived {
            o.refusals.push(Refusal {
                unit: "-".into(),
                rule: "exemption-revived".into(),
                detail: m.clone(),
                hint: format!(
                    "an inert coordinator approval must not be woken by a merge: ask the coordinator to delete the line, or the \
                     listing it stands on, now that the history it names is here, or to land over it with \
                     LAGENT_ALLOW_SPEC_CHANGE naming `cutoff:<why>` (the same allow that governs {CUTOFF} and {EXEMPT} themselves)"
                ),
            });
        }
    }
    // A past dropped by a named exemption rather than a record is an acceptance the reader must see, even when everything else is
    // green: the check never reports such a unit as satisfying the one-model rule (review item 5).
    for m in i.cutoff_accepts {
        o.notes.push(m.clone());
    }
    if let Some(a) = i.attrib {
        check_attributions(a, &mut o);
    }
    // New lines: well formed, a known unit, a unique id, written by the commit of the prover they name.
    let base_n = i.base_log.lines().count();
    // An INVALID line is a refusal whatever else the log holds. A line nobody can parse, and a `statement` record that breaks the
    // prover/reviewer pairing, is skipped by every search for a covering record: with an older valid record of the same unit the run
    // said ok and a refused review read as coverage (2026-11-17). A line NEW in this branch is refused by the loop below, whose hint
    // says it may still be rewritten; a line of the base is history, so only the coordinator lands over it.
    for e in i.entries.iter().filter(|e| e.line <= base_n) {
        let m = match &e.res {
            Err(m) => m.clone(),
            Ok(r) => {
                // Only a statement record of an ACCEPTED verdict is judged: a `rejected` or `escalated` line is history that
                // authorises nothing (never a refusal), and a seed names no roles at all.
                if r.kind != "statement" || r.verdict != "accepted" {
                    continue;
                }
                match validate(r, need_kind(r), i.verdict_ok) {
                    Ok(()) => continue,
                    Err(m) => m,
                }
            }
        };
        o.refusals.push(Refusal {
            unit: e.res.as_ref().map_or("-".to_string(), |r| r.unit.clone()),
            rule: "invalid-record-line".into(),
            detail: format!("{LOG} line {} (id `{}`): {m}", e.line, e.id),
            hint: "the line is older than this branch and the log is append-only, so it cannot be fixed here: report it and the \
                   coordinator decides (a statement record needs a reviewer of a known family, not `other`, that is neither the \
                   prover's family nor the prover or the implementer)"
                .into(),
        });
    }
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
    // The authorship walk (docs/ci/PROOF-ROLES.md): what it could not read is said once, for the run.
    let lbn = &i.authors.lb[..i.authors.lb.len().min(10)];
    if i.authors.lb_missing {
        if i.merge_gate {
            o.refusals.push(Refusal {
                unit: "-".into(),
                rule: "history-lower-bound-missing".into(),
                detail: format!(
                    "the history lower bound {lbn} is not in this repository (a shallow clone?): authorship of the older commits cannot be read"
                ),
                hint: "fetch the full history (the merge gate needs every commit since the commit named on line 1 of docs/ci/proof-roles-cutoff.txt)".into(),
            });
        } else {
            o.notes.push(format!(
                "the history lower bound {lbn} is not in this repository (a shallow clone?): the authorship walk could not run"
            ));
        }
    }
    if let Some(why) = &i.authors.ledger_broken {
        if i.merge_gate {
            o.refusals.push(Refusal {
                unit: "-".into(),
                rule: "ledger-broken".into(),
                detail: format!(
                    "the commit ledger of {name} does not verify: {why}: no entry of it was read",
                    name = authorship::LEDGER_ENV
                ),
                hint: "rebuild it with `lagent attest` (every commit of the range needs its entry); the trailers alone decide when the chain is broken".into(),
            });
        } else {
            o.notes.push(format!(
                "the commit ledger of {} does not verify: {why}: its entries are ignored for this run and the trailers decide",
                authorship::LEDGER_ENV
            ));
        }
    }
    for (c, why) in &i.authors.contradictions {
        o.refusals.push(Refusal {
            unit: "-".into(),
            rule: "attribution-contradicts-ledger".into(),
            detail: format!("commit {c}: {why}"),
            hint: "the ledger is what the lane ran: the coordinator corrects the attribution file or the entry, never the branch".into(),
        });
    }
    for n in &i.authors.notes {
        o.notes.push(n.clone());
    }
    if !i.authors.merge_unreadable.is_empty() {
        let list = i.authors.merge_unreadable.join(", ");
        if i.merge_gate {
            o.refusals.push(Refusal {
                unit: "-".into(),
                rule: "merge-authorship-unreadable".into(),
                detail: format!(
                    "the merge(s) {list} could not be read: neither the automatic merge nor a comparison with any of its parents could be computed, so whose work it is stays unknown"
                ),
                hint: "fetch the full history (a shallow clone hides a merge's parents); the gate cannot pass a merge it cannot attribute".into(),
            });
        } else {
            o.notes.push(format!(
                "the merge(s) {list} could not be read: neither the automatic merge nor a comparison with any parent could be computed"
            ));
        }
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
        let ua = i.authors.authors(&u.unit);
        // A unit the range does not touch is no authorship question of this run: `roles --audit` reports what its history shows.
        let touched = ua.commits.iter().any(|a| a.in_range);
        let pre_rule_rec = valid_pre_rule(i, &u.unit);
        let info = authorship::range_change(ua);
        let Some(need) = need else {
            // Only the code changed (no record is needed for that): the walk still says whether a model that shaped this unit's
            // statement or proof text also shaped code in it, counting every commit since the lower bound.
            if touched {
                let pwhy = "the proof stream moved and no fingerprint did";
                if builder_proof_text(&mut o, &u.unit, pwhy, &info.proof) {
                    o.unchanged += 1;
                    continue;
                }
                authorship_rules(
                    i,
                    &mut o,
                    &u.unit,
                    ua,
                    pre_rule_rec
                        .map(|p| p.impl_model.as_str())
                        .map(model_norm)
                        .as_ref(),
                    "code changed in this range",
                );
            }
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
            // Work before `started_before` was written under no rule and is judged by the independent review; every commit after it
            // is walked, so the authorship sets hold the commits after it only.
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
            pre_rule_rec
        };
        if let Some(p) = pre {
            o.notes.push(format!(
                "{}: record `{}` covers the current fingerprints; PRE-RULE record `{}` still exempts the commits up to {}",
                u.unit, rec.id, p.id, p.started_before
            ));
        }
        let pre_impl: Option<String> = if rec.kind == "pre-rule" {
            Some(model_norm(&rec.impl_model))
        } else {
            pre.map(|p| model_norm(&p.impl_model))
        };
        // Who wrote the statement or proof text: every commit that changed it must carry a strong trailer.
        if builder_proof_text(&mut o, &u.unit, why, &info.proof) {
            continue;
        }
        let pm = model_norm(&rec.prover_model);
        // The one rule (roles 1.1): no model may have shaped both this unit's code and its statement or proof text since the lower
        // bound, whichever merge the work came through. A record is needed only when the range moved the unit, so it is read always.
        // The exempt code of a `pre-rule` unit counts as its implementer's.
        if authorship_rules(i, &mut o, &u.unit, ua, pre_impl.as_ref(), why) > 0 {
            continue;
        }
        // The declared prover must have written some of the statement or proof text itself: a signature on another model's proof
        // proves nothing. A `pre-rule` prover is the independent reviewer of the exempt work and need not have written anything.
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
        }
        // Code commits: the implementer must be among their trailers; the prover must not. A code commit that names no model was
        // already refused by `authorship_rules`, which reads the range the same way whichever path a unit takes.
        if !info.exec.is_empty() {
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

/// The models of an authoring commit, or the mark that nothing names one (roles 1.3).
fn authored_models(a: &authorship::Authored) -> String {
    if a.models.is_empty() {
        "NO-MODEL-NAMED".to_string()
    } else {
        a.models.join(",")
    }
}

/// `roles --authors <unit>`: every commit that shaped the unit since the lower bound, with the model and where the model came from,
/// then both sets and their intersection (docs/ci/PROOF-ROLES.md).
pub fn authors_report(name: &str, w: &authorship::Walk, base: &str) -> String {
    let ua = w.authors(name);
    let mut s = format!(
        "{name}\tsince {}\tbase {}\n",
        authorship::short(&w.lb),
        authorship::short(base)
    );
    for a in &ua.commits {
        for (side, on) in [("code", a.code), ("proof", a.proof)] {
            if !on {
                continue;
            }
            s.push_str(&format!(
                "{side}\t{}\t{}\t{}\t{}\n",
                authorship::short(&a.commit),
                authored_models(a),
                a.source.label(),
                if a.in_range { "range" } else { "history" }
            ));
        }
    }
    for (label, code) in [("CodeAuthors", true), ("ProofAuthors", false)] {
        let m = if code {
            authorship::code_authors(ua)
        } else {
            authorship::proof_authors(ua)
        };
        let text = if m.is_empty() {
            "-".to_string()
        } else {
            m.iter()
                .map(|(k, v)| {
                    let who = if k.is_empty() {
                        "NO-MODEL-NAMED".to_string()
                    } else {
                        k.clone()
                    };
                    if v.is_empty() {
                        who
                    } else {
                        format!("{who}({})", v.join(" "))
                    }
                })
                .collect::<Vec<_>>()
                .join(" ")
        };
        s.push_str(&format!("{label}\t{text}\n"));
    }
    let clash: Vec<String> = authorship::code_authors(ua)
        .keys()
        .filter(|m| authorship::proof_authors(ua).contains_key(*m))
        .cloned()
        .collect();
    s.push_str(&format!(
        "Clash\t{}\n",
        if clash.is_empty() {
            "none".to_string()
        } else {
            format!("{}: one-model-wrote-code-and-proof", clash.join(" "))
        }
    ));
    if let Some(pi) = &ua.pre_rule_impl {
        // The exempt past ends AT the record's `started_before`, a commit after the lower bound; naming the bound said the code was
        // written before roles landed at all, which is the wrong end of the range (review 4, message nit).
        let up_to = ua.pre_rule_start.as_ref().map_or_else(
            || "up to its record's `started_before`".to_string(),
            |s| format!("up to `started_before` {}", authorship::short(s)),
        );
        s.push_str(&format!("Pre-rule\t{pi} wrote the code exempt {up_to}\n"));
    }
    if let Some(ec) = &ua.exempt_code {
        s.push_str(&format!(
            "Exempt\t{ec} is named as the code author of the exempted past by {EXEMPT}\n"
        ));
    }
    s
}

/// `roles --audit`: one line per unit with a fingerprint, whatever its history shows, since `--check` reads only the units the range
/// touches. Report only: the exit code never changes (roles 1.1).
pub fn audit_report(fps: &[UnitFp], w: &authorship::Walk) -> String {
    let set = |u: &authorship::UnitAuthors, code: bool| -> String {
        let m = if code {
            authorship::code_authors(u)
        } else {
            authorship::proof_authors(u)
        };
        if m.is_empty() {
            return "-".into();
        }
        m.iter()
            .map(|(k, v)| {
                let who = if k.is_empty() { "?" } else { k.as_str() };
                format!("{who}({})", v.len())
            })
            .collect::<Vec<_>>()
            .join(" ")
    };
    let mut s = String::from("unit\tcode\tproof\tclash\tno-model\n");
    let mut bad = 0usize;
    for u in fps.iter().filter(|u| u.fp.is_some()) {
        let ua = w.authors(&u.unit);
        let code = authorship::code_authors(ua);
        let proof = authorship::proof_authors(ua);
        let clash: Vec<String> = code
            .keys()
            .filter(|m| proof.contains_key(*m))
            .cloned()
            .collect();
        if !clash.is_empty() {
            bad += 1;
        }
        s.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\n",
            u.unit,
            set(ua, true),
            set(ua, false),
            if clash.is_empty() {
                "-".to_string()
            } else {
                clash.join(" ")
            },
            ua.commits.iter().filter(|a| a.models.is_empty()).count()
        ));
    }
    s.push_str(&format!(
        "roles: {bad} unit(s) with a model on both sides, of {} walked ({} commits, {} blobs, {:.1} s)\n",
        fps.iter().filter(|u| u.fp.is_some()).count(),
        w.commits,
        w.blobs,
        w.secs
    ));
    s
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

/// The trailer models exactly as the commit message carries them (no correction applied).
fn raw_commit_trailers(root: &Path, c: &str) -> Vec<String> {
    git(root, &["log", "-1", "--format=%B", c])
        .map(|m| trailer_models(&m))
        .unwrap_or_default()
}

/// raw log line -> the models of the oldest non-merge commit of base..HEAD that added it, read by `models(C)` (roles 1.6):
/// the ledger first, then the coordinator's correction file, then the trailers. What `trailer-mismatch` reads is what the
/// walk read for the same commit, not the raw trailer of it.
fn added_map(
    root: &Path,
    base: &str,
    r: &authorship::ModelResolver,
) -> BTreeMap<String, Vec<String>> {
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
        let tr = r
            .models(
                c,
                &git(root, &["log", "-1", "--format=%B", c]).unwrap_or_default(),
            )
            .0;
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

/// The code part of a `fn` header: attributes and signature up to the first clause keyword. Verifier and gate attributes are not
/// code (rustc compiles the item either way, the choice of gate is the prover's); `-> (r: T)` reads as `-> T`.
fn fn_code_header(t: &[Tok], in_verus: &[bool]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut k = 0;
    while k < t.len() {
        if let Some(e) = spec::attr_end(t, k) {
            let e = e.min(t.len()).max(k + 1);
            let gate = t[k..e]
                .iter()
                .any(|x| x.ident && matches!(x.s.as_str(), "cfg" | "cfg_attr"));
            if !t[k..e]
                .iter()
                .any(|x| x.s == "verus_spec" || x.s == "verus_verify")
                && !gate
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

/// Is `path` one of the protected exemption files (the coordinator's named approvals)?
pub fn is_exempt_path(p: &str) -> bool {
    p == EXEMPT || (p.starts_with("docs/ci/proof-roles-exemptions-") && p.ends_with(".tsv"))
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

/// Is a `//@omit` tag on a line that is not an attribute? A tag may only hide an attribute line, a `#[..]` or a `#![..]` that carries
/// nothing else: the content of a tagged line is shipped code that only the verifier is blind to, so a tag on a `use`, a statement, an
/// expression or a fn would hide executable code from the proof, which is the one thing the tag must not do. One message per line,
/// `path:line: ...`. `roles --check` refuses a file only when its count rises against the base: main has one such line, reported to
/// the coordinator (docs/ci/PROOF-ROLES.md).
pub fn omit_bad_tags_of(path: &str, src: &str) -> Vec<String> {
    src.lines()
        .enumerate()
        .filter(|(_, l)| l.trim_end().ends_with("//@omit"))
        .map(|(n, l)| {
            (
                n,
                l.trim_end()
                    .strip_suffix("//@omit")
                    .unwrap_or_default()
                    .trim_start(),
            )
        })
        .filter(|(_, body)| !(body.starts_with("#[") || body.starts_with("#![")))
        .map(|(n, body)| {
            format!(
                "{path}:{}: `//@omit` on `{body}`: omit may only hide attributes, so a tag belongs on a `#[..]` or `#![..]` line",
                n + 1
            )
        })
        .collect()
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

/// The files whose newness decides whether a unit is new: a verified unit's closure (else its inventory inputs); for a script
/// unit only its OWN files, because the crates a mutants script mutates are inputs it merely reads, so a new mutants unit over an
/// existing crate is still new. `None`: the unit is unknown.
fn newness_files(work_files: Option<&Vec<String>>, inv: Option<&Unit>) -> Option<Vec<String>> {
    if let Some(u) = inv.filter(|u| is_script_unit(&u.kind)) {
        return Some(own_files_of(u));
    }
    match (work_files, inv) {
        (Some(w), _) if !w.is_empty() => Some(w.clone()),
        (_, Some(u)) => Some(u.inputs.clone()),
        _ => None,
    }
}

/// Units whose script is their own target and whose `inputs` are only what the script mutates or runs over.
fn is_script_unit(kind: &str) -> bool {
    matches!(kind, "mutants" | "mutations")
}

/// The files that make a script unit what it is: the target script and the directory it sits in (`verification/<x>/`).
fn own_files_of(u: &Unit) -> Vec<String> {
    let mut v = vec![u.target.clone()];
    if let Some((dir, _)) = u.target.rsplit_once('/') {
        v.push(format!("{dir}/"));
    }
    v
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
        let n_attr = attribution_map(work_tree.read(ATTRIB).as_deref()).len();
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
                    "}},\"reviewed\":{rv},\"pre_rule\":{wv},\"not_reviewed\":{nr},\"attribution_corrections\":{n_attr},\"units\":["
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
                println!("attribution corrections\t{n_attr}");
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
        eprintln!("roles: attribution corrections: {n_attr} commit(s) ({ATTRIB})");
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
    let one = val(args, "--authors");
    if !has("--check") && !has("--audit") && one.is_none() {
        return Err("usage: roles --check [--base REF] [--merge-gate] | --list [--json|--summary] | --template KIND [--unit U] | --seed [--print] | --audit | --authors UNIT [--base REF]".into());
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
    let attrib_work = work_tree.read(ATTRIB);
    // The record line is read by `models(C)`, so the ledger decides it exactly as it decides the walk (roles 1.6).
    let ledger = std::env::var(authorship::LEDGER_ENV)
        .ok()
        .map(PathBuf::from);
    let added_res = authorship::ModelResolver::new(ledger.as_deref(), attrib_work.as_deref());
    let added = added_map(&root, &base, &added_res);
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
    let verdict_modified: Vec<String> = gt
        .files_under(VERDICT_DIR.trim_end_matches('/'))
        .into_iter()
        .filter(|f| gt.read(f) != work_tree.read(f))
        .collect();
    let cutoff_work = work_tree.read(CUTOFF);
    let exempt_work = work_tree.read(EXEMPT);
    let exists = |c: &str| commit_exists(&root, c);
    let ancestor = |a: &str, b: &str| is_ancestor_of(&root, a, b);
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
        .filter(|c| is_cutoff_path(c) || is_exempt_path(c))
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
        let Some(files) = newness_files(
            work_units.get(name).map(|w| &w.files),
            inv.get(name).copied(),
        ) else {
            return false;
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
    // Same ratchet shape for a tag on a line that is not an attribute: no branch may add one, and the one line main carries
    // (`services/lucina-events/src/rules.rs`) is tolerated until the coordinator says otherwise.
    let mut omit_bad_tags: Vec<String> = Vec::new();
    for u in fps
        .iter()
        .filter(|u| u.kind == "shipped" || u.kind == "verus")
    {
        for f in &u.files {
            if let Some(src) = work_tree.read(f) {
                let now = omit_bad_tags_of(f, &src);
                let before = gt.read(f).map_or(0, |b| omit_bad_tags_of(f, &b).len());
                if now.len() > before {
                    omit_bad_tags.extend(now);
                }
            }
        }
    }
    omit_bad_tags.sort();
    omit_bad_tags.dedup();
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
    let attrib_changed: Vec<String> = changed
        .iter()
        .filter(|c| is_attrib_path(c))
        .cloned()
        .collect();
    let attrib_base = gt.read(ATTRIB);
    let ancestor_of_base = |c: &str| is_ancestor_of(&root, c, &base);
    let is_merge = |c: &str| is_merge_commit(&root, c);
    let raw_models = |c: &str| raw_commit_trailers(&root, c);
    let attrib_ctx = AttribCtx {
        work: attrib_work.as_deref(),
        base: attrib_base.as_deref(),
        changed: &attrib_changed,
        allow: allow_env.iter().any(|v| allow_token(v, "attribution")),
        allow_strong: allow_env
            .iter()
            .any(|v| allow_token(v, "attribution-strong")),
        ancestor_of_base: &ancestor_of_base,
        is_merge: &is_merge,
        raw_models: &raw_models,
    };
    // Authorship (docs/ci/PROOF-ROLES.md): ONE walk of `LB..HEAD`, every unit at once, the streams cached per blob. The
    // persistent cache serves `check-fast` only; the merge gate never reads it, so what it pays is the cold cost.
    // The coordinator's exemption file: a named approval for a listed unit whose past no `pre-rule` record covers. A line nobody
    // can read exempts nobody and is refused (`exemptions-malformed`).
    let mut cut = match cutoff_work.as_deref() {
        None => Cutoff {
            commit: HISTORY_LOWER_BOUND.to_string(),
            units: Vec::new(),
            exempt: Vec::new(),
        },
        Some(t) => match parse_cutoff(t) {
            Ok(mut c) => {
                c.commit = history_lower_bound(Some(t));
                c
            }
            Err(e) => {
                eprintln!("roles: {CUTOFF}: {e}; read as no cutoff, every pre-rule record refused");
                Cutoff {
                    commit: history_lower_bound(Some(t)),
                    units: Vec::new(),
                    exempt: Vec::new(),
                }
            }
        },
    };
    let listed = listed_starts(Some(&cut));
    cut.exempt = exempt_work
        .as_deref()
        .and_then(|t| parse_exemptions(t, &listed, &exists, &ancestor).ok())
        .unwrap_or_default();
    // A line whose `to_commit` is not in HEAD's history approves a past this repository does not contain, so it exempts nothing and
    // must name nobody (review 3, weaknesses 3 and 4). The line is reported, not silently dropped.
    let inert_exempt = drop_inert_exempt(&mut cut.exempt, &|c| ancestor(c, "HEAD"));
    // Review 4, weakness 2: the other side of inertness. A line or a listing whose commit the RANGE brought in was inert where the
    // branch started and is live now, so a merge - not the coordinator - turned a dormant approval into an exemption of real history.
    // A clone that does not hold the range base at all (shallow, grafted) cannot say what the range brought in: like inertness, the
    // judgement needs the history, and without it nothing is refused.
    let revived = if commit_exists(&root, &base) {
        revived_exemptions(&cut, &|c| ancestor(c, "HEAD"), &|c| ancestor(c, &base))
    } else {
        Vec::new()
    };
    // The exempt past of a pre-rule unit and the implementer who stands for it, from one filter of the records.
    let (pre, pre_impl) = pre_rule_maps(&entries, &cut, &exists, &|r| {
        validate(r, need_kind(r), &vok).is_ok()
    });
    // Review 5, scenario N: a cutoff line is not an exemption by itself. A listed unit whose past reaches the walk needs either a
    // valid `pre-rule` record or an exemption line for the very commit the list fixes for it.
    let reaches = |sb: &str| reaches_walk(&root, &cut.commit, sb);
    let gaps = cutoff_gaps(&cut, &pre, &reaches);
    // Review item 5: the same units whose past the exemption lines DO drop are reported as accepted, not as satisfying the rule.
    let accepts = cutoff_acceptances(&cut, &pre, &reaches);
    let res = authorship::ModelResolver::new(ledger.as_deref(), attrib_base.as_deref());
    let cache = if merge_gate {
        None
    } else {
        authorship::cache_path(&root)
    };
    let mut authors = walk_with_pre_rules(
        &root,
        &base,
        &fps,
        &cut,
        &pre,
        &pre_impl,
        &res,
        cache.as_deref(),
    )?;
    authors.notes.extend(inert_exempt);
    eprintln!(
        "authorship walk: {} commits, {} blobs, {:.1} s",
        authors.commits, authors.blobs, authors.secs
    );
    if let Some(name) = one {
        if !fps.iter().any(|u| u.unit == name) {
            return Err(format!("no unit {name}"));
        }
        print!("{}", authors_report(&name, &authors, &base));
        return Ok(0);
    }
    if has("--audit") {
        print!("{}", audit_report(&fps, &authors));
        return Ok(0);
    }
    let input = CheckIn {
        base: &bmap,
        work: &fps,
        entries: &entries,
        base_log: &base_log,
        work_log: &log,
        verdict_ok: &vok,
        added: &added_fn,
        authors: &authors,
        verdict_modified: &verdict_modified,
        merge_gate,
        cutoff_work: cutoff_work.as_deref(),
        cutoff_changed: &cutoff_changed,
        exempt_work: exempt_work.as_deref(),
        cutoff_gaps: &gaps,
        cutoff_accepts: &accepts,
        exemption_revived: &revived,
        allow_cutoff,
        exists: &exists,
        ancestor: &ancestor,
        omit_hazards: &omit_hazards,
        omit_bad_tags: &omit_bad_tags,
        registration_exempt: &registration_exempt,
        extractor_changed: &extractor_changed,
        unit_files_changed: &unit_files_changed,
        allow_extractor,
        attrib: Some(&attrib_ctx),
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

    /// The unit fingerprints of `peers`, with the files of the unit under review replaced by `files`.
    fn ufps(peers: &[Unit], name: &str, files: &[String]) -> Vec<UnitFp> {
        peers
            .iter()
            .map(|u| UnitFp {
                unit: u.name.clone(),
                group: u.group.clone(),
                kind: u.kind.clone(),
                files: if u.name == name {
                    files.to_vec()
                } else {
                    std::iter::once(u.target.clone())
                        .chain(u.inputs.iter().cloned())
                        .collect()
                },
                fp: Some(Fp {
                    stmt: String::new(),
                    proof: String::new(),
                    exec: String::new(),
                    data: String::new(),
                }),
                counts: [0; 3],
                stmt_count: 0,
                stmt_keys: vec![],
            })
            .collect()
    }

    /// The authorship of one unit in a temp repository: the walk starts at `base`, so every commit it finds is in range
    /// (roles 1.1). `owned_at_base` and the rename list are gone with the old rule and ignored.
    fn walk_in(
        root: &Path,
        base: &str,
        name: &str,
        peers: &[Unit],
        files: &[String],
        skip_ancestors_of: Option<&str>,
    ) -> authorship::Walk {
        let cut = Cutoff {
            commit: base.to_string(),
            units: vec![],
            exempt: Vec::new(),
        };
        let mut pre = BTreeMap::new();
        if let Some(s) = skip_ancestors_of {
            pre.insert(name.to_string(), s.to_string());
        }
        authorship::walk(
            root,
            base,
            &ufps(peers, name, files),
            &cut,
            &pre,
            &authorship::ModelResolver::default(),
            None,
        )
        .unwrap_or_else(|e| panic!("walk: {e}"))
    }

    /// The same walk with the coordinator's list carried and nothing behind it: `listed` is the `unit`, `started_before` of the
    /// cutoff lines, and `Cutoff.exempt` is empty - no `pre-rule` record and no exemption line, so the list exempts nobody
    /// (review 5, scenario N).
    fn walk_listed(
        root: &Path,
        base: &str,
        name: &str,
        peers: &[Unit],
        files: &[String],
        listed: &[(&str, &str)],
    ) -> authorship::Walk {
        let cut = Cutoff {
            commit: base.to_string(),
            units: listed
                .iter()
                .map(|(u, c)| (u.to_string(), c.to_string(), "the list alone".to_string()))
                .collect(),
            exempt: Vec::new(),
        };
        authorship::walk(
            root,
            base,
            &ufps(peers, name, files),
            &cut,
            &BTreeMap::new(),
            &authorship::ModelResolver::default(),
            None,
        )
        .unwrap_or_else(|e| panic!("walk: {e}"))
    }

    /// The same walk with the coordinator's list AND its exemption file carried: `listed` is the cutoff rows (unit,
    /// started_before), `lines` the exemption rows (unit, from_commit, to_commit). A line is what gives a listed unit without a
    /// `pre-rule` record an exempt past at all.
    fn walk_exempted(
        root: &Path,
        base: &str,
        name: &str,
        peers: &[Unit],
        files: &[String],
        listed: &[(&str, &str)],
        lines: &[(&str, &str, &str)],
    ) -> authorship::Walk {
        walk_exempted_authors(root, base, name, peers, files, listed, lines, SON, SON)
    }

    /// The same with the two author fields of the lines given: what the coordinator's line records about the range it approves
    /// is what the walk counts, not what the dropped commits' trailers said (`review 2, New 2`).
    #[allow(clippy::too_many_arguments)]
    fn walk_exempted_authors(
        root: &Path,
        base: &str,
        name: &str,
        peers: &[Unit],
        files: &[String],
        listed: &[(&str, &str)],
        lines: &[(&str, &str, &str)],
        code_author: &str,
        statement_author: &str,
    ) -> authorship::Walk {
        let cut = Cutoff {
            commit: base.to_string(),
            units: listed
                .iter()
                .map(|(u, c)| (u.to_string(), c.to_string(), "listed".to_string()))
                .collect(),
            exempt: lines
                .iter()
                .map(|(u, f, t)| Exemption {
                    unit: (*u).to_string(),
                    from: (*f).to_string(),
                    to: (*t).to_string(),
                    code_author: code_author.to_string(),
                    statement_author: statement_author.to_string(),
                    approver: "Juno Lucina (coordinator)".into(),
                    approved_on: "2026-11-12".into(),
                    note: "the range the coordinator approved".into(),
                })
                .collect(),
        };
        authorship::walk(
            root,
            base,
            &ufps(peers, name, files),
            &cut,
            &BTreeMap::new(),
            &authorship::ModelResolver::default(),
            None,
        )
        .unwrap_or_else(|e| panic!("walk: {e}"))
    }

    /// The fingerprints of a unit as they are in the tree of one revision.
    fn rev_fp(root: &Path, rev: &str, name: &str, peers: &[Unit]) -> Option<Fp> {
        let t = GitTree {
            root: root.to_path_buf(),
            rev: rev.to_string(),
        };
        let (v, _) = fingerprints(&t, peers);
        v.iter().find(|u| u.unit == name)?.fp.clone()
    }

    fn change_info(
        root: &Path,
        base: &str,
        name: &str,
        peers: &[Unit],
        files: &[String],
        skip_ancestors_of: Option<&str>,
    ) -> ChangeInfo {
        authorship::range_change(
            walk_in(root, base, name, peers, files, skip_ancestors_of).authors(name),
        )
    }

    /// The models that shaped the code (`code`) or the statement and proof text of a unit since the lower bound (roles 1.1).
    fn models_of(w: &authorship::Walk, unit: &str, code: bool) -> Vec<String> {
        let ua = w.authors(unit);
        let m = if code {
            authorship::code_authors(ua)
        } else {
            authorship::proof_authors(ua)
        };
        m.keys().cloned().collect()
    }

    /// A walk holding the facts of a hand-built `ChangeInfo`, for a `CheckIn` assembled without git (roles 1.1).
    fn walk_of(unit: &str, i: &ChangeInfo) -> authorship::Walk {
        let mut w = authorship::Walk::default();
        let mut ua = authorship::UnitAuthors {
            commits: vec![],
            pre_rule_impl: None,
            pre_rule_start: None,
            exempt_code: None,
        };
        for (n, models) in i.exec.iter().enumerate() {
            ua.commits.push(authorship::Authored {
                commit: format!("eeeeeeee{n:02}"),
                models: models.clone(),
                source: authorship::Source::Trailer,
                code: true,
                proof: false,
                paths: vec!["crates/x/src/rules.rs".into()],
                in_range: true,
            });
        }
        for (id, models) in &i.proof {
            ua.commits.push(authorship::Authored {
                commit: id.clone(),
                models: models.clone(),
                source: authorship::Source::Trailer,
                code: false,
                proof: true,
                paths: vec!["crates/x/src/rules.rs".into()],
                in_range: true,
            });
        }
        w.units.insert(unit.to_string(), ua);
        w
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
        exempt_work: Option<String>,
        cutoff_gaps: Vec<String>,
        cutoff_accepts: Vec<String>,
        exemption_revived: Vec<String>,
        allow_cutoff: bool,
        exec_override: Option<Vec<Vec<String>>>,
        exists_ok: bool,
        /// Whether the check's `ancestor` answer is yes: an exemption line whose `from_commit` is not in the past of its
        /// `to_commit` names no stretch of history (review 2, New 3).
        ancestor_ok: bool,
        extractor_changed: Vec<String>,
        unit_files_changed: Vec<String>,
        allow_extractor: bool,
        omit_hazards: Vec<String>,
        omit_bad_tags: Vec<String>,
        registration_exempt: Vec<String>,
        /// Do not add the default statement/proof commit by the records' provers.
        bare_proof: bool,
        /// Commits since the history lower bound (earlier merges included); `None`: no extra history.
        history: Option<ChangeInfo>,
        hist_avail: bool,
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
            exempt_work: None,
            cutoff_gaps: vec![],
            cutoff_accepts: vec![],
            exemption_revived: vec![],
            allow_cutoff: false,
            exec_override: None,
            exists_ok: true,
            ancestor_ok: true,
            extractor_changed: vec![],
            unit_files_changed: vec![],
            allow_extractor: false,
            omit_hazards: vec![],
            omit_bad_tags: vec![],
            registration_exempt: vec![],
            bare_proof: false,
            history: None,
            hist_avail: true,
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
        // The authorship facts of the scenario, synthesised into the walk `check` reads. A commit that changed code carries no id
        // of its own here (the range's code commits are numbered), which is all the rules need.
        // A pre-rule scenario is walked from `started_before`: what the check sees is the work AFTER it, so a scenario that
        // declares none has nothing (the exempt past is dropped from both sets, roles 1.5).
        let listed_pre_rule = s.cutoff.is_some() && s.records.iter().any(|r| r.kind == "pre-rule");
        let after = listed_pre_rule || s.after_exec.is_some() || !s.after_proof.is_empty();
        let exec: Vec<Vec<String>> = if after {
            s.after_exec.clone().unwrap_or_default()
        } else {
            s.exec_override.clone().unwrap_or_else(|| {
                if s.exec_models.is_empty() {
                    vec![]
                } else {
                    vec![s.exec_models.clone()]
                }
            })
        };
        let proof: Vec<(String, Vec<String>)> = if after {
            s.after_proof.clone()
        } else if !s.proof_commits.is_empty() {
            s.proof_commits.clone()
        } else if s.bare_proof || s.base.as_ref() == Some(&s.work) {
            // The default stands for "the record's prover wrote the text", which is a fact of a unit the range MOVED. With the
            // fingerprints equal, nothing moved unless the scenario names a proof commit; the real walk would report none.
            vec![]
        } else {
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
        };
        let mut ua = authorship::UnitAuthors {
            commits: vec![],
            pre_rule_impl: None,
            pre_rule_start: None,
            exempt_code: None,
        };
        {
            let push = |commit: String,
                        models: Vec<String>,
                        code: bool,
                        proof: bool,
                        in_range: bool,
                        ua: &mut authorship::UnitAuthors| {
                if let Some(x) = ua
                    .commits
                    .iter_mut()
                    .find(|a| a.commit == commit && a.in_range == in_range)
                {
                    x.code |= code;
                    x.proof |= proof;
                    for m in models {
                        if !x.models.contains(&m) {
                            x.models.push(m);
                        }
                    }
                    return;
                }
                ua.commits.push(authorship::Authored {
                    commit,
                    models,
                    source: authorship::Source::Trailer,
                    code,
                    proof,
                    paths: vec!["crates/x/src/rules.rs".into()],
                    in_range,
                });
            };
            for (n, m) in exec.iter().enumerate() {
                push(
                    format!("eeeeeeee{n:02}"),
                    m.clone(),
                    true,
                    false,
                    true,
                    &mut ua,
                );
            }
            for (c, m) in proof.iter() {
                push(c.clone(), m.clone(), false, true, true, &mut ua);
            }
            if let Some(h) = &s.history {
                for (n, m) in h.exec.iter().enumerate() {
                    push(
                        format!("hhhhhhhh{n:02}"),
                        m.clone(),
                        true,
                        false,
                        false,
                        &mut ua,
                    );
                }
                for (c, m) in h.proof.iter() {
                    push(c.clone(), m.clone(), false, true, false, &mut ua);
                }
            }
        }
        let mut walk = authorship::Walk {
            lb: HISTORY_LOWER_BOUND.into(),
            ..Default::default()
        };
        walk.lb_missing = !s.hist_avail;
        if !ua.commits.is_empty() {
            walk.units.insert("verus/u".into(), ua);
        }
        check(&CheckIn {
            base: &base,
            work: &work,
            entries: &entries,
            base_log: &s.base_log,
            work_log: &log,
            verdict_ok: &vok,
            added: &added,
            authors: &walk,
            verdict_modified: &s.verdict_modified,
            merge_gate: s.merge_gate,
            cutoff_work: s.cutoff.as_deref(),
            cutoff_changed: &s.cutoff_changed,
            exempt_work: s.exempt_work.as_deref(),
            cutoff_gaps: &s.cutoff_gaps,
            cutoff_accepts: &s.cutoff_accepts,
            exemption_revived: &s.exemption_revived,
            allow_cutoff: s.allow_cutoff,
            exists: &|_: &str| s.exists_ok,
            ancestor: &|_: &str, _: &str| s.ancestor_ok,
            omit_hazards: &s.omit_hazards,
            omit_bad_tags: &s.omit_bad_tags,
            registration_exempt: &s.registration_exempt,
            extractor_changed: &s.extractor_changed,
            unit_files_changed: &s.unit_files_changed,
            allow_extractor: s.allow_extractor,
            attrib: None,
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

    /// Review 3 (J3b): the builder rule must not wait for a fingerprint. A moved `//@omit` tag is proof text the keyed tags of
    /// the proof stream see while no fingerprint moves, so the record path never runs and the unit used to pass.
    #[test]
    fn a_builder_who_moves_proof_text_no_fingerprint_sees_is_refused() {
        let mut s = scn(Some(a()), a(), vec![]);
        s.proof_commits = vec![("qqqqqqqqqq".into(), vec![SON.into()])];
        let o = run(&s);
        assert!(
            rules(&o).contains(&"proof-or-statement-written-by-a-builder"),
            "{:?}",
            o.refusals
        );
        assert!(
            o.refusals[0]
                .detail
                .contains("the proof stream moved and no fingerprint did"),
            "{}",
            o.refusals[0].detail
        );
        // the same edit by a strong model is no builder refusal, and the range holds no code for it to clash with
        let mut s = scn(Some(a()), a(), vec![]);
        s.proof_commits = vec![("qqqqqqqqqq".into(), vec![OPUS.into()])];
        assert!(run(&s).refusals.is_empty());
    }

    /// Review 5 (scenarios D and E) and review D/E/D2: a commit of the range that changed the code and names no model at all
    /// slipped past the one-model rule on its own branch, and its cost landed on the next branch that read the same history as
    /// `author-unknown`. The range refuses it itself, with or without a record, and names the commit that lacks the trailer -
    /// not a later one, and not the branch that happened to read the history.
    #[test]
    fn an_untrailered_code_commit_of_the_range_is_refused_on_its_own_branch() {
        // the range's code commits are synthesised as `eeeeeeee00`, `eeeeeeee01`, ... by `run`
        const R0: &str = "commit eeeeeeee00";
        // E: an exec-only change with no history and no record: nothing but the refusal is due
        let mut w = a();
        w.exec = "dddddddddddddddd".into();
        let exec_only = w.clone();
        let mut s = scn(Some(a()), w.clone(), vec![]);
        s.exec_override = Some(vec![vec![]]);
        let o = run(&s);
        assert_eq!(
            rules(&o),
            ["code-commit-without-trailer"],
            "{:?}",
            o.refusals
        );
        assert!(
            o.refusals[0]
                .detail
                .contains("carries no Co-Authored-By trailer"),
            "{}",
            o.refusals[0].detail
        );
        assert!(
            o.refusals[0].detail.contains(R0),
            "the refusal names the commit that lacks the trailer: {}",
            o.refusals[0].detail
        );
        // D: the same edit on top of a history that already holds Opus proof text, which is the shape that used to pass
        let mut s = scn(Some(a()), w.clone(), vec![]);
        s.exec_override = Some(vec![vec![]]);
        s.history = Some(ChangeInfo {
            exec: vec![],
            proof: vec![("hhhhhhhhhh".into(), vec![OPUS.into()])],
        });
        let o = run(&s);
        assert_eq!(
            rules(&o),
            ["code-commit-without-trailer"],
            "the branch that made the edit is the one that refuses it"
        );
        assert!(
            o.refusals[0].detail.contains(R0),
            "and it names that commit, not the history it lands on: {}",
            o.refusals[0].detail
        );
        // a record involved: the range's other code commit is a builder's, the untrailered one is still refused at once
        let mut w = a();
        w.stmt = "1111111111111111".into();
        let mut s = scn(Some(a()), w.clone(), vec![rec("statement", "verus/u", &w)]);
        s.exec_override = Some(vec![vec![], vec![SON.into()]]);
        let o = run(&s);
        assert_eq!(
            rules(&o),
            ["code-commit-without-trailer"],
            "the record path refuses it too"
        );
        assert!(
            o.refusals[0].detail.contains(R0),
            "and still at the commit that lacks it, though a later commit of the same range is trailered: {}",
            o.refusals[0].detail
        );
        // the same range with a trailer on every code commit is nobody's trailer problem
        let mut s = scn(Some(a()), w.clone(), vec![rec("statement", "verus/u", &w)]);
        s.exec_override = Some(vec![vec![SON.into()], vec![SON.into()]]);
        let o = run(&s);
        assert!(o.refusals.is_empty(), "{:?}", o.refusals);
        // the earlier-history shape of the same gap (review D2): a code commit below the base that names nobody is refused
        // as `author-unknown` on ITS OWN id, and the range's trailered commit is not charged for it
        let mut s = scn(Some(a()), exec_only, vec![]);
        s.exec_override = Some(vec![vec![SON.into()]]);
        s.history = Some(ChangeInfo {
            exec: vec![vec![]],
            proof: vec![],
        });
        let o = run(&s);
        assert_eq!(
            rules(&o),
            ["author-unknown"],
            "the history commit lacks the trailer, not the range's: {:?}",
            o.refusals
        );
        assert!(
            o.refusals[0].detail.contains("commit hhhhhhhh00")
                && o.refusals[0].detail.contains("changed this unit's code"),
            "the refusal names the untrailered history commit: {}",
            o.refusals[0].detail
        );
        assert!(
            !o.refusals[0].detail.contains(R0),
            "and not the range's own commit: {}",
            o.refusals[0].detail
        );
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
        // a commit that shaped the code counts even when the code part ends where it started: the rule reads commits, not net effects
        let mut w2 = a();
        w2.stmt = "1111111111111111".into();
        let mut s2 = scn(
            Some(a()),
            w2.clone(),
            vec![rec("statement", "verus/u", &w2)],
        );
        s2.exec_models = vec![OPUS.into()];
        assert!(
            rules(&run(&s2)).contains(&"one-model-wrote-code-and-proof"),
            "{:?}",
            run(&s2).refusals
        );
        s2.exec_models = vec![SON.into()];
        assert!(run(&s2).refusals.is_empty(), "{:?}", run(&s2).refusals);
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

    /// The hole (2026-11-17): a line of the log that cannot be read, or a statement record the roles rules refuse, was skipped in
    /// silence, so a branch whose unit an OLDER valid record covers reported ok with a refused review sitting in the log. An
    /// invalid line is a refusal of its own, named by file, line and id, whatever other records match.
    #[test]
    fn an_invalid_line_of_the_base_log_is_refused_although_a_valid_record_matches() {
        let mut w = a();
        w.stmt = "1111111111111111".into();
        let good = rec("statement", "verus/u", &w);
        let invalid = |o: &Outcome| {
            o.refusals
                .iter()
                .find(|x| x.rule == "invalid-record-line")
                .unwrap_or_else(|| panic!("{:?}", o.refusals))
                .detail
                .clone()
        };
        // a malformed line of the base, and a valid record of the same unit that covers the change: still refused, and the bad line
        // is named so the reader finds it.
        let mut s = scn(Some(a()), w.clone(), vec![good.clone()]);
        s.base_log = "# h\nbroken\tline\n".into();
        let d = invalid(&run(&s));
        assert!(d.contains("line 2"), "{d}");
        assert!(d.contains("broken"), "{d}");
        assert!(d.contains("expected 19"), "{d}");
        // a line that parses but is a refused statement record: a reviewer of the prover's own family
        let mut bad = rec("statement", "verus/u", &a());
        bad.id = "old-review".into();
        bad.verdict_ref = "docs/ci/verdicts/old-review.md".into();
        bad.reviewer_model = "claude-haiku-4-5".into();
        bad.reviewer_family = "claude".into();
        let mut s = scn(Some(a()), w.clone(), vec![good]);
        s.base_log = format!("# h\n{}\n", line(&bad));
        let d = invalid(&run(&s));
        assert!(d.contains("line 2"), "{d}");
        assert!(d.contains("old-review"), "{d}");
        assert!(d.contains("is the prover's family"), "{d}");
        // a log of only valid lines stays ok: the same scenario, with a well-formed statement record of the base
        let mut older = rec("statement", "verus/u", &a());
        older.id = "old-ok".into();
        older.verdict_ref = "docs/ci/verdicts/old-ok.md".into();
        let mut s = scn(Some(a()), w.clone(), vec![rec("statement", "verus/u", &w)]);
        s.base_log = format!("# h\n{}\n", line(&older));
        assert!(run(&s).refusals.is_empty(), "{:?}", run(&s).refusals);
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

    const ROWS_BASE: &str = r#"
verus! {
pub fn rows_ok(rows: &[(u32, u32)], height: u32) -> Option<u64> {
    let mut total: u64 = 0;
    let mut i: usize = 0;
    #[cfg_attr(verus_keep_ghost, verus_spec(
        invariant
            0 <= i <= rows@.len(),
            total as int == rows_sum(rows@.subrange(0, i as int)),
        decreases rows@.len() - i
    ))]
    while i < rows.len() {
        let (y, h) = rows[i];
        if h == 0 || y >= height || h > height - y {
            return None;
        }
        if h as u64 > u64::MAX - total {
            return None;
        }
        total = total + h as u64;
        i += 1;
    }
    Some(total)
}
}
"#;

    const ROWS_WITH: &str = r#"
verus! {
pub fn rows_ok(rows: &[(u32, u32)], height: u32) -> Option<u64> {
    let mut total: u64 = 0;
    let mut i: usize = 0;
    #[cfg_attr(verus_keep_ghost, verus_spec(
        invariant
            0 <= i <= rows@.len(),
            total as int == rows_sum(rows@.subrange(0, i as int)),
        decreases rows@.len() - i
    ))]
    while i < rows.len() {
        let (y, h) = rows[i];
        if h == 0 || y >= height || h > height - y {
            #[cfg(verus_keep_ghost)]
            proof! {
                assert(!row_inside(rows@[i as int], height));
            }
            return None;
        }
        if h as u64 > u64::MAX - total {
            #[cfg(verus_keep_ghost)]
            proof! {
                assert(rows@.subrange(0, i as int + 1).drop_last() =~= rows@.subrange(0, i as int));
                lemma_rows_prefix_le(rows@, i as int + 1);
            }
            return None;
        }
        total = total + h as u64;
        i += 1;
        #[cfg(verus_keep_ghost)]
        proof! {
            assert(rows@.subrange(0, i as int).drop_last() =~= rows@.subrange(0, i as int - 1));
        }
    }
    #[cfg(verus_keep_ghost)]
    proof! {
        assert(rows@.subrange(0, i as int) =~= rows@);
    }
    Some(total)
}
}
"#;

    // The four ghost blocks of `rows_ok` (aeaa4f6fa, a prover's commit that changed no executable line) each cut one exec run in two,
    // and the `;;` of the cut is part of exec_fp: the block was proof text, but where the prover put it was code.

    const PB_REFUSAL: &str = concat!(
        "            #[cfg(verus_keep_ghost)]\n",
        "            proof! {\n",
        "                assert(!row_inside(rows@[i as int], height));\n",
        "            }\n",
    );
    const PB_OVERFLOW: &str = concat!(
        "            #[cfg(verus_keep_ghost)]\n",
        "            proof! {\n",
        "                assert(rows@.subrange(0, i as int + 1).drop_last() =~= rows@.subrange(0, i as int));\n",
        "                lemma_rows_prefix_le(rows@, i as int + 1);\n",
        "            }\n",
    );
    const PB_STEP: &str = concat!(
        "        #[cfg(verus_keep_ghost)]\n",
        "        proof! {\n",
        "            assert(rows@.subrange(0, i as int).drop_last() =~= rows@.subrange(0, i as int - 1));\n",
        "        }\n",
    );
    const PB_DONE: &str = concat!(
        "    #[cfg(verus_keep_ghost)]\n",
        "    proof! {\n",
        "        assert(rows@.subrange(0, i as int) =~= rows@);\n",
        "    }\n",
    );
    const ALL_PB: &[&str] = &[PB_REFUSAL, PB_OVERFLOW, PB_STEP, PB_DONE];

    /// `ROWS_WITH` with the blocks kept that are (`keep_it`) or are not (`!keep_it`) the one named `pick`.
    fn rows_cut(pick: &str, keep_it: bool) -> String {
        ALL_PB.iter().fold(ROWS_WITH.to_string(), |s, b| {
            if (*b == pick) == keep_it {
                s
            } else {
                s.replace(b, "")
            }
        })
    }

    #[test]
    fn a_whole_gated_proof_block_is_proof_text_and_not_code() {
        let (b, w) = (fp_of(ROWS_BASE), fp_of(ROWS_WITH));
        assert_ne!(b.proof, w.proof, "the blocks are proof text");
        assert_eq!(
            (&w.stmt, &w.exec, &w.data),
            (&b.stmt, &b.exec, &b.data),
            "the four rows_ok blocks moved the code:\n{}",
            classify("c/src/rules.rs", ROWS_WITH).exec_text
        );
        for p in ALL_PB {
            for (name, src) in [("lone", rows_cut(p, true)), ("missing", rows_cut(p, false))] {
                let f = fp_of(&src);
                assert_eq!(
                    (&f.stmt, &f.exec, &f.data),
                    (&b.stmt, &b.exec, &b.data),
                    "the {name} block {p:?} moved the code"
                );
                assert_ne!(
                    f.proof, b.proof,
                    "the {name} block {p:?} left the proof text alone"
                );
            }
        }
        // the same block one statement earlier: the code is the same code
        let moved = ROWS_WITH.replace(PB_STEP, "").replace(
            "        total = total + h as u64;",
            &format!("{PB_STEP}        total = total + h as u64;"),
        );
        let m = fp_of(&moved);
        assert_eq!((&m.stmt, &m.exec, &m.data), (&b.stmt, &b.exec, &b.data));
        assert_ne!(m.proof, b.proof);
    }

    const ONE: &str = r#"
verus! {
pub fn f(x: u32) -> (y: u32) ensures y > x {
    let mut y = x;
    y += 1;
    y
}
}
"#;

    /// `src` with `block` put between `let mut y = x;` and `y += 1;`, in the middle of the executable body.
    fn in_middle(src: &str, block: &str) -> String {
        src.replace("    y += 1;", &format!("{block}    y += 1;"))
    }

    #[test]
    fn a_proof_block_of_any_shape_stays_out_of_the_code() {
        let b = fp_of(ONE);
        for block in [
            "    #[cfg(verus_keep_ghost)]\n    proof! { g(y); }\n",
            "    #[cfg(verus_keep_ghost)]\n    proof! { g(y); };\n",
            "    #[cfg(verus_keep_ghost)]\n    proof!(g(y));\n",
            "    #[cfg(verus_keep_ghost)]\n    proof![g(y)];\n",
            "    #[cfg(verus_keep_ghost)]\n    proof_with! { g(y); }\n",
            "    #[cfg(verus_keep_ghost)]\n    proof_with!(g(y));\n",
        ] {
            let w = fp_of(&in_middle(ONE, block));
            assert_eq!(
                (&w.stmt, &w.exec, &w.data),
                (&b.stmt, &b.exec, &b.data),
                "{block:?} moved the code"
            );
            assert_ne!(w.proof, b.proof, "{block:?} left the proof text alone");
        }
    }

    /// The exemption is the GATE, not the call: `proof!` without `#[cfg(verus_keep_ghost)]` is a macro any crate can define and the
    /// normal build can compile, so it keeps its place in the code stream exactly as it had before this rule existed.
    #[test]
    fn an_ungated_proof_block_still_shows_in_the_code() {
        let b = fp_of(ONE);
        for block in ["    proof! { g(y); }\n", "    proof! { g(y); };\n"] {
            let src = in_middle(ONE, block);
            let w = fp_of(&src);
            // Its body is proof text and its place is code: exactly what this branch did before the rule landed.
            assert_ne!(
                w.exec, b.exec,
                "{block:?}: a bare block hid its place from the code"
            );
            assert_ne!(
                w.proof, b.proof,
                "{block:?}: a bare block is not proof text"
            );
            assert_eq!((&w.stmt, &w.data), (&b.stmt, &b.data), "{block:?}");
            // and an edit inside it stays a proof change, not a code change — it is the same class as before
            let edited = fp_of(&src.replace("g(y)", "g(y, z)"));
            assert_eq!(
                edited.exec, w.exec,
                "{block:?}: the class of a bare block moved"
            );
            assert_ne!(
                edited.proof, w.proof,
                "{block:?}: the body of a bare block is code"
            );
            // the same block further down the same body is a different code fingerprint: the gate, not the block, is what is invisible
            let moved = fp_of(&ONE.replace("    y += 1;", &format!("    y += 1;\n{block}")));
            assert_ne!(
                moved.exec, w.exec,
                "{block:?}: where a bare block sits was free"
            );
        }
    }

    /// `#[cfg(not(verus_keep_ghost))]` is the opposite gate: what it gates is in the shipped build, so the block is code in full and
    /// an edit inside it is a code change.
    #[test]
    fn a_not_gated_proof_block_is_code_through_and_through() {
        let b = fp_of(ONE);
        let block = "    #[cfg(not(verus_keep_ghost))]\n    proof! { g(y); }\n";
        let src = in_middle(ONE, block);
        let w = fp_of(&src);
        assert_ne!(w.exec, b.exec, "a not-gated block hid itself from the code");
        let edited = fp_of(&src.replace("g(y)", "g(y, z)"));
        assert_ne!(
            edited.exec, w.exec,
            "an edit inside a not-gated block left the code alone"
        );
    }

    #[test]
    fn the_ghost_gate_in_front_of_anything_else_is_code() {
        let b = fp_of(ONE);
        for gated in ["let mut z = y;", "y += 2;", "if y > 0 { y -= 1; }", "g(y);"] {
            let block = format!("    #[cfg(verus_keep_ghost)]\n    {gated}\n");
            let w = fp_of(&in_middle(ONE, &block));
            assert_ne!(w.exec, b.exec, "the gate on `{gated}` hid code");
        }
        // `not(..)` is the other way round: the block is compiled by the normal build, so its place is code
        let w = fp_of(&in_middle(
            ONE,
            "    #[cfg(not(verus_keep_ghost))]\n    proof! { g(y); }\n",
        ));
        assert_ne!(w.exec, b.exec, "a not(verus_keep_ghost) gate hid code");
    }

    #[test]
    fn a_file_that_brought_its_own_proof_macro_has_no_ghost_blocks() {
        for header in [
            "macro_rules! proof { ($e:expr) => { $e }; }\n",
            "macro_rules! proof_with { ($e:expr) => { $e }; }\n",
            "use crate::ghost::m as proof;\n",
            "use crate::ghost::{m as proof_with, n};\n",
            "use crate::ghost::proof;\n",
            "use crate::ghost::proof_with;\n",
            "use crate::ghost::{proof, n};\n",
            "use crate::ghost::{n, proof_with};\n",
            "use crate::ghost::{a::{proof, b}, n};\n",
        ] {
            let src = format!("{header}{ONE}");
            let b = fp_of(&src);
            let block = "    #[cfg(verus_keep_ghost)]\n    proof! { g(y); }\n";
            let w = fp_of(&in_middle(&src, block));
            assert_ne!(
                w.exec, b.exec,
                "{header:?}: `proof!` is this file's own macro"
            );
            // and a change inside the block is a code change there
            let edited = fp_of(&in_middle(&src, block).replace("g(y)", "g(x)"));
            assert_ne!(
                edited.exec, w.exec,
                "{header:?}: the body of the own macro is code"
            );
        }
    }

    /// A `use` that does not bind the name leaves the file alone: only a name `proof`/`proof_with` in scope is an own macro.
    #[test]
    fn a_use_that_binds_another_name_keeps_the_ghost_block_exemption() {
        for header in [
            "use crate::ghost::proof::n;\n",
            "use crate::ghost::proof_as_medicine;\n",
            "use crate::ghost::proof::deep::n;\n",
            "use crate::ghost::{n, o};\n",
        ] {
            let src = format!("{header}{ONE}");
            let b = fp_of(&src);
            let w = fp_of(&in_middle(
                &src,
                "    #[cfg(verus_keep_ghost)]\n    proof! { g(y); }\n",
            ));
            assert_eq!(
                (&w.stmt, &w.exec, &w.data),
                (&b.stmt, &b.exec, &b.data),
                "{header:?}: a path that passes through the word cost the file its exemption"
            );
            assert_ne!(w.proof, b.proof, "{header:?}");
        }
    }

    #[test]
    fn a_proof_call_behind_a_path_or_under_another_name_is_code() {
        let b = classify("c/src/one.rs", ONE);
        assert!(b.errors.is_empty(), "{:?}", b.errors);
        for block in [
            "    #[cfg(verus_keep_ghost)]\n    foo::proof! { g(y); }\n",
            "    #[cfg(verus_keep_ghost)]\n    ::proof! { g(y); }\n",
            "    #[cfg(verus_keep_ghost)]\n    my_proof! { g(y); }\n",
            "    #[cfg(verus_keep_ghost)]\n    proof_with_extra! { g(y); }\n",
        ] {
            let src = in_middle(ONE, block);
            let c = classify("c/src/one.rs", &src);
            assert!(c.errors.is_empty(), "{block:?}: {:?}", c.errors);
            assert!(
                c.exec_text.contains("proof"),
                "{block:?} left the code: {}",
                c.exec_text
            );
            let edited = classify("c/src/one.rs", &src.replace("g(y)", "g(x)"));
            assert_ne!(
                edited.exec_text, c.exec_text,
                "{block:?}: the body of the call hid from the code"
            );
        }
    }

    #[test]
    fn an_unclosed_proof_block_is_not_a_ghost_block() {
        let b = classify("c/src/one.rs", ONE);
        let src = in_middle(ONE, "    #[cfg(verus_keep_ghost)]\n    proof! { g(y);\n");
        let c = classify("c/src/one.rs", &src);
        assert_ne!(
            c.exec_text, b.exec_text,
            "an unclosed block left the code untouched"
        );
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
    fn an_omit_tag_is_proof_text_and_its_line_is_code() {
        let src = "#[derive(Debug)]\npub struct S { pub a: u32 }\npub fn f(s: S) -> u32 //@ret\n//@ ensures ret == s.a,\n{\n    s.a\n}\n";
        let tagged = src.replace("#[derive(Debug)]", "#[derive(Debug)] //@omit");
        let (a, b) = (shipped_fp(src), shipped_fp(&tagged));
        // The TAG is the prover's choice of what the verifier reads: the proof fingerprint moves, and the line leaves the
        // program Verus reads (exec_fp), while the statements and the data a statement refers to do not.
        assert_ne!(a.proof, b.proof, "adding //@omit moves the proof text");
        assert_ne!(a.exec, b.exec, "the tagged line leaves what Verus reads");
        assert_eq!(a.stmt, b.stmt);
        assert_eq!(a.data, b.data);
        // and it is no code: the program rustc reads is the same
        assert_eq!(
            shipped_core(src).code,
            shipped_core(&tagged).code,
            "the tag itself is not code"
        );
        let cb = cl(&tagged);
        assert!(
            cb.proof_text.contains("omit: #[derive(Debug)] //@omit"),
            "{}",
            cb.proof_text
        );
        assert!(
            !cb.author_proof.contains("Debug"),
            "the proof stream holds the tag of the line, never its content: {}",
            cb.author_proof
        );
        // the CONTENT of the tagged line is code alone: editing it moves the code stream and no stream the prover owns
        let edited = tagged.replace(
            "#[derive(Debug)] //@omit",
            "#[derive(Debug, Clone)] //@omit",
        );
        let c = shipped_fp(&edited);
        assert_ne!(
            shipped_core(&tagged).code,
            shipped_core(&edited).code,
            "the content is in the code stream"
        );
        // main counts the tagged line into the proof fingerprint whole (spec 39: the fingerprints of the shipped units
        // do not change), so a content change moves it and needs a record; what the line is NOT is proof TEXT for the
        // authorship rule, which the two streams above hold.
        assert_ne!(
            b.proof, c.proof,
            "the proof fingerprint follows the line, as on main"
        );
        let (bt, ct) = (cl(&tagged), cl(&edited));
        assert_eq!(
            bt.author_proof, ct.author_proof,
            "and the proof STREAM does not follow it: {} | {}",
            bt.author_proof, ct.author_proof
        );
        assert_ne!(bt.author_code, ct.author_code, "the content is code");
        assert_eq!(b.stmt, c.stmt);
        assert_eq!(b.exec, c.exec);
        assert_eq!(b.data, c.data);
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
        let m = added_map(&d, &base, &authorship::ModelResolver::default());
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
        let authors = walk_of("shipped/x/rules", &i);
        let o = check(&CheckIn {
            base: &basem,
            work: &work,
            entries: &entries,
            base_log: LOG_HEADER,
            work_log: &format!("{LOG_HEADER}{line_text}\n"),
            verdict_ok: &vok,
            added: &added,
            authors: &authors,
            verdict_modified: &[],
            merge_gate: true,
            cutoff_work: None,
            cutoff_changed: &[],
            exempt_work: None,
            cutoff_gaps: &[],
            cutoff_accepts: &[],
            exemption_revived: &[],
            allow_cutoff: false,
            exists: &|_: &str| false,
            ancestor: &|_: &str, _: &str| false,
            omit_hazards: &[],
            omit_bad_tags: &[],
            registration_exempt: &[],
            extractor_changed: &[],
            unit_files_changed: &[],
            allow_extractor: false,
            attrib: None,
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
        let m = added_map(&d, &base, &authorship::ModelResolver::default());
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
        let m = added_map(&d, &base, &authorship::ModelResolver::default());
        assert!(m.contains_key(&line3), "{m:?}");
        let _ = fs::remove_dir_all(&d);
    }

    /// Review 8: `trailer-mismatch` reads `models(C)`, not the raw trailer of the commit. A ledger entry for the commit
    /// that added the record line decides it, so the mismatch check sees exactly what the walk saw.
    #[test]
    fn the_record_line_is_read_by_the_ledger_before_its_trailer() {
        let d = tmp_repo("added-ledger");
        g(&d, &["init", "-q", "-b", "main"]);
        fs::create_dir_all(d.join("docs/ci")).unwrap();
        fs::write(d.join(LOG), LOG_HEADER).unwrap();
        let base = commit(&d, "base", Some("Claude Opus 5.5"));
        let line = "rec-9\t2026-11-12\tproof-edit\tshipped/x/rules\t-\t-\t-\t-\tclaude-sonnet-5-5\tclaude\tclaude-opus-5-5\tclaude\tstrong\t-\t-\taccepted\tdocs/ci/verdicts/rec-9.md\t-\tn";
        fs::write(d.join(LOG), format!("{LOG_HEADER}{line}\n")).unwrap();
        let added_by = commit(&d, "record by the builder", Some("Claude Sonnet 5.5"));
        let bare = added_map(&d, &base, &authorship::ModelResolver::default());
        assert_eq!(
            bare.get(line),
            Some(&vec!["claude-sonnet-5-5".to_string()]),
            "the trailer alone names the builder: {bare:?}"
        );
        // the lane's ledger says the commit was made by the strong prover's session
        let led = ledger_at(
            &d,
            &ledger_line(&added_by, "Claude Opus 5.5", authorship::LEDGER_REPO, ZEROS),
        );
        let r = authorship::ModelResolver::new(Some(&led), None);
        assert!(r.ledger_broken.is_none(), "{:?}", r.ledger_broken);
        assert_eq!(
            added_map(&d, &base, &r).get(line),
            Some(&vec!["claude-opus-5-5".to_string()]),
            "the ledger decides, as it does for the walk"
        );
        let _ = fs::remove_dir_all(&d);
    }

    /// Review 8: a blob the object store does not have is an UNKNOWN version, not an absent file. The walk credits the
    /// commit that holds it on both sides and says so, so a missing object can never read as "nothing changed here".
    #[test]
    fn a_missing_blob_is_read_as_an_unknown_version_not_as_an_absent_file() {
        let d = tmp_repo("gone");
        g(&d, &["init", "-q", "-b", "main"]);
        fs::create_dir_all(d.join("crates/x/src")).unwrap();
        fs::write(d.join("README"), "x\n").unwrap();
        let lb = commit(&d, "lb", Some("Claude Opus 5.5"));
        let unit = Unit {
            name: "shipped/x/rules".into(),
            group: "g".into(),
            kind: "shipped".into(),
            target: "crates/x/src/rules.rs".into(),
            inputs: vec![],
        };
        let files = vec!["crates/x/src/rules.rs".to_string()];
        fs::write(d.join("crates/x/src/rules.rs"), RULES_RS).unwrap();
        let base = commit(&d, "the shipped unit", Some("Claude Sonnet 5.5"));
        fs::write(
            d.join("crates/x/src/rules.rs"),
            RULES_RS.replace("i += 1;", "i += 1; let _z = 0;"),
        )
        .unwrap();
        let gone = commit(
            &d,
            "a version whose object is lost",
            Some("Claude Fable 5.2"),
        );
        let oid = g(&d, &["rev-parse", &format!("{gone}:crates/x/src/rules.rs")]);
        let loose = d
            .join(".git")
            .join("objects")
            .join(&oid[..2])
            .join(&oid[2..]);
        assert!(loose.is_file(), "a fresh commit holds a loose object");
        fs::remove_file(&loose).unwrap();
        let r = authorship::ModelResolver::default();
        let w = authorship::walk(
            &d,
            &base,
            &ufps(std::slice::from_ref(&unit), &unit.name, &files),
            &Cutoff {
                commit: lb.clone(),
                units: vec![],
                exempt: Vec::new(),
            },
            &BTreeMap::new(),
            &r,
            None,
        )
        .expect("the walk reads a repository with a lost object");
        assert!(
            w.notes
                .iter()
                .any(|n| n.contains(&authorship::short(&oid))
                    && n.contains("not in this repository")),
            "the walk says which object is gone: {:?}",
            w.notes
        );
        let a = w
            .authors(&unit.name)
            .commits
            .iter()
            .find(|x| x.commit == gone)
            .expect("the unreadable version is credited, not skipped");
        assert!(
            a.code && a.proof,
            "an unknown version counts on both sides: {a:?}"
        );
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

    /// The two content streams of one source, as the rule reads them (roles 1.1).
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct Core {
        code: u64,
        proof: u64,
    }

    fn streams_at(path: &str, src: &str) -> Core {
        let s = authorship::streams(path, src, false);
        assert!(s.ok, "the source is classifiable: {path}");
        Core {
            code: s.code,
            proof: s.proof,
        }
    }

    fn core_of(src: &str) -> Core {
        streams_at("crates/x/src/lib.rs", src)
    }

    /// Adding clauses or a `//@omit` tag is no code change: the program rustc compiles is the same.
    fn promotion_only(b: &Core, a: &Core) -> bool {
        b.code == a.code
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
        assert!(promotion_only(
            &core_of(&vsrc("pub fn f(x: u32) -> u32 requires x < 9 { x + 1 }")),
            &core_of(&vsrc("pub fn f(x: u32) -> u32 { x + 1 }"))
        ));
        assert_ne!(stmt.stmt, bare_fp.stmt);
        // a body edited by the prover while it adds clauses is caught even when clauses are all it "meant" to add
        assert!(!promotion_only(
            &bare,
            &with("pub fn f(x: u32) -> u32 ensures true", "x")
        ));
    }

    fn shipped_fp(src: &str) -> Fp {
        let t = MemTree([("crates/x/src/rules.rs".to_string(), src.to_string())].into());
        let (v, e) = fingerprints(&t, &[rules_unit().0]);
        assert!(e.is_empty(), "{e:?}");
        v[0].fp.clone().unwrap()
    }

    fn shipped_core(src: &str) -> Core {
        streams_at("crates/x/src/rules.rs", src)
    }

    const SBARE: &str = "pub fn f(n: usize) -> usize\n{\n    n + 1\n}\n";
    const SCLAUSES: &str = "pub fn f(n: usize) -> usize //@ret\n//@ requires n < 10,\n//@ ensures ret == n + 1,\n{\n    n + 1\n}\n";

    // The real pair that was a false positive: dfff1d83f (plain) -> 52518f468 (a prover adds `//@ ensures`, spec fns, a lemma, a
    // `//@ret` tag and a `//@omit` on the derive; no code line changes).
    const SLIDE_BEFORE: &str = include_str!("fixtures/slide_rules_before.rs.txt");
    const SLIDE_AFTER: &str = include_str!("fixtures/slide_rules_after.rs.txt");

    #[test]
    fn real_prover_commit_with_omit_ret_and_clauses_does_not_move_the_code_stream() {
        let (b, a) = (shipped_core(SLIDE_BEFORE), shipped_core(SLIDE_AFTER));
        assert_eq!(b.code, a.code, "the program rustc reads is the same");
        assert!(promotion_only(&b, &a));
        // the prover's lines are proof text, so the proof side moves
        let (cb, ca) = (
            classify("crates/x/src/rules.rs", SLIDE_BEFORE),
            classify("crates/x/src/rules.rs", SLIDE_AFTER),
        );
        assert_ne!(cb.proof_text, ca.proof_text);
        assert!(
            ca.proof_text
                .contains("omit: #[derive(Clone, Copy, Debug, PartialEq, Eq)] //@omit"),
            "{}",
            ca.proof_text
        );
        assert!(
            !ca.author_proof.contains("PartialEq"),
            "the proof stream holds the tag of the line, not its content: {}",
            ca.author_proof
        );
        // a prover that also changes one code token is an exec change
        let changed = SLIDE_AFTER.replace("let edge = off;", "let edge = off + 1;");
        assert_ne!(changed, SLIDE_AFTER);
        assert!(!promotion_only(&b, &shipped_core(&changed)));
        assert_ne!(a.code, shipped_core(&changed).code);
    }

    #[test]
    fn omit_line_content_is_code_but_its_tag_is_not() {
        let plain = "#[derive(Clone, Copy, Debug)]\npub struct S { pub a: u32 }\npub fn f(s: S) -> u32\n{\n    s.a\n}\n";
        let tagged = plain.replace("Debug)]\n", "Debug)] //@omit\n");
        // the content of the tagged line is code: splitting the derive or rewrapping it moves the code stream and nothing else
        let split = tagged.replace(
            "#[derive(Clone, Copy, Debug)] //@omit",
            "#[derive(Clone, Copy)] #[derive(Debug, PartialEq)] //@omit",
        );
        let wrapped = tagged.replace(
            "#[derive(Clone, Copy, Debug)] //@omit",
            "#[derive( Clone, Copy, Debug )] //@omit",
        );
        let (p, t, s, w) = (cl(plain), cl(&tagged), cl(&split), cl(&wrapped));
        for c in [&p, &t, &s, &w] {
            assert!(c.errors.is_empty(), "{:?}", c.errors);
        }
        assert_ne!(
            t.author_code, s.author_code,
            "the content is in the code stream"
        );
        assert_eq!(
            t.author_proof, s.author_proof,
            "and not in the proof stream: the fingerprint representation carries the whole tagged line (test 39: shipped proofs stay digestible), the stream keys the tag"
        );
        assert_eq!(t.exec_text, s.exec_text);
        assert_eq!(t.stmts, s.stmts);
        assert_eq!(
            t.author_code, w.author_code,
            "a rewrap that adds no token moves nothing"
        );
        // the tag itself is not code, and it is proof text
        assert_eq!(p.author_code, t.author_code);
        assert_ne!(p.proof_text, t.proof_text);
        assert!(
            t.proof_text
                .contains("omit: #[derive(Clone, Copy, Debug)] //@omit"),
            "{}",
            t.proof_text
        );
        assert!(
            !t.author_proof.contains("Copy"),
            "the proof stream holds the tag of the line, never its content: {}",
            t.author_proof
        );
    }

    fn cl(src: &str) -> FileClass {
        classify("crates/x/src/rules.rs", src)
    }

    fn clp(path: &str, src: &str) -> FileClass {
        classify(path, src)
    }

    #[test]
    fn the_harness_items_of_a_verification_file_are_in_neither_stream() {
        let harness = "// a comment\n#![feature(proc_macro_hygiene)]\n#[path = \"../../crates/x/src/lib.rs\"]\npub mod x;\n\nuse vstd::prelude::*;\n\nfn main() {}\n";
        let c = clp("verification/x/rules.rs", harness);
        assert!(c.author_code.is_empty(), "{:?}", c.author_code);
        assert!(
            c.author_proof.trim().is_empty(),
            "the proof stream of a bare harness is empty: {}",
            c.author_proof
        );
        // adding the harness is therefore no authorship at all
        assert_eq!(
            clp("verification/x/rules.rs", "// nothing\n").author_code,
            Vec::<String>::new()
        );
        // `use` of the crate under proof is code, not harness
        let with_glob = "use x::*;\nfn main() {}\n";
        assert!(
            clp("verification/x/rules.rs", with_glob)
                .author_code
                .iter()
                .any(|t| t == "*"),
            "{:?}",
            clp("verification/x/rules.rs", with_glob).author_code
        );
    }

    #[test]
    fn a_cfg_attr_verus_payload_is_not_code_but_the_item_under_it_is() {
        let bare = "pub fn f(n: u32) -> u32 { n + 1 }\n";
        let gated = "#[cfg_attr(verus_keep_ghost, verus_verify(ensures f_ret: n + 1))]\npub fn f(n: u32) -> u32 { n + 1 }\n";
        assert_eq!(
            clp("crates/x/src/lib.rs", gated).author_code,
            clp("crates/x/src/lib.rs", bare).author_code,
            "the prover who binds the code to a spec adds no code token"
        );
        assert!(
            !clp("crates/x/src/lib.rs", gated)
                .author_code
                .iter()
                .any(|t| t == "verus_keep_ghost"),
            "{:?}",
            clp("crates/x/src/lib.rs", gated).author_code
        );
        assert!(
            clp("crates/x/src/lib.rs", gated)
                .author_proof
                .contains("verus_verify"),
            "the payload is proof text"
        );
        // on a const and a struct too
        let s_bare = "pub const N: u32 = 3;\npub struct P { pub x: u32 }\n";
        let s_gated = "#[cfg_attr(verus_keep_ghost, verifier::transparent)]\npub const N: u32 = 3;\n#[cfg_attr(verus_keep_ghost, verus_struct)]\npub struct P { pub x: u32 }\n";
        assert_eq!(
            clp("crates/x/src/lib.rs", s_gated).author_code,
            clp("crates/x/src/lib.rs", s_bare).author_code
        );
    }

    #[test]
    fn content_under_a_gate_nobody_compiles_is_proof_text_not_code() {
        let src = "#[cfg(verus_keep_ghost)]\nverus! {\n    pub open spec fn s(n: u32) -> bool { n > 0 }\n}\n\npub fn f(n: u32) -> u32 { n + 1 }\n";
        let c = clp("crates/x/src/lib.rs", src);
        assert!(
            !c.author_code.iter().any(|t| t == "spec"),
            "the verus block is not code: {:?}",
            c.author_code
        );
        assert!(c.author_proof.contains("spec"), "it is proof text");
        assert!(
            c.author_code.iter().any(|t| t == "f") && c.author_code.iter().any(|t| t == "+"),
            "the fn outside the gate is code: {:?}",
            c.author_code
        );
        // `cfg(any())` and `cfg(false)` are dead alike
        for dead in ["#[cfg(any())]", "#[cfg(false)]", "#[cfg(not(all()))]"] {
            let s = format!("{dead}\nmod hidden {{\n    pub fn g() -> u32 {{ 7 }}\n}}\n");
            let x = clp("crates/x/src/lib.rs", &s);
            assert!(
                !x.author_code.iter().any(|t| t == "7"),
                "{dead}: a gate no build compiles hides its code"
            );
            assert!(
                x.author_proof.contains("7"),
                "{dead}: what only the verifier may read is proof text"
            );
        }
    }

    /// Case (a) of the spec, the errseq harness commit: a ghost-gated `use`, `cfg_attr` gates on the shipped items and a
    /// ghost-gated `verus!` block. None of it is in the program rustc compiles, so the harness commit is proof only.
    #[test]
    fn the_errseq_harness_shape_adds_no_code_token() {
        let plain = "pub const MAX_ERRNO: u32 = 4095;\n\npub fn f(n: u32) -> u32 { n + 1 }\n";
        let harnessed = "#[cfg(verus_keep_ghost)]\nuse vstd::prelude::*;\n\n\
             #[cfg_attr(verus_keep_ghost, verus_verify)]\npub const MAX_ERRNO: u32 = 4095;\n\n\
             #[cfg_attr(verus_keep_ghost, verus_verify)]\npub fn f(n: u32) -> u32 { n + 1 }\n\n\
             #[cfg(verus_keep_ghost)]\nverus! {\n    pub open spec fn s(n: u32) -> bool { n > 0 }\n\n    pub proof fn lemma(v: u32)\n        requires v <= MAX_ERRNO\n        ensures v > 0\n    { }\n}\n";
        let (a, b) = (
            clp("crates/x/src/lib.rs", plain),
            clp("crates/x/src/lib.rs", harnessed),
        );
        assert_eq!(
            a.author_code, b.author_code,
            "the prover who wraps existing code in the verifier writes no code (spec 1.1.3, 1.1.6)"
        );
        assert_ne!(a.author_proof, b.author_proof, "and it writes proof text");
        for tok in ["verus", "vstd", "prelude", "lemma"] {
            assert!(
                !b.author_code.iter().any(|t| t == tok),
                "`{tok}` is under a gate rustc never compiles: {:?}",
                b.author_code
            );
        }
    }

    /// H2 of the review: `#[cfg(verus_keep_ghost)]` in front of a shipped guard. rustc drops the guard, Verus keeps it, so the
    /// code stream must lose it; a gate that hides code from the build is a code change, whoever makes it.
    #[test]
    fn gating_a_shipped_guard_under_cfg_verus_keep_ghost_is_a_code_change() {
        let shipped = "pub fn f(name: &[u8]) -> bool {\n    if name[0] == 45 { return false; }\n    true\n}\n";
        let gated = "pub fn f(name: &[u8]) -> bool {\n    #[cfg(verus_keep_ghost)]\n    if name[0] == 45 { return false; }\n    true\n}\n";
        let (a, b) = (
            clp("crates/x/src/lib.rs", shipped),
            clp("crates/x/src/lib.rs", gated),
        );
        assert_ne!(
            a.author_code, b.author_code,
            "rustc stops compiling the guard, so the code stream moved"
        );
        assert!(
            b.author_proof.contains("45"),
            "the guard is now read only by the verifier: {}",
            b.author_proof
        );
        // and the same gate on an item, not a statement: the item leaves the build
        let g = "#[cfg(verus_keep_ghost)]\npub const MAX: u32 = 3;\n";
        assert!(
            !clp("crates/x/src/lib.rs", &format!("{g}\npub fn f() {{ }}\n"))
                .author_code
                .iter()
                .any(|t| t == "MAX"),
            "a gated const is not in the program rustc compiles"
        );
    }

    /// The window-manager case (`verus/juno-surface/rules`, prover re-check 5 section 4) on the AUTHORSHIP pair: a closed
    /// `proof! { }` block with the exact `#[cfg(verus_keep_ghost)]` gate directly in front of it is proof text, so adding,
    /// removing or moving a whole gated block leaves `exec_text` AND the authorship code stream equal and moves only proof
    /// text. The four `rows_ok` blocks of `aeaa4f6fa` moved exec, which read the prover's own commit as a code change and put
    /// claude-opus-5-5 on both sides of the unit. The narrowness is main's (`ghost_gate`, `bare_proof_name`,
    /// `own_proof_macro`, `delim_end`); what this test adds over main's is that the pair moves neither authorship stream and
    /// that the same gate over an ordinary exec guard is still code (the guard case below).
    #[test]
    fn a_ghost_block_pair_moves_proof_text_and_not_the_authorship_code() {
        const GATE12: &str = "#[cfg(verus_keep_ghost)]\n            ";
        const GATE4: &str = "#[cfg(verus_keep_ghost)]\n    ";
        let src = |ls: &[&str]| ls.join("\n");
        let plain = src(&[
            "pub fn rows_ok(rows: &[(u32, u32)], height: u32) -> Option<u64> {",
            "    let mut total: u64 = 0;",
            "    let mut i = 0;",
            "    while i < rows.len() {",
            "        let (y, h) = rows[i];",
            "        if h == 0 || y >= height {",
            "            return None;",
            "        }",
            "        if h as u64 > u64::MAX - total {",
            "            return None;",
            "        }",
            "        total = total + h as u64;",
            "        i += 1;",
            "    }",
            "    Some(total)",
            "}",
            "",
        ]);
        let block = |what: &str| {
            format!(
                "{}proof! {{\n                assert({what});\n            }}\n            ",
                GATE12
            )
        };
        let (b1, b2, b3) = (
            block("!row_inside(rows@[i as int], height)"),
            block("lemma_rows_prefix_le(rows@, i as int + 1)"),
            block("rows@.subrange(0, i as int) =~= rows@"),
        );
        // the three blocks put at the three sites of one shape, so a variant differs only in where they sit
        let at = |first: &str, second: &str, third: &str| -> String {
            plain
                .replace(
                    "if h == 0 || y >= height {\n            return",
                    &format!("if h == 0 || y >= height {{\n            {first}return"),
                )
                .replace(
                    "if h as u64 > u64::MAX - total {\n            return",
                    &format!("if h as u64 > u64::MAX - total {{\n            {second}return"),
                )
                .replace(
                    "        i += 1;\n    }",
                    &format!("        i += 1;\n        {third}    }}"),
                )
        };
        let (ghosted, moved) = (at(&b1, &b2, &b3), at(&b3, &b1, &b2));
        let (a, b, m) = (
            clp("crates/x/src/rules.rs", &plain),
            clp("crates/x/src/rules.rs", &ghosted),
            clp("crates/x/src/rules.rs", &moved),
        );
        assert!(
            a.errors.is_empty() && b.errors.is_empty() && m.errors.is_empty(),
            "all three read clean: {:?} | {:?} | {:?}",
            a.errors,
            b.errors,
            m.errors
        );
        assert_eq!(
            a.exec_text, b.exec_text,
            "a gated proof block is not in the program rustc compiles"
        );
        assert_eq!(
            a.exec_text, m.exec_text,
            "the same blocks in another order: the program is still the same"
        );
        assert_ne!(
            a.proof_text, b.proof_text,
            "and the block is proof text, gate included"
        );
        assert_ne!(
            b.proof_text, m.proof_text,
            "proof text is source order, so moving a block is a proof-side change"
        );
        for blk in [&b1, &b2, &b3] {
            let alone = clp("crates/x/src/rules.rs", blk).proof_text;
            assert!(
                m.proof_text.contains(&alone),
                "{alone} is still proof text where it moved to: {}",
                m.proof_text
            );
        }
        assert_eq!(
            a.author_code, m.author_code,
            "so the authorship code stream moves for neither the add nor the move"
        );
        // `proof_with!` is the same shape
        let bare = src(&["pub fn f(n: u32) -> u32 {", "    n", "}", ""]);
        let p = src(&[
            "pub fn f(n: u32) -> u32 {",
            GATE4,
            "    proof_with! {",
            "        assert(n == n);",
            "    }",
            "    n",
            "}",
            "",
        ]);
        assert_eq!(
            clp("crates/x/src/rules.rs", &bare).exec_text,
            clp("crates/x/src/rules.rs", &p).exec_text,
            "a gated proof_with! block is proof text too"
        );
        assert!(
            clp("crates/x/src/rules.rs", &p)
                .author_proof
                .contains("proof_with"),
            "and it stands in the proof stream: {}",
            clp("crates/x/src/rules.rs", &p).author_proof
        );
        // the gate over ordinary exec code is still code (H2 above, kept as the other half of the split)
        let guard = src(&[
            "pub fn f(name: &[u8]) -> bool {",
            GATE4,
            "    if name[0] == 45 { return false; }",
            "    true",
            "}",
            "",
        ]);
        assert_ne!(
            clp(
                "crates/x/src/rules.rs",
                &src(&["pub fn f(name: &[u8]) -> bool {", "    true", "}", ""])
            )
            .exec_text,
            clp("crates/x/src/rules.rs", &guard).exec_text,
            "a gated guard is an item of the build that Verus alone reads: the code moved"
        );
    }

    /// Case (a) of the spec, second half: the commit that follows the harness and adds only functions. It sits between the
    /// ghost-gated `verus!` block and the gates on the shipped items, so the proof stream must not move: where a proof run stops
    /// is layout. (The run list split there, and the Sonnet commit of errseq was credited with proof authorship.)
    #[test]
    fn adding_code_between_two_proof_regions_writes_no_proof_token() {
        let before = "#[cfg(verus_keep_ghost)]\nverus! {\n    pub open spec fn s(n: u32) -> bool { n > 0 }\n}\n\n\
             #[cfg_attr(verus_keep_ghost, verus_verify)]\npub fn f(n: u32) -> u32 { n + 1 }\n";
        let after = "#[cfg(verus_keep_ghost)]\nverus! {\n    pub open spec fn s(n: u32) -> bool { n > 0 }\n}\n\n\
             pub fn g(n: u32) -> u32 { n }\n\n\
             #[cfg_attr(verus_keep_ghost, verus_verify)]\npub fn f(n: u32) -> u32 { n + 1 }\n";
        let (a, b) = (
            clp("crates/x/src/lib.rs", before),
            clp("crates/x/src/lib.rs", after),
        );
        assert_eq!(
            a.author_proof, b.author_proof,
            "a commit that only adds functions wrote no proof text"
        );
        assert_ne!(
            a.author_code, b.author_code,
            "and it is seen as the code commit it is"
        );
    }

    #[test]
    fn a_gate_attribute_is_proof_text_and_the_item_under_it_stays_code() {
        let bare = "pub fn f(n: u32) -> u32 { n + 1 }\n";
        let gated = "#[cfg(not(verus_keep_ghost))]\npub fn f(n: u32) -> u32 { n + 1 }\n";
        let c = clp("crates/x/src/lib.rs", gated);
        assert_eq!(
            c.author_code,
            clp("crates/x/src/lib.rs", bare).author_code,
            "hiding the fn from the verifier adds no code token"
        );
        assert!(
            c.author_proof.contains("gate@cfg f"),
            "the gate is keyed by the item it hides: {}",
            c.author_proof
        );
        // a gate on another item is another key
        let other = "#[cfg(not(verus_keep_ghost))]\npub fn g(n: u32) -> u32 { n + 1 }\n";
        assert_ne!(
            clp("crates/x/src/lib.rs", gated).author_proof,
            clp("crates/x/src/lib.rs", other).author_proof
        );
        // test code is code: rustc compiles it
        let t = "#[cfg(test)]\nmod t {\n    #[test]\n    fn tt() { assert_eq!(1, 2); }\n}\n";
        assert!(
            clp("crates/x/src/lib.rs", t)
                .author_code
                .iter()
                .any(|x| x == "tt"),
            "a prover who adds a test authors code"
        );
    }

    #[test]
    fn the_omit_tags_are_keyed_by_the_attribute_and_the_item_they_hide() {
        let a = "#[derive(Copy, Clone)] //@omit\npub struct A { pub x: u32 }\n";
        let b = "#[derive(Copy, Clone)] //@omit\npub struct A {\n    pub x: u32,\n}\n";
        let c = "#[derive(Copy)] //@omit\npub struct B { pub x: u32 }\n";
        let ka = || cl(a).author_proof.clone();
        // a re-wrap of the tagged line and of the item: neutral for both streams
        assert_eq!(ka(), cl(b).author_proof);
        assert_eq!(cl(a).author_code, cl(b).author_code);
        // a tag moved to another item: proof text, though the count is equal
        assert_ne!(ka(), cl(c).author_proof);
        assert!(ka().contains("omit@derive A"), "{}", ka());
        assert!(cl(c).author_proof.contains("omit@derive B"));
        // and the tagged line itself is code
        assert_ne!(cl(a).author_code, cl(c).author_code);
        // a tag added to a line that held none changes the proof stream only: the program rustc reads is the same
        let plain = "#[derive(Copy, Clone)]\npub struct A { pub x: u32 }\n";
        let tagged = "#[derive(Copy, Clone)] //@omit\npub struct A { pub x: u32 }\n";
        assert_eq!(
            cl(plain).author_code,
            cl(tagged).author_code,
            "the tag itself is not code and the tagged line is not emitted twice"
        );
        assert_ne!(cl(plain).author_proof, cl(tagged).author_proof);
    }

    #[test]
    fn a_tag_that_attaches_to_no_item_is_keyed_by_its_line() {
        let c = cl("#![allow(dead_code)] //@omit\npub struct S { pub a: u32 }\n");
        assert!(c.author_proof.contains("omit@? 1"), "{}", c.author_proof);
        // an inner attribute is in neither stream: rustc drops it, Verus never reads it
        assert!(
            !c.author_code.iter().any(|x| x == "allow"),
            "{:?}",
            c.author_code
        );
    }

    #[test]
    fn a_tag_may_only_hide_an_attribute_line() {
        let ok =
            "#[derive(Debug)] //@omit\n#![allow(dead_code)] //@omit\npub struct S { pub a: u32 }\n";
        assert!(omit_bad_tags_of("c/rules.rs", ok).is_empty(), "{ok}");
        // a tag that hides executable code, in any of its shapes
        for bad in [
            "use alloc::vec::Vec; //@omit\n",
            "pub fn f(n: u32) -> u32 //@omit\n{\n    n\n}\n",
            "pub struct S { pub a: u32 }\npub fn f(s: S) -> u32\n{\n    let x = s.a; //@omit\n    x\n}\n",
            "pub const K: usize = 1; //@omit\n",
            "// a comment //@omit\n",
        ] {
            let v = omit_bad_tags_of("c/rules.rs", bad);
            assert_eq!(v.len(), 1, "{bad} => {v:?}");
            assert!(v[0].contains("omit may only hide attributes"), "{v:?}");
        }
        // the tag is what ends the line, as in the transform, and needs no space before it
        assert!(omit_bad_tags_of("c/rules.rs", "#[derive(Debug)]//@omit\n").is_empty());
        // a line that merely mentions the tag is not one, so nothing is hidden and nothing is reported
        assert!(
            omit_bad_tags_of("c/rules.rs", "// every //@omit line hides an attribute\n").is_empty()
        );
    }

    #[test]
    fn ret_tag_compares_as_the_plain_return_type() {
        let plain = "pub fn f(n: u32) -> u32\n{\n    n\n}\n";
        let tagged = "pub fn f(n: u32) -> u32 //@ret\n//@ ensures ret == n,\n{\n    n\n}\n";
        assert_eq!(shipped_core(plain).code, shipped_core(tagged).code);
        let other = tagged.replace("-> u32", "-> u64");
        assert_ne!(shipped_core(plain).code, shipped_core(&other).code);
    }

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

    // ---- the `//@omit` line: what it says is the builder's code, the tag is the prover's proof text

    /// The builder's code: nothing hidden from Verus, the fn bare of clauses.
    const OPAIN: &str = "#[derive(Clone, Copy, Debug, PartialEq, Eq)]\npub struct S { pub a: u32 }\npub fn f(s: S) -> u32\n{\n    s.a\n}\n";
    /// Fable's version of the same code: the clauses, and the derive tagged so Verus does not read it. What rustc reads is
    /// OPAIN's, token for token: the tag hides the line from the verifier, it does not take it out of the program.
    const OTAG_PROVED: &str = "#[derive(Clone, Copy, Debug, PartialEq, Eq)] //@omit\npub struct S { pub a: u32 }\npub fn f(s: S) -> u32 //@ret\n//@ ensures ret == s.a,\n{\n    s.a\n}\n";
    const DLINE: &str = "#[derive(Clone, Copy, Debug, PartialEq, Eq)]";

    fn put_rules(d: &Path, s: &str) {
        fs::write(d.join("crates/x/src/rules.rs"), s).unwrap();
    }

    /// base (`pub const K`), then Sonnet writes the code, then Fable puts the clauses on the fn and tags the derive. The tag
    /// comes in with the prover's commit: a builder may not add one (test `a_builder_that_moves_an_omit_tag_...`).
    fn omit_repo(tag: &str) -> (PathBuf, String) {
        let d = tmp_repo(tag);
        g(&d, &["init", "-q", "-b", "main"]);
        fs::create_dir_all(d.join("crates/x/src")).unwrap();
        put_rules(&d, "pub const K: usize = 1;\n");
        let base = commit(&d, "base", Some("Claude Sonnet 5.5"));
        put_rules(&d, OPAIN);
        commit(&d, "builder: the code", Some("Claude Sonnet 5.5"));
        put_rules(&d, OTAG_PROVED);
        commit(
            &d,
            "prover: the clauses and the tag",
            Some("Claude Fable 5.2"),
        );
        (d, base)
    }

    /// The check over a repository: the unit's fingerprints read from `base` and HEAD, the commits walked for real, the `//@omit`
    /// tag ratchet between the two trees, and one record of `kind` by `prover` over `implementer` carrying HEAD's fingerprints
    /// (`rec` `None`: the log stays as the base has it, so the unit needs a record it does not have).
    fn omit_check(
        d: &Path,
        base: &str,
        record: Option<(&str, &str, &str)>,
    ) -> (Outcome, ChangeInfo) {
        let (unit, files) = rules_unit();
        let units = [unit.clone()];
        let w = rev_fp(d, "HEAD", &unit.name, &units).expect("HEAD fingerprints");
        let b = rev_fp(d, base, &unit.name, &units);
        let info = change_info(d, base, &unit.name, &units, &files, None);
        let bad_now = omit_bad_tags_of(
            "crates/x/src/rules.rs",
            &fs::read_to_string(d.join("crates/x/src/rules.rs")).unwrap(),
        );
        let bad_base = omit_bad_tags_of(
            "crates/x/src/rules.rs",
            &g(d, &["show", &format!("{base}:crates/x/src/rules.rs")]),
        );
        let omit_bad_tags = if bad_now.len() > bad_base.len() {
            bad_now
        } else {
            vec![]
        };
        let (entries, work_log, rec_line, rec_prover) = match record {
            Some((kind, prover, implementer)) => {
                let mut r = rec(kind, &unit.name, &w);
                r.prover_model = prover.into();
                r.impl_model = implementer.into();
                let l = line(&r);
                (
                    vec![Entry {
                        line: 9,
                        raw: l.clone(),
                        id: r.id.clone(),
                        res: Ok(r),
                    }],
                    format!("{LOG_HEADER}{l}\n"),
                    Some(l),
                    Some(prover.to_string()),
                )
            }
            None => (vec![], LOG_HEADER.to_string(), None, None),
        };
        let added = |raw: &str| match (&rec_line, &rec_prover) {
            (Some(l), Some(p)) if raw == l.as_str() => Added::Committed(vec![p.clone()]),
            _ => Added::Unknown,
        };
        let mut basem = BTreeMap::new();
        if let Some(b) = b {
            basem.insert(unit.name.clone(), b);
        }
        let mut u = ufp(&w);
        u.unit = unit.name.clone();
        let work = vec![u];
        let o = check(&CheckIn {
            base: &basem,
            work: &work,
            entries: &entries,
            base_log: LOG_HEADER,
            work_log: &work_log,
            verdict_ok: &|_: &str, _: &Record| true,
            added: &added,
            authors: &walk_of(&unit.name, &info),
            verdict_modified: &[],
            merge_gate: false,
            cutoff_work: None,
            cutoff_changed: &[],
            exempt_work: None,
            cutoff_gaps: &[],
            cutoff_accepts: &[],
            exemption_revived: &[],
            allow_cutoff: false,
            exists: &|_: &str| true,
            ancestor: &|_: &str, _: &str| true,
            omit_hazards: &[],
            omit_bad_tags: &omit_bad_tags,
            registration_exempt: &[],
            extractor_changed: &[],
            unit_files_changed: &[],
            allow_extractor: false,
            attrib: None,
        });
        (o, info)
    }

    fn omit_rec(
        d: &Path,
        base: &str,
        kind: &str,
        prover: &str,
        implementer: &str,
    ) -> (Outcome, ChangeInfo) {
        omit_check(d, base, Some((kind, prover, implementer)))
    }

    #[test]
    fn a_builder_rewrapping_a_tagged_derive_is_code_the_provers_record_still_covers_it() {
        let (d, base) = omit_repo("omit-rewrap");
        // the same tokens, laid out as the shipped code likes: Verus never reads the line, so no fingerprint of the prover moves
        put_rules(
            &d,
            &OTAG_PROVED.replace(DLINE, "#[derive(Debug, PartialEq, Eq, Clone, Copy)]"),
        );
        commit(
            &d,
            "builder: rewrap the hidden derive",
            Some("Claude Sonnet 5.5"),
        );
        let (o, info) = omit_rec(&d, &base, "statement", FABLE, SON);
        assert_eq!(
            info.exec,
            vec![vec![SON.to_string()], vec![SON.to_string()]],
            "both builder commits wrote code: {:?}",
            info.exec
        );
        assert_eq!(info.proof.len(), 1, "{:?}", info.proof);
        assert_eq!(info.proof[0].1, vec![FABLE.to_string()]);
        assert!(o.refusals.is_empty(), "{:?}", o.refusals);
        assert_eq!(o.covered.len(), 1, "{:?}", o.covered);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn the_prover_that_edits_the_content_of_a_tagged_line_is_refused_as_code() {
        let (d, base) = omit_repo("omit-prover-touches-code");
        put_rules(
            &d,
            &OTAG_PROVED.replace(DLINE, "#[derive(Clone, Copy, Debug)]"),
        );
        commit(
            &d,
            "prover: drop two traits off the hidden derive",
            Some("Claude Fable 5.2"),
        );
        let (o, info) = omit_rec(&d, &base, "statement", FABLE, SON);
        assert!(
            info.exec.iter().any(|t| t == &vec![FABLE.to_string()]),
            "the hidden line is shipped code: {:?}",
            info.exec
        );
        assert!(
            rules(&o).contains(&"one-model-wrote-code-and-proof"),
            "{:?}",
            o.refusals
        );
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_builder_that_moves_an_omit_tag_writes_proof_text_and_is_refused() {
        for (tag, from, to, what) in [
            (
                "omit-untag",
                OTAG_PROVED,
                "#[derive(Clone, Copy, Debug, PartialEq, Eq)]\npub struct S { pub a: u32 }\npub fn f(s: S) -> u32 //@ret\n//@ ensures ret == s.a,\n{\n    s.a\n}\n",
                "removes",
            ),
            (
                "omit-untag-back",
                "#[derive(Clone, Copy, Debug, PartialEq, Eq)]\npub struct S { pub a: u32 }\npub fn f(s: S) -> u32 //@ret\n//@ ensures ret == s.a,\n{\n    s.a\n}\n",
                OTAG_PROVED,
                "adds",
            ),
        ] {
            let d = tmp_repo(tag);
            g(&d, &["init", "-q", "-b", "main"]);
            fs::create_dir_all(d.join("crates/x/src")).unwrap();
            put_rules(&d, from);
            let base = commit(&d, "base", Some("Claude Sonnet 5.5"));
            put_rules(&d, to);
            commit(
                &d,
                &format!("builder {what} the tag"),
                Some("Claude Sonnet 5.5"),
            );
            let (o, info) = omit_rec(&d, &base, "statement", FABLE, SON);
            assert!(
                info.exec.is_empty(),
                "the tag is not code ({what}): {:?}",
                info.exec
            );
            assert_eq!(info.proof.len(), 1, "{what}: {:?}", info.proof);
            assert_eq!(info.proof[0].1, vec![SON.to_string()], "{what}");
            assert!(
                rules(&o).contains(&"proof-or-statement-written-by-a-builder"),
                "{what}: {:?}",
                o.refusals
            );
            let _ = fs::remove_dir_all(&d);
        }
    }

    #[test]
    fn a_tag_on_a_line_that_is_not_an_attribute_is_refused() {
        let (d, base) = omit_repo("omit-bad-tag");
        // the same shape main carries once (services/lucina-events/src/rules.rs:14): a `use` hidden from the verifier
        put_rules(&d, &format!("use alloc::vec::Vec; //@omit\n{OTAG_PROVED}"));
        commit(
            &d,
            "builder: hide the use from Verus",
            Some("Claude Sonnet 5.5"),
        );
        let (o, _) = omit_rec(&d, &base, "statement", FABLE, SON);
        assert!(
            rules(&o).contains(&"omit-tag-hides-code-from-the-proof"),
            "{:?}",
            o.refusals
        );
        assert!(
            o.refusals
                .iter()
                .any(|r| r.detail.contains("omit may only hide attributes")),
            "{:?}",
            o.refusals
        );
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_prover_that_only_tags_a_line_needs_a_proof_edit_record_and_that_suffices() {
        // 52518f468 on its own: the derive line keeps every character, the tag comes to it. The real pair of the fixtures, walked
        // one commit at a time, with the unit already registered at the base.
        assert_eq!(omit_bad_tags_of("c/r.rs", SLIDE_AFTER).len(), 0);
        let untagged = SLIDE_AFTER.replace("PartialEq, Eq)] //@omit", "PartialEq, Eq)]");
        assert_ne!(untagged, SLIDE_AFTER);
        let d = tmp_repo("omit-tag-only");
        g(&d, &["init", "-q", "-b", "main"]);
        fs::create_dir_all(d.join("crates/x/src")).unwrap();
        put_rules(&d, &untagged);
        let base = commit(
            &d,
            "sonnet: the proved rule, derive visible to Verus",
            Some("Claude Sonnet 5.5"),
        );
        put_rules(&d, SLIDE_AFTER);
        commit(
            &d,
            "fable: hide the derive from Verus",
            Some("Claude Fable 5.2"),
        );
        let (o, info) = omit_rec(&d, &base, "proof-edit", FABLE, SON);
        assert!(
            info.exec.is_empty(),
            "no code line changed: {:?}",
            info.exec
        );
        assert_eq!(info.proof.len(), 1, "{:?}", info.proof);
        assert!(o.refusals.is_empty(), "{:?}", o.refusals);
        // without a record the tag is a proof edit that needs one, and one proof-edit record is enough
        let (o2, _) = omit_check(&d, &base, None);
        assert_eq!(
            rules(&o2),
            ["proof-edit-needs-strong-prover"],
            "{:?}",
            o2.refusals
        );
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_new_unit_created_by_the_implementer_is_walked_commit_by_commit() {
        // the shape the window-manager chat reported on juno-perf-land: 6383415da (Opus, plain code, a file new to the range) ->
        // dfff1d83f (Sonnet, a doc comment) -> 52518f468 (Fable: clauses, spec fns, a lemma, `//@ret` and `//@omit`). The unit has
        // no fingerprint at the range base, so every commit is compared with its own parent.
        let d = tmp_repo("slide-new");
        g(&d, &["init", "-q", "-b", "main"]);
        fs::create_dir_all(d.join("crates/x/src")).unwrap();
        put_rules(&d, "pub const MAX_W: u32 = 65_536;\n");
        let base = commit(&d, "base: no slide rule yet", Some("Claude Sonnet 5.5"));
        put_rules(&d, SLIDE_BEFORE);
        commit(
            &d,
            "opus: the slide rule, no proof",
            Some("Claude Opus 5.5"),
        );
        put_rules(
            &d,
            &SLIDE_BEFORE.replace("both take their spans", "both read their spans"),
        );
        commit(&d, "sonnet: rewords the header", Some("Claude Sonnet 5.5"));
        put_rules(&d, SLIDE_AFTER);
        commit(
            &d,
            "fable: the clauses, the spec fns, the lemma and the omit tag",
            Some("Claude Fable 5.2"),
        );
        let (unit, files) = rules_unit();
        let units = [unit.clone()];
        let info = change_info(&d, &base, &unit.name, &units, &files, None);
        assert!(
            !info.exec.iter().flatten().any(|m| m == FABLE),
            "the prover wrote no code: {:?}",
            info.exec
        );
        assert_eq!(
            info.exec,
            vec![vec!["claude-opus-5-5".to_string()]],
            "the creating commit is code by content (it holds no statement and no proof text), and the reworded comment is \
             neither: {:?}",
            info.exec
        );
        assert_eq!(info.proof.len(), 1, "{:?}", info.proof);
        assert_eq!(info.proof[0].1, vec![FABLE.to_string()]);
        let (o, _) = omit_rec(&d, &base, "statement", FABLE, "claude-opus-5-5");
        assert!(o.refusals.is_empty(), "{:?}", o.refusals);
        assert_eq!(o.covered.len(), 1, "{:?}", o.covered);
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
        let o = check(&CheckIn {
            base: &base,
            work: &work,
            entries: &entries,
            base_log: "",
            work_log: "",
            verdict_ok: &vok,
            added: &added,
            authors: &authorship::Walk::default(),
            verdict_modified: &[],
            merge_gate: true,
            cutoff_work: Some("aaa"),
            cutoff_changed: &["docs/ci/proof-roles-cutoff.txt".to_string()],
            exempt_work: None,
            cutoff_gaps: &[],
            cutoff_accepts: &[],
            exemption_revived: &[],
            allow_cutoff: false,
            exists: &|_: &str| true,
            ancestor: &|_: &str, _: &str| true,
            omit_hazards: &[],
            omit_bad_tags: &[],
            registration_exempt: &[],
            extractor_changed: &[],
            unit_files_changed: &[],
            allow_extractor: false,
            attrib: None,
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

    /// As production calls it: the unit alone as the peer.
    fn info_of_two(d: &Path, base: &str) -> ChangeInfo {
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
    }

    /// Spec 26: a conflict resolved to one parent is that parent's content, so the merge authored nothing of its own.
    #[test]
    fn a_conflict_resolved_to_one_side_of_a_merge_authors_nothing() {
        let (unit, files) = rules_unit();
        // a conflict resolved to the branch side: the merge chose a parent, it authored nothing
        let (d, base) = merge_repo("theirs");
        g(&d, &["checkout", "-q", "-b", "feat"]);
        fs::write(
            d.join("crates/x/src/rules.rs"),
            RULES_RS.replace("invariant i <= n", "invariant i <= n + 5"),
        )
        .unwrap();
        commit(&d, "feat proof", Some("Claude Opus 5.5"));
        let feat = g(&d, &["rev-parse", "HEAD"])[..10].to_string();
        g(&d, &["checkout", "-q", "main"]);
        fs::write(
            d.join("crates/x/src/rules.rs"),
            RULES_RS.replace("invariant i <= n", "invariant i <= n + 6"),
        )
        .unwrap();
        commit(&d, "main proof", Some("Claude Opus 5.5"));
        let main_proof = g(&d, &["rev-parse", "HEAD"])[..10].to_string();
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
        assert!(
            i.proof.iter().any(|(c, _)| *c == feat)
                && i.proof.iter().any(|(c, _)| *c == main_proof),
            "both sides wrote proof text of their own: {:?}",
            i.proof
        );
        // spec 26: the resolution is one parent's content as it is, so the merge authors nothing here
        assert!(
            i.proof.iter().all(|(c, _)| *c != merge),
            "a conflict resolved to one side is neutral: {:?}",
            i.proof
        );
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
    fn a_new_mutants_unit_over_an_existing_crate_is_new_by_its_own_files() {
        let mu = Unit {
            name: "mutants/x-new".into(),
            group: "g".into(),
            kind: "mutants".into(),
            target: "verification/x-new/mutants.sh".into(),
            inputs: vec![
                "apps/lucina-glass/src/slide_rules.rs".into(),
                "verification/x-new/".into(),
            ],
        };
        // the crate existed at the base; the script and its directory did not
        let base_files = ["apps/lucina-glass/src/slide_rules.rs"];
        let file_new = |f: &str| {
            !base_files
                .iter()
                .any(|b| b.starts_with(f) || f.starts_with(b))
        };
        let files = newness_files(None, Some(&mu)).unwrap();
        assert_eq!(
            files,
            ["verification/x-new/mutants.sh", "verification/x-new/"]
        );
        assert!(files.iter().all(|f| file_new(f)), "unit is new");
        // a verified unit keeps its closure (an old file in it makes it not new)
        let vu = Unit {
            kind: "verus".into(),
            ..mu.clone()
        };
        let all = newness_files(None, Some(&vu)).unwrap();
        assert!(!all.iter().all(|f| file_new(f)));
        // a mutants script that existed at the base is not new
        let old = |f: &str| f != "verification/x-new/mutants.sh";
        assert!(!files.iter().all(|f| old(f)));
        // registration: only added lines => exempt; an altered existing line => not
        let p = "tools/ci/proof-inventory.tsv";
        let base = "# Generated\nverus/a/rules\tg\tverus\tverification/a/rules.rs\tx\n";
        let work = format!("{base}mutants/x-new\tg\tmutants\tverification/x-new/mutants.sh\tx\n");
        let unit_new = |u: &str| u == "mutants/x-new" && files.iter().all(|f| file_new(f));
        assert!(registration_only(p, base, &work, &unit_new, &yes, &yes).is_ok());
        let altered = work.replace("verification/a/rules.rs", "verification/a/other.rs");
        assert!(registration_only(p, base, &altered, &unit_new, &yes, &yes).is_err());
        // the unit is not new => refused even with only added lines
        assert!(registration_only(p, base, &work, &no, &yes, &yes).is_err());
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

    #[test]
    fn a_grok_model_name_is_the_grok_family_a_word_holding_it_is_not() {
        for (m, f) in [
            ("grok-4.7-high", "grok"),
            ("grok-4", "grok"),
            ("xai/grok-3", "grok"),
            ("cursor-grok", "grok"),
            ("Grok 4.7 High", "grok"),
            // the rule is the WORD `grok` of the normalised name, not the letters in it
            ("groking", "other"),
            ("grokish-lane", "other"),
            // every other name keeps the family it had
            ("claude-sonnet-5-5", "claude"),
            ("Claude Opus 5.5 (1M context)", "claude"),
            ("qwen3.8-27b", "qwen"),
            ("zen-deepseek-v4.1-flash-free", "deepseek"),
            ("glm-5.3-flash", "glm"),
            ("mistral-large", "mistral"),
            ("qoder-efficient", "qoder"),
            ("gpt-6.1-sol", "gpt"),
            ("something-new", "other"),
        ] {
            assert_eq!(family(m), f, "family of {m}");
        }
        assert!(amend::FAMILIES.contains(&"grok"), "grok is a known family");
    }

    #[test]
    fn a_grok_reviewer_meets_the_same_pairing_rule_as_the_other_lane_families() {
        let mut w = a();
        w.stmt = "1111111111111111".into();
        let vok = |_: &str, _: &Record| true;
        // reviewed by the grok lane, prover and implementer both Claude
        let mut r = rec("statement", "verus/u", &w);
        r.reviewer_model = "grok-4.7-high".into();
        r.reviewer_family = "grok".into();
        assert!(
            parse_fields(&line(&r).split('\t').collect::<Vec<_>>()).is_ok(),
            "the family derived from `grok-4.7-high` is grok"
        );
        assert!(validate(&r, Need::Statement, &vok).is_ok());
        // as author: grok code, a strong prover of another name, a reviewer of a third family
        let mut au = r.clone();
        au.impl_model = "grok-4.7-high".into();
        au.impl_family = "grok".into();
        au.reviewer_model = "zen-deepseek-flash".into();
        au.reviewer_family = "deepseek".into();
        assert!(parse_fields(&line(&au).split('\t').collect::<Vec<_>>()).is_ok());
        assert!(
            validate(&au, Need::Statement, &vok).is_ok(),
            "{:?}",
            validate(&au, Need::Statement, &vok)
        );
        // nothing is relaxed for grok: the reviewer family is still never the prover's ...
        let mut same = r.clone();
        same.reviewer_model = "claude-haiku-4-5".into();
        same.reviewer_family = "claude".into();
        let e = validate(&same, Need::Statement, &vok).unwrap_err();
        assert!(e.contains("is the prover's family"), "{e}");
        // ... a stated family that disagrees with the model name is malformed ...
        let mut claim = r.clone();
        claim.reviewer_model = "something-new".into();
        claim.reviewer_family = "grok".into();
        assert!(parse_fields(&line(&claim).split('\t').collect::<Vec<_>>()).is_err());
        // ... and `other` is still nobody's reviewer family.
        let mut o = r.clone();
        o.reviewer_model = "something-new".into();
        o.reviewer_family = "other".into();
        let e = validate(&o, Need::Statement, &vok).unwrap_err();
        assert!(e.contains("not a known family"), "{e}");
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
    // ---- attribution corrections (docs/ci/PROOF-ROLES.md)

    const AT_X: &str = "1111111111111111111111111111111111111111";
    const AT_Y: &str = "2222222222222222222222222222222222222222";

    fn at_line(c: &str, m: &str) -> String {
        format!("{c}\t{m}\tlead chat said it stamped its own line on a worker commit\n")
    }

    struct At {
        work: Option<String>,
        base: Option<String>,
        changed: Vec<String>,
        allow: bool,
        allow_strong: bool,
        ancestor: bool,
        merge: bool,
        raw: Vec<String>,
    }

    fn at() -> At {
        At {
            work: None,
            base: None,
            changed: vec![],
            allow: false,
            allow_strong: false,
            ancestor: true,
            merge: false,
            raw: vec!["claude-sonnet-5-5".into()],
        }
    }

    fn run_at(t: &At) -> Outcome {
        let w = a();
        let entries = parse_log("");
        let base = BTreeMap::new();
        let work = vec![ufp(&w)];
        let vok = |_: &str, _: &Record| true;
        let added = |_: &str| Added::Unknown;
        let anc = |_: &str| t.ancestor;
        let mg = |_: &str| t.merge;
        let raw = |_: &str| t.raw.clone();
        let ctx = AttribCtx {
            work: t.work.as_deref(),
            base: t.base.as_deref(),
            changed: &t.changed,
            allow: t.allow,
            allow_strong: t.allow_strong,
            ancestor_of_base: &anc,
            is_merge: &mg,
            raw_models: &raw,
        };
        check(&CheckIn {
            base: &base,
            work: &work,
            entries: &entries,
            base_log: "",
            work_log: "",
            verdict_ok: &vok,
            added: &added,
            authors: &authorship::Walk::default(),
            verdict_modified: &[],
            merge_gate: true,
            cutoff_work: None,
            cutoff_changed: &[],
            exempt_work: None,
            cutoff_gaps: &[],
            cutoff_accepts: &[],
            exemption_revived: &[],
            allow_cutoff: false,
            exists: &|_: &str| true,
            ancestor: &|_: &str, _: &str| true,
            omit_hazards: &[],
            omit_bad_tags: &[],
            registration_exempt: &[],
            extractor_changed: &[],
            unit_files_changed: &[],
            allow_extractor: false,
            attrib: Some(&ctx),
        })
    }

    fn attr_rules(o: &Outcome) -> Vec<&str> {
        o.refusals
            .iter()
            .map(|r| r.rule.as_str())
            .filter(|r| r.starts_with("attribution"))
            .collect()
    }

    fn used(o: &Outcome) -> bool {
        o.notes
            .iter()
            .any(|n| n.starts_with("attribution corrections used: "))
    }

    #[test]
    fn an_absent_attribution_file_changes_nothing_and_prints_zero() {
        let o = run_at(&at());
        assert!(attr_rules(&o).is_empty(), "{:?}", o.refusals);
        assert!(
            o.notes
                .contains(&"attribution corrections used: 0".to_string())
        );
        assert!(attribution_map(None).is_empty());
    }

    #[test]
    fn a_valid_attribution_file_is_accepted_and_counted() {
        let mut t = at();
        t.work = Some(format!(
            "# comment\n\n{}{}",
            at_line(AT_X, "Claude Sonnet 5.5"),
            at_line(AT_Y, "claude-opus-5-5")
        ));
        t.raw = vec!["claude-opus-5-5".into()];
        let o = run_at(&t);
        assert!(attr_rules(&o).is_empty(), "{:?}", o.refusals);
        assert!(
            o.notes
                .contains(&"attribution corrections used: 2".to_string())
        );
        let m = attribution_map(t.work.as_deref());
        assert_eq!(m[AT_X], "claude-sonnet-5-5");
        assert_eq!(m[AT_Y], "claude-opus-5-5");
    }

    #[test]
    fn malformed_attribution_files_are_refused_whole() {
        let reason = "r";
        let bad = [
            format!("{AT_X}\tclaude-sonnet-5-5\t{reason}\n{AT_X}\tclaude-sonnet-5-5\tagain\n"), // duplicate
            format!("{}\tclaude-sonnet-5-5\t{reason}\n", &AT_X[..7]), // short id
            format!("{}\tclaude-sonnet-5-5\t{reason}\n", "A".repeat(40)), // upper case
            format!(
                "{}\tclaude-sonnet-5-5\t{reason}\n",
                &"ABCDEF".repeat(7)[..40]
            ),
            format!("{AT_X}\tclaude-sonnet-5-5\t\n"), // empty reason
            format!("{AT_X}\tclaude-sonnet-5-5\t   \n"),
            format!("{AT_X}\tgpt-6-1-sol\t{reason}\n"), // unknown class
            format!("{AT_X}\t\t{reason}\n"),            // empty model
            format!("{AT_X}\tclaude-sonnet-5-5\n"),     // two fields
            format!("{AT_X} claude-sonnet-5-5 {reason}\n"), // no tabs
        ];
        for b in &bad {
            assert!(parse_attributions(b).is_err(), "{b:?}");
            let mut t = at();
            t.work = Some(b.clone());
            let o = run_at(&t);
            assert!(
                attr_rules(&o).contains(&"attribution-malformed"),
                "{b:?}: {:?}",
                o.refusals
            );
            assert!(
                attribution_map(Some(b)).is_empty(),
                "an invalid file corrects nothing"
            );
        }
    }

    #[test]
    fn a_correction_must_name_a_commit_already_on_the_base_and_may_name_a_merge() {
        let mut t = at();
        t.work = Some(at_line(AT_X, "claude-sonnet-5-5"));
        t.ancestor = false; // in the range under review, not on main, or not an object at all
        let o = run_at(&t);
        assert!(
            attr_rules(&o).contains(&"attribution-not-on-base"),
            "{:?}",
            o.refusals
        );
        // spec 25: the walk reads what a merge authored itself, so a merge may be corrected; it is noted, never refused
        t.ancestor = true;
        t.merge = true;
        let o = run_at(&t);
        assert!(attr_rules(&o).is_empty(), "{:?}", o.refusals);
        assert!(
            o.notes
                .iter()
                .any(|n| n.contains("a merge commit is corrected")),
            "{:?}",
            o.notes
        );
        assert!(used(&o), "{:?}", o.notes);
        t.merge = false;
        assert!(attr_rules(&run_at(&t)).is_empty());
    }

    #[test]
    fn the_attribution_file_is_append_only_against_the_base() {
        let base = at_line(AT_X, "claude-sonnet-5-5");
        let mut t = at();
        t.base = Some(base.clone());
        t.changed = vec![ATTRIB.into()];
        t.allow = true;
        // appending is fine
        t.work = Some(format!("{base}{}", at_line(AT_Y, "claude-sonnet-5-5")));
        assert!(
            attr_rules(&run_at(&t)).is_empty(),
            "{:?}",
            run_at(&t).refusals
        );
        // an existing line changed
        t.work = Some(at_line(AT_X, "claude-haiku-5"));
        assert!(attr_rules(&run_at(&t)).contains(&"attribution-not-append-only"));
        // a line inserted before an existing one
        t.work = Some(format!("{}{base}", at_line(AT_Y, "claude-sonnet-5-5")));
        assert!(attr_rules(&run_at(&t)).contains(&"attribution-not-append-only"));
        // the line removed, or the file deleted
        t.work = Some(String::new());
        assert!(attr_rules(&run_at(&t)).contains(&"attribution-not-append-only"));
        t.work = None;
        assert!(attr_rules(&run_at(&t)).contains(&"attribution-not-append-only"));
    }

    #[test]
    fn the_attribution_file_is_protected_unless_the_allow_token_is_given() {
        let mut t = at();
        t.work = Some(at_line(AT_X, "claude-sonnet-5-5"));
        t.changed = vec![ATTRIB.into()]; // created in the range
        let o = run_at(&t);
        assert!(
            attr_rules(&o).contains(&"attribution-protected"),
            "{:?}",
            o.refusals
        );
        t.allow = true;
        assert!(attr_rules(&run_at(&t)).is_empty());
        // the token itself: `attribution:<reason>`, not the bare word and not the strong token
        assert!(allow_token("attribution: lead chat stamp", "attribution"));
        assert!(allow_token("cutoff:x; attribution:y", "attribution"));
        assert!(!allow_token("attribution", "attribution"));
        assert!(!allow_token("attribution-strong:y", "attribution"));
        assert!(!allow_token("attribution:y", "attribution-strong"));
        assert!(is_attrib_path(ATTRIB));
        assert!(is_attrib_path("docs/ci/proof-roles-attributions-2.txt"));
        assert!(!is_attrib_path("docs/ci/proof-roles-cutoff.txt"));
        // a deleted file in the range is protected too
        t.work = None;
        t.base = None;
        assert!(attr_rules(&run_at(&t)).is_empty());
        t.allow = false;
        assert!(attr_rules(&run_at(&t)).contains(&"attribution-protected"));
    }

    #[test]
    fn re_attributing_a_builder_commit_to_a_strong_model_needs_the_strong_token() {
        let mut t = at(); // trailer: sonnet
        t.work = Some(at_line(AT_X, "Claude Opus 5.5"));
        t.changed = vec![ATTRIB.into()];
        t.allow = true;
        let o = run_at(&t);
        assert!(
            attr_rules(&o).contains(&"attribution-strong"),
            "{:?}",
            o.refusals
        );
        t.allow_strong = true;
        let o = run_at(&t);
        assert!(attr_rules(&o).is_empty(), "{:?}", o.refusals);
        assert!(
            o.notes.iter().any(|n| n.starts_with("ATTRIBUTION-STRONG:")),
            "{:?}",
            o.notes
        );
        // an Opus-stamped commit corrected to Sonnet, or an Opus commit kept Opus, needs no strong token
        t.allow_strong = false;
        t.raw = vec!["claude-opus-5-5".into()];
        t.work = Some(at_line(AT_X, "claude-sonnet-5-5"));
        assert!(attr_rules(&run_at(&t)).is_empty());
        t.work = Some(at_line(AT_X, "claude-opus-5-5"));
        assert!(attr_rules(&run_at(&t)).is_empty());
        // a line that was already in the base is not "added" again
        t.raw = vec!["claude-sonnet-5-5".into()];
        t.base = t.work.clone();
        t.changed = vec![];
        assert!(
            attr_rules(&run_at(&t)).is_empty(),
            "{:?}",
            run_at(&t).refusals
        );
        assert!(used(&run_at(&t)));
    }

    #[test]
    fn the_walk_reads_the_corrected_model_instead_of_the_trailer() {
        let d = tmp_repo("attrib");
        g(&d, &["init", "-q", "-b", "main"]);
        fs::create_dir_all(d.join("crates/x/src")).unwrap();
        fs::create_dir_all(d.join("docs/ci")).unwrap();
        fs::write(d.join("crates/x/src/rules.rs"), RULES_RS).unwrap();
        fs::write(d.join(LOG), LOG_HEADER).unwrap();
        let lb = commit(&d, "lb", Some("Claude Sonnet 5.5"));
        let unit = Unit {
            name: "shipped/x/rules".into(),
            group: "g".into(),
            kind: "shipped".into(),
            target: "crates/x/src/rules.rs".into(),
            inputs: vec![],
        };
        let files = vec!["crates/x/src/rules.rs".to_string()];
        // a worker's exec change, stamped Opus by a lead chat; it is history, below the branch's base
        fs::write(
            d.join("crates/x/src/rules.rs"),
            RULES_RS.replace("i += 1;", "i += 1; let _z = 0;"),
        )
        .unwrap();
        let stamped = commit(&d, "exec", Some("Claude Opus 5.5"));
        fs::write(d.join("unrelated.txt"), "b\n").unwrap();
        let base = commit(&d, "base", Some("Claude Sonnet 5.5"));
        // the prover's commit is in the range under review
        fs::write(
            d.join("crates/x/src/rules.rs"),
            RULES_RS
                .replace("i += 1;", "i += 1; let _z = 0;")
                .replace("invariant i <= n", "invariant i <= n + 0"),
        )
        .unwrap();
        let proof = commit(&d, "proof", Some("Claude Fable 5.2"));
        let wlk = |attrib: &str| {
            let r = authorship::ModelResolver {
                attrib: attribution_map(Some(attrib)),
                ..Default::default()
            };
            authorship::walk(
                &d,
                &base,
                &ufps(std::slice::from_ref(&unit), &unit.name, &files),
                &Cutoff {
                    commit: lb.clone(),
                    units: vec![],
                    exempt: Vec::new(),
                },
                &BTreeMap::new(),
                &r,
                None,
            )
            .unwrap()
        };
        // the trailer stands until the file corrects it
        assert_eq!(models_of(&wlk(""), &unit.name, true), ["claude-opus-5-5"]);
        assert_eq!(
            models_of(
                &wlk(&at_line(&stamped, "Claude Sonnet 5.5")),
                &unit.name,
                true
            ),
            ["claude-sonnet-5-5"],
            "the correction replaces the trailer"
        );
        // another commit is untouched
        assert_eq!(
            models_of(&wlk(&at_line(AT_Y, "claude-sonnet-5-5")), &unit.name, true),
            ["claude-opus-5-5"]
        );
        // a branch cannot re-attribute its own range: the proof commit is not an ancestor of the base
        assert_eq!(
            models_of(
                &wlk(&at_line(&proof, "Claude Sonnet 5.5")),
                &unit.name,
                false
            ),
            ["claude-fable-5-2"],
            "the correction of a commit in the range is ignored"
        );
        // the git helpers the check uses
        assert!(is_ancestor_of(&d, &stamped, &base));
        assert!(
            !is_ancestor_of(&d, &base, &stamped),
            "a later commit is not on the base"
        );
        assert!(!is_ancestor_of(&d, AT_Y, &stamped), "an unknown commit");
        assert!(
            !is_ancestor_of(&d, &stamped[..7], &base),
            "no abbreviations"
        );
        assert!(!is_merge_commit(&d, &stamped));
        g(&d, &["checkout", "-q", "-b", "side", &base]);
        fs::write(d.join("side.txt"), "s").unwrap();
        commit(&d, "side", Some("Claude Sonnet 5.5"));
        g(&d, &["checkout", "-q", "main"]);
        g(
            &d,
            &[
                "-c",
                "core.editor=true",
                "merge",
                "-q",
                "--no-ff",
                "-m",
                "m",
                "side",
            ],
        );
        assert!(is_merge_commit(&d, &g(&d, &["rev-parse", "HEAD"])));
        let _ = fs::remove_dir_all(&d);
    }

    // ---- history-aware cross-model rule

    fn hist_scn() -> Scn {
        let mut w = a();
        w.proof = "2222222222222222".into();
        let mut r = rec("proof-edit", "verus/u", &w);
        r.reviewer_model = "-".into();
        r.reviewer_family = "-".into();
        scn(Some(a()), w, vec![r])
    }

    fn ci(exec: &[&[&str]], proof: &[&[&str]]) -> ChangeInfo {
        ChangeInfo {
            exec: exec
                .iter()
                .map(|t| t.iter().map(|m| m.to_string()).collect())
                .collect(),
            proof: proof
                .iter()
                .map(|t| {
                    (
                        "hhhhhhhhhh".to_string(),
                        t.iter().map(|m| m.to_string()).collect(),
                    )
                })
                .collect(),
        }
    }

    #[test]
    fn code_by_a_model_in_an_earlier_merge_and_its_proof_in_this_one_is_refused() {
        // Opus wrote the code in M1 (before the base), the proof now: the branch alone sees only the proof commit
        let mut s = hist_scn();
        s.history = Some(ci(&[&[OPUS]], &[&[OPUS]]));
        let o = run(&s);
        assert!(
            rules(&o).contains(&"one-model-wrote-code-and-proof"),
            "{:?}",
            o.refusals
        );
        // Sonnet code in M1, Opus proof in M2: passes
        let mut s = hist_scn();
        s.history = Some(ci(&[&[SON]], &[&[OPUS]]));
        let o = run(&s);
        assert!(o.refusals.is_empty(), "{:?}", o.refusals);
        // no commits since the lower bound
        let mut s = hist_scn();
        s.history = Some(ChangeInfo::default());
        assert!(run(&s).refusals.is_empty());
    }

    #[test]
    fn the_history_rule_counts_a_pre_rule_implementer_and_skips_when_the_bound_is_missing() {
        // pre-rule record: implementer Opus (exempt code); a later proof commit by Opus in the history is refused
        let mut w = a();
        w.stmt = "1111111111111111".into();
        let mut sc = scn(Some(a()), w.clone(), vec![pre(&w)]);
        sc.cutoff = Some(CUT.into());
        sc.history = Some(ci(&[], &[&[OPUS]]));
        let o = run(&sc);
        assert!(
            rules(&o).contains(&"one-model-wrote-code-and-proof"),
            "{:?}",
            o.refusals
        );
        assert_eq!(history_lower_bound(None), HISTORY_LOWER_BOUND);
        assert_eq!(
            history_lower_bound(Some(CUT)),
            "0123456789abcdef0123456789abcdef01234567"
        );
        assert_eq!(history_lower_bound(Some("garbage")), HISTORY_LOWER_BOUND);
    }

    /// roles 1.4: a unit file that moves in the range keeps the authors it got under either name, and the move alone
    /// is a registration change, not authorship (docs/ci/PROOF-ROLES.md, Authorship by content).
    #[test]
    fn a_unit_file_moved_in_the_range_keeps_its_authors_and_a_pure_move_is_neutral() {
        let d = tmp_repo("moved");
        g(&d, &["init", "-q", "-b", "main"]);
        fs::create_dir_all(d.join("crates/x/src")).unwrap();
        fs::write(d.join("README"), "x\n").unwrap();
        let lb = commit(&d, "lb", Some("Claude Opus 5.5"));
        let unit = Unit {
            name: "shipped/x/rules".into(),
            group: "g".into(),
            kind: "shipped".into(),
            target: "crates/x/src/state.rs".into(),
            inputs: vec![],
        };
        // Sonnet writes the file under the old name; the unit is registered later under the new one
        let old = "crates/x/src/rules.rs".to_string();
        fs::write(d.join("crates/x/src/rules.rs"), RULES_RS).unwrap();
        let own = commit(&d, "own", Some("Claude Sonnet 5.5"));
        g(
            &d,
            &["mv", "crates/x/src/rules.rs", "crates/x/src/state.rs"],
        );
        let mv = commit(&d, "moved the file, nothing else", None);
        fs::write(
            d.join("crates/x/src/state.rs"),
            RULES_RS.replace("invariant i <= n", "invariant i <= n + 0"),
        )
        .unwrap();
        let edited = commit(&d, "proof", Some("Claude Opus 5.5"));
        let head = g(&d, &["rev-parse", "HEAD"]);
        // the unit's own files, as `enrolled.toml` names them at HEAD
        let files = vec!["crates/x/src/state.rs".to_string()];
        let r = authorship::ModelResolver::default();
        let walk_from = |from: &str| {
            authorship::walk(
                &d,
                from,
                &ufps(std::slice::from_ref(&unit), &unit.name, &files),
                &Cutoff {
                    commit: lb.clone(),
                    units: vec![],
                    exempt: Vec::new(),
                },
                &BTreeMap::new(),
                &r,
                None,
            )
            .unwrap()
        };
        // the merge under review starts with the proof edit: the move and the creation are in the earlier merges it lands on
        let w = walk_from(&mv);
        assert_eq!(
            models_of(&w, &unit.name, true),
            ["claude-sonnet-5-5".to_string()],
            "the walk starts from the file at HEAD and follows the move back to the name Sonnet wrote under"
        );
        assert_eq!(
            models_of(&w, &unit.name, false),
            [
                "claude-opus-5-5".to_string(),
                "claude-sonnet-5-5".to_string()
            ],
            "the creation moved the proof stream too, the edit moved it again"
        );
        let moved_in = w
            .authors(&unit.name)
            .commits
            .iter()
            .any(|a| a.commit == mv && (a.code || a.proof));
        assert!(
            !moved_in,
            "a pure move is a registration change: it authors neither stream"
        );
        assert!(
            w.authors(&unit.name)
                .commits
                .iter()
                .any(|a| a.commit == own && a.paths.iter().any(|p| p == &old)),
            "the creation is found under the name it wrote, through the move"
        );
        let i = authorship::range_change(w.authors(&unit.name));
        assert_eq!(
            i.exec,
            Vec::<Vec<String>>::new(),
            "nothing in the range itself wrote code"
        );
        assert_eq!(
            i.proof,
            vec![(
                authorship::short(&edited),
                vec!["claude-opus-5-5".to_string()]
            )],
            "the range moved the proof stream only, and by a model that wrote none of the code in it"
        );
        // and a model that writes the code of the moved file in the range meets the proof it landed with
        fs::write(
            d.join("crates/x/src/state.rs"),
            RULES_RS
                .replace("invariant i <= n", "invariant i <= n + 0")
                .replace("i += 1;", "i += 1; let _z = 0;"),
        )
        .unwrap();
        commit(&d, "code by the prover", Some("Claude Opus 5.5"));
        let w2 = walk_from(&head);
        assert_eq!(
            models_of(&w2, &unit.name, true),
            [
                "claude-opus-5-5".to_string(),
                "claude-sonnet-5-5".to_string()
            ],
            "Opus authored code through the move, Sonnet stayed from the creation"
        );
        let _ = fs::remove_dir_all(&d);
    }

    /// Review K1: `git rm` of the unit file and `git mv` of a staged file over it in ONE commit. Git reports a
    /// modification of the unit file and the deletion of the staged one, with no rename, so the blob the two share is the
    /// only way back to what Opus staged: without it the move launders the staged code and the walk credits the mover.
    #[test]
    fn a_file_moved_over_the_unit_file_in_one_commit_keeps_the_author_of_the_staged_code() {
        let d = tmp_repo("k1");
        g(&d, &["init", "-q", "-b", "main"]);
        fs::create_dir_all(d.join("crates/x/src")).unwrap();
        fs::write(d.join("README"), "x\n").unwrap();
        let lb = commit(&d, "lb", Some("Claude Opus 5.5"));
        let unit = Unit {
            name: "shipped/x/rules".into(),
            group: "g".into(),
            kind: "shipped".into(),
            target: "crates/x/src/rules.rs".into(),
            inputs: vec![],
        };
        let files = vec!["crates/x/src/rules.rs".to_string()];
        // the unit file as shipped: plain code, no statement and no proof text in it
        fs::write(
            d.join("crates/x/src/rules.rs"),
            "pub struct A { pub a: u32 }\npub fn g(n: usize) -> usize {\n    n\n}\n",
        )
        .unwrap();
        commit(&d, "the shipped unit", Some("Claude Sonnet 5.5"));
        // Opus stages its own body of the file under another name, outside the unit
        let staged = "pub fn f(n: usize) -> usize {\n    let mut i = 0;\n    while i < n { i += 1; }\n    i + n\n}\n";
        fs::write(d.join("crates/x/src/staged.rs"), staged).unwrap();
        let stage = commit(&d, "stage the new body", Some("Claude Opus 5.5"));
        // Sonnet replaces the unit file with it in one commit: `rm` plus `mv`, which git reports as a modification and a deletion
        g(&d, &["rm", "-q", "crates/x/src/rules.rs"]);
        g(
            &d,
            &["mv", "crates/x/src/staged.rs", "crates/x/src/rules.rs"],
        );
        let mv = commit(&d, "overwrite move", Some("Claude Sonnet 5.5"));
        // Opus then proves the body it staged
        fs::write(
            d.join("crates/x/src/rules.rs"),
            staged.replace(
                "pub fn f(n: usize) -> usize {",
                "pub fn f(n: usize) -> usize\n//@ ensures n == n,\n{",
            ),
        )
        .unwrap();
        let proves = commit(&d, "the ensures", Some("Claude Opus 5.5"));
        let r = authorship::ModelResolver::default();
        let w = authorship::walk(
            &d,
            &mv,
            &ufps(std::slice::from_ref(&unit), &unit.name, &files),
            &Cutoff {
                commit: lb.clone(),
                units: vec![],
                exempt: Vec::new(),
            },
            &BTreeMap::new(),
            &r,
            None,
        )
        .unwrap();
        let ua = w.authors(&unit.name);
        let seen: Vec<(String, bool, bool, Vec<String>)> = ua
            .commits
            .iter()
            .map(|a| {
                (
                    authorship::short(&a.commit),
                    a.code,
                    a.proof,
                    a.paths.clone(),
                )
            })
            .collect();
        assert!(
            ua.commits.iter().any(|a| {
                a.commit == stage && a.code && a.paths.iter().any(|p| p == "crates/x/src/staged.rs")
            }),
            "the staged code is credited to the commit that wrote it, through the overwrite move: {seen:?}"
        );
        assert!(
            !ua.commits
                .iter()
                .any(|a| a.commit == mv && (a.code || a.proof)),
            "the move authors nothing itself: {seen:?}"
        );
        let p = ua
            .commits
            .iter()
            .find(|x| x.commit == proves)
            .expect("the proving commit is credited");
        assert!(p.proof && !p.code, "the clause is proof alone: {p:?}");
        assert_eq!(
            clash_of(ua),
            vec!["claude-opus-5-5".to_string()],
            "Opus wrote the code it now proves: {seen:?}"
        );
        let _ = fs::remove_dir_all(&d);
    }

    /// Review K1c: the same laundering without a rename or a deletion. `cp staged.rs rules.rs` leaves the source alone, so git
    /// reports ONE modification of the unit file, `-C` sees no copy (it only looks at added paths) and the mover would be credited
    /// with the body Opus staged. The blob the two files share still ties them: the copy authors nothing, the source joins the
    /// lineage, and Opus stands on the code side of its own proof.
    #[test]
    fn a_file_copied_over_the_unit_file_keeps_the_author_of_the_staged_code() {
        let d = tmp_repo("k1c");
        g(&d, &["init", "-q", "-b", "main"]);
        fs::create_dir_all(d.join("crates/x/src")).unwrap();
        fs::write(d.join("README"), "x\n").unwrap();
        let lb = commit(&d, "lb", Some("Claude Opus 5.5"));
        let unit = Unit {
            name: "shipped/x/rules".into(),
            group: "g".into(),
            kind: "shipped".into(),
            target: "crates/x/src/rules.rs".into(),
            inputs: vec![],
        };
        let files = vec!["crates/x/src/rules.rs".to_string()];
        fs::write(
            d.join("crates/x/src/rules.rs"),
            "pub struct A { pub a: u32 }\npub fn g(n: usize) -> usize {\n    n\n}\n",
        )
        .unwrap();
        commit(&d, "the shipped unit", Some("Claude Sonnet 5.5"));
        let staged = "pub fn f(n: usize) -> usize {\n    let mut i = 0;\n    while i < n { i += 1; }\n    i + n\n}\n";
        fs::write(d.join("crates/x/src/staged.rs"), staged).unwrap();
        let stage = commit(&d, "stage the new body", Some("Claude Opus 5.5"));
        // Sonnet copies it over the unit file and never touches the source again
        fs::write(d.join("crates/x/src/rules.rs"), staged).unwrap();
        let cp = commit(&d, "copy over, source kept", Some("Claude Sonnet 5.5"));
        fs::write(
            d.join("crates/x/src/rules.rs"),
            staged.replace(
                "pub fn f(n: usize) -> usize {",
                "pub fn f(n: usize) -> usize\n//@ ensures n == n,\n{",
            ),
        )
        .unwrap();
        let proves = commit(&d, "the ensures", Some("Claude Opus 5.5"));
        let w = copy_over_walk(&d, &lb, &cp, &unit, &files);
        let ua = w.authors(&unit.name);
        let seen: Vec<(String, bool, bool, Vec<String>)> = ua
            .commits
            .iter()
            .map(|a| {
                (
                    authorship::short(&a.commit),
                    a.code,
                    a.proof,
                    a.paths.clone(),
                )
            })
            .collect();
        assert!(
            ua.commits.iter().any(|a| {
                a.commit == stage && a.code && a.paths.iter().any(|p| p == "crates/x/src/staged.rs")
            }),
            "the copy must not launder the staged code: {seen:?}"
        );
        assert!(
            !ua.commits
                .iter()
                .any(|a| a.commit == cp && (a.code || a.proof)),
            "the copy authors nothing itself: {seen:?}"
        );
        let p = ua
            .commits
            .iter()
            .find(|x| x.commit == proves)
            .expect("the proving commit is credited");
        assert!(p.proof && !p.code, "the clause is proof alone: {p:?}");
        assert_eq!(
            clash_of(ua),
            vec!["claude-opus-5-5".to_string()],
            "Opus wrote the code it now proves, through the copy: {seen:?}"
        );
        let _ = fs::remove_dir_all(&d);
    }

    /// Review K1d: the copy AND a rewrite of the source in one commit (`cp staged.rs rules.rs; git rm staged.rs; echo other >
    /// staged.rs`). Git reports two modifications, so neither `deleted_blob_of` (nothing is deleted) nor `same_blob_at` (the
    /// parent holds the content at a path the commit DOES touch) finds the source on its own.
    #[test]
    fn a_file_copied_over_the_unit_file_while_the_source_is_rewritten_keeps_the_author_of_the_staged_code()
     {
        let d = tmp_repo("k1d");
        g(&d, &["init", "-q", "-b", "main"]);
        fs::create_dir_all(d.join("crates/x/src")).unwrap();
        fs::write(d.join("README"), "x\n").unwrap();
        let lb = commit(&d, "lb", Some("Claude Opus 5.5"));
        let unit = Unit {
            name: "shipped/x/rules".into(),
            group: "g".into(),
            kind: "shipped".into(),
            target: "crates/x/src/rules.rs".into(),
            inputs: vec![],
        };
        let files = vec!["crates/x/src/rules.rs".to_string()];
        fs::write(
            d.join("crates/x/src/rules.rs"),
            "pub struct A { pub a: u32 }\npub fn g(n: usize) -> usize {\n    n\n}\n",
        )
        .unwrap();
        commit(&d, "the shipped unit", Some("Claude Sonnet 5.5"));
        let staged = "pub fn f(n: usize) -> usize {\n    let mut i = 0;\n    while i < n { i += 1; }\n    i + n\n}\n";
        fs::write(d.join("crates/x/src/staged.rs"), staged).unwrap();
        let stage = commit(&d, "stage the new body", Some("Claude Opus 5.5"));
        fs::write(d.join("crates/x/src/rules.rs"), staged).unwrap();
        fs::write(d.join("crates/x/src/staged.rs"), "// other\n").unwrap();
        let cp = commit(&d, "copy over, source rewritten", Some("Claude Sonnet 5.5"));
        fs::write(
            d.join("crates/x/src/rules.rs"),
            staged.replace(
                "pub fn f(n: usize) -> usize {",
                "pub fn f(n: usize) -> usize\n//@ ensures n == n,\n{",
            ),
        )
        .unwrap();
        let proves = commit(&d, "the ensures", Some("Claude Opus 5.5"));
        let w = copy_over_walk(&d, &lb, &cp, &unit, &files);
        let ua = w.authors(&unit.name);
        let seen: Vec<(String, bool, bool, Vec<String>)> = ua
            .commits
            .iter()
            .map(|a| {
                (
                    authorship::short(&a.commit),
                    a.code,
                    a.proof,
                    a.paths.clone(),
                )
            })
            .collect();
        assert!(
            ua.commits.iter().any(|a| {
                a.commit == stage && a.code && a.paths.iter().any(|p| p == "crates/x/src/staged.rs")
            }),
            "the rewrite of the source must not launder the staged code either: {seen:?}"
        );
        assert!(
            !ua.commits
                .iter()
                .any(|a| a.commit == cp && (a.code || a.proof)),
            "the copy authors nothing itself: {seen:?}"
        );
        let p = ua
            .commits
            .iter()
            .find(|x| x.commit == proves)
            .expect("the proving commit is credited");
        assert!(p.proof && !p.code, "the clause is proof alone: {p:?}");
        assert_eq!(
            clash_of(ua),
            vec!["claude-opus-5-5".to_string()],
            "Opus wrote the code it now proves, through the copy over a rewritten source: {seen:?}"
        );
        let _ = fs::remove_dir_all(&d);
    }

    /// Review K1m: the laundering of K1c done INSIDE a merge commit. The occurrence matching ran only for single-parent commits, so
    /// `git merge --no-ff --no-commit side` plus `cp staged.rs rules.rs` in the merge itself is reported as a plain modification of
    /// the unit file against the automatic merge: `-C` never looks at a merge, the source is left alone, and the mover is credited
    /// with the body Opus staged while Opus vanishes from the code side. A merge delta is now judged against its automatic merge,
    /// exactly as the rest of the merge logic already does.
    #[test]
    fn a_file_copied_over_the_unit_file_inside_a_merge_commit_keeps_the_author_of_the_staged_code()
    {
        let d = tmp_repo("k1m");
        g(&d, &["init", "-q", "-b", "main"]);
        fs::create_dir_all(d.join("crates/x/src")).unwrap();
        fs::write(d.join("README"), "x\n").unwrap();
        let lb = commit(&d, "lb", Some("Claude Opus 5.5"));
        let unit = Unit {
            name: "shipped/x/rules".into(),
            group: "g".into(),
            kind: "shipped".into(),
            target: "crates/x/src/rules.rs".into(),
            inputs: vec![],
        };
        let files = vec!["crates/x/src/rules.rs".to_string()];
        fs::write(
            d.join("crates/x/src/rules.rs"),
            "pub struct A { pub a: u32 }\npub fn g(n: usize) -> usize {\n    n\n}\n",
        )
        .unwrap();
        commit(&d, "the shipped unit", Some("Claude Sonnet 5.5"));
        let staged = "pub fn f(n: usize) -> usize {\n    let mut i = 0;\n    while i < n { i += 1; }\n    i + n\n}\n";
        fs::write(d.join("crates/x/src/staged.rs"), staged).unwrap();
        let stage = commit(&d, "stage the new body", Some("Claude Opus 5.5"));
        // a side branch that touches nothing of the unit, so the merge itself stays automatic
        g(&d, &["checkout", "-q", "-b", "side"]);
        fs::create_dir_all(d.join("docs")).unwrap();
        fs::write(d.join("docs/side.md"), "a note\n").unwrap();
        commit(&d, "a note on the side", Some("Claude Sonnet 5.5"));
        g(&d, &["checkout", "-q", "main"]);
        g(&d, &["merge", "-q", "--no-ff", "--no-commit", "side"]);
        fs::write(d.join("crates/x/src/rules.rs"), staged).unwrap();
        let cp = commit(&d, "merge the note, copy over", Some("Claude Sonnet 5.5"));
        let parents = g(&d, &["rev-list", "--parents", "-n", "1", &cp]);
        assert_eq!(
            parents.split_whitespace().count(),
            3,
            "the copy must sit in a merge commit: {parents}"
        );
        fs::write(
            d.join("crates/x/src/rules.rs"),
            staged.replace(
                "pub fn f(n: usize) -> usize {",
                "pub fn f(n: usize) -> usize\n//@ ensures n == n,\n{",
            ),
        )
        .unwrap();
        let proves = commit(&d, "the ensures", Some("Claude Opus 5.5"));
        let w = copy_over_walk(&d, &lb, &cp, &unit, &files);
        let ua = w.authors(&unit.name);
        let seen: Vec<(String, bool, bool, Vec<String>)> = ua
            .commits
            .iter()
            .map(|a| {
                (
                    authorship::short(&a.commit),
                    a.code,
                    a.proof,
                    a.paths.clone(),
                )
            })
            .collect();
        assert!(
            ua.commits.iter().any(|a| {
                a.commit == stage && a.code && a.paths.iter().any(|p| p == "crates/x/src/staged.rs")
            }),
            "a copy made inside a merge must not launder the staged code either: {seen:?}"
        );
        assert!(
            !ua.commits
                .iter()
                .any(|a| a.commit == cp && (a.code || a.proof)),
            "the merge copy authors nothing itself: {seen:?}"
        );
        let p = ua
            .commits
            .iter()
            .find(|x| x.commit == proves)
            .expect("the proving commit is credited");
        assert!(p.proof && !p.code, "the clause is proof alone: {p:?}");
        assert_eq!(
            clash_of(ua),
            vec!["claude-opus-5-5".to_string()],
            "Opus wrote the code it now proves, through a copy in a merge commit: {seen:?}"
        );
        let _ = fs::remove_dir_all(&d);
    }

    /// Review K1e: the laundering of K1c with a source of another extension, `cp NOTES.txt rules.rs`. The occurrence map required the
    /// same extension, so the staged body hidden in a scratch file of a different name escaped the match and the mover was credited
    /// with it. The match is on the bytes, not on the name: any blob the walk saw at another path is the source. A scratch file of
    /// prose or code without clauses classifies as code alone, so the source adds nothing to its own author's proof side.
    #[test]
    fn a_file_of_another_extension_copied_over_the_unit_file_keeps_the_author_of_the_staged_code() {
        let d = tmp_repo("k1e");
        g(&d, &["init", "-q", "-b", "main"]);
        fs::create_dir_all(d.join("crates/x/src")).unwrap();
        fs::write(d.join("README"), "x\n").unwrap();
        let lb = commit(&d, "lb", Some("Claude Opus 5.5"));
        let unit = Unit {
            name: "shipped/x/rules".into(),
            group: "g".into(),
            kind: "shipped".into(),
            target: "crates/x/src/rules.rs".into(),
            inputs: vec![],
        };
        let files = vec!["crates/x/src/rules.rs".to_string()];
        fs::write(
            d.join("crates/x/src/rules.rs"),
            "pub struct A { pub a: u32 }\npub fn g(n: usize) -> usize {\n    n\n}\n",
        )
        .unwrap();
        commit(&d, "the shipped unit", Some("Claude Sonnet 5.5"));
        let staged = "pub fn f(n: usize) -> usize {\n    let mut i = 0;\n    while i < n { i += 1; }\n    i + n\n}\n";
        fs::write(d.join("crates/x/src/new-body.txt"), staged).unwrap();
        let stage = commit(&d, "stage the new body as a note", Some("Claude Opus 5.5"));
        fs::write(d.join("crates/x/src/rules.rs"), staged).unwrap();
        let cp = commit(&d, "copy the note over", Some("Claude Sonnet 5.5"));
        fs::write(
            d.join("crates/x/src/rules.rs"),
            staged.replace(
                "pub fn f(n: usize) -> usize {",
                "pub fn f(n: usize) -> usize\n//@ ensures n == n,\n{",
            ),
        )
        .unwrap();
        let proves = commit(&d, "the ensures", Some("Claude Opus 5.5"));
        let w = copy_over_walk(&d, &lb, &cp, &unit, &files);
        let ua = w.authors(&unit.name);
        let seen: Vec<(String, bool, bool, Vec<String>)> = ua
            .commits
            .iter()
            .map(|a| {
                (
                    authorship::short(&a.commit),
                    a.code,
                    a.proof,
                    a.paths.clone(),
                )
            })
            .collect();
        assert!(
            ua.commits.iter().any(|a| {
                a.commit == stage
                    && a.code
                    && !a.proof
                    && a.paths.iter().any(|p| p == "crates/x/src/new-body.txt")
            }),
            "the staged body in a `.txt` must keep its author on the code side, not the proof side: {seen:?}"
        );
        assert!(
            !ua.commits
                .iter()
                .any(|a| a.commit == cp && (a.code || a.proof)),
            "the copy authors nothing itself: {seen:?}"
        );
        let p = ua
            .commits
            .iter()
            .find(|x| x.commit == proves)
            .expect("the proving commit is credited");
        assert!(p.proof && !p.code, "the clause is proof alone: {p:?}");
        assert_eq!(
            clash_of(ua),
            vec!["claude-opus-5-5".to_string()],
            "Opus wrote the code it now proves, through a copy of a file of another extension: {seen:?}"
        );
        let _ = fs::remove_dir_all(&d);
    }

    /// Review 4, K1par and the `slide_rules` regression, in the shape `git` gives them. The walk reads `git log --topo-order` and
    /// takes a LARGER index for an older commit, but a commit of a parallel branch is only listed below the judged one; it is never
    /// reached from it. So the blob a sibling branch parks there is not a version this commit could have copied from, and a match
    /// on the bytes alone lets an unmerged copy launder authorship (K1par) or a test fixture steal a proof commit (the fixture this
    /// branch added for `slide_rules`). `park` is the name the sibling branch holds the body under: the `.rs` scratch file is the
    /// working laundering, the `.txt` fixture is the regression.
    fn a_blob_parked_on_a_parallel_branch_is_not_a_predecessor(tag: &str, park: &str) {
        let d = tmp_repo(tag);
        g(&d, &["init", "-q", "-b", "main"]);
        fs::create_dir_all(d.join("crates/x/src")).unwrap();
        fs::create_dir_all(d.join("tools/ci/fixtures")).unwrap();
        fs::write(d.join("README"), "x\n").unwrap();
        let lb = commit(&d, "lb", Some("Claude Opus 5.5"));
        let unit = Unit {
            name: "shipped/x/rules".into(),
            group: "g".into(),
            kind: "shipped".into(),
            target: "crates/x/src/rules.rs".into(),
            inputs: vec![],
        };
        let files = vec!["crates/x/src/rules.rs".to_string()];
        put_rules(
            &d,
            "pub struct A { pub a: u32 }\npub fn g(n: usize) -> usize {\n    n\n}\n",
        );
        let base = commit(&d, "the shipped unit", Some("Claude Sonnet 5.5"));
        let staged = "pub fn f(n: usize) -> usize {\n    let mut i = 0;\n    while i < n { i += 1; }\n    i + n\n}\n";
        // the side branch: Opus writes the body into the unit file
        g(&d, &["checkout", "-q", "-b", "side"]);
        put_rules(&d, staged);
        let body = commit(&d, "opus: the new body", Some("Claude Opus 5.5"));
        // the first-parent line, AFTER `body` and without ever seeing it: the same bytes under another name
        g(&d, &["checkout", "-q", "main"]);
        fs::write(d.join(park), staged).unwrap();
        let parked = commit(
            &d,
            "the same bytes, on a branch that never met the body",
            Some("Claude Sonnet 5.5"),
        );
        // the two meet, and Opus proves the result
        g(&d, &["merge", "-q", "--no-ff", "-m", "merge side", "side"]);
        put_rules(
            &d,
            &staged.replace(
                "pub fn f(n: usize) -> usize {",
                "pub fn f(n: usize) -> usize\n//@ ensures n == n,\n{",
            ),
        );
        let proves = commit(&d, "opus: the ensures", Some("Claude Opus 5.5"));
        let order = g(
            &d,
            &[
                "log",
                "--topo-order",
                "--format=%H %s",
                &format!("{lb}..HEAD"),
            ],
        );
        let w = copy_over_walk(&d, &lb, &base, &unit, &files);
        let ua = w.authors(&unit.name);
        let seen: Vec<(String, bool, bool, Vec<String>)> = ua
            .commits
            .iter()
            .map(|a| {
                (
                    authorship::short(&a.commit),
                    a.code,
                    a.proof,
                    a.paths.clone(),
                )
            })
            .collect();
        // the scenario needs the order the bug lived in: the parked copy listed below the body it never reached
        let at = |id: &str| {
            let p = authorship::short(id);
            order
                .lines()
                .position(|l| l.starts_with(&p))
                .unwrap_or_else(|| panic!("`{p}` is not in the walked log: {order}"))
        };
        assert!(
            at(&parked) > at(&body),
            "the fixture needs the sibling commit to be read as older by the log order, which is what the walk used to trust: \
             {order}"
        );
        assert!(
            ua.commits
                .iter()
                .any(|a| a.commit == body && a.code && !a.proof),
            "the commit that wrote the body is its author; a copy on a branch it never reached cannot take that from it: {seen:?}"
        );
        assert!(
            !ua.commits
                .iter()
                .any(|a| a.commit == parked && (a.code || a.proof)),
            "the sibling branch authored nothing of this unit: {seen:?}"
        );
        let p = ua
            .commits
            .iter()
            .find(|x| x.commit == proves)
            .expect("the proving commit is credited");
        assert!(p.proof && !p.code, "the clause is proof alone: {p:?}");
        assert_eq!(
            clash_of(ua),
            vec!["claude-opus-5-5".to_string()],
            "Opus wrote the code it now proves: the parked copy must not launder it into Sonnet's hands: {seen:?}"
        );
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn k1par_blob_parked_on_a_parallel_branch_is_not_a_predecessor_of_the_commit_it_never_reached()
    {
        a_blob_parked_on_a_parallel_branch_is_not_a_predecessor("k1par", "crates/x/src/parked.rs");
    }

    #[test]
    fn k1par_fixture_on_a_sibling_branch_is_not_the_source_of_a_commit_on_another_branch() {
        a_blob_parked_on_a_parallel_branch_is_not_a_predecessor(
            "perfland-fixture",
            "tools/ci/fixtures/body_after.rs.txt",
        );
    }

    /// The unit as a prover edits it and as the parallel-branch fixture holds it, for the `slide_rules_*` and `k1par_*` tests.
    fn side_walk(d: &Path, lb: &str, base: &str) -> (Unit, authorship::Walk) {
        let (unit, files) = rules_unit();
        let w = copy_over_walk(d, lb, base, &unit, &files);
        (unit, w)
    }

    /// perf-land, `slide_rules` (the regression of review 4): Sonnet's plain unit file is the range base; on a SIDE branch Qoder
    /// commits the fixture `slide_rules_after.rs.txt`, byte for byte the file Fable's proof commit produces; on the main line Fable
    /// writes the clauses into the unit file, and the two meet in a merge. The fixture commit is listed below Fable's by the log
    /// order but is no ancestor of it, so it is not the source of the proof commit: Fable is credited with the proof text, Qoder
    /// with nothing, and nobody wrote code and proof both.
    #[test]
    fn slide_rules_fixture_on_a_parallel_branch_does_not_take_the_proof_from_the_prover() {
        let d = tmp_repo("slide-rules-fixture");
        g(&d, &["init", "-q", "-b", "main"]);
        fs::create_dir_all(d.join("crates/x/src")).unwrap();
        fs::create_dir_all(d.join("tools/ci/lucina-ci/src/fixtures")).unwrap();
        fs::write(d.join("README"), "x\n").unwrap();
        let lb = commit(&d, "lb", Some("Claude Opus 5.5"));
        put_rules(&d, SLIDE_BEFORE);
        let base = commit(&d, "the slide rule, plain", Some("Claude Sonnet 5.5"));
        g(&d, &["checkout", "-q", "-b", "side"]);
        fs::write(
            d.join("tools/ci/lucina-ci/src/fixtures/slide_rules_after.rs.txt"),
            SLIDE_AFTER,
        )
        .unwrap();
        let fixture = commit(
            &d,
            "fixtures for the roles tests",
            Some("Qoder (auto-free)"),
        );
        g(&d, &["checkout", "-q", "-b", "perf-land", "main"]);
        put_rules(&d, SLIDE_AFTER);
        let proves = commit(&d, "fable: prove slide_spans", Some("Claude Fable 5.2"));
        // this branch (the one holding the fixture) merges perf-land in, as the real landing does
        g(&d, &["checkout", "-q", "side"]);
        g(
            &d,
            &[
                "merge",
                "-q",
                "--no-ff",
                "-m",
                "merge perf-land",
                "perf-land",
            ],
        );
        let order = g(
            &d,
            &[
                "log",
                "--topo-order",
                "--format=%H",
                &format!("{base}..HEAD"),
            ],
        );
        let (unit, w) = side_walk(&d, &lb, &base);
        let ua = w.authors(&unit.name);
        let pos = |id: &str| order.lines().position(|l| l == id).expect("walked");
        assert!(
            pos(&fixture) > pos(&proves),
            "the scenario needs the fixture read as older by the log order: {order}"
        );
        let seen: Vec<(String, bool, bool)> = ua
            .commits
            .iter()
            .map(|a| (authorship::short(&a.commit), a.code, a.proof))
            .collect();
        let p = ua
            .commits
            .iter()
            .find(|x| x.commit == proves)
            .unwrap_or_else(|| panic!("the prover is credited: {seen:?}"));
        assert!(p.proof && !p.code, "Fable wrote the proof text: {seen:?}");
        assert!(
            !ua.commits
                .iter()
                .any(|a| a.commit == fixture && (a.code || a.proof)),
            "the fixture's author wrote nothing of this unit: {seen:?}"
        );
        assert!(
            clash_of(ua).is_empty(),
            "no model wrote the code and the proof of slide_rules: {seen:?}"
        );
        let _ = fs::remove_dir_all(&d);
    }

    /// Review 5, B4/B4r: Opus commits a body; AFTERWARDS a builder parks the very same bytes in a scratch file (two commits: a plain
    /// copy, then the edit), on another line; the Opus commit is re-parented on top of the park by `cherry-pick` (`rebase = false`)
    /// or `rebase --onto` (`rebase = true`), so the park IS an ancestor and the ancestry test passes. A copy match must never take
    /// the code credit off the strong model's commit: Opus wrote the body, then proves it, and that stays a one-model refusal.
    fn b4_reparented_opus_commit_keeps_its_code_credit(tag: &str, rebase: bool) {
        let d = tmp_repo(tag);
        g(&d, &["init", "-q", "-b", "main"]);
        fs::create_dir_all(d.join("crates/x/src")).unwrap();
        fs::write(d.join("README"), "x\n").unwrap();
        let lb = commit(&d, "lb", Some("Claude Opus 5.5"));
        let (unit, files) = rules_unit();
        put_rules(
            &d,
            "pub struct A { pub a: u32 }\npub fn g(n: usize) -> usize {\n    n\n}\n",
        );
        let base = commit(&d, "the shipped unit", Some("Claude Sonnet 5.5"));
        let staged = "pub fn f(n: usize) -> usize {\n    let mut i = 0;\n    while i < n { i += 1; }\n    i + n\n}\n";
        g(&d, &["checkout", "-q", "-b", "opus"]);
        put_rules(&d, staged);
        let body = commit(&d, "opus: the body", Some("Claude Opus 5.5"));
        g(&d, &["checkout", "-q", "main"]);
        fs::create_dir_all(d.join("docs/tmp")).unwrap();
        fs::write(
            d.join("docs/tmp/park.rs"),
            "pub struct A { pub a: u32 }\npub fn g(n: usize) -> usize {\n    n\n}\n",
        )
        .unwrap();
        commit(&d, "sonnet: a copy of the file", Some("Claude Sonnet 5.5"));
        fs::write(d.join("docs/tmp/park.rs"), staged).unwrap();
        let park = commit(&d, "sonnet: the body bytes", Some("Claude Sonnet 5.5"));
        let body2 = if rebase {
            g(&d, &["checkout", "-q", "opus"]);
            g(&d, &["rebase", "-q", "--onto", &park, &base]);
            g(&d, &["rev-parse", "HEAD"])
        } else {
            g(&d, &["cherry-pick", &body]);
            g(&d, &["rev-parse", "HEAD"])
        };
        assert_ne!(body, body2, "the Opus commit was re-parented");
        put_rules(
            &d,
            &staged.replace(
                "pub fn f(n: usize) -> usize {",
                "pub fn f(n: usize) -> usize\n//@ ensures n == n,\n{",
            ),
        );
        commit(&d, "opus: the ensures", Some("Claude Opus 5.5"));
        let w = copy_over_walk(&d, &lb, &base, &unit, &files);
        let ua = w.authors(&unit.name);
        let seen: Vec<(String, bool, bool)> = ua
            .commits
            .iter()
            .map(|a| (authorship::short(&a.commit), a.code, a.proof))
            .collect();
        assert!(
            ua.commits.iter().any(|a| a.commit == body2 && a.code),
            "the re-parented Opus commit still wrote the body: {seen:?}"
        );
        assert_eq!(
            clash_of(ua),
            vec!["claude-opus-5-5".to_string()],
            "Opus wrote the code it proves; a builder's scratch copy must not launder it: {seen:?}"
        );
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn b4_cherry_pick_of_the_opus_commit_onto_a_builder_park_keeps_the_code_with_opus() {
        b4_reparented_opus_commit_keeps_its_code_credit("b4-cherry", false);
    }

    #[test]
    fn b4r_rebase_of_the_opus_branch_onto_a_builder_park_keeps_the_code_with_opus() {
        b4_reparented_opus_commit_keeps_its_code_credit("b4r-rebase", true);
    }

    /// K1s, a KNOWN GAP (flaw fl-8ac82ac3: a near-copy with edited bytes escapes authorship matching and needs content
    /// similarity). Opus stages the body with one extra trailing newline; a builder copies it with that single byte dropped, so no
    /// blob matches; Opus then proves what is, byte for byte, its own code, and the walk credits the builder with the code. This
    /// asserts TODAY's behaviour (no clash). When the gap is closed this test fails and is flipped to must-refuse in the same change.
    #[test]
    fn known_gap_k1s_near_copy() {
        let d = tmp_repo("k1s");
        g(&d, &["init", "-q", "-b", "main"]);
        fs::create_dir_all(d.join("crates/x/src")).unwrap();
        fs::write(d.join("README"), "x\n").unwrap();
        let lb = commit(&d, "lb", Some("Claude Opus 5.5"));
        let (unit, files) = rules_unit();
        put_rules(
            &d,
            "pub struct A { pub a: u32 }\npub fn g(n: usize) -> usize {\n    n\n}\n",
        );
        let base = commit(&d, "the shipped unit", Some("Claude Sonnet 5.5"));
        let staged = "pub fn f(n: usize) -> usize {\n    let mut i = 0;\n    while i < n { i += 1; }\n    i + n\n}\n";
        fs::write(d.join("crates/x/src/staged.rs"), format!("{staged}\n")).unwrap();
        commit(&d, "opus stages the body", Some("Claude Opus 5.5"));
        put_rules(&d, staged);
        commit(
            &d,
            "sonnet: the copy, one byte shorter",
            Some("Claude Sonnet 5.5"),
        );
        put_rules(
            &d,
            &staged.replace(
                "pub fn f(n: usize) -> usize {",
                "pub fn f(n: usize) -> usize\n//@ ensures n == n,\n{",
            ),
        );
        commit(&d, "opus: the ensures", Some("Claude Opus 5.5"));
        let w = copy_over_walk(&d, &lb, &base, &unit, &files);
        assert!(
            clash_of(w.authors(&unit.name)).is_empty(),
            "today the near-copy is not matched (fl-8ac82ac3); if this fails the gap is closed: flip it to must-refuse"
        );
        let _ = fs::remove_dir_all(&d);
    }

    /// The legitimate case the ancestor rule must keep: Opus stages its body, a commit that IS an ancestor of everything after it
    /// renames the staged file on main, a feature branch cut from there copies the renamed file over the unit file (Sonnet), and
    /// Fable proves the result. The renamed file is a version in the copier's past: it authors nothing, the staged author keeps
    /// the code, and the prover, a different model, is clean.
    #[test]
    fn ancestor_rename_keeps_the_staged_author_and_refuses_nothing() {
        let d = tmp_repo("ancestor-rename");
        g(&d, &["init", "-q", "-b", "main"]);
        fs::create_dir_all(d.join("crates/x/src")).unwrap();
        fs::write(d.join("README"), "x\n").unwrap();
        let lb = commit(&d, "lb", Some("Claude Opus 5.5"));
        let (unit, _files) = rules_unit();
        put_rules(
            &d,
            "pub struct A { pub a: u32 }\npub fn g(n: usize) -> usize {\n    n\n}\n",
        );
        let base = commit(&d, "the shipped unit", Some("Claude Sonnet 5.5"));
        let staged = "pub fn f(n: usize) -> usize {\n    let mut i = 0;\n    while i < n { i += 1; }\n    i + n\n}\n";
        fs::write(d.join("crates/x/src/staged.rs"), staged).unwrap();
        let stage = commit(&d, "opus stages the body", Some("Claude Opus 5.5"));
        g(
            &d,
            &["mv", "crates/x/src/staged.rs", "crates/x/src/moved.rs"],
        );
        let rename = commit(&d, "sonnet tidies the name", Some("Claude Sonnet 5.5"));
        g(&d, &["checkout", "-q", "-b", "feature"]);
        fs::write(d.join("crates/x/src/rules.rs"), staged).unwrap();
        let cp = commit(&d, "sonnet copies the body in", Some("Claude Sonnet 5.5"));
        put_rules(
            &d,
            &staged.replace(
                "pub fn f(n: usize) -> usize {",
                "pub fn f(n: usize) -> usize\n//@ ensures n == n,\n{",
            ),
        );
        let proves = commit(&d, "fable: the ensures", Some("Claude Fable 5.2"));
        g(&d, &["checkout", "-q", "main"]);
        g(
            &d,
            &["merge", "-q", "--no-ff", "-m", "merge feature", "feature"],
        );
        let files = vec!["crates/x/src/rules.rs".to_string()];
        let w = copy_over_walk(&d, &lb, &base, &unit, &files);
        let ua = w.authors(&unit.name);
        let seen: Vec<(String, bool, bool, Vec<String>)> = ua
            .commits
            .iter()
            .map(|a| {
                (
                    authorship::short(&a.commit),
                    a.code,
                    a.proof,
                    a.paths.clone(),
                )
            })
            .collect();
        assert!(
            ua.commits.iter().any(|a| a.commit == stage && a.code),
            "the staged code is credited to the commit that wrote it: {seen:?}"
        );
        for (name, id) in [("the rename", &rename), ("the copy", &cp)] {
            assert!(
                !ua.commits
                    .iter()
                    .any(|a| &a.commit == id && (a.code || a.proof)),
                "{name} authors nothing: {seen:?}"
            );
        }
        let p = ua
            .commits
            .iter()
            .find(|x| x.commit == proves)
            .expect("credited");
        assert!(p.proof && !p.code, "{p:?}");
        assert!(
            clash_of(ua).is_empty(),
            "Opus code proved by Fable is the intended split, nothing to refuse: {seen:?}"
        );
        let _ = fs::remove_dir_all(&d);
    }

    /// The walk the copy-over scenarios need: the copy commit as the base, the lower bound as the cutoff.
    fn copy_over_walk(
        d: &Path,
        lb: &str,
        cp: &str,
        unit: &Unit,
        files: &[String],
    ) -> authorship::Walk {
        authorship::walk(
            d,
            cp,
            &ufps(std::slice::from_ref(unit), &unit.name, files),
            &Cutoff {
                commit: lb.to_string(),
                units: vec![],
                exempt: Vec::new(),
            },
            &BTreeMap::new(),
            &authorship::ModelResolver::default(),
            None,
        )
        .expect("the walk of a crafted repository")
    }

    #[test]
    fn the_history_walk_sees_earlier_merges_honours_corrections_and_pre_rule_exemptions() {
        let d = tmp_repo("history");
        g(&d, &["init", "-q", "-b", "main"]);
        fs::create_dir_all(d.join("crates/x/src")).unwrap();
        fs::create_dir_all(d.join("docs/ci")).unwrap();
        fs::write(d.join("crates/x/src/rules.rs"), RULES_RS).unwrap();
        fs::write(d.join(LOG), LOG_HEADER).unwrap();
        let lb = commit(&d, "lb", Some("Claude Sonnet 5.5"));
        let unit = Unit {
            name: "shipped/x/rules".into(),
            group: "g".into(),
            kind: "shipped".into(),
            target: "crates/x/src/rules.rs".into(),
            inputs: vec![],
        };
        let files = vec!["crates/x/src/rules.rs".to_string()];
        // M1: code by Opus on a side branch, merged
        g(&d, &["checkout", "-q", "-b", "m1"]);
        fs::write(
            d.join("crates/x/src/rules.rs"),
            RULES_RS.replace("i += 1;", "i += 1; let _z = 0;"),
        )
        .unwrap();
        let code = commit(&d, "code", Some("Claude Opus 5.5"));
        g(&d, &["checkout", "-q", "main"]);
        g(
            &d,
            &[
                "-c",
                "core.editor=true",
                "merge",
                "-q",
                "--no-ff",
                "-m",
                "M1",
                "m1",
            ],
        );
        let base = g(&d, &["rev-parse", "HEAD"]);
        // M2: statements + proof by Opus
        let p = RULES_RS
            .replace("i += 1;", "i += 1; let _z = 0;")
            .replace("invariant i <= n", "invariant i <= n + 0");
        fs::write(d.join("crates/x/src/rules.rs"), p).unwrap();
        commit(&d, "proof", Some("Claude Opus 5.5"));
        let wlk = |from: &str, since: Option<&str>, attrib: &str| -> authorship::Walk {
            let r = authorship::ModelResolver {
                attrib: attribution_map(Some(attrib)),
                ..Default::default()
            };
            let mut pre = BTreeMap::new();
            if let Some(x) = since {
                pre.insert(unit.name.clone(), x.to_string());
            }
            authorship::walk(
                &d,
                &base,
                &ufps(std::slice::from_ref(&unit), &unit.name, &files),
                &Cutoff {
                    commit: from.to_string(),
                    units: vec![],
                    exempt: Vec::new(),
                },
                &pre,
                &r,
                None,
            )
            .unwrap()
        };
        let code_of = |from: &str, since: Option<&str>, attrib: &str| {
            models_of(&wlk(from, since, attrib), &unit.name, true)
        };
        let proof_of =
            |from: &str, since: Option<&str>| models_of(&wlk(from, since, ""), &unit.name, false);
        // the range holds only the proof commit; the walk from the lower bound sees both sides, with Opus on both
        assert!(
            authorship::range_change(wlk(&base, None, "").authors(&unit.name))
                .exec
                .is_empty()
        );
        assert_eq!(code_of(&lb, None, ""), ["claude-opus-5-5"]);
        assert_eq!(proof_of(&lb, None), ["claude-opus-5-5"]);
        assert!(
            is_ancestor_of(&d, &code, &base),
            "the corrected commit is on the base"
        );
        // the coordinator corrects the M1 commit to Sonnet: no model on both sides any more
        assert_eq!(
            code_of(&lb, None, &at_line(&code, "Claude Sonnet 5.5")),
            ["claude-sonnet-5-5"]
        );
        // a pre-rule started_before at M1 exempts the code commit, and everything before the merge on both sides
        assert!(code_of(&lb, Some(&base), "").is_empty());
        assert_eq!(proof_of(&lb, Some(&base)), ["claude-opus-5-5"]);
        // no commits since the lower bound for a unit nobody touched
        let head = g(&d, &["rev-parse", "HEAD"]);
        assert!(wlk(&head, None, "").units.is_empty());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_code_only_change_by_the_model_that_proved_the_unit_in_an_earlier_merge_is_refused() {
        // statements and proof unchanged, only the code changed in this range
        let code_only = |code: &str, proof_by: Option<&str>| {
            let mut w = a();
            w.exec = "3333333333333333".into();
            let mut s = scn(Some(a()), w, vec![]);
            s.exec_models = vec![code.to_string()];
            s.history = Some(match proof_by {
                Some(p) => ci(&[], &[&[p]]),
                None => ChangeInfo::default(),
            });
            run(&s)
        };
        // Opus proved the unit in M1, edits the code in M2
        let o = code_only(OPUS, Some(OPUS));
        assert!(
            rules(&o).contains(&"one-model-wrote-code-and-proof"),
            "{:?}",
            o.refusals
        );
        // a builder edits the code of a proved unit: fine
        assert!(code_only(SON, Some(OPUS)).refusals.is_empty());
        // a unit with no statement/proof history: nothing to intersect
        assert!(code_only(OPUS, None).refusals.is_empty());
    }

    #[test]
    fn a_missing_history_lower_bound_is_a_refusal_at_the_merge_gate_and_a_note_locally() {
        let mut s = hist_scn();
        s.hist_avail = false;
        s.merge_gate = true;
        let o = run(&s);
        assert!(
            rules(&o).contains(&"history-lower-bound-missing"),
            "{:?}",
            o.refusals
        );
        s.merge_gate = false;
        let o = run(&s);
        assert!(
            !rules(&o).contains(&"history-lower-bound-missing"),
            "{:?}",
            o.refusals
        );
        assert!(
            o.notes.iter().any(|n| n.contains("lower bound")),
            "{:?}",
            o.notes
        );
    }

    #[test]
    fn the_history_walk_follows_a_unit_file_moved_in_an_earlier_merge() {
        let d = tmp_repo("histmove");
        g(&d, &["init", "-q", "-b", "main"]);
        fs::create_dir_all(d.join("crates/x/src")).unwrap();
        fs::create_dir_all(d.join("docs/ci")).unwrap();
        fs::write(d.join("crates/x/src/old.rs"), RULES_RS).unwrap();
        fs::write(d.join(LOG), LOG_HEADER).unwrap();
        let lb = commit(&d, "lb", Some("Claude Sonnet 5.5"));
        // code by Opus under the OLD name
        fs::write(
            d.join("crates/x/src/old.rs"),
            RULES_RS.replace("i += 1;", "i += 1; let _z = 0;"),
        )
        .unwrap();
        commit(&d, "code", Some("Claude Opus 5.5"));
        // a pure move by Sonnet
        g(&d, &["mv", "crates/x/src/old.rs", "crates/x/src/rules.rs"]);
        commit(&d, "move", Some("Claude Sonnet 5.5"));
        // proof text by Opus under the new name
        let p = RULES_RS
            .replace("i += 1;", "i += 1; let _z = 0;")
            .replace("invariant i <= n", "invariant i <= n + 0");
        fs::write(d.join("crates/x/src/rules.rs"), p).unwrap();
        commit(&d, "proof", Some("Claude Opus 5.5"));
        let unit = Unit {
            name: "shipped/x/rules".into(),
            group: "g".into(),
            kind: "shipped".into(),
            target: "crates/x/src/rules.rs".into(),
            inputs: vec![],
        };
        let new = vec!["crates/x/src/rules.rs".to_string()];
        // the walk starts from the unit's files at HEAD and follows the move back to the old name on its own (roles 1.4):
        // Opus code under the old name, nothing for the pure move, Opus proof under the new one
        let w = walk_in(&d, &lb, &unit.name, std::slice::from_ref(&unit), &new, None);
        let i = authorship::range_change(w.authors(&unit.name));
        assert_eq!(i.exec, vec![vec!["claude-opus-5-5".to_string()]], "{i:?}");
        assert_eq!(i.proof.len(), 1, "{i:?}");
        assert_eq!(i.proof[0].1, vec!["claude-opus-5-5".to_string()]);
        assert_eq!(
            models_of(&w, &unit.name, true),
            ["claude-opus-5-5".to_string()],
            "the move itself authors nothing"
        );
        let _ = fs::remove_dir_all(&d);
    }

    /// Review I3b: `merge-tree` refuses unrelated histories, so the automatic merge of such a merge commit cannot be
    /// computed. Nothing being read against nothing let the commit that weakened the `ensures` clause inside the merge
    /// author nothing at all. The merge is read against every parent instead, the way a conflicted path is.
    #[test]
    fn a_merge_of_unrelated_histories_is_read_against_every_parent() {
        let d = tmp_repo("merge-unrelated");
        g(&d, &["init", "-q", "-b", "main"]);
        fs::create_dir_all(d.join("crates/x/src")).unwrap();
        fs::create_dir_all(d.join("docs/ci")).unwrap();
        fs::write(d.join("crates/x/src/rules.rs"), RULES_RS).unwrap();
        fs::write(d.join(LOG), LOG_HEADER).unwrap();
        let lb = commit(&d, "lb", Some("Claude Opus 5.5"));
        let branched = RULES_RS.replace("invariant i <= n", "invariant i <= n + 0");
        fs::write(d.join("crates/x/src/rules.rs"), branched.clone()).unwrap();
        let proof = commit(&d, "branch proof", Some("Claude Fable 5.2"));
        // a root with no history in common with main
        g(&d, &["checkout", "-q", "--orphan", "o"]);
        g(&d, &["rm", "-rqf", "."]);
        fs::write(d.join("f.txt"), "z\n").unwrap();
        commit(&d, "orphan root", Some("Claude Sonnet 5.5"));
        // back on the branch, the unrelated root merged in, and the statement weakened in the merge itself
        g(&d, &["checkout", "-q", "-f", "-b", "s-i3b", &proof]);
        assert!(gx(
            &d,
            &[
                "merge",
                "--no-ff",
                "--no-commit",
                "--allow-unrelated-histories",
                "o"
            ]
        ));
        fs::write(
            d.join("crates/x/src/rules.rs"),
            branched.replace("ensures ret == n", "ensures ret == n || true"),
        )
        .unwrap();
        let merged = commit(&d, "merge of the unrelated root", Some("Claude Sonnet 5.5"));
        let (unit, files) = rules_unit();
        let w = walk_in(
            &d,
            &lb,
            &unit.name,
            std::slice::from_ref(&unit),
            &files,
            None,
        );
        assert!(
            w.notes.iter().any(|n| n.contains("could not be computed")),
            "{:?}",
            w.notes
        );
        let i = authorship::range_change(w.authors(&unit.name));
        // oldest first: the branch's proof commit, then the merge; the merge is credited with what it changed and with
        // no code, since its body is its first parent's
        assert_eq!(i.proof.len(), 2, "{i:?}");
        assert_eq!(i.proof[1].0, merged[..10], "{i:?}");
        assert_eq!(i.proof[1].1, vec!["claude-sonnet-5-5".to_string()], "{i:?}");
        assert!(i.exec.is_empty(), "the merge brought no code: {i:?}");
        assert_eq!(
            models_of(&w, &unit.name, false),
            [
                "claude-fable-5-2".to_string(),
                "claude-sonnet-5-5".to_string()
            ],
            "the proof text of both commits is seen"
        );
        assert!(models_of(&w, &unit.name, true).is_empty(), "no code credit");
        let _ = fs::remove_dir_all(&d);
    }

    /// Review 5 (scenario N): the cutoff list named a unit and a commit, and the walk exempted every ancestor of that commit
    /// on the strength of the list alone - no `pre-rule` record, so no implementer standing in for the exempt code either. The
    /// list is only the coordinator's permission to hold such a record; the record is what exempts.
    #[test]
    fn a_cutoff_listing_without_a_pre_rule_record_exempts_no_history() {
        let d = tmp_repo("listed-only");
        g(&d, &["init", "-q", "-b", "main"]);
        fs::create_dir_all(d.join("crates/x/src")).unwrap();
        fs::create_dir_all(d.join("docs/ci")).unwrap();
        fs::write(d.join("crates/x/src/rules.rs"), RULES_RS).unwrap();
        fs::write(d.join(LOG), LOG_HEADER).unwrap();
        let lb = commit(&d, "lb", Some("Claude Opus 5.5"));
        // one Opus commit that shaped BOTH sides after the lower bound: the clash the listing used to erase
        fs::write(
            d.join("crates/x/src/rules.rs"),
            RULES_RS
                .replace("i += 1;", "i += 1; let _z = 0;")
                .replace("ensures ret == n", "ensures ret == n + 0"),
        )
        .unwrap();
        let m0 = commit(&d, "opus wrote code and proof", Some("Claude Opus 5.5"));
        let (unit, files) = rules_unit();
        let opus = "claude-opus-5-5".to_string();
        let by_opus = vec![opus.clone()];
        // the list alone: M0 is read, and one model stands on both sides (spec 25 and the control of scenario N)
        let w = walk_listed(
            &d,
            &lb,
            &unit.name,
            std::slice::from_ref(&unit),
            &files,
            &[(&unit.name as &str, m0.as_str())],
        );
        assert_eq!(models_of(&w, &unit.name, true), by_opus);
        assert_eq!(models_of(&w, &unit.name, false), by_opus);
        // the same started_before behind a valid `pre-rule` record: the exempt past is the record's, and both sets are empty
        let w = walk_in(
            &d,
            &lb,
            &unit.name,
            std::slice::from_ref(&unit),
            &files,
            Some(&m0),
        );
        assert!(models_of(&w, &unit.name, true).is_empty());
        assert!(models_of(&w, &unit.name, false).is_empty());
        // and with neither, the same walk as above
        let w = walk_in(
            &d,
            &lb,
            &unit.name,
            std::slice::from_ref(&unit),
            &files,
            None,
        );
        assert_eq!(models_of(&w, &unit.name, true), by_opus);
        let _ = fs::remove_dir_all(&d);
    }

    /// An exemption line is a range, not a blanket: it drops the commits between `from_commit` (exclusive) and `to_commit`
    /// (inclusive) and stops there. The first later commit that shapes the unit is walked and judged by the full rule, so the
    /// coordinator's approval of a unit's past cannot carry a clash that happened after it.
    #[test]
    fn an_exemption_line_drops_its_range_and_nothing_after_it() {
        let d = tmp_repo("exempt-range");
        g(&d, &["init", "-q", "-b", "main"]);
        fs::create_dir_all(d.join("crates/x/src")).unwrap();
        fs::create_dir_all(d.join("docs/ci")).unwrap();
        fs::write(d.join("crates/x/src/rules.rs"), RULES_RS).unwrap();
        fs::write(d.join(LOG), LOG_HEADER).unwrap();
        let lb = commit(&d, "lb", Some("Claude Opus 5.5"));
        // the unit's creating commit: one Sonnet session writes the code AND its clauses
        fs::write(
            d.join("crates/x/src/rules.rs"),
            RULES_RS
                .replace("i += 1;", "i += 1; let _z = 0;")
                .replace("ensures ret == n", "ensures ret == n + 0"),
        )
        .unwrap();
        let born = commit(&d, "sonnet wrote both sides", Some("Claude Sonnet 5.5"));
        // the commit the list fixes for the unit, shaping nothing of it: the line ends here
        fs::write(d.join("docs/ci/x.md"), "an unrelated page\n").unwrap();
        let last = commit(&d, "the listed commit", Some("Claude Opus 5.5"));
        // a later clash, by the same model: OUTSIDE the approved range, so the rule reads it
        fs::write(
            d.join("crates/x/src/rules.rs"),
            RULES_RS
                .replace("i += 1;", "i += 1; let _z = 1;")
                .replace("ensures ret == n", "ensures ret == n + 1"),
        )
        .unwrap();
        let later = commit(
            &d,
            "sonnet again, after the range",
            Some("Claude Sonnet 5.5"),
        );
        let (unit, files) = rules_unit();
        let u = unit.name.as_str();
        let peers = std::slice::from_ref(&unit);
        let listed = [(u as &str, last.as_str())];
        let seen = |w: &authorship::Walk| -> Vec<String> {
            w.authors(u)
                .commits
                .iter()
                .map(|a| authorship::short(&a.commit))
                .collect()
        };
        // no line at all: the whole range is walked, both clashes seen
        let w = walk_listed(&d, &lb, u, peers, &files, &listed);
        assert_eq!(
            seen(&w),
            vec![authorship::short(&born), authorship::short(&later)],
            "the listing alone exempts nobody"
        );
        assert_eq!(clash_of(w.authors(u)).len(), 1, "Sonnet on both sides");
        // the coordinator's line for (lb, last]: the creating commit is gone from the walk, the later clash is not
        let w = walk_exempted(&d, &lb, u, peers, &files, &listed, &[(u, &lb, &last)]);
        assert_eq!(
            seen(&w),
            vec![authorship::short(&later)],
            "the line drops its own range and nothing else"
        );
        assert_eq!(
            clash_of(w.authors(u)),
            vec![SON.to_string()],
            "the commit after `to_commit` falls under the full rule"
        );
        // the same line drawn to the later commit covers it too: nothing of the unit is left to judge
        let w = walk_exempted(
            &d,
            &lb,
            u,
            peers,
            &files,
            &[(u, later.as_str())],
            &[(u, &lb, &later)],
        );
        assert!(seen(&w).is_empty(), "{:?}", seen(&w));
        // The commits are gone from the walk, but the line's `code_author` stands for the code it approved (review 2, New 2), so
        // the code side is not empty: it names what the coordinator's file records. The proof side holds only what was walked.
        assert_eq!(
            models_of(&w, u, true),
            vec![SON.to_string()],
            "the exemption's code author is counted, the dropped commits are not"
        );
        assert!(models_of(&w, u, false).is_empty());
        let _ = fs::remove_dir_all(&d);
    }

    /// Review 2, New 2: an exemption approves a range, it does not erase who wrote it. The line's `code_author` stands for the
    /// exempt code exactly as a `pre-rule` record's implementer does, so the model that wrote the approved code and now edits the
    /// proof text meets the one rule. `not established` names nobody and joins nothing.
    #[test]
    fn an_exemption_line_s_code_author_stands_for_the_exempt_code() {
        let d = tmp_repo("exempt-author");
        g(&d, &["init", "-q", "-b", "main"]);
        fs::create_dir_all(d.join("crates/x/src")).unwrap();
        fs::create_dir_all(d.join("docs/ci")).unwrap();
        fs::write(d.join("crates/x/src/rules.rs"), RULES_RS).unwrap();
        fs::write(d.join(LOG), LOG_HEADER).unwrap();
        let lb = commit(&d, "lb", Some("Claude Sonnet 5.5"));
        // M0: one strong model writes the body AND its clause. The coordinator approves exactly this range.
        fs::write(
            d.join("crates/x/src/rules.rs"),
            RULES_RS
                .replace("i += 1;", "i += 1; let _z = 0;")
                .replace("ensures ret == n", "ensures ret == n + 0"),
        )
        .unwrap();
        let m0 = commit(&d, "opus wrote both sides", Some("Claude Opus 5.5"));
        // the commit the list fixes for the unit: it shapes nothing of it, the line ends here
        fs::write(d.join("docs/ci/x.md"), "an unrelated page\n").unwrap();
        let last = commit(&d, "the listed commit", Some("Claude Sonnet 5.5"));
        // the range: the same strong model edits ONLY the clause of the body it wrote itself
        fs::write(
            d.join("crates/x/src/rules.rs"),
            RULES_RS
                .replace("i += 1;", "i += 1; let _z = 0;")
                .replace("ensures ret == n", "ensures ret == n + 1"),
        )
        .unwrap();
        let proves = commit(&d, "opus edits the clause", Some("Claude Opus 5.5"));
        let (unit, files) = rules_unit();
        let u = unit.name.as_str();
        let peers = std::slice::from_ref(&unit);
        let listed = [(u as &str, last.as_str())];
        let line = [(u, lb.as_str(), last.as_str())];
        let seen = |w: &authorship::Walk| -> Vec<String> {
            w.authors(u)
                .commits
                .iter()
                .map(|a| authorship::short(&a.commit))
                .collect()
        };
        // without the line the whole range is walked and the clash is plain to see
        let w = walk_listed(&d, &lb, u, peers, &files, &listed);
        assert!(seen(&w).contains(&authorship::short(&m0)));
        assert_eq!(clash_of(w.authors(u)), vec![OPUS.to_string()]);
        // with the line, and the line naming Opus for the code: the range is still exempt, the author is not forgotten
        let w = walk_exempted_authors(
            &d,
            &lb,
            u,
            peers,
            &files,
            &listed,
            &line,
            OPUS,
            NOT_ESTABLISHED,
        );
        assert_eq!(
            seen(&w),
            vec![authorship::short(&proves)],
            "the line still drops only its own range"
        );
        assert_eq!(
            models_of(&w, u, true),
            vec![OPUS.to_string()],
            "the exempt code keeps the author its line records"
        );
        assert_eq!(
            clash_of(w.authors(u)),
            vec![OPUS.to_string()],
            "an approved past does not let a model prove its own code"
        );
        // `not established` names nobody: nothing is added to either side
        let w = walk_exempted_authors(
            &d,
            &lb,
            u,
            peers,
            &files,
            &listed,
            &line,
            NOT_ESTABLISHED,
            NOT_ESTABLISHED,
        );
        assert!(models_of(&w, u, true).is_empty(), "{:?}", seen(&w));
        assert!(clash_of(w.authors(u)).is_empty());
        let _ = fs::remove_dir_all(&d);
    }

    /// Review 2, New 3: `from_commit` must be in the past of `to_commit`. A line that names a sibling commit as its origin draws
    /// no stretch of history at all, and the walk reads `to_commit` minus `from_commit` as everything up to `to_commit`: an
    /// approval of three commits would exempt the whole life of the unit. Such a line is refused as malformed, with its line
    /// number, and a commit that is not in this repository is refused the same way.
    #[test]
    fn an_exemption_line_must_name_a_real_stretch_of_history() {
        let d = tmp_repo("exempt-shape");
        g(&d, &["init", "-q", "-b", "main"]);
        fs::write(d.join("README"), "x\n").unwrap();
        let born = commit(&d, "root", Some("Claude Sonnet 5.5"));
        fs::write(d.join("a"), "1\n").unwrap();
        let first = commit(&d, "the unit's first commit", Some("Claude Sonnet 5.5"));
        // a sibling of the listed commit: the same parent, neither one an ancestor of the other
        g(&d, &["checkout", "-q", "-b", "sibling", &first]);
        fs::write(d.join("b"), "2\n").unwrap();
        let sib = commit(&d, "a side branch", Some("Claude Opus 5.5"));
        g(&d, &["checkout", "-q", "main"]);
        fs::write(d.join("c"), "3\n").unwrap();
        let last = commit(&d, "the listed commit", Some("Claude Opus 5.5"));
        let mut listed = BTreeMap::new();
        listed.insert("verus/u".to_string(), last.clone());
        let line = |from: &str| -> String {
            format!(
                "verus/u\t{from}\t{last}\tclaude-opus-5-5\t{NOT_ESTABLISHED}\tJuno Lucina (coordinator)\t2026-11-12\tthe range the coordinator approved\n"
            )
        };
        let exists = |c: &str| commit_exists(&d, c);
        let anc = |a: &str, b: &str| is_ancestor_of(&d, a, b);
        // the honest line: the unit's first commit up to the commit the list fixes
        assert!(
            parse_exemptions(&line(&first), &listed, &exists, &anc).is_ok(),
            "a real range parses"
        );
        // a sibling is not the origin of that range
        let e = parse_exemptions(&line(&sib), &listed, &exists, &anc)
            .expect_err("a sibling from_commit names no stretch of history");
        assert!(
            e.contains("line 1") && e.contains("not an ancestor") && e.contains("to_commit"),
            "{e}"
        );
        // and neither does a commit that is not in this repository
        let gone = "0123456789abcdef0123456789abcdef01234567";
        let e = parse_exemptions(&line(gone), &listed, &exists, &anc)
            .expect_err("an absent commit names no range");
        assert!(
            e.contains("line 1") && e.contains("not a commit in this repository"),
            "{e}"
        );
        // the same line at the merge gate is the malformed refusal, naming the file
        let cut = parse_cutoff(CUT).expect("the fixture list parses");
        let (cu, sb) = (&cut.units[0].0, &cut.units[0].1.clone());
        let mut s = scn(None, a(), vec![]);
        s.cutoff = Some(CUT.into());
        s.exempt_work = Some(format!(
            "{cu}\t{born}\t{sb}\tclaude-opus-5-5\t{NOT_ESTABLISHED}\tJuno Lucina (coordinator)\t2026-11-12\tthe range\n"
        ));
        s.ancestor_ok = false;
        let o = run(&s);
        let r = o
            .refusals
            .iter()
            .find(|r| r.rule == "exemptions-malformed")
            .unwrap_or_else(|| panic!("a line whose range is not history: {:?}", o.refusals));
        assert!(
            r.detail.contains("line 1") && r.detail.contains(EXEMPT),
            "{}",
            r.detail
        );
        let _ = fs::remove_dir_all(&d);
    }

    /// A line nobody can read exempts nobody, and a line that names no listed unit is the same: both are refused, so a typo
    /// in the coordinator's file cannot silently withdraw an exemption or invent one.
    #[test]
    fn a_malformed_exemption_line_is_refused_and_exempts_nobody() {
        let (h1, h2) = (
            "0123456789abcdef0123456789abcdef01234567",
            "abcdef0123456789abcdef0123456789abcdef01",
        );
        let mut listed = BTreeMap::new();
        listed.insert("verus/u".to_string(), h2.to_string());
        // these lines are judged for their SHAPE only: the commits are invented hex, so history is granted
        let (always, a_anc) = (|_: &str| true, |_: &str, _: &str| true);
        let good = "verus/u\t0123456789abcdef0123456789abcdef01234567\tabcdef0123456789abcdef0123456789abcdef01\tclaude-sonnet-5-5\tnot established\tJuno Lucina (coordinator)\t2026-11-12\tthe creating commit and the range the coordinator approved\n";
        let got = parse_exemptions(good, &listed, &always, &a_anc)
            .expect("the shape the shipped file uses parses");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].unit, "verus/u");
        assert_eq!(got[0].statement_author, NOT_ESTABLISHED);
        for (why, l) in [
            (
                "seven fields, not eight",
                good.trim_end().rsplit_once('\t').unwrap().0,
            ),
            ("an empty field", &good.replace("not established", "")),
            (
                "an abbreviated commit",
                &good.replace("abcdef0123456789abcdef0123456789abcdef01", "abcdef01"),
            ),
            (
                "a date that is not YYYY-MM-DD",
                &good.replace("2026-11-12", "12-11-2026"),
            ),
            (
                "a unit the cutoff list does not carry",
                &good.replace("verus/u\t", "verus/other\t"),
            ),
            (
                "the right unit at a commit the list does not fix for it",
                &good.replace(h2, h1),
            ),
            (
                "a tab inside a note",
                &good.replace("the creating commit", "a\tb"),
            ),
        ] {
            assert!(
                parse_exemptions(l, &listed, &always, &a_anc).is_err(),
                "{why}: `{l}` must be refused"
            );
        }
        // comments and blank lines carry no weight
        assert!(parse_exemptions("# the coordinator's file\n\n", &listed, &always, &a_anc).is_ok());
        // and the refusal, by name, at the merge gate
        let mut s = scn(None, a(), vec![]);
        s.cutoff = Some(CUT.into());
        s.exempt_work = Some(
            "verus/u\t0123456789abcdef0123456789abcdef01234567\tabcdef01\tsonnet\topus\tJuno\t2026-11-12\tright unit, short commit\n"
                .into(),
        );
        let o = run(&s);
        let r = o
            .refusals
            .iter()
            .find(|r| r.rule == "exemptions-malformed")
            .unwrap_or_else(|| panic!("a 9-digit commit in the file: {:?}", o.refusals));
        assert!(
            r.detail.contains("line 1") && r.detail.contains(EXEMPT),
            "{}",
            r.detail
        );
    }

    /// Review 5, scenario N at the gate: a cutoff line whose unit holds neither a `pre-rule` record nor an exemption is refused,
    /// named for the unit. A record, a line, or a listed commit that lies at or below the lower bound closes it.
    #[test]
    fn a_cutoff_listing_with_no_record_and_no_line_is_refused() {
        let cut = parse_cutoff(CUT).expect("the fixture list parses");
        let reaches = |_: &str| true;
        let none = BTreeMap::new();
        let gaps = cutoff_gaps(&cut, &none, &reaches);
        assert_eq!(gaps.len(), 1, "{gaps:?}");
        assert!(
            gaps[0].contains("verus/u")
                && gaps[0].contains("no pre-rule record or exemption")
                && gaps[0].contains(EXEMPT),
            "{}",
            gaps[0]
        );
        // the commit lies at or below the lower bound: the walk would have dropped it anyway, so nothing is exempted away
        assert!(cutoff_gaps(&cut, &none, &|_| false).is_empty());
        // a valid `pre-rule` record for the unit closes it (roles 1.5)
        let mut pre = BTreeMap::new();
        pre.insert("verus/u".to_string(), cut.units[0].1.clone());
        assert!(cutoff_gaps(&cut, &pre, &reaches).is_empty());
        // so does the coordinator's line for the very commit the list fixes
        let mut lined = cut.clone();
        lined.exempt = parse_exemptions(
            &format!("verus/u\t{}\t{}\tclaude-sonnet-5-5\tnot established\tJuno Lucina (coordinator)\t2026-11-12\tthe creating commit\n", HISTORY_LOWER_BOUND, cut.units[0].1),
            &listed_starts(Some(&cut)),
            &|_: &str| true,
            &|_: &str, _: &str| true,
        )
        .expect("a line for a listed unit parses");
        assert!(cutoff_gaps(&lined, &none, &reaches).is_empty());
        // ...and the same line is REPORTED as an acceptance, never as a clean rule (review item 5): the note names the unit, says
        // the past is not carried by a record, and quotes the two author fields as the file records them.
        let accepts = cutoff_acceptances(&lined, &none, &reaches);
        assert_eq!(accepts.len(), 1, "{accepts:?}");
        assert!(
            accepts[0].contains("verus/u")
                && accepts[0].contains("NOT by a `pre-rule`")
                && accepts[0].contains("claude-sonnet-5-5")
                && accepts[0].contains(NOT_ESTABLISHED)
                && accepts[0].contains("the rule is not satisfied for it"),
            "{}",
            accepts[0]
        );
        // a unit that holds the record is not an acceptance: the record carries it, roles 1.5
        assert!(cutoff_acceptances(&lined, &pre, &reaches).is_empty());
        // a past the walk never reaches exempts nothing, so nothing is reported either
        assert!(cutoff_acceptances(&lined, &none, &|_| false).is_empty());
        // and the gate prints it as a note, so a green run still reads as an acceptance
        let mut s = scn(None, a(), vec![]);
        s.cutoff = Some(CUT.into());
        s.cutoff_accepts = cutoff_acceptances(&lined, &none, &reaches);
        let o = run(&s);
        assert!(
            o.notes
                .iter()
                .any(|n| n.contains("verus/u") && n.contains("the rule is not satisfied for it")),
            "an honoured exemption must not read as a clean rule: {:?}",
            o.notes
        );
        s.cutoff_accepts = Vec::new();
        // and the refusal the gate reports, with the unit named
        s.cutoff_gaps = cutoff_gaps(&cut, &none, &reaches);
        let o = run(&s);
        let r = o
            .refusals
            .iter()
            .find(|r| r.rule == "cutoff-listing-without-exemption")
            .unwrap_or_else(|| panic!("a listing with nothing behind it: {:?}", o.refusals));
        assert!(r.detail.contains("verus/u"), "{}", r.detail);
        s.cutoff_gaps = Vec::new();
        assert!(
            !rules(&run(&s)).contains(&"cutoff-listing-without-exemption"),
            "no gap, no refusal"
        );
    }

    /// Review 5, scenario N held against the shipped files: a cutoff listing is no exemption of its own. Every listed unit whose
    /// `started_before` falls inside the walked range holds either a `pre-rule` record or a named line of the coordinator's
    /// exemption file, and every such line names a listed unit that holds no record. So the exception cannot drift to a third
    /// unit and dies with the line it names.
    #[test]
    fn the_exception_names_exactly_the_listed_units_without_a_record() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let cut = parse_cutoff(
            &fs::read_to_string(root.join(CUTOFF)).expect("the coordinator's list is in the tree"),
        )
        .expect("the coordinator's list parses");
        let exempt = match fs::read_to_string(root.join(EXEMPT)) {
            Ok(t) => parse_exemptions(
                &t,
                &listed_starts(Some(&cut)),
                &|c| commit_exists(&root, c),
                &|a, b| is_ancestor_of(&root, a, b),
            )
            .expect("the coordinator's exemption file parses against the coordinator's list"),
            Err(_) => Vec::new(),
        };
        let logged: BTreeSet<String> =
            parse_log(&fs::read_to_string(root.join(LOG)).expect("the record log is in the tree"))
                .iter()
                .filter_map(|e| e.res.as_ref().ok())
                .filter(|r| r.kind == "pre-rule")
                .map(|r| r.unit.clone())
                .collect();
        if !commit_exists(&root, &cut.commit) {
            // a shallow clone: whether a listed commit is at or before the lower bound cannot be judged here
            return;
        }
        // a listing is only worth explaining when the walked range can reach it: a commit at or below the lower bound the walk
        // would have dropped anyway, and a commit that is not in HEAD's history at all (the branch it lives on was never merged,
        // or this clone cannot see it), exempt nothing and name nobody (review 3, weaknesses 3 and 4)
        let inert = |c: &str| !reaches_walk(&root, &cut.commit, c);
        for (u, c, _) in &cut.units {
            let mut listed = BTreeMap::new();
            listed.insert(u.clone(), c.clone());
            let by_line = exempt_past(u, &BTreeMap::new(), &listed, &exempt).is_some();
            if logged.contains(u) {
                assert!(
                    !by_line,
                    "{u}: a unit with a `pre-rule` record exempts through the record, not through a line"
                );
            } else if !inert(c) {
                assert!(
                    exempt.iter().any(|e| &e.unit == u && &e.to == c),
                    "{u} is listed in {CUTOFF} at {c}, inside the walked range, with no `pre-rule` record and no line of {EXEMPT}: \
                     `roles --check` refuses it (`cutoff-listing-without-exemption`), so write the record or the line"
                );
                assert!(by_line, "{u}: the line must exempt it");
            }
        }
        // the file names nothing the list does not: a unit no line of {CUTOFF} carries exempts nothing and is refused
        for e in &exempt {
            assert!(
                cut.units.iter().any(|(x, y, _)| x == &e.unit && y == &e.to),
                "{}: {EXEMPT} must name a unit and a commit the cutoff list carries",
                e.unit
            );
            assert!(
                !logged.contains(&e.unit),
                "{}: {EXEMPT} names a unit that already holds a `pre-rule` record: the record exempts it, drop the line",
                e.unit
            );
        }
        // Item 5: what the tool REPORTS about these units. A line without a record is an acceptance the reader must see, so the
        // note names the unit, says the past is not carried by a record, and quotes the author fields exactly as the coordinator
        // recorded them - `not established` included - never the rule as satisfied.
        let mut shipped = cut.clone();
        shipped.exempt = exempt.clone();
        let recorded: BTreeMap<String, String> = cut
            .units
            .iter()
            .filter(|(u, _, _)| logged.contains(u))
            .map(|(u, c, _)| (u.clone(), c.clone()))
            .collect();
        let accepts = cutoff_acceptances(&shipped, &recorded, &|c| !inert(c));
        for e in &exempt {
            let m = accepts
                .iter()
                .find(|m| m.contains(&e.unit))
                .unwrap_or_else(|| panic!("{}: an accepted past is reported: {accepts:?}", e.unit));
            assert!(
                m.contains("NOT by a `pre-rule`") && m.contains("the rule is not satisfied for it"),
                "an acceptance reads as an acceptance, not as a clean rule: {m}"
            );
            assert!(
                m.contains(&e.code_author) && m.contains(&e.statement_author),
                "{}: the note quotes the authors the file records: {m}",
                e.unit
            );
        }
        assert_eq!(
            accepts.len(),
            exempt.len(),
            "nothing outside the lines the file carries is reported: {accepts:?}"
        );
    }

    /// Review 3, weakness 5: the number in a malformed-exemption message is the LINE OF THE FILE, comments and blank lines
    /// counted. The shipped file is a run of header comments, so counting only data lines told a coordinator "line 3" for what
    /// `grep -n` finds at line 17.
    #[test]
    fn a_malformed_exemption_line_is_numbered_by_its_line_in_the_file() {
        let (h1, h2) = (
            "0123456789abcdef0123456789abcdef01234567",
            "abcdef0123456789abcdef0123456789abcdef01",
        );
        let mut listed = BTreeMap::new();
        listed.insert("verus/u".to_string(), h2.to_string());
        let (always, a_anc) = (|_: &str| true, |_: &str, _: &str| true);
        // sixteen lines of header, exactly the shape of the shipped file: the first data line is the seventeenth
        let header = format!("{}{}", "# a comment line\n".repeat(10), "\n".repeat(6));
        assert_eq!(header.lines().count(), 16, "{header:?}");
        // a line of the wrong SHAPE: too few fields
        let e = parse_exemptions(
            &format!("{header}verus/u\t{h1}\t{h2}\n"),
            &listed,
            &always,
            &a_anc,
        )
        .expect_err("a line nobody can read is refused");
        assert!(
            e.contains("line 17") && e.contains("expected unit<TAB>from_commit"),
            "{e}"
        );
        // a line of the right shape that names no listed unit
        let e = parse_exemptions(
            &format!("{header}verus/other\t{h1}\t{h2}\tclaude-sonnet-5-5\t{NOT_ESTABLISHED}\tJuno Lucina (coordinator)\t2026-11-12\tthe range\n"),
            &listed,
            &always,
            &a_anc,
        )
        .expect_err("a line that exempts no listing is refused");
        assert!(
            e.contains("line 17") && e.contains("exempts no cutoff listing"),
            "{e}"
        );
        // and the message that names the stretch of history reads the same way, with no doubled spaces
        let e = parse_exemptions(
            &format!("{header}verus/u\t{h1}\t{h2}\tclaude-sonnet-5-5\t{NOT_ESTABLISHED}\tJuno Lucina (coordinator)\t2026-11-1\tnote\n"),
            &listed,
            &always,
            &a_anc,
        )
        .expect_err("a date nobody can read is refused");
        assert!(e.contains("line 17") && e.contains("not YYYY-MM-DD"), "{e}");
        // no message in the parser runs two spaces together: they read as a formatting bug
        for text in [
            format!(
                "{header}verus/u\tzz\t{h2}\tsonnet\tx\tJuno Lucina (coordinator)\t2026-11-12\tnote\n"
            ),
            format!(
                "{header}verus/other\t{h1}\t{h2}\tsonnet\tx\tJuno Lucina (coordinator)\t2026-11-12\tnote\n"
            ),
        ] {
            let e = parse_exemptions(&text, &listed, &always, &a_anc)
                .expect_err("each of these is refused");
            assert!(!e.contains("  "), "a message with a run of spaces: {e:?}");
        }
    }

    /// Review 3, weaknesses 3 and 4: what an exemption line and a cutoff listing are worth depends on the history THIS repository
    /// has. A line whose `to_commit` sits on a branch that was never merged approves a past no walk can reach: it exempts nothing,
    /// its `code_author` must not reach the rule as a phantom, and the listing it stands behind is not a gap the coordinator has to
    /// explain - refusing it would punish every single-branch and shallow clone. A listing the walk DOES reach still needs its
    /// record or its line, and a valid `pre-rule` record still closes it.
    #[test]
    fn a_listing_or_line_that_names_history_this_branch_lacks_is_inert_and_reported() {
        let d = tmp_repo("inert-listing");
        g(&d, &["init", "-q", "-b", "main"]);
        fs::write(d.join("README"), "x\n").unwrap();
        let born = commit(&d, "root", Some("Claude Sonnet 5.5"));
        fs::write(d.join("u"), "1\n").unwrap();
        let first = commit(&d, "the unit's first commit", Some("Claude Sonnet 5.5"));
        fs::write(d.join("u"), "2\n").unwrap();
        let listed = commit(&d, "the commit the list fixes", Some("Claude Opus 5.5"));
        // a branch that is never merged: its commits are objects here but in nobody's history
        g(&d, &["checkout", "-q", "-b", "gone", &first]);
        fs::write(d.join("u"), "3\n").unwrap();
        let orphan = commit(
            &d,
            "a commit on a branch that was never merged",
            Some("Claude Opus 5.5"),
        );
        g(&d, &["checkout", "-q", "main"]);
        assert!(
            commit_exists(&d, &orphan) && !is_ancestor_of(&d, &orphan, "HEAD"),
            "the fixture needs a commit that exists but is not in HEAD's history"
        );
        let exists = |c: &str| commit_exists(&d, c);
        let anc = |a: &str, b: &str| is_ancestor_of(&d, a, b);
        // the predicate `roles --check` uses: the walk reads `LB..HEAD` and nothing else
        let reaches = |c: &str| reaches_walk(&d, &born, c);
        let line = |u: &str, from: &str, to: &str| -> String {
            format!(
                "{u}\t{from}\t{to}\tclaude-sonnet-5-5\t{NOT_ESTABLISHED}\tJuno Lucina (coordinator)\t2026-11-12\tthe range approved\n"
            )
        };
        // two listings, one reachable and one not, each WITHOUT a record or a line
        let mut cut = Cutoff {
            commit: born.clone(),
            units: vec![
                ("shipped/u".to_string(), listed.clone(), "listed".into()),
                (
                    "shipped/orphan".to_string(),
                    orphan.clone(),
                    "listed".into(),
                ),
            ],
            exempt: Vec::new(),
        };
        let none = BTreeMap::new();
        let gaps = cutoff_gaps(&cut, &none, &reaches);
        assert_eq!(
            gaps.len(),
            1,
            "only a listing the walk can reach is unexplained: {gaps:?}"
        );
        assert!(
            gaps[0].contains("shipped/u") && !gaps[0].contains("shipped/orphan"),
            "{gaps:?}"
        );
        // the same for the lines: an inert line is dropped and REPORTED, a live one is kept
        let mut exempt = parse_exemptions(
            &format!(
                "{}{}",
                line("shipped/u", &first, &listed),
                line("shipped/orphan", &first, &orphan)
            ),
            &listed_starts(Some(&cut)),
            &exists,
            &anc,
        )
        .expect("both lines parse against the list they name");
        let notes = drop_inert_exempt(&mut exempt, &|c| is_ancestor_of(&d, c, "HEAD"));
        assert_eq!(exempt.len(), 1, "{exempt:?}");
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(
            notes[0].contains("shipped/orphan")
                && notes[0].contains("inert")
                && notes[0].contains(EXEMPT),
            "an inert line is reported, never kept silently: {notes:?}"
        );
        assert_eq!(
            notes[0].matches("claude-sonnet-5-5").count(),
            0,
            "and its authors name nobody in the rule: {}",
            notes[0]
        );
        // with the live line the reachable listing is covered and nothing is accepted behind the inert one
        cut.exempt = exempt;
        assert!(
            cutoff_gaps(&cut, &none, &reaches).is_empty(),
            "{:?}",
            cutoff_gaps(&cut, &none, &reaches)
        );
        let accepts = cutoff_acceptances(&cut, &none, &reaches);
        assert_eq!(accepts.len(), 1, "{accepts:?}");
        assert!(
            accepts[0].contains("shipped/u") && !accepts[0].contains("shipped/orphan"),
            "{accepts:?}"
        );
        // a valid `pre-rule` record closes the reachable listing the same way (roles 1.5)
        let mut rec = BTreeMap::new();
        rec.insert("shipped/u".to_string(), listed.clone());
        let mut bare = cut.clone();
        bare.exempt = Vec::new();
        assert!(cutoff_gaps(&bare, &rec, &reaches).is_empty());
        assert!(cutoff_acceptances(&bare, &rec, &reaches).is_empty());
        let _ = fs::remove_dir_all(&d);
    }

    /// Review 4, weakness 2 (Opus's scenario b4): inertness is judged against HEAD, so a line that sleeps while the branch it names
    /// is unmerged WAKES when some range merges that branch - and what it then drops from both authorship sets is real history this
    /// range brought in. That is a coordinator approval applied by a merge, so the range that revives it is refused
    /// (`exemption-revived`) unless the coordinator lands it over with the `cutoff:` allow. A branch whose base already holds the
    /// commit is not reviving anything, and neither is a range that cannot see it: the same data, three different ranges.
    #[test]
    fn a_line_or_listing_a_merge_revives_is_refused_by_the_range_that_merged_it() {
        let d = tmp_repo("revived-line");
        g(&d, &["init", "-q", "-b", "main"]);
        fs::write(d.join("README"), "x\n").unwrap();
        let born = commit(&d, "root", Some("Claude Sonnet 5.5"));
        fs::write(d.join("u"), "1\n").unwrap();
        let first = commit(&d, "the unit's first commit", Some("Claude Sonnet 5.5"));
        // the range base: the coordinator's listing and line are already here, for a commit this history does not hold
        fs::write(d.join("v"), "2\n").unwrap();
        let base = commit(&d, "where the branch starts", Some("Claude Sonnet 5.5"));
        // the abandoned branch, cut before the listing: it holds the commit the listing names
        g(&d, &["checkout", "-q", "-b", "ab", &first]);
        fs::write(d.join("w"), "3\n").unwrap();
        let listed = commit(&d, "the commit the list fixes", Some("Claude Opus 5.5"));
        g(&d, &["checkout", "-q", "main"]);
        let exists = |c: &str| commit_exists(&d, c);
        let anc = |a: &str, b: &str| is_ancestor_of(&d, a, b);
        let live = |c: &str| is_ancestor_of(&d, c, "HEAD");
        let cut_of = |with_line: bool| -> Cutoff {
            let mut cut = Cutoff {
                commit: born.clone(),
                units: vec![(
                    "shipped/u".to_string(),
                    listed.clone(),
                    "the abandoned branch".into(),
                )],
                exempt: Vec::new(),
            };
            if with_line {
                cut.exempt = parse_exemptions(
                    &format!(
                        "shipped/u\t{first}\t{listed}\tclaude-sonnet-5-5\t{NOT_ESTABLISHED}\tJuno Lucina (coordinator)\t2026-11-13\tthe range approved\n"
                    ),
                    &listed_starts(Some(&cut)),
                    &exists,
                    &anc,
                )
                .expect("the line names the listing it stands on");
            }
            cut
        };
        // 1. before the merge: the commit is in nobody's history, so the line is INERT (dropped and noted, review 3) and reviving
        //    is not what happens here. `roles --check` reads only what `drop_inert_exempt` kept, which is nothing.
        let mut sleeping = cut_of(true);
        let notes = drop_inert_exempt(&mut sleeping.exempt, &live);
        assert_eq!(notes.len(), 1, "the line is inert and reported: {notes:?}");
        let at_base = |c: &str| is_ancestor_of(&d, c, &base);
        assert!(
            revived_exemptions(&sleeping, &live, &at_base).is_empty(),
            "an inert line revives nothing: {:?}",
            revived_exemptions(&sleeping, &live, &at_base)
        );
        // 2. a Sonnet merge of the abandoned branch: the same data is now LIVE, and the range that merged it is refused. Both the
        //    line and the bare listing (the ground of a `pre-rule` record) are named.
        g(&d, &["merge", "-q", "--no-ff", "-m", "merge ab", "ab"]);
        let mut woken = cut_of(true);
        assert!(
            drop_inert_exempt(&mut woken.exempt, &live).is_empty(),
            "the merge makes the line live"
        );
        let r = revived_exemptions(&woken, &live, &at_base);
        assert_eq!(r.len(), 1, "the line names its listing once: {r:?}");
        assert!(
            r[0].contains(EXEMPT)
                && r[0].contains("shipped/u")
                && r[0].contains(&authorship::short(&listed))
                && r[0].contains("LIVE"),
            "the message names the file, the unit and the commit: {}",
            r[0]
        );
        let bare = revived_exemptions(&cut_of(false), &live, &at_base);
        assert_eq!(bare.len(), 1, "{bare:?}");
        assert!(
            bare[0].contains(CUTOFF) && bare[0].contains("shipped/u"),
            "a revived listing is named too, for a `pre-rule` record stands on it: {}",
            bare[0]
        );
        // 3. the range whose BASE already holds the merged commit revives nothing: the refusal is once, for the merge that did it
        let tip = g(&d, &["rev-parse", "HEAD"]);
        let after = |c: &str| is_ancestor_of(&d, c, &tip);
        assert!(
            revived_exemptions(&woken, &live, &after).is_empty(),
            "{:?}",
            revived_exemptions(&woken, &live, &after)
        );
        // 4. the gate refuses by name, and the coordinator's `cutoff:` allow is the only way over it
        let mut s = scn(None, a(), vec![]);
        s.exemption_revived = r.clone();
        assert!(
            rules(&run(&s)).contains(&"exemption-revived"),
            "{:?}",
            rules(&run(&s))
        );
        s.allow_cutoff = true;
        assert!(
            !rules(&run(&s)).contains(&"exemption-revived"),
            "the allow is the coordinator's, as for the cutoff files themselves: {:?}",
            rules(&run(&s))
        );
        let _ = fs::remove_dir_all(&d);
    }

    /// Review 3, cosmetic 6: `--check` counts the implementer of a covering `pre-rule` record on the code side, the report did
    /// not, and printed `Clash none` for a unit the same run refuses. Both views now come from one walk that is handed the records,
    /// so `--authors`, `--audit` and `--check` ask the same question of the same history.
    #[test]
    fn the_pre_rule_implementer_is_on_the_code_side_of_every_view() {
        let d = tmp_repo("pre-rule-view");
        g(&d, &["init", "-q", "-b", "main"]);
        fs::create_dir_all(d.join("crates/x/src")).unwrap();
        fs::write(d.join("crates/x/src/rules.rs"), RULES_RS).unwrap();
        let lb = commit(&d, "the unit before roles", Some("Claude Sonnet 5.5"));
        // the exempt past: Sonnet's code, ended at `before`, which the `pre-rule` record of the unit carries
        let with_code = RULES_RS.replace("i += 1;", "i += 1; let _z = 0;");
        fs::write(d.join("crates/x/src/rules.rs"), with_code.clone()).unwrap();
        let before = commit(&d, "code before the cutoff", Some("Claude Sonnet 5.5"));
        // after it, the same model edits proof text only
        fs::write(
            d.join("crates/x/src/rules.rs"),
            with_code.replace("invariant i <= n", "invariant i <= n + 0"),
        )
        .unwrap();
        let base = g(&d, &["rev-parse", "HEAD"]);
        commit(&d, "a later proof edit", Some("Claude Sonnet 5.5"));
        let unit = Unit {
            name: "shipped/x/rules".into(),
            group: "g".into(),
            kind: "shipped".into(),
            target: "crates/x/src/rules.rs".into(),
            inputs: vec![],
        };
        let files = vec!["crates/x/src/rules.rs".to_string()];
        let fps = ufps(std::slice::from_ref(&unit), &unit.name, &files);
        let cut = Cutoff {
            commit: lb.clone(),
            units: vec![],
            exempt: Vec::new(),
        };
        let mut pre = BTreeMap::new();
        pre.insert(unit.name.clone(), before.clone());
        let mut pre_impl = BTreeMap::new();
        pre_impl.insert(unit.name.clone(), model_norm("Claude Sonnet 5.5"));
        let res = authorship::ModelResolver::default();
        // the walk alone: the record is invisible to it, so the code side is empty and the report reads as a clean unit
        let bare = authorship::walk(&d, &base, &fps, &cut, &pre, &res, None).unwrap();
        assert!(
            bare.authors(&unit.name).pre_rule_impl.is_none(),
            "the walk reads commits, not records"
        );
        assert!(
            authors_report(&unit.name, &bare, &base).contains("Clash\tnone"),
            "this is exactly what `--authors` used to print for a unit `--check` refuses: {}",
            authors_report(&unit.name, &bare, &base)
        );
        // the same walk with the record handed to it, as every command path now does
        let w = walk_with_pre_rules(&d, &base, &fps, &cut, &pre, &pre_impl, &res, None).unwrap();
        let ua = w.authors(&unit.name);
        assert_eq!(
            ua.pre_rule_impl.as_deref(),
            Some("claude-sonnet-5-5"),
            "the implementer stands for the exempt code"
        );
        assert_eq!(
            clash_of(ua),
            vec!["claude-sonnet-5-5".to_string()],
            "the model that wrote the exempt code now writes proof text over it"
        );
        let r = authors_report(&unit.name, &w, &base);
        assert!(
            r.contains("Clash\tclaude-sonnet-5-5: one-model-wrote-code-and-proof"),
            "the report names the clash it refuses on: {r}"
        );
        // review 4, message nit: the exempt code was written between the lower bound and `started_before`, so the line names the
        // commit the exemption ends at, not the bound it does not touch
        assert!(
            r.contains(&format!(
                "Pre-rule\tclaude-sonnet-5-5 wrote the code exempt up to `started_before` {}",
                authorship::short(&before)
            )),
            "the Pre-rule line names the end of the exempt stretch: {r}"
        );
        assert!(
            !r.contains("before the lower bound"),
            "the exempt commits are NOT below the lower bound: {r}"
        );
        assert!(
            audit_report(&fps, &w).contains("claude-sonnet-5-5"),
            "and `--audit` reads the same: {}",
            audit_report(&fps, &w)
        );
        let _ = fs::remove_dir_all(&d);
    }

    /// The shipped files held against review 3, weaknesses 3 and 4: `docs/ci/proof-roles-exemptions.tsv` carries no unit today,
    /// and the cutoff list still carries units whose approved past lives on a branch that was never merged here. Neither may
    /// reach `roles --check` as a gap or an acceptance, while a listing the walk CAN reach must hold its record or its line, which
    /// `the_exception_names_exactly_the_listed_units_without_a_record` checks. So the tool with these files exits 0 and the
    /// coordinator may drop the two lines whenever it suits.
    #[test]
    fn the_shipped_files_leave_no_reachable_listing_unexplained_and_no_inert_one_refused() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let cut =
            parse_cutoff(&fs::read_to_string(root.join(CUTOFF)).expect("the shipped cutoff list"))
                .expect("the shipped cutoff list parses");
        let listed = listed_starts(Some(&cut));
        let exempt = match fs::read_to_string(root.join(EXEMPT)) {
            Ok(t) => parse_exemptions(&t, &listed, &|c| commit_exists(&root, c), &|a, b| {
                is_ancestor_of(&root, a, b)
            })
            .expect("the shipped exemption file parses against the shipped list"),
            Err(_) => Vec::new(),
        };
        let logged: BTreeMap<String, String> =
            parse_log(&fs::read_to_string(root.join(LOG)).expect("the record log is in the tree"))
                .iter()
                .filter_map(|e| e.res.as_ref().ok())
                .filter(|r| r.kind == "pre-rule")
                .map(|r| (r.unit.clone(), r.started_before.clone()))
                .collect();
        let mut cut = cut;
        let mut exempt = exempt;
        // what the command path does with both files, on the same predicate
        let notes = drop_inert_exempt(&mut exempt, &|c| is_ancestor_of(&root, c, "HEAD"));
        cut.exempt = exempt.clone();
        let reaches = |c: &str| reaches_walk(&root, &cut.commit, c);
        let gaps = cutoff_gaps(&cut, &logged, &reaches);
        assert!(
            gaps.is_empty(),
            "the shipped files must not refuse a clone that cannot see an unmerged branch: {gaps:?}"
        );
        let accepts = cutoff_acceptances(&cut, &logged, &reaches);
        assert!(
            accepts.is_empty(),
            "no acceptance is reported for a unit the file does not carry: {accepts:?}"
        );
        // every line that was dropped said so, and nothing else was
        for e in &notes {
            assert!(e.contains("inert") && e.contains(EXEMPT), "{e}");
        }
        // not vacuous: the list DOES carry listings this branch cannot reach, and each of them is inert above
        let dead = cut
            .units
            .iter()
            .filter(|(_, c, _)| !is_ancestor_of(&root, c, "HEAD"))
            .count();
        println!(
            "shipped files: {} listings, {dead} of them history this branch does not have, \
             {gaps:?} gaps and {accepts:?} acceptances reported",
            cut.units.len()
        );
    }

    /// roles 1.3, spec 25 and 36: a commit of the earlier history that shaped a unit and that no source names is
    /// recorded as unattributed, which is what the `author-unknown` refusal keys on. A trailer on the range's own
    /// commit does not excuse it, and an attribution line for it does.
    #[test]
    fn an_unnamed_commit_in_the_earlier_history_is_recorded_without_a_model() {
        let d = tmp_repo("unnamed");
        g(&d, &["init", "-q", "-b", "main"]);
        fs::create_dir_all(d.join("crates/x/src")).unwrap();
        fs::write(d.join("crates/x/src/rules.rs"), RULES_RS).unwrap();
        let lb = commit(&d, "lb", Some("Claude Opus 5.5"));
        let unit = Unit {
            name: "shipped/x/rules".into(),
            group: "g".into(),
            kind: "shipped".into(),
            target: "crates/x/src/rules.rs".into(),
            inputs: vec![],
        };
        let files = vec!["crates/x/src/rules.rs".to_string()];
        // an earlier merge writes code here and names nobody
        fs::write(
            d.join("crates/x/src/rules.rs"),
            RULES_RS.replace("i += 1;", "i += 1; let _z = 0;"),
        )
        .unwrap();
        let unnamed = commit(&d, "code with no trailer", None);
        // the range under review rewrites the proof, trailered
        fs::write(
            d.join("crates/x/src/rules.rs"),
            RULES_RS
                .replace("i += 1;", "i += 1; let _z = 0;")
                .replace("invariant i <= n", "invariant i <= n + 0"),
        )
        .unwrap();
        commit(&d, "proof", Some("Claude Fable 5.2"));
        let base = g(&d, &["rev-parse", "HEAD~1"]);
        let walk_with = |attrib: Option<String>| {
            let r = authorship::ModelResolver::new(None, attrib.as_deref());
            authorship::walk(
                &d,
                &base,
                &ufps(std::slice::from_ref(&unit), &unit.name, &files),
                &Cutoff {
                    commit: lb.clone(),
                    units: vec![],
                    exempt: Vec::new(),
                },
                &BTreeMap::new(),
                &r,
                None,
            )
            .unwrap()
        };
        let w = walk_with(None);
        let a = w
            .authors(&unit.name)
            .commits
            .iter()
            .find(|x| x.commit == unnamed)
            .expect("the unnamed commit shaped the unit");
        assert!(a.code && !a.proof, "{a:?}");
        assert!(
            a.models.is_empty(),
            "nothing names its author, so the walk says so rather than guessing: {:?}",
            a.models
        );
        assert!(
            !a.in_range,
            "it is in the earlier history the range lands on"
        );
        // the coordinator's correction reaches the same entry
        let fixed = format!("{unnamed}\tClaude Sonnet 5.5\tlead chat stamped its own line\n");
        let w = walk_with(Some(fixed));
        let a = w
            .authors(&unit.name)
            .commits
            .iter()
            .find(|x| x.commit == unnamed)
            .expect("the corrected commit still shaped the unit");
        assert_eq!(a.models, ["claude-sonnet-5-5".to_string()]);
        assert_eq!(a.source, authorship::Source::Attribution);
        let _ = fs::remove_dir_all(&d);
    }

    // ---------------------------------------------------------------- the commit ledger (spec 9, 10, 11)

    /// One ledger line as the session wrapper writes it: `prev` is the lowercase hex SHA-256 of the previous line
    /// as stored, without its newline.
    fn ledger_line(commit: &str, model: &str, repo: &str, prev: &str) -> String {
        format!(
            "{{\"v\":1,\"commit\":\"{commit}\",\"repo\":\"{repo}\",\"model\":\"{model}\",\"prev\":\"{prev}\"}}\n"
        )
    }

    fn ledger_at(dir: &Path, text: &str) -> PathBuf {
        let p = dir.join("ledger.tsv");
        fs::write(&p, text).unwrap();
        p
    }

    const C1: &str = "1111111111111111111111111111111111111111";
    const C2: &str = "2222222222222222222222222222222222222222";
    const ZEROS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

    /// Spec 9 and the source order of roles 1.3: the ledger answers first, before a trailer that names someone else,
    /// and a commit it does not know still falls back to the trailers. Entries of another repository are not read.
    #[test]
    fn the_ledger_names_the_model_before_the_trailer_and_another_repos_chain_is_not_read() {
        let d = tmp_repo("ledger-order");
        let son = ledger_line(C1, "Claude Sonnet 5.5", authorship::LEDGER_REPO, ZEROS);
        let p = ledger_at(&d, &son);
        let r = authorship::ModelResolver::new(Some(&p), None);
        assert!(r.ledger_broken.is_none(), "{:?}", r.ledger_broken);
        assert_eq!(r.ledger_lines, 1, "{:?}", r.ledger);
        let msg = "work\n\nCo-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>\n".to_string();
        let (m, src, contra) = r.models(C1, &msg);
        assert_eq!(m, ["claude-sonnet-5-5".to_string()]);
        assert_eq!(src, authorship::Source::Ledger);
        assert!(contra.is_none());
        // a commit the ledger does not hold: the trailer decides, as it always did
        let (m, src, _) = r.models(C2, &msg);
        assert_eq!(m, ["claude-opus-5-5".to_string()]);
        assert_eq!(src, authorship::Source::Trailer);
        // the same chain written for another repository holds nothing for this one
        let other = son.replace(authorship::LEDGER_REPO, "SomeOtherRepo");
        let p = ledger_at(&d, &other);
        let r = authorship::ModelResolver::new(Some(&p), None);
        assert!(
            r.ledger.is_empty(),
            "another repository's entries are not ours"
        );
        assert_eq!(
            r.models(C1, &msg).1,
            authorship::Source::Trailer,
            "and the trailers still decide"
        );
        let _ = fs::remove_dir_all(&d);
    }

    /// Spec 10: when the correction file and the ledger name different models the check is told, and the ledger wins
    /// the reading because it is the evidence the session wrote itself.
    #[test]
    fn a_ledger_that_disagrees_with_the_attribution_file_is_reported_and_decides() {
        let d = tmp_repo("ledger-conflict");
        let son = ledger_line(C1, "Claude Sonnet 5.5", authorship::LEDGER_REPO, ZEROS);
        let p = ledger_at(&d, &son);
        let attrib = format!("{C1}\tClaude Fable 5.2\tlead chat stamped its own line\n");
        let r = authorship::ModelResolver::new(Some(&p), Some(&attrib));
        let (m, src, contra) = r.models(C1, "");
        assert_eq!(m, ["claude-sonnet-5-5".to_string()]);
        assert_eq!(src, authorship::Source::Ledger);
        let why = contra.expect("the two sources name different models");
        assert!(why.contains("attribution file"), "{why}");
        assert!(why.contains("claude-fable-5-2"), "{why}");
        // agreement is not a contradiction
        let same = format!("{C1}\tClaude Sonnet 5.5\tsame model\n");
        let r = authorship::ModelResolver::new(Some(&p), Some(&same));
        assert!(r.models(C1, "").2.is_none());
        // and a correction alone still reads as a correction
        let r = authorship::ModelResolver::new(None, Some(&attrib));
        let (m, src, _) = r.models(C1, "");
        assert_eq!(m, ["claude-fable-5-2".to_string()]);
        assert_eq!(src, authorship::Source::Attribution);
        let _ = fs::remove_dir_all(&d);
    }

    /// Spec 11: a chain that does not verify is reported whole and used for nothing; `check` refuses it at the merge
    /// gate and notes it locally (asserted below on the reason the resolver gives).
    #[test]
    fn a_corrupted_ledger_is_broken_as_a_whole_and_names_no_model() {
        let d = tmp_repo("ledger-broken");
        let l1 = ledger_line(C1, "Claude Sonnet 5.5", authorship::LEDGER_REPO, ZEROS);
        let prev = authorship::sha256_hex(l1.trim_end().as_bytes());
        let l2 = ledger_line(C2, "Claude Opus 5.5", authorship::LEDGER_REPO, &prev);
        let good = format!("{l1}{l2}");
        let r = authorship::ModelResolver::new(Some(&ledger_at(&d, &good)), None);
        assert_eq!(
            r.ledger.len(),
            2,
            "{:?} / {} / {:?}",
            r.ledger_broken,
            r.ledger_lines,
            r.ledger
        );
        assert!(r.ledger_broken.is_none());
        // one byte of the second line's `prev` flipped: the chain stops there
        let flipped = good.replacen(&prev, &format!("f{}", &prev[1..]), 1);
        let r = authorship::ModelResolver::new(Some(&ledger_at(&d, &flipped)), None);
        let why = r.ledger_broken.clone().expect("a flipped digest is broken");
        assert!(why.contains("line 2"), "{why}");
        assert!(
            r.ledger.is_empty() || r.ledger.len() < 2,
            "nothing whole is taken from a broken chain: {:?}",
            r.ledger
        );
        // no trailing newline
        let r = authorship::ModelResolver::new(Some(&ledger_at(&d, good.trim_end())), None);
        assert!(r.ledger_broken.is_some());
        // a chain that verifies but belongs to another repository holds nothing for this one
        let o1 = ledger_line(C1, "Claude Sonnet 5.5", "NotThisRepo", ZEROS);
        let o2 = ledger_line(
            C2,
            "Claude Opus 5.5",
            "NotThisRepo",
            &authorship::sha256_hex(o1.trim_end().as_bytes()),
        );
        let r = authorship::ModelResolver::new(Some(&ledger_at(&d, &format!("{o1}{o2}"))), None);
        assert!(r.ledger_broken.is_none(), "{:?}", r.ledger_broken);
        assert!(r.ledger.is_empty());
        let _ = fs::remove_dir_all(&d);
    }

    // ---------------------------------------------------------------- the real history (spec R1, R3, R4)
    //
    // These read the repository the check runs in: a temp repository cannot show what the rule does to the real
    // merges, and a clone without `main` cannot show it either, so they are opt-in (see `real_history_wanted`).

    /// The real-history tests read the checkout they run in and take seconds, so they are opt-in rather than
    /// `#[ignore]` (the diff gate refuses an ignored test): run them with
    /// `LUCINA_CI_REAL_HISTORY=1 cargo test -- the_real_ the_audit_`.
    fn real_history_wanted() -> bool {
        if std::env::var("LUCINA_CI_REAL_HISTORY").is_ok_and(|v| v == "0") {
            return false;
        }
        std::env::var("LUCINA_CI_REAL_HISTORY").is_ok()
    }

    /// The repository the test reads: the checkout it runs in, or the one LUCINA_CI_TEST_REPO names.
    fn real_root() -> PathBuf {
        match std::env::var("LUCINA_CI_TEST_REPO") {
            Ok(p) => PathBuf::from(p),
            Err(_) => top().expect("not inside a git repository"),
        }
    }

    /// The unit inventory as the merge gate reads it: at `rev`, with the fingerprints the extractor gives there.
    fn real_units(root: &Path, rev: &str) -> Vec<UnitFp> {
        rooted(&GitTree {
            root: root.to_path_buf(),
            rev: rev.to_string(),
        })
        .expect("the inventory does not read")
        .1
    }

    /// The base of the range: the merge base of `main` and HEAD, which is what a landing is reviewed against.
    fn real_base(root: &Path) -> String {
        let main = g(root, &["rev-parse", "main"]);
        g(root, &["merge-base", &main, "HEAD"])
    }

    /// The sanity walk of the real range: the lower bound is the cutoff commit, every unit resolves against real history,
    /// and the authorship the walk finds is clash-free and names a model for every commit it credits (spec 2, 14, 30, 32).
    /// This is not spec R1, which is the errseq case further down.
    #[test]
    fn the_real_history_walk_starts_at_the_cutoff_and_names_every_author() {
        if !real_history_wanted() {
            eprintln!("skipped: LUCINA_CI_REAL_HISTORY is not set");
            return;
        }
        let root = real_root();
        let base = real_base(&root);
        let text = std::fs::read_to_string(root.join(CUTOFF)).expect("no cutoff file");
        let cut = parse_cutoff(&text).expect("the cutoff file does not parse");
        assert_eq!(
            cut.commit, HISTORY_LOWER_BOUND,
            "line 3 of the cutoff file is the lower bound the tool falls back to"
        );
        assert!(
            g(
                &root,
                &["cat-file", "-e", &format!("{}^{{commit}}", cut.commit)]
            )
            .is_empty(),
            "the lower bound exists as a commit object"
        );
        let units = real_units(&root, "HEAD");
        // trailers only: what the rule sees in a clone that has neither the ledger nor a correction file
        let res = authorship::ModelResolver::default();
        let t0 = std::time::Instant::now();
        let w = authorship::walk(&root, &base, &units, &cut, &BTreeMap::new(), &res, None).unwrap();
        let walked: Vec<&str> = units
            .iter()
            .filter(|u| u.fp.is_some() && tracked(&u.kind))
            .map(|u| u.unit.as_str())
            .collect();
        assert!(!w.lb_missing, "the lower bound is in this repository");
        assert!(
            w.notes.is_empty(),
            "the walk reports nothing to note: {:?}",
            w.notes
        );
        assert!(
            w.commits > 200,
            "the range LB..HEAD holds the real merges: {} commits",
            w.commits
        );
        for (name, a) in &w.units {
            assert!(
                walked.contains(&name.as_str()),
                "the walk reports only units the inventory tracks: {name}"
            );
            assert!(
                !a.commits.iter().any(|x| x.commit.is_empty()),
                "every authorship entry names its commit: {name}"
            );
        }
        assert!(
            w.authors("shipped/not-a-unit").commits.is_empty(),
            "an unknown unit has no authorship, and asking for it is not an error"
        );
        // the units real history actually moved, seen by the rule rather than by the fingerprint diff
        let seen: Vec<(&str, String, String)> = walked
            .iter()
            .map(|u| {
                (
                    *u,
                    models_of(&w, u, true).join(","),
                    models_of(&w, u, false).join(","),
                )
            })
            .filter(|(_, c, p)| !c.is_empty() || !p.is_empty())
            .collect();
        assert!(
            seen.len() >= 3,
            "the walk finds real authorship on real history: {seen:?}"
        );
        for (u, code, proof) in &seen {
            println!("  {u}: code [{code}] proof [{proof}]");
        }
        assert!(
            !w.units
                .values()
                .any(|a| a.commits.iter().any(|x| x.models.is_empty() && x.in_range)),
            "every commit in the range that the walk credits names a model"
        );
        // A model on both sides is what the gate refuses. This walk is READ STRICTER than the gate: no records and no
        // ledger, so a unit whose exempt past a validated `pre-rule` record covers still shows its older authorship here.
        // What has to hold over the real history is that nothing outside the cutoff list is on both sides.
        let clash: Vec<&str> = w
            .units
            .iter()
            .filter(|(_, a)| !clash_of(a).is_empty())
            .map(|(u, _)| u.as_str())
            .collect();
        for u in &clash {
            assert!(
                cut.units.iter().any(|(x, _, _)| x == u),
                "{u}: a model on both sides with no cutoff entry to cover it: {clash:?}"
            );
        }
        let audit = audit_report(&units, &w);
        println!(
            "  {} unit(s) with a model on both sides without the records, all of them cutoff-listed: {clash:?}",
            clash.len()
        );
        assert!(
            audit.contains("with a model on both sides"),
            "the audit report counts them: {audit}"
        );
        println!(
            "real walk: {} commits, {} blobs, {:.1} s, {} units with authorship, {seen:?}",
            w.commits,
            w.blobs,
            w.secs,
            seen.len()
        );
        let _ = t0;
    }

    /// Spec R4: the cost of the audit. Cold (no stream cache) against `main` is the case a merge gate pays; warm is what a
    /// developer sees; the lower bound itself is the worst base. The bound is the spec's 60 s target with room for a
    /// loaded machine, and the line printed is the one the owner reads.
    #[test]
    fn the_audit_of_the_real_history_stays_under_its_time_budget() {
        if !real_history_wanted() {
            eprintln!("skipped: LUCINA_CI_REAL_HISTORY is not set");
            return;
        }
        let root = real_root();
        let main = real_base(&root);
        let worst = g(&root, &["merge-base", HISTORY_LOWER_BOUND, "HEAD"]);
        let units = real_units(&root, "HEAD");
        let res = authorship::ModelResolver::default();
        let cut = parse_cutoff(&std::fs::read_to_string(root.join(CUTOFF)).expect("cutoff file"))
            .expect("cutoff parses");
        let cache = authorship::cache_path(&root);
        if let Some(p) = &cache {
            let _ = std::fs::remove_file(p);
        }
        let mut runs: Vec<(String, f64)> = Vec::new();
        for (what, base, use_cache) in [
            ("main, no cache", &main, false),
            ("main, warm", &main, true),
            ("lower bound, warm", &worst, true),
        ] {
            let t = std::time::Instant::now();
            let w = authorship::walk(
                &root,
                base,
                &units,
                &cut,
                &BTreeMap::new(),
                &res,
                if use_cache { cache.as_deref() } else { None },
            )
            .unwrap();
            assert!(w.commits > 200, "{what}: the range is the real history");
            runs.push((
                format!("{what}: {} commits, {} blobs", w.commits, w.blobs),
                t.elapsed().as_secs_f64(),
            ));
        }
        for (what, secs) in &runs {
            println!("R4: {what} in {secs:.1} s");
            assert!(
                *secs < 90.0,
                "{what} took {secs:.1} s, target is under 60 s"
            );
        }
        // a warm walk does not read the repository again
        let t = std::time::Instant::now();
        let warm = authorship::walk(
            &root,
            &main,
            &units,
            &cut,
            &BTreeMap::new(),
            &res,
            cache.as_deref(),
        )
        .unwrap();
        let warm_secs = t.elapsed().as_secs_f64();
        println!(
            "R4: warm rerun {} blobs read in {warm_secs:.1} s",
            warm.blobs
        );
    }

    /// The real cutoff list (not spec R3, which is the committed `--audit` expectation): a unit listed keeps its exempt past out of both sets: none of the commits the walk
    /// credits for it is an ancestor of the `started_before` the list names for it.
    #[test]
    fn the_real_cutoff_list_keeps_each_units_exempt_past_out_of_the_walk() {
        if !real_history_wanted() {
            eprintln!("skipped: LUCINA_CI_REAL_HISTORY is not set");
            return;
        }
        let root = real_root();
        let base = real_base(&root);
        let cut = parse_cutoff(&std::fs::read_to_string(root.join(CUTOFF)).expect("cutoff file"))
            .expect("cutoff parses");
        let units = real_units(&root, "HEAD");
        // trailers only: what the rule sees in a clone that has neither the ledger nor a correction file
        let res = authorship::ModelResolver::default();
        let pre: BTreeMap<String, String> = cut
            .units
            .iter()
            .map(|(u, c, _)| (u.clone(), c.clone()))
            .collect();
        let w = authorship::walk(&root, &base, &units, &cut, &pre, &res, None).unwrap();
        let mut checked = 0;
        for (unit, started) in &pre {
            let ua = w.authors(unit);
            if ua.commits.is_empty() {
                continue;
            }
            checked += 1;
            for a in &ua.commits {
                let out = std::process::Command::new("git")
                    .arg("merge-base")
                    .arg("--is-ancestor")
                    .arg(&a.commit)
                    .arg(started)
                    .current_dir(&root)
                    .output()
                    .expect("git merge-base");
                assert!(
                    !out.status.success(),
                    "{unit}: {} is in the exempt past of {started} yet the walk credited it",
                    a.commit
                );
            }
        }
        assert!(
            checked > 0,
            "the cutoff list names units this repository still tracks"
        );
        println!(
            "list: {checked} listed units have authorship past their exempt bound, none before it"
        );
    }

    /// A private checkout of a branch of the real repository, for a test whose range ends at a branch this session is not
    /// on: `walk` reads `LB..HEAD` of the checkout it is given, so the branch has to BE the `HEAD`. The objects are cloned
    /// once per test process with `--no-checkout` (hardlinks on the same filesystem, a second) under the target directory,
    /// which is build output and disposable, and every caller gets a detached worktree of its own, so tests that run in
    /// parallel do not share a `HEAD`.
    fn real_branch_head(tag: &str, rev: &str) -> PathBuf {
        static CLONE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
        let src = real_root();
        let dir = CLONE.get_or_init(|| {
            let d = src
                .join("target")
                .join(format!("lucina-roles-clone-{}", std::process::id()));
            let _ = fs::remove_dir_all(&d);
            if !d.join(".git").is_dir() {
                let o = Command::new("git")
                    .args(["clone", "--quiet", "--no-checkout"])
                    .arg(&src)
                    .arg(&d)
                    .output()
                    .expect("git clone");
                assert!(
                    o.status.success(),
                    "clone of {}: {}",
                    src.display(),
                    String::from_utf8_lossy(&o.stderr)
                );
            }
            d
        });
        let wt = dir.join(format!("wt-{tag}"));
        let _ = fs::remove_dir_all(&wt);
        g(
            dir,
            &[
                "worktree",
                "add",
                "--quiet",
                "--detach",
                &wt.to_string_lossy(),
                rev,
            ],
        );
        assert_eq!(
            g(&wt, &["rev-parse", "HEAD"]),
            g(dir, &["rev-parse", rev]),
            "the worktree of {rev} is at its tip"
        );
        wt
    }

    /// The models on both sides of a unit: the intersection `check` refuses as `one-model-wrote-code-and-proof` (roles 1.1).
    fn clash_of(ua: &authorship::UnitAuthors) -> Vec<String> {
        let code = authorship::code_authors(ua);
        authorship::proof_authors(ua)
            .keys()
            .filter(|m| code.contains_key(m.as_str()))
            .cloned()
            .collect()
    }

    /// The jsonl ledger of commits, as the gate sees it: the file named by `LAGENT_COMMIT_LEDGER`, `None` when unset.
    fn real_ledger() -> Option<PathBuf> {
        std::env::var(authorship::LEDGER_ENV)
            .ok()
            .map(PathBuf::from)
    }

    /// What the walk credits a unit by: (commit, code, proof) newest first, the order of its own history.
    fn unit_versions(ua: &authorship::UnitAuthors) -> Vec<(String, bool, bool)> {
        let mut v: Vec<(String, bool, bool)> = ua
            .commits
            .iter()
            .map(|x| (x.commit.clone(), x.code, x.proof))
            .collect();
        v.sort_by(|a, b| b.0.cmp(&a.0));
        v
    }

    /// The commits of a unit with the side they moved and the model that named them, as one line each.
    fn versions_of(ua: &authorship::UnitAuthors) -> Vec<String> {
        ua.commits
            .iter()
            .map(|x| {
                format!(
                    "{} {} {} {}",
                    authorship::short(&x.commit),
                    if x.code { "code" } else { "----" },
                    if x.proof { "proof" } else { "-----" },
                    x.models.join("+")
                )
            })
            .collect()
    }

    /// R1 (spec 6): the real errseq case read at the `errseq-verus` tip 10d3b01f1 - the port commit 63574703a of
    /// `kernel/lucina-errseq/src/lib.rs` over the code that was already there, the mask commit c7868d208 that follows the
    /// same code, and the harness commit 630ed93b0 that adds only verifier text. The range is asserted as it is, with the
    /// records and the ledger of the tip; the review's case in a range of its own is the second half, where the two
    /// attribution lines of the coordinator are what clears it (spec 8) and the trailers alone refuse it (spec 7).
    #[test]
    fn the_real_errseq_creation_passes_with_the_attribution_lines_and_refuses_with_trailers_only() {
        if !real_history_wanted() {
            eprintln!("skipped: LUCINA_CI_REAL_HISTORY is not set");
            return;
        }
        const UNIT: &str = "verus/errseq/rules";
        const PORT: &str = "63574703af95097effe33a9b19c10b2fc6897b62";
        const MASK: &str = "c7868d2087520dff83023a9b47e350be8fa67d9f";
        const HARNESS: &str = "630ed93b0909915ce374ff57e312f5827755e611";
        const TIP: &str = "10d3b01f11714794d14e7e0923bca06f80b119e1";
        let root = real_branch_head("r1", TIP);
        for c in [PORT, MASK, HARNESS] {
            if !commit_exists(&root, c) {
                println!(
                    "SKIPPED: R1 needs commit {}, this repository does not hold it",
                    authorship::short(c)
                );
                return;
            }
        }
        if !root.join(CUTOFF).is_file() || !root.join(LOG).is_file() {
            println!(
                "SKIPPED: R1 needs {CUTOFF} and {LOG} at {}",
                authorship::short(TIP)
            );
            return;
        }
        // The range a landing of that branch is reviewed against, in a checkout of the tip itself.
        let range = g(&root, &["merge-base", "origin/main", "HEAD"]);
        let units = real_units(&root, "HEAD");
        let cut = parse_cutoff(&fs::read_to_string(root.join(CUTOFF)).expect("cutoff"))
            .expect("cutoff parses");
        let w = authorship::walk(
            &root,
            &range,
            &units,
            &cut,
            &BTreeMap::new(),
            &authorship::ModelResolver::default(),
            None,
        )
        .unwrap();
        // The resolver is the default one on purpose: what the records of the tip do NOT say. The commit that ported
        // `lib.rs` was made by the Sonnet lane and carries an Opus trailer, and no record corrects it, so the trailer
        // stands and Opus ends up on both sides of the unit, which is the finding the review calls spec 7.
        let ua = w.authors(UNIT);
        let vs = unit_versions(ua);
        for c in [PORT, HARNESS] {
            assert!(
                vs.contains(&(c.to_string(), c != HARNESS, c != PORT)),
                "{UNIT}: {} is not credited as it should be: {:?}",
                authorship::short(c),
                versions_of(ua)
            );
        }
        assert!(
            !vs.iter().any(|x| x.0 == MASK),
            "{UNIT}: {} follows code that is the same after it, so the walk gives it no credit: {:?}",
            authorship::short(MASK),
            versions_of(ua)
        );
        let port = ua.commits.iter().find(|x| x.commit == PORT).expect("port");
        assert_eq!(
            port.models,
            vec![OPUS.to_string()],
            "the trailer of the port names Opus"
        );
        assert_eq!(
            port.source,
            authorship::Source::Trailer,
            "and nothing corrects it"
        );
        assert_eq!(
            clash_of(ua),
            vec![OPUS.to_string()],
            "the misattributed commit of the history is enough to put a model on both sides: {:?}",
            versions_of(ua)
        );
        let harness = ua
            .commits
            .iter()
            .find(|x| x.commit == HARNESS)
            .expect("harness");
        assert!(
            harness.proof && !harness.code,
            "the harness commit writes no code: its `verus_keep_ghost` text and its empty `verus!` shell are outside the \
             code stream (spec 1.1.3): {harness:?}"
        );
        println!(
            "R1 range {}..{}: code {:?}, proof {:?}{}",
            authorship::short(&range),
            authorship::short(TIP),
            models_of(&w, UNIT, true),
            models_of(&w, UNIT, false),
            if clash_of(ua).is_empty() {
                ", no model on both sides".to_string()
            } else {
                format!(": {} on both sides", clash_of(ua).join("+"))
            }
        );
        for line in versions_of(ua) {
            println!("R1 {line}");
        }
        // The same range with the coordinator's attribution file: the two lines name the Sonnet lane for the port and for
        // the follow-up that recompiled the same file, which is what the records of the tip do not say. An attribution
        // corrects a commit that was already on main at the base (roles 1.6), and both of these are, so the code side of
        // the unit becomes the builder alone and the creation over existing code passes (spec 8).
        let text = format!(
            "{PORT}\t{SON}\tthe Sonnet lane wrote the port, the trailer names Opus\n\
             {MASK}\t{SON}\tthe same lane made the clippy follow-up\n"
        );
        let res = authorship::ModelResolver::new(None, Some(&text));
        assert_eq!(res.attrib.len(), 2, "the two lines parse: {:?}", res.attrib);
        let fixed =
            authorship::walk(&root, &range, &units, &cut, &BTreeMap::new(), &res, None).unwrap();
        let ua = fixed.authors(UNIT);
        let port = ua.commits.iter().find(|x| x.commit == PORT).expect("port");
        assert_eq!(
            port.models,
            vec![SON.to_string()],
            "the line names the lane that wrote it"
        );
        assert_eq!(
            port.source,
            authorship::Source::Attribution,
            "and says where it came from"
        );
        assert!(
            port.code && !port.proof,
            "and the file it wrote is code alone: {port:?}"
        );
        assert_eq!(
            models_of(&fixed, UNIT, true),
            vec![SON.to_string()],
            "the code side is the builder alone: {:?}",
            versions_of(ua)
        );
        assert!(
            authorship::proof_authors(ua).contains_key(OPUS),
            "the statements are a strong prover's"
        );
        assert!(
            clash_of(ua).is_empty(),
            "with the two lines the creation over existing code passes: {:?}",
            versions_of(ua)
        );
        println!(
            "R1 with the two lines: code {:?}, proof {:?}, no model on both sides",
            models_of(&fixed, UNIT, true),
            models_of(&fixed, UNIT, false)
        );
        for line in versions_of(ua) {
            println!("R1 {line}");
        }
    }

    /// R2 (spec 6): the real slide case, `shipped/lucina-glass/slide_rules` over the `juno-perf-land` tip bf22e37b3, read
    /// with the records and the ledger of that tip: which commits the walk credits, which side each of them moved, which
    /// model named them and whether one model stands on both sides. The case is the one the review names: Opus staged the
    /// plain code (6383415da), Fable wrote the statement and the proof (52518f4686).
    #[test]
    fn the_real_glass_slide_rules_has_opus_on_the_code_side_and_fable_on_the_proof_side() {
        const UNIT: &str = "shipped/lucina-glass/slide_rules";
        const FABLE: &str = "claude-fable-5-2";
        const CODE: &str = "6383415da1dc9e837068c5bc2b834ee3574720ad";
        const PROOF: &str = "52518f468616dc4eac8d8bad168af9b07a7a5a9d";
        const TIP: &str = "bf22e37b3aab25510a1c2db9a153b456992dfee6";
        if !real_history_wanted() {
            eprintln!("skipped: LUCINA_CI_REAL_HISTORY is not set");
            return;
        }
        for c in [CODE, PROOF, TIP] {
            if !commit_exists(&real_root(), c) {
                println!(
                    "SKIPPED: R2 needs commit {}, this repository does not hold it",
                    authorship::short(c)
                );
                return;
            }
        }
        let root = real_branch_head("r2", TIP);
        if !root.join(CUTOFF).is_file() || !root.join(LOG).is_file() {
            println!(
                "SKIPPED: R2 needs {CUTOFF} and {LOG} at {}",
                authorship::short(TIP)
            );
            return;
        }
        let range = g(&root, &["merge-base", "origin/main", "HEAD"]);
        let units = real_units(&root, "HEAD");
        let cut = parse_cutoff(&fs::read_to_string(root.join(CUTOFF)).expect("cutoff"))
            .expect("cutoff parses");
        let w = authorship::walk(
            &root,
            &range,
            &units,
            &cut,
            &BTreeMap::new(),
            &authorship::ModelResolver::new(real_ledger().as_deref(), None),
            None,
        )
        .unwrap();
        let ua = w.authors(UNIT);
        assert!(
            !ua.commits.is_empty(),
            "{UNIT} has no authorship in {range}: the unit is missing from the walk"
        );
        let vs = unit_versions(ua);
        for c in [CODE, PROOF] {
            assert!(
                vs.contains(&(c.to_string(), c == CODE, c == PROOF)),
                "{UNIT}: {} is not credited as it should be: {:?}",
                authorship::short(c),
                versions_of(ua)
            );
        }
        println!(
            "R2 range {}..{}: code {:?}, proof {:?}{}",
            authorship::short(&range),
            authorship::short(TIP),
            models_of(&w, UNIT, true),
            models_of(&w, UNIT, false),
            if clash_of(ua).is_empty() {
                ", no model on both sides".to_string()
            } else {
                format!(": {} on both sides", clash_of(ua).join("+"))
            }
        );
        for line in versions_of(ua) {
            println!("R2 {line}");
        }
        assert!(
            models_of(&w, UNIT, true).contains(&OPUS.to_string()),
            "Opus shaped the code of {UNIT}: {:?}",
            models_of(&w, UNIT, true)
        );
        assert!(
            models_of(&w, UNIT, false).contains(&FABLE.to_string()),
            "Fable wrote the statement and proof of {UNIT}: {:?}",
            models_of(&w, UNIT, false)
        );
        assert!(
            clash_of(ua).is_empty(),
            "the two sides of {UNIT} are two models: {:?}",
            clash_of(ua)
        );
    }

    /// The window-manager pair the prover measured (`prover-recheck-5-report.md` section 4): `aeaa4f6fa` added the four
    /// ghost-gated `proof!` blocks of `rows_ok` to `services/lucina-surface-rules/src/rules.rs` as of `8463f5e08` and moved
    /// `exec_fp` with them (83f80067ef2599b7 into 1457c82c66d3ab8b), which read the prover's own commit as a code change.
    /// With the gate treated as part of the proof block the two revs read one value, d58576697414d3a9. The unit is not in
    /// the inventory yet, so the `Unit` here is the shape the coordinator will register.
    #[test]
    fn the_real_surface_proof_blocks_move_no_exec_fingerprint() {
        if !real_history_wanted() {
            eprintln!("skipped: LUCINA_CI_REAL_HISTORY is not set");
            return;
        }
        const BEFORE: &str = "8463f5e086abaaba720daf6588f80906db1a5894";
        const AFTER: &str = "aeaa4f6fa8ca7e2165ae85cd712726427c381c40";
        let root = real_root();
        for rev in [BEFORE, AFTER] {
            assert!(
                g(&root, &["cat-file", "-e", &format!("{rev}^{{commit}}")]).is_empty(),
                "{rev} is not in the checkout the test reads"
            );
        }
        let unit = Unit {
            name: "verus/juno-surface/rules".into(),
            group: "juno-surface".into(),
            kind: "verus".into(),
            target: "verification/juno-surface/rules.rs".into(),
            inputs: vec![],
        };
        let at = |rev: &str| -> Fp {
            let tree = GitTree {
                root: root.clone(),
                rev: rev.to_string(),
            };
            let (fps, errs) = fingerprints(&tree, std::slice::from_ref(&unit));
            assert!(
                errs.is_empty(),
                "{rev}: the extractor read problems: {errs:?}"
            );
            assert_eq!(fps.len(), 1, "{rev}: one unit in, one fingerprint out");
            fps[0].fp.clone().expect("a verus unit is tracked")
        };
        let (a, b) = (at(BEFORE), at(AFTER));
        assert_eq!(
            a.exec, b.exec,
            "the commit added gated proof blocks only: no executable token moved"
        );
        assert_ne!(a.proof, b.proof, "and the proof text is what changed");
        assert_ne!(a.stmt, b.stmt, "with the statements of R-1..R-6");
        println!(
            "surface exec_fp {} at both revs; proof_fp {} into {}",
            a.exec, a.proof, b.proof
        );
    }
}
