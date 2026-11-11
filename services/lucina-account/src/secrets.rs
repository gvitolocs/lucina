//! The paste-and-OK card's data and the composer's guard (`web/src/lib/secrets.ts`, `components/SecretCard.tsx`).
//!
//! A value is a [`Secret`]: no `Debug`, wiped on drop, and it can only become the body of `POST /v1/secrets`. Nothing takes a
//! `Secret` and sends it through chat. The composer runs [`detect`] before a message gets a bubble, so a pasted key is caught
//! before it reaches the timeline.
use std::sync::OnceLock;

use regex::Regex;
use serde_json::{json, Value};
use zeroize::Zeroize;

pub const COMMAND: &str = "/secret";
pub const MAX_VALUE: usize = 65_536;

/// A secret value. Its contents leave only through [`SaveSecret`].
pub struct Secret(String);

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Secret({} characters)", self.0.chars().count())
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl Secret {
    /// From what was pasted: a trailing newline often comes along from a file and is not part of it. Wipes what it was given.
    pub fn new(mut pasted: String) -> Secret {
        let value = pasted.trim_end_matches(['\r', '\n']).to_owned();
        pasted.zeroize();
        Secret(value)
    }

    pub fn is_valid(&self) -> bool {
        !self.0.is_empty() && self.0.len() <= MAX_VALUE
    }

    /// A hint that names a value without showing it: `sk-…, 51`.
    pub fn hint(&self) -> String {
        let n = self.0.chars().count();
        let head: String = self.0.chars().take(3).collect();
        format!("{head}…, {n}")
    }

    /// Characters, for the masked field.
    pub fn len(&self) -> usize {
        self.0.chars().count()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// `POST /v1/secrets`.
#[derive(Debug)]
pub struct SaveSecret {
    pub name: String,
    pub project: String,
    pub environment: String,
    pub path: String,
    pub overwrite: bool,
    pub value: Secret,
    /// The queue entry this save answers.
    pub request: Option<String>,
}

impl SaveSecret {
    /// The bytes of the request. The caller wipes them after sending.
    pub(crate) fn body(&self) -> Vec<u8> {
        let mut body = json!({
            "name": self.name,
            "project": self.project,
            "environment": self.environment,
            "path": self.path,
            "overwrite": self.overwrite,
            "createFolder": self.path != "/",
            "value": self.value.0,
            "confirmed": true,
        });
        if let Some(r) = &self.request {
            body["request"] = r.as_str().into();
        }
        let bytes = serde_json::to_vec(&body).expect("serializes");
        if let Some(Value::String(v)) = body.get_mut("value") {
            v.zeroize();
        }
        bytes
    }
}

fn s(v: &Value) -> String {
    v.as_str().unwrap_or("").to_owned()
}

/// `GET /v1/secrets/requests`: names, never values.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SecretQueue {
    pub requests: Vec<SecretRequest>,
    pub active: Option<String>,
    pub queued: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SecretRequest {
    pub id: String,
    pub name: String,
    pub project: String,
    pub environment: String,
    pub path: String,
    pub reason: String,
    pub who: String,
    /// `queued`, `active`, `saved`, `dismissed` or `expired`.
    pub status: String,
    pub fingerprint: Option<String>,
    pub action: Option<String>,
}

impl SecretQueue {
    pub fn parse(v: &Value) -> SecretQueue {
        SecretQueue {
            requests: v["requests"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|r| {
                            let id = r["id"].as_str().filter(|i| !i.is_empty())?.to_owned();
                            Some(SecretRequest {
                                id,
                                name: s(&r["name"]),
                                project: s(&r["project"]),
                                environment: s(&r["environment"]),
                                path: s(&r["path"]),
                                reason: s(&r["reason"]),
                                who: s(&r["who"]),
                                status: s(&r["status"]),
                                fingerprint: r["fingerprint"].as_str().map(str::to_owned),
                                action: r["action"].as_str().map(str::to_owned),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default(),
            active: v["active"].as_str().map(str::to_owned),
            queued: v["queued"].as_u64().unwrap_or(0),
        }
    }

    pub fn find(&self, id: &str) -> Option<&SecretRequest> {
        self.requests.iter().find(|r| r.id == id)
    }
}

/// `GET /v1/secrets/catalog`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub slug: Option<String>,
    pub alias: Option<String>,
    pub environments: Vec<String>,
}

impl Project {
    pub fn parse(p: &Value) -> Option<Project> {
        let id = p["id"].as_str()?.to_owned();
        Some(Project {
            name: p["name"].as_str().unwrap_or(&id).to_owned(),
            id,
            slug: p["slug"].as_str().map(str::to_owned),
            alias: p["alias"].as_str().map(str::to_owned),
            environments: p["environments"]
                .as_array()
                .map(|e| e.iter().filter_map(|x| x["slug"].as_str().map(str::to_owned)).collect())
                .unwrap_or_default(),
        })
    }

    pub fn parse_list(v: &Value) -> Vec<Project> {
        let mut out: Vec<Project> = v["projects"].as_array().map(|a| a.iter().filter_map(Project::parse).collect()).unwrap_or_default();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    fn keys(&self) -> Vec<String> {
        [self.alias.as_deref(), self.slug.as_deref(), Some(self.name.as_str()), self.name.split_whitespace().next()]
            .into_iter()
            .flatten()
            .map(str::to_lowercase)
            .collect()
    }

    /// `prod` if it has one, else the first (`pick`).
    pub fn default_environment(&self) -> Option<&str> {
        self.environments.iter().find(|e| *e == "prod").or(self.environments.first()).map(String::as_str)
    }
}

/// A project by id, or by alias, slug or name in any case: what a card's `project` names. A card that names one the catalog
/// does not have is never saved elsewhere.
pub fn find_project<'a>(reference: &str, projects: &'a [Project]) -> Option<&'a Project> {
    let key = reference.to_lowercase();
    projects.iter().find(|p| p.id == reference).or_else(|| projects.iter().find(|p| p.keys().contains(&key)))
}

/// `NAME_RE`: `[A-Za-z0-9_.@-]{1,256}`.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 256 && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'@' | b'-'))
}

/// `GET /v1/secrets/names`: what a folder holds. Names and dates, never a value.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Folder {
    pub folders: Vec<String>,
    pub secrets: Vec<SecretName>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SecretName {
    pub key: String,
    pub updated_at: Option<String>,
}

impl Folder {
    pub fn parse(v: &Value) -> Folder {
        Folder {
            folders: v["folders"].as_array().map(|a| a.iter().filter_map(|f| f.as_str().map(str::to_owned)).collect()).unwrap_or_default(),
            secrets: v["secrets"]
                .as_array()
                .map(|a| a.iter().filter_map(|s| Some(SecretName { key: s["key"].as_str()?.to_owned(), updated_at: s["updated_at"].as_str().map(str::to_owned) })).collect())
                .unwrap_or_default(),
        }
    }
}

/// `joinFolder`.
pub fn join_folder(parent: &str, child: &str) -> String {
    if parent == "/" { format!("/{child}") } else { format!("{parent}/{child}") }
}

/// `parentFolder`.
pub fn parent_folder(path: &str) -> String {
    match path.rfind('/') {
        Some(i) if i > 0 => path[..i].to_owned(),
        _ => "/".to_owned(),
    }
}

/// `/`, `/a/b`, `a/b/` -> `/a/b`; `None` when a segment is not allowed.
pub fn normalize_folder(path: &str) -> Option<String> {
    let t = path.trim().trim_matches('/');
    if t.is_empty() {
        return Some("/".to_owned());
    }
    for seg in t.split('/') {
        let ok = !seg.is_empty()
            && seg.len() <= 128
            && seg != "."
            && seg != ".."
            && seg.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'@' | b'-'));
        if !ok {
            return None;
        }
    }
    Some(format!("/{t}"))
}

/// `/secret` in any case, then whitespace or the end: never a chat message.
pub fn is_command(text: &str) -> bool {
    let t = text.trim_start();
    let Some(head) = t.get(..7) else { return false };
    head.eq_ignore_ascii_case(COMMAND) && t[7..].chars().next().is_none_or(char::is_whitespace)
}

/// `POST /v1/secrets`'s answer: names and a fingerprint.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SecretReceipt {
    pub key: String,
    pub action: String,
    pub fingerprint: String,
    pub next_item_id: Option<String>,
}

