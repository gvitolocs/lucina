//! The amendment protocol of the spec freeze (docs/ci/SPEC-AMENDMENTS.md).
//!
//! A changed or removed specification statement (src/spec.rs) is acceptable only through an amendment: one line of the append-only
//! log `docs/ci/spec-amendments.tsv`, named in the merge allow reason as `amend:<id>`. Pure logic only; the git and environment
//! plumbing is in `spec::cmd`.
//!
//! Log line (12 tab-separated fields, none empty, no tabs or newlines inside):
//!   id, date, workspace, kind, keys, evidence_kind, evidence_ref, author_model, reviewer_model, reviewer_family, verdict, reason
//! Blank lines and lines starting with `#` are comments. `keys` is a comma-separated list of statement keys exactly as
//! `lucina-ci spec --check` prints them (so a key may not contain a comma), or `*mutants*` for kind `mutant`.

use std::collections::BTreeSet;

pub const LOG: &str = "docs/ci/spec-amendments.tsv";
pub const REQUIRED_ENV: &str = "LUCINA_SPEC_AMEND_REQUIRED";
pub const LEGACY_MARK: &str = "LEGACY ALLOW (no amendment)";

pub const KINDS: &[&str] = &["relax", "change", "remove", "mutant"];
pub const EVIDENCE: &[&str] = &[
    "failing-proof",
    "counterexample",
    "model-trace",
    "failing-test",
    "original-differs",
];
pub const VERDICTS: &[&str] = &["accepted", "rejected", "escalated"];
pub const FAMILIES: &[&str] = &[
    "claude", "gemini", "gpt", "qwen", "mistral", "deepseek", "glm", "other",
];
const FIELDS: usize = 12;
const MUTANT_ALL: &[&str] = &["*mutants*", "*"];

