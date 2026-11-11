//! Decisions and Approvals, as `web/src/api/decisions.ts`, `approvals.ts`, `views/Decision*.tsx`, `Approval*.tsx` have them.
//!
//! A decision is answered with an ordinary chat message (there is no mutation route). An approval is decided on its own route,
//! and only for the `payloadHash` shown. Juno has no biometric key, so like the web it can **deny** a sensitive request but not
//! approve one; the server enforces this too (`403 biometric_required`).
use serde_json::Value;

use crate::client::ApiError;

fn s(v: &Value) -> String {
    v.as_str().unwrap_or("").to_owned()
}

fn o(v: &Value) -> Option<String> {
    v.as_str().filter(|x| !x.is_empty()).map(str::to_owned)
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Decision {
    pub id: String,
    /// `pending`, `decided`, `expired` or `withdrawn`.
    pub status: String,
    pub question: String,
    pub options: Vec<String>,
    pub recommendation: Option<String>,
    pub chosen_option: Option<String>,
    pub decided_by: Option<String>,
    pub reason: Option<String>,
    pub impact: String,
    pub urgency: String,
    pub reversible: bool,
    pub asking_client: String,
    pub asking_session: Option<String>,
    pub created_at: String,
    /// `(kind, text, tier)`.
    pub evidence: Vec<(String, String, u64)>,
    /// As the server sent it, for the RAM cache.
    pub raw: Value,
}

impl Decision {
    pub fn parse(v: &Value) -> Option<Decision> {
        Some(Decision {
            id: o(&v["decision_id"])?,
            status: s(&v["status"]),
            question: s(&v["question"]),
            options: v["options"].as_array().map(|a| a.iter().filter_map(o).collect()).unwrap_or_default(),
            recommendation: o(&v["recommendation"]),
            chosen_option: o(&v["chosen_option"]),
            decided_by: o(&v["decided_by"]),
            reason: o(&v["reason"]),
            impact: s(&v["impact"]),
            urgency: s(&v["urgency"]),
            reversible: v["reversible"].as_bool() == Some(true),
            asking_client: s(&v["asking_client"]),
            asking_session: o(&v["asking_session"]),
            created_at: s(&v["created_at"]),
            evidence: v["evidence"]
                .as_array()
                .map(|a| a.iter().map(|e| (s(&e["kind"]), s(&e["text"]), e["tier"].as_u64().unwrap_or(0))).collect())
                .unwrap_or_default(),
            raw: v.clone(),
        })
    }

    /// "agent (session)".
    pub fn asker(&self) -> String {
        match &self.asking_session {
            Some(session) => format!("{} ({session})", self.asking_client),
            None => self.asking_client.clone(),
        }
    }
}

/// The decision a button's words answer: `Decision dc-…: 1. Yes` -> `dc-…`.
pub fn answers_decision(text: &str) -> Option<&str> {
    let rest = text.strip_prefix("Decision ")?;
    let (id, _) = rest.split_once(':')?;
    (id.starts_with("dc-") && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')).then_some(id)
}

/// A decision question as the chat shows it (`web/src/lib/decisionText.ts`): the server words it
/// `claude (session (local_8721b9…)) needs a decision: …`, ending with how to answer by text. Session ids mean nothing to the
/// owner, so a short heading stands over the question. `None` for any other message.
pub fn tidy_question(text: &str) -> Option<(String, String)> {
    let (asker, rest) = text.split_once(" needs a decision: ")?;
    let parts: Vec<&str> = asker
        .split(['(', ')'])
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.starts_with("local_") && !s.starts_with("session"))
        .collect();
    let mut seen: Vec<&str> = Vec::new();
    for p in parts {
        if !seen.contains(&p) {
            seen.push(p);
        }
    }
    let label = if seen.is_empty() { asker.trim().to_owned() } else { seen.join(" · ") };
    let body = match rest.rfind("Tap an option (or open Secretary") {
        Some(at) => rest[..at].trim_end(),
        None => rest.trim_end(),
    };
    Some((format!("Decision for you · {label}"), body.to_owned()))
}

/// The chat message that picks option `n` (from 1).
pub fn answer_text(id: &str, n: usize, option: &str) -> String {
    format!("Decision {id}: {n}. {option}")
}

/// The chat message that answers in the owner's words.
pub fn free_text(id: &str, text: &str) -> String {
    format!("Decision {id}: {text}")
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Approval {
    pub id: String,
    pub requester_kind: String,
    pub requester_name: String,
    pub requester_detail: Option<String>,
    pub action: String,
    pub summary: String,
    /// The exact thing that will run or be revealed.
    pub details: String,
    /// `low`, `medium` or `high`; anything else reads as `medium`.
    pub risk: String,
    pub sensitive: bool,
    pub payload_hash: String,
    pub created_at: String,
    pub expires_at: String,
    pub escalated: bool,
    pub you_are_main: bool,
    /// `pending`, `approved`, `denied` or `expired`.
    pub status: String,
    pub decided_via: Option<String>,
    /// As the server sent it, for the RAM cache.
    pub raw: Value,
}

impl Approval {
    /// Network data: an item without an id or hash is dropped (`parseApproval`).
    pub fn parse(v: &Value) -> Option<Approval> {
        let r = &v["requester"];
        let (kind, name, detail) = if r.is_object() { (s(&r["kind"]), s(&r["name"]), o(&r["detail"])) } else { (String::new(), s(r), None) };
        let risk = match v["risk"].as_str() {
            Some(x @ ("low" | "medium" | "high")) => x.to_owned(),
            _ => "medium".to_owned(),
        };
        let status = match v["status"].as_str() {
            Some(x @ ("pending" | "approved" | "denied" | "expired")) => x.to_owned(),
            _ => "pending".to_owned(),
        };
        Some(Approval {
            id: o(&v["id"])?,
            requester_kind: kind,
            requester_name: name,
            requester_detail: detail,
            action: s(&v["action"]),
            summary: s(&v["summary"]),
            details: s(&v["details"]),
            risk,
            sensitive: v["sensitive"].as_bool() == Some(true),
            payload_hash: v["payloadHash"].as_str()?.to_owned(),
            created_at: s(&v["createdAt"]),
            expires_at: s(&v["expiresAt"]),
            escalated: v["escalated"].as_bool() == Some(true),
            you_are_main: v["youAreMain"].as_bool() == Some(true),
            status,
            decided_via: o(&v["decidedVia"]).or_else(|| o(&v["decidedBy"])),
            raw: v.clone(),
        })
    }

    /// The server's `{approvals: [...]}`; a bare array and `{items: [...]}` are accepted too.
    pub fn parse_list(v: &Value) -> Vec<Approval> {
        let list = v.as_array().or_else(|| v["approvals"].as_array()).or_else(|| v["items"].as_array());
        list.map(|a| a.iter().filter_map(Approval::parse).collect()).unwrap_or_default()
    }

    /// Can this device press Approve? Never for a sensitive request.
    pub fn can_approve(&self) -> bool {
        !self.sensitive
    }

    pub fn requester(&self) -> &str {
        if self.requester_name.is_empty() { &self.requester_kind } else { &self.requester_name }
    }
}

/// What a decide came to (`decideApproval` in `web/src/state/approvals.ts`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Approved,
    Denied,
    Gone,
    Expired,
    /// Someone else answered first.
    Decided,
    /// The request changed under the screen: look again.
    Changed,
    /// Sensitive: only Face ID or Touch ID can approve it.
    NeedsFaceId,
    /// The main device has the first 30 s.
    NotMain,
    Error,
}

impl Outcome {
    pub fn of(approve: bool, result: Result<(), ApiError>) -> Outcome {
        let Err(e) = result else {
            return if approve { Outcome::Approved } else { Outcome::Denied };
        };
        match (e.status(), e.code()) {
            (Some(404), _) => Outcome::Gone,
            (Some(410), _) => Outcome::Expired,
            (Some(409), Some("payload_mismatch")) => Outcome::Changed,
            (Some(409), _) => Outcome::Decided,
            (Some(403), Some("biometric_required")) => Outcome::NeedsFaceId,
            (Some(403), Some("not_main_device")) => Outcome::NotMain,
            _ => Outcome::Error,
        }
    }

    /// The request is over for this screen.
    pub fn closes(self) -> bool {
        matches!(self, Outcome::Approved | Outcome::Denied | Outcome::Gone | Outcome::Expired | Outcome::Decided)
    }

    pub fn text(self) -> &'static str {
        match self {
            Outcome::Approved => "Approved.",
            Outcome::Denied => "Denied.",
            Outcome::Gone => "This request is gone.",
            Outcome::Expired => "This request expired.",
            Outcome::Decided => "Already answered on another device.",
            Outcome::Changed => "The request changed. Look at it again.",
            Outcome::NeedsFaceId => "Approve this one with Face ID on your iPhone.",
            Outcome::NotMain => "Your main device gets the first 30 seconds. Try again in a moment.",
            Outcome::Error => "That did not go through. Try again.",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn answers_are_the_words_lucina_reads() {
        assert_eq!(answer_text("dc-1", 2, "Later"), "Decision dc-1: 2. Later");
        assert_eq!(free_text("dc-1", "no, wait"), "Decision dc-1: no, wait");
    }

    #[test]
    fn a_buttons_words_name_its_decision() {
        assert_eq!(answers_decision("Decision dc-23df0450680f: 1. Store it"), Some("dc-23df0450680f"));
        assert_eq!(answers_decision("Decision pending: yes"), None);
        assert_eq!(answers_decision("ciao"), None);
    }

    #[test]
    fn a_question_gets_a_heading_and_loses_how_to_answer() {
        let t = "claude (LucinaOS coordinator (local_8721b9)) needs a decision: May I?\nWhy not.\n\n1. Yes\n\nTap an option (or open Secretary → Decisions), or answer \"Decision dc-1: <your words>\".";
        let (head, body) = tidy_question(t).unwrap();
        assert_eq!(head, "Decision for you · claude · LucinaOS coordinator");
        assert_eq!(body, "May I?\nWhy not.\n\n1. Yes");
        assert!(tidy_question("ciao").is_none());
    }

    #[test]
    fn approvals_drop_what_does_not_fit() {
        let good = json!({"id":"a1","payloadHash":"h","requester":{"kind":"agent","name":"Claude"},"risk":"odd","sensitive":true});
        let a = Approval::parse(&good).unwrap();
        assert_eq!((a.risk.as_str(), a.status.as_str(), a.requester()), ("medium", "pending", "Claude"));
        assert!(!a.can_approve());
        assert!(Approval::parse(&json!({"id":"a2"})).is_none());
        assert_eq!(Approval::parse_list(&json!({"approvals":[good.clone(), {"id":"x"}]})).len(), 1);
        assert_eq!(Approval::parse_list(&json!([good])).len(), 1);
        assert_eq!(Approval::parse(&json!({"id":"a","payloadHash":"h","requester":"bare"})).unwrap().requester(), "bare");
    }

    #[test]
    fn outcomes_follow_the_web_page() {
        let http = |s, c: &str| Err(ApiError::Http(s, c.into(), String::new()));
        assert_eq!(Outcome::of(true, Ok(())), Outcome::Approved);
        assert_eq!(Outcome::of(false, Ok(())), Outcome::Denied);
        assert_eq!(Outcome::of(true, http(404, "x")), Outcome::Gone);
        assert_eq!(Outcome::of(true, http(410, "x")), Outcome::Expired);
        assert_eq!(Outcome::of(true, http(409, "payload_mismatch")), Outcome::Changed);
        assert_eq!(Outcome::of(true, http(409, "approval_decided")), Outcome::Decided);
        assert_eq!(Outcome::of(true, http(403, "biometric_required")), Outcome::NeedsFaceId);
        assert_eq!(Outcome::of(true, http(403, "not_main_device")), Outcome::NotMain);
        assert_eq!(Outcome::of(true, Err(ApiError::Network("x".into()))), Outcome::Error);
        assert!(Outcome::Decided.closes() && !Outcome::Changed.closes());
    }
}