impl SecretReceipt {
    pub fn parse(v: &Value) -> SecretReceipt {
        SecretReceipt {
            key: s(&v["key"]),
            action: s(&v["action"]),
            fingerprint: s(&v["fingerprint"]),
            next_item_id: v["nextItemId"].as_str().map(str::to_owned),
        }
    }
}

// ---------------------------------------------------------------- detector

/// Token shapes, the same list as Lucina's redactor (lucina-secrets redact.rs), through `web/src/lib/secrets.ts`.
const SHAPES: &[&str] = &[
    r"-----BEGIN [A-Z0-9 ]{1,40}-----(?s:.)*?-----END [A-Z0-9 ]{1,40}-----",
    r"-----BEGIN [A-Z0-9 ]{0,40}PRIVATE KEY-----(?:[A-Za-z0-9+/=\s]|\\[nr])*",
    r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}",
    r"\bsk-[A-Za-z0-9_-]{16,}",
    r"\b(?:sk|rk|pk)_(?:live|test)_[A-Za-z0-9]{16,}",
    r"\bgh[pousr]_[A-Za-z0-9]{20,}",
    r"\bgithub_pat_[A-Za-z0-9_]{20,}",
    r"\bglpat-[A-Za-z0-9_-]{20,}",
    r"\b(?:mcp|sec|svc)_[A-Za-z0-9_-]{16,}",
    r"\bxox[abposr]-[A-Za-z0-9-]{10,}",
    r"\bxapp-[0-9]-[A-Za-z0-9-]{10,}",
    r"\bAKIA[0-9A-Z]{16}\b",
    r"\bAIza[0-9A-Za-z_-]{35}",
    r"\b\d{8,10}:AA[A-Za-z0-9_-]{30,}",
    r"\bst\.[A-Za-z0-9-]{8,}\.[a-f0-9]{16,}\.[a-f0-9]{16,}",
    r"\bhf_[A-Za-z0-9]{30,}",
    r"\bnpm_[A-Za-z0-9]{36}\b",
    r"\bpypi-AgE[A-Za-z0-9_-]{50,}",
    r"\bSG\.[A-Za-z0-9_-]{16,}\.[A-Za-z0-9_-]{16,}",
    r"\bre_[A-Za-z0-9]{8}_[A-Za-z0-9]{16,}",
    r"\bdop_v1_[a-f0-9]{64}\b",
    r"\bAGE-SECRET-KEY-1[0-9A-Z]{58}\b",
    r"\bya29\.[A-Za-z0-9_-]{20,}",
    r"\b1//0[A-Za-z0-9_-]{30,}",
    r"\b(?:glsa|sntrys|lin_api)_[A-Za-z0-9_=-]{20,}",
    r"\bSK[0-9a-f]{32}\b",
    r"\b[MNO][A-Za-z0-9_-]{23,27}\.[A-Za-z0-9_-]{6}\.[A-Za-z0-9_-]{27,40}",
];