/// The model family of a model name, by substring of the lower-cased name (first match wins). Unknown names are `other`; two
/// `other` models count as the SAME family, because nothing shows they differ.
pub fn family(model: &str) -> &'static str {
    let m = model.to_ascii_lowercase();
    let has = |ws: &[&str]| ws.iter().any(|w| m.contains(w));
    if has(&["claude", "sonnet", "opus", "haiku", "fable"]) {
        "claude"
    } else if has(&["gemini", "gemma"]) {
        "gemini"
    } else if has(&["gpt", "codex", "openai"]) || m.starts_with("o1") || m.starts_with("o3") {
        "gpt"
    } else if has(&["qwen"]) {
        "qwen"
    } else if has(&["mistral", "codestral", "devstral", "vibe"]) {
        "mistral"
    } else if has(&["deepseek"]) {
        "deepseek"
    } else if has(&["glm", "zcode", "z.ai"]) {
        "glm"
    } else {
        "other"
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Amendment {
    pub id: String,
    pub date: String,
    pub workspace: String,
    pub kind: String,
    pub keys: Vec<String>,
    pub evidence_kind: String,
    pub evidence_ref: String,
    pub author_model: String,
    pub reviewer_model: String,
    pub reviewer_family: String,
    pub verdict: String,
    pub reason: String,
}

/// One data line of the log: well formed or not. A malformed line does not block the others; it is refused only when named.
#[derive(Clone, Debug)]
pub struct Entry {
    pub id: String,
    pub line: usize,
    pub res: Result<Amendment, String>,
}

fn id_ok(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn date_ok(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b.iter().enumerate().all(|(i, c)| match i {
            4 | 7 => *c == b'-',
            _ => c.is_ascii_digit(),
        })
}

fn parse_line(f: &[&str]) -> Result<Amendment, String> {
    if f.len() != FIELDS {
        return Err(format!("{} fields, need {FIELDS}", f.len()));
    }
    const NAMES: [&str; FIELDS] = [
        "id",
        "date",
        "workspace",
        "kind",
        "keys",
        "evidence_kind",
        "evidence_ref",
        "author_model",
        "reviewer_model",
        "reviewer_family",
        "verdict",
        "reason",
    ];
    for (i, v) in f.iter().enumerate() {
        if v.trim().is_empty() {
            return Err(format!("field {} is empty", NAMES[i]));
        }
    }
    if !id_ok(f[0]) {
        return Err("id may hold only letters, digits, '.', '_' and '-'".into());
    }
    if !date_ok(f[1]) {
        return Err("date must be YYYY-MM-DD".into());
    }
    let one_of = |v: &str, set: &[&str], n: &str| {
        if set.contains(&v) {
            Ok(())
        } else {
            Err(format!("{n} {v:?} is not one of {}", set.join("|")))
        }
    };
    one_of(f[3], KINDS, "kind")?;
    one_of(f[5], EVIDENCE, "evidence_kind")?;
    one_of(f[9], FAMILIES, "reviewer_family")?;
    one_of(f[10], VERDICTS, "verdict")?;
    let keys: Vec<String> = f[4]
        .split(',')
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
        .collect();
    if keys.is_empty() {
        return Err("keys is empty".into());
    }
    if f[3] != "mutant" && keys.iter().any(|k| MUTANT_ALL.contains(&k.as_str())) {
        return Err("`*mutants*` is only for kind mutant".into());
    }
    Ok(Amendment {
        id: f[0].into(),
        date: f[1].into(),
        workspace: f[2].into(),
        kind: f[3].into(),
        keys,
        evidence_kind: f[5].into(),
        evidence_ref: f[6].into(),
        author_model: f[7].into(),
        reviewer_model: f[8].into(),
        reviewer_family: f[9].into(),
        verdict: f[10].into(),
        reason: f[11].into(),
    })
}

pub fn parse_log(text: &str) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for (i, l) in text.lines().enumerate() {
        if l.trim().is_empty() || l.starts_with('#') {
            continue;
        }
        let f: Vec<&str> = l.split('\t').collect();
        let id = f[0].trim().to_string();
        let mut res = parse_line(&f);
        if res.is_ok() && !seen.insert(id.clone()) {
            res = Err(format!("duplicate id {id}: the first line with it counts"));
        }
        out.push(Entry {
            id,
            line: i + 1,
            res,
        });
    }
    out
}

/// The log may only grow: every line of the base file stays, byte-identical and in place; new lines come after.
pub fn append_only(base: &str, work: &str) -> Result<(), String> {
    let b: Vec<&str> = base.lines().collect();
    let w: Vec<&str> = work.lines().collect();
    if w.len() < b.len() {
        return Err(format!(
            "{LOG} has {} lines, the base commit's has {}: lines were removed",
            w.len(),
            b.len()
        ));
    }
    for (i, l) in b.iter().enumerate() {
        if *l != w[i] {
            return Err(format!(
                "{LOG} line {} differs from the base commit's (the log is append-only)",
                i + 1
            ));
        }
    }
    Ok(())
}

/// The amendment ids a merge allow reason names: every `amend:<id>` token.
pub fn named_ids(reason: &str) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    for tok in reason.split(|c: char| c.is_whitespace() || c == ',' || c == ';') {
        if let Some(id) = tok.strip_prefix("amend:") {
            let id = id.trim_end_matches(['.', ')', ']', '"', '\'']);
            if !id.is_empty() && !v.contains(&id.to_string()) {
                v.push(id.to_string());
            }
        }
    }
    v
}

/// An amendment that authorized a change, for the banner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Used {
    pub id: String,
    pub evidence_kind: String,
    pub reviewer_model: String,
}

/// Is amendment `a` valid as an authorization at all (before it is matched to keys)?
fn valid(a: &Amendment) -> Result<(), String> {
    if a.verdict != "accepted" {
        return Err(format!(
            "verdict is {} (only accepted authorizes anything)",
            a.verdict
        ));
    }
    let author = family(&a.author_model);
    if family(&a.reviewer_model) != a.reviewer_family {
        return Err(format!(
            "reviewer_family {} does not match reviewer_model {} ({})",
            a.reviewer_family,
            a.reviewer_model,
            family(&a.reviewer_model)
        ));
    }
    if a.reviewer_family == author {
        return Err(format!(
            "reviewer family {} is the author's family (author {} is {author})",
            a.reviewer_family, a.author_model
        ));
    }
    if a.reviewer_model.eq_ignore_ascii_case(&a.author_model) {
        return Err("reviewer_model equals author_model".into());
    }
    Ok(())
}

fn covers(a: &Amendment, key: &str, is_mutant: bool) -> bool {
    if is_mutant {
        if a.kind == "mutant" {
            a.keys
                .iter()
                .any(|k| MUTANT_ALL.contains(&k.as_str()) || k == key)
        } else {
            a.keys.iter().any(|k| k == key)
        }
    } else {
        a.kind != "mutant" && a.keys.iter().any(|k| k == key)
    }
}

/// Every changed or removed statement (`(key, is_mutant)`) must be covered by a named, valid, accepted amendment of the log.
/// Returns the amendments used, or every reason for refusal.
pub fn authorize(
    changed: &[(String, bool)],
    ids: &[String],
    log: &[Entry],
) -> Result<Vec<Used>, Vec<String>> {
    let mut problems: Vec<String> = Vec::new();
    let mut good: Vec<&Amendment> = Vec::new();
    for id in ids {
        match log.iter().find(|e| &e.id == id) {
            None => problems.push(format!("amend:{id}: no such line in {LOG}")),
            Some(e) => match &e.res {
                Err(m) => problems.push(format!(
                    "amend:{id}: {LOG} line {} is malformed: {m}",
                    e.line
                )),
                Ok(a) => match valid(a) {
                    Err(m) => problems.push(format!("amend:{id}: {m}")),
                    Ok(()) => good.push(a),
                },
            },
        }
    }
    for (key, is_mutant) in changed {
        if !good.iter().any(|a| covers(a, key, *is_mutant)) {
            problems.push(format!(
                "{key}: not covered by any accepted amendment named in the allow reason"
            ));
        }
    }
    if !problems.is_empty() {
        return Err(problems);
    }
    let mut used: Vec<Used> = Vec::new();
    for a in good {
        if changed.iter().any(|(k, m)| covers(a, k, *m)) {
            used.push(Used {
                id: a.id.clone(),
                evidence_kind: a.evidence_kind.clone(),
                reviewer_model: a.reviewer_model.clone(),
            });
        }
    }
    Ok(used)
}