/// `password=…`, `"token": "…"`: group 1 is the value.
const PAIR: &str = r#"(?i)\b(?:password|passwd|pwd|passphrase|secret|client[_-]?secret|api[_-]?key|apikey|access[_-]?token|auth[_-]?token|token)["']?\s*[:=]\s*["']?([^\s"'&,;\[\]{}/][^\s"'&,;\[\]{}]{7,})"#;
const BEARER: &str = r"(?i)\b(?:bearer|basic)\s+([A-Za-z0-9._~+/-]{16,}=*)";
const ENTROPY: &str = r"[A-Za-z0-9+/_-]{32,}={0,2}";

struct Patterns {
    shapes: Vec<Regex>,
    pair: Regex,
    bearer: Regex,
    entropy: Regex,
}

fn patterns() -> &'static Patterns {
    static P: OnceLock<Patterns> = OnceLock::new();
    P.get_or_init(|| Patterns {
        shapes: SHAPES.iter().map(|s| Regex::new(s).expect("a shape compiles")).collect(),
        pair: Regex::new(PAIR).expect("compiles"),
        bearer: Regex::new(BEARER).expect("compiles"),
        entropy: Regex::new(ENTROPY).expect("compiles"),
    })
}

fn shannon(s: &str) -> f64 {
    let mut counts = std::collections::HashMap::new();
    let mut n = 0.0;
    for c in s.chars() {
        *counts.entry(c).or_insert(0.0) += 1.0;
        n += 1.0;
    }
    counts.values().map(|c: &f64| -(c / n) * (c / n).log2()).sum()
}

/// 32+ characters with lower, upper case and 3+ digits, high entropy.
pub fn looks_random(s: &str) -> bool {
    let core = s.trim_end_matches('=');
    core.len() >= 32
        && core.bytes().any(|b| b.is_ascii_lowercase())
        && core.bytes().any(|b| b.is_ascii_uppercase())
        && core.bytes().filter(u8::is_ascii_digit).count() >= 3
        && shannon(core) >= 4.2
}

/// Byte spans of `text` that look like secrets, merged where they overlap.
pub fn detect(text: &str) -> Vec<(usize, usize)> {
    let p = patterns();
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut add = |start: usize, end: usize| {
        if start > 0 && text.as_bytes()[start - 1] == b'[' {
            return;
        }
        if text[start..end].starts_with("secret://") {
            return;
        }
        spans.push((start, end));
    };
    for re in &p.shapes {
        for m in re.find_iter(text) {
            add(m.start(), m.end());
        }
    }
    for re in [&p.pair, &p.bearer] {
        for c in re.captures_iter(text) {
            let v = c.get(1).expect("the group is not optional");
            if !v.as_str().starts_with("secret://") {
                add(v.start(), v.end());
            }
        }
    }
    for m in p.entropy.find_iter(text) {
        let t = m.as_str();
        if text[..m.start()].ends_with("gdrive:") && (25..=60).contains(&t.len()) && t.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-') {
            continue;
        }
        let base64ish = t.contains('+') || t.ends_with('=');
        if !base64ish && t.matches('/').count() >= 2 {
            let mut off = m.start();
            for seg in t.split('/') {
                if looks_random(seg) {
                    add(off, off + seg.len());
                }
                off += seg.len() + 1;
            }
        } else if looks_random(t) {
            add(m.start(), m.end());
        }
    }
    spans.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
    let mut out: Vec<(usize, usize)> = Vec::new();
    for d in spans {
        match out.last_mut() {
            Some(last) if d.0 < last.1 => last.1 = last.1.max(d.1),
            _ => out.push(d),
        }
    }
    out
}

/// The text with every span replaced by `[secret:token]`.
pub fn mask(text: &str, spans: &[(usize, usize)]) -> String {
    let mut out = String::new();
    let mut last = 0;
    for &(start, end) in spans {
        out.push_str(&text[last..start]);
        out.push_str("[secret:token]");
        last = end;
    }
    out.push_str(&text[last..]);
    out
}