pub fn template() -> String {
    format!(
        "# Append ONE line to {LOG} (tab-separated, 12 fields, no field empty):\n\
# id\tdate\tworkspace\tkind\tkeys\tevidence_kind\tevidence_ref\tauthor_model\treviewer_model\treviewer_family\tverdict\treason\n\
# kind: {}   evidence_kind: {}   verdict: {}   reviewer_family: {}\n\
# then merge with:  lagent merge --allow spec-change=\"amend:<id> <why>\"\n\
AMD-{}-01\t<YYYY-MM-DD>\t<branch or workspace>\trelax\t<key>,<key>\t<evidence_kind>\t<path, or command and output file or commit>\t<author model>\t<reviewer model>\t<reviewer family>\taccepted\t<why this is necessary, not weaker, and small>\n",
        KINDS.join("|"),
        EVIDENCE.join("|"),
        VERDICTS.join("|"),
        FAMILIES.join("|"),
        "<unit>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(
        id: &str,
        kind: &str,
        keys: &str,
        author: &str,
        rev: &str,
        fam: &str,
        verdict: &str,
    ) -> String {
        format!(
            "{id}\t2026-11-11\twt/x\t{kind}\t{keys}\tfailing-proof\tdocs/x.out\t{author}\t{rev}\t{fam}\t{verdict}\ttight"
        )
    }
    fn log(lines: &[String]) -> Vec<Entry> {
        parse_log(&lines.join("\n"))
    }
    fn ch(keys: &[(&str, bool)]) -> Vec<(String, bool)> {
        keys.iter().map(|(k, m)| (k.to_string(), *m)).collect()
    }
    fn ids(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn families_from_model_names() {
        for (m, f) in [
            ("claude-sonnet-5-5", "claude"),
            ("Opus 5.5", "claude"),
            ("gpt-6.1-sol", "gpt"),
            ("codex", "gpt"),
            ("gemini-3-pro", "gemini"),
            ("qwen3.8-27b", "qwen"),
            ("mistral-large", "mistral"),
            ("deepseek-v4.1-flash", "deepseek"),
            ("GLM-5.3-Flash", "glm"),
            ("grok-4.7", "other"),
        ] {
            assert_eq!(family(m), f, "{m}");
        }
    }

    #[test]
    fn accepted_amendment_covering_all_keys_passes() {
        let l = log(&[line(
            "A1",
            "change",
            "k1,k2",
            "claude-sonnet-5-5",
            "gpt-6.1-sol",
            "gpt",
            "accepted",
        )]);
        let u = authorize(&ch(&[("k1", false), ("k2", false)]), &ids(&["A1"]), &l).unwrap();
        assert_eq!(u.len(), 1);
        assert_eq!(u[0].id, "A1");
        assert_eq!(u[0].evidence_kind, "failing-proof");
        assert_eq!(u[0].reviewer_model, "gpt-6.1-sol");
    }

    #[test]
    fn two_amendments_may_share_the_cover() {
        let l = log(&[
            line("A1", "change", "k1", "claude-x", "gpt-x", "gpt", "accepted"),
            line(
                "A2", "remove", "k2", "claude-x", "gemini-x", "gemini", "accepted",
            ),
        ]);
        let u = authorize(
            &ch(&[("k1", false), ("k2", false)]),
            &ids(&["A1", "A2"]),
            &l,
        )
        .unwrap();
        assert_eq!(u.len(), 2);
    }

    #[test]
    fn missing_key_is_refused() {
        let l = log(&[line(
            "A1", "change", "k1", "claude-x", "gpt-x", "gpt", "accepted",
        )]);
        let e = authorize(&ch(&[("k1", false), ("k2", false)]), &ids(&["A1"]), &l).unwrap_err();
        assert!(e.iter().any(|m| m.starts_with("k2:")), "{e:?}");
    }

    #[test]
    fn unknown_id_is_refused() {
        let e = authorize(&ch(&[("k1", false)]), &ids(&["NOPE"]), &[]).unwrap_err();
        assert!(e[0].contains("no such line"));
    }

    #[test]
    fn same_family_and_same_model_are_refused() {
        let l = log(&[
            line(
                "S1",
                "change",
                "k1",
                "claude-sonnet-5-5",
                "claude-opus-5-5",
                "claude",
                "accepted",
            ),
            line(
                "S2", "change", "k1", "grok-a", "grok-b", "other", "accepted",
            ),
            line(
                "S3", "change", "k1", "claude-x", "gpt-x", "claude", "accepted",
            ),
        ]);
        for id in ["S1", "S2", "S3"] {
            let e = authorize(&ch(&[("k1", false)]), &ids(&[id]), &l).unwrap_err();
            assert!(e[0].contains(id), "{e:?}");
        }
        assert!(
            authorize(&ch(&[("k1", false)]), &ids(&["S1"]), &l).unwrap_err()[0]
                .contains("author's family")
        );
        // S3: the stated family disagrees with the model name
        assert!(
            authorize(&ch(&[("k1", false)]), &ids(&["S3"]), &l).unwrap_err()[0]
                .contains("does not match")
        );
    }

    #[test]
    fn rejected_and_escalated_never_authorize() {
        let l = log(&[
            line("R1", "change", "k1", "claude-x", "gpt-x", "gpt", "rejected"),
            line(
                "E1",
                "change",
                "k1",
                "claude-x",
                "gpt-x",
                "gpt",
                "escalated",
            ),
        ]);
        for id in ["R1", "E1"] {
            let e = authorize(&ch(&[("k1", false)]), &ids(&[id]), &l).unwrap_err();
            assert!(e[0].contains("only accepted"), "{e:?}");
        }
    }

    #[test]
    fn mutant_kind_allows_only_mutant_keys() {
        let l = log(&[line(
            "M1",
            "mutant",
            "*mutants*",
            "claude-x",
            "gpt-x",
            "gpt",
            "accepted",
        )]);
        assert!(authorize(&ch(&[("m1", true), ("m2", true)]), &ids(&["M1"]), &l).is_ok());
        let e = authorize(&ch(&[("m1", true), ("spec1", false)]), &ids(&["M1"]), &l).unwrap_err();
        assert!(e.iter().any(|m| m.starts_with("spec1:")), "{e:?}");
        // a mutant-kind amendment listing a spec key does not cover it either
        let l2 = log(&[line(
            "M2", "mutant", "spec1", "claude-x", "gpt-x", "gpt", "accepted",
        )]);
        assert!(authorize(&ch(&[("spec1", false)]), &ids(&["M2"]), &l2).is_err());
        // `*mutants*` in a non-mutant kind is a malformed line
        let l3 = log(&[line(
            "M3",
            "relax",
            "*mutants*",
            "claude-x",
            "gpt-x",
            "gpt",
            "accepted",
        )]);
        assert!(l3[0].res.is_err());
        assert!(authorize(&ch(&[("m1", true)]), &ids(&["M3"]), &l3).is_err());
    }

    #[test]
    fn malformed_lines_only_matter_when_named() {
        let l = parse_log("# c\n\nBAD\tonly\ttwo\n");
        assert_eq!(l.len(), 1);
        assert!(l[0].res.is_err());
        let good = line("G1", "change", "k1", "claude-x", "gpt-x", "gpt", "accepted");
        let l = parse_log(&format!("BAD\ta\n{good}\n"));
        assert!(authorize(&ch(&[("k1", false)]), &ids(&["G1"]), &l).is_ok());
        // empty field, bad date, duplicate id
        assert!(
            parse_log(&good.replace("failing-proof\tdocs/x.out", "failing-proof\t"))[0]
                .res
                .is_err()
        );
        assert!(
            parse_log(&good.replace("2026-11-11", "11/11/2026"))[0]
                .res
                .is_err()
        );
        let dup = parse_log(&format!("{good}\n{good}\n"));
        assert!(dup[0].res.is_ok() && dup[1].res.is_err());
    }

    #[test]
    fn append_only_rules() {
        let a = "# h\nL1\nL2\n";
        assert!(append_only(a, a).is_ok());
        assert!(append_only(a, "# h\nL1\nL2\nL3\n").is_ok());
        assert!(append_only("", "L1\n").is_ok());
        assert!(append_only(a, "# h\nL1\n").unwrap_err().contains("removed"));
        assert!(
            append_only(a, "# h\nL1x\nL2\n")
                .unwrap_err()
                .contains("line 2")
        );
        assert!(
            append_only(a, "L0\n# h\nL1\nL2\n").is_err(),
            "inserting before is not appending"
        );
    }

    #[test]
    fn reason_names_amendments() {
        assert_eq!(
            named_ids("amend:A1 and amend:B-2, amend:A1."),
            ["A1", "B-2"]
        );
        assert!(named_ids("tightened the bound").is_empty());
        assert!(named_ids("amend:").is_empty());
    }

    #[test]
    fn template_parses_as_a_line_shape() {
        let t = template();
        let data = t.lines().find(|l| !l.starts_with('#')).unwrap();
        assert_eq!(data.split('\t').count(), FIELDS);
    }
}