/// A name for a pasted token from its shape.
pub fn suggest_name(v: &str) -> &'static str {
    if v.starts_with("sk-ant-") {
        "ANTHROPIC_API_KEY"
    } else if v.starts_with("sk-") {
        "OPENAI_API_KEY"
    } else if ["ghp_", "gho_", "ghu_", "ghs_", "ghr_", "github_pat_"].iter().any(|p| v.starts_with(p)) {
        "GITHUB_TOKEN"
    } else if v.starts_with("xox") || v.starts_with("xapp-") {
        "SLACK_TOKEN"
    } else if v.starts_with("AIza") {
        "GOOGLE_API_KEY"
    } else if v.starts_with("hf_") {
        "HF_TOKEN"
    } else if v.starts_with("-----BEGIN") {
        "PRIVATE_KEY"
    } else {
        ""
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pasted_key_is_caught_and_ordinary_words_are_not() {
        let key = format!("sk-{}", "a1B2".repeat(10));
        let text = format!("usa questa {key} per favore");
        let spans = detect(&text);
        assert_eq!(spans.len(), 1);
        assert_eq!(mask(&text, &spans), "usa questa [secret:token] per favore");
        assert_eq!(detect("password=hunter2hunter2").len(), 1);
        assert_eq!(detect("Authorization: Bearer abcdefghijklmnop1234").len(), 1);
        assert!(!detect(&format!("the token is sec_{}", "a".repeat(43))).is_empty());
        for ok in [
            "Ricordami alle 18:30 di chiamare la mamma",
            "my password is long and I forgot it",
            "see https://example.com/some/long/path/with/words/only/for/reading",
            "secret://poko/prod/KEY",
            "[sk-abcdefghijklmnopqrstuvwxyz]",
        ] {
            assert!(detect(ok).is_empty(), "{ok}");
        }
    }

    #[test]
    fn a_random_looking_path_segment_alone_is_flagged() {
        let seg = "aB3dE6gH9jK2mN5pQ8sT1vW4yZ7bC0eF";
        assert_eq!(detect(&format!("x/y/{seg}/z")).len(), 1);
    }

    #[test]
    fn the_secret_command_never_is_a_message() {
        assert!(is_command("/secret"));
        assert!(is_command("  /SECRET NAME v"));
        assert!(!is_command("/secrets"));
        assert!(!is_command("/sec"));
        assert!(!is_command("è/secret"));
    }

    #[test]
    fn a_value_shows_only_its_hint_and_loses_the_trailing_newline() {
        let v = Secret::new("sk-abc\r\n".to_owned());
        assert_eq!(v.hint(), "sk-…, 6");
        assert!(v.is_valid());
        assert!(!format!("{v:?}").contains("abc"));
        assert!(!Secret::new("\n".to_owned()).is_valid());
    }

    #[test]
    fn the_body_has_the_value_and_says_confirmed() {
        let b = SaveSecret {
            name: "K".into(),
            project: "p".into(),
            environment: "prod".into(),
            path: "/a".into(),
            overwrite: false,
            value: Secret::new("v1".into()),
            request: Some("r1".into()),
        }
        .body();
        let v: Value = serde_json::from_slice(&b).unwrap();
        assert_eq!(v["value"], "v1");
        assert_eq!(v["confirmed"], true);
        assert_eq!(v["createFolder"], true);
        assert_eq!(v["request"], "r1");
    }

    /// What the broker's catalog act builds (infisical-mcp `src/broker/mod.rs`): `folders` are names, `secrets` have no value.
    #[test]
    fn a_folder_reads_as_the_broker_writes_it() {
        let v = json!({"project": "p", "environment": "prod", "path": "/", "folders": ["mail", "wifi"],
            "secrets": [{"key": "A_KEY", "version": 3, "updated_at": "2026-11-01T22:00:00Z"}, {"key": "B_KEY", "version": null, "updated_at": null}]});
        let f = Folder::parse(&v);
        assert_eq!(f.folders, ["mail", "wifi"]);
        assert_eq!(f.secrets[0], SecretName { key: "A_KEY".into(), updated_at: Some("2026-11-01T22:00:00Z".into()) });
        assert_eq!(f.secrets[1].updated_at, None);
        assert_eq!(join_folder("/", "a"), "/a");
        assert_eq!(join_folder("/a", "b"), "/a/b");
        assert_eq!(parent_folder("/a/b"), "/a");
        assert_eq!(parent_folder("/a"), "/");
    }

    #[test]
    fn folders_and_projects() {
        assert_eq!(normalize_folder("a/b/").as_deref(), Some("/a/b"));
        assert_eq!(normalize_folder("").as_deref(), Some("/"));
        assert_eq!(normalize_folder("/a/../b"), None);
        let list = Project::parse_list(&serde_json::json!({"projects":[{"id":"1","name":"Poko Bot","alias":"poko","environments":[{"slug":"dev"},{"slug":"prod"}]}]}));
        assert_eq!(find_project("POKO", &list).unwrap().id, "1");
        assert_eq!(find_project("poko bot", &list).unwrap().default_environment(), Some("prod"));
        assert!(find_project("other", &list).is_none());
    }
}
