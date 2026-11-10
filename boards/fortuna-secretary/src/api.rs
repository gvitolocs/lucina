//! The requests Fortuna makes and how it reads the answers: the same `/v1` routes and fields as Juno's
//! `lucina-account` (sign-in, session, timeline, chat) and `lucina-agents-client` (Lucina Agents), plus the share route
//! the phones use for voice notes (SecretaryIOS docs/share.md v1.2). Every field is optional on the way in, so a
//! newer server still reads; what is missing becomes empty, never a crash.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use serde_json::{json, Map, Value};

use crate::http::escape;

pub const HOST: &str = "getsecretary.tech";
/// The name the owner sees in his device list and on the approval screen.
pub const DEVICE_NAME: &str = "Fortuna (Nintendo DSi)";
/// Shown in the server's logs and stored as the voice note's media source.
pub const SHARE_SOURCE: &str = "fortuna-chat";
pub const AGENTS: &str = "/v1/agents";

fn parse(body: &[u8]) -> Option<Value> {
    serde_json::from_slice(body).ok()
}

fn s(v: &Value) -> Option<String> {
    v.as_str().filter(|x| !x.is_empty()).map(str::to_string)
}

/// An id that the server may send as a string or a number.
fn id(v: &Value) -> Option<String> {
    match v {
        Value::String(x) if !x.is_empty() => Some(x.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// The server's error code (`{"error": {"code"}}` or `{"code"}`), for the log and the screen.
pub fn error_code(body: &[u8]) -> Option<String> {
    let v = parse(body)?;
    s(&v["error"]["code"]).or_else(|| s(&v["code"])).or_else(|| s(&v["error"]))
}

// ---- sign-in ----

pub const QR_START: &str = "/v1/session/qr";

pub fn qr_start_body() -> String {
    json!({ "device": { "name": DEVICE_NAME } }).to_string()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QrStart {
    pub approve_url: String,
    pub short_code: Option<String>,
    pub poll_token: String,
    pub expires_in: u64,
}

pub fn parse_qr_start(body: &[u8]) -> Option<QrStart> {
    let v = parse(body)?;
    Some(QrStart {
        approve_url: s(&v["approveUrl"])?,
        short_code: s(&v["shortCode"]),
        poll_token: s(&v["pollToken"])?,
        expires_in: v["expiresIn"].as_u64().unwrap_or(120),
    })
}

pub fn qr_poll_path(poll_token: &str) -> String {
    let mut p = String::from("/v1/session/qr/poll?t=");
    escape(poll_token, &mut p);
    p
}

/// No `Debug`: `Approved` holds the token.
#[derive(PartialEq, Eq)]
pub enum PollAnswer {
    Pending,
    Denied,
    Expired,
    Approved(String),
}

impl core::fmt::Debug for PollAnswer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            PollAnswer::Pending => "Pending",
            PollAnswer::Denied => "Denied",
            PollAnswer::Expired => "Expired",
            PollAnswer::Approved(_) => "Approved(..)",
        })
    }
}

pub fn parse_qr_poll(body: &[u8]) -> Option<PollAnswer> {
    let v = parse(body)?;
    match v["status"].as_str()? {
        "pending" => Some(PollAnswer::Pending),
        "denied" => Some(PollAnswer::Denied),
        "expired" => Some(PollAnswer::Expired),
        "approved" => {
            let t = v["token"].as_str()?;
            crate::rules::token_shape(t.as_bytes()).then(|| PollAnswer::Approved(t.into()))
        }
        _ => None,
    }
}

pub const SESSION: &str = "/v1/session";

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Session {
    pub authenticated: bool,
    pub device_id: Option<String>,
}

pub fn parse_session(body: &[u8]) -> Option<Session> {
    let v = parse(body)?;
    Some(Session { authenticated: v["authenticated"].as_bool() == Some(true), device_id: s(&v["deviceId"]) })
}

// ---- chat ----

/// The chat session of this DSi: `fortuna-<device id>`, stable for as long as the sign-in (Juno: `juno-<device id>`).
pub fn session_id(device_id: &str) -> String {
    let mut s = String::from("fortuna-");
    for c in device_id.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-').take(55) {
        s.push(c);
    }
    s
}

/// `GET /v1/timeline?scope=mine`: the newest page, or what came after `after`.
pub fn timeline_path(after: Option<&str>, limit: u32) -> String {
    let mut p = String::from("/v1/timeline?limit=");
    let mut d = [0u8; 10];
    p.push_str(crate::http::dec(limit.clamp(1, 200), &mut d));
    p.push_str("&scope=mine");
    if let Some(a) = after {
        p.push_str("&after=");
        escape(a, &mut p);
    }
    p
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Item {
    pub id: String,
    pub lucina: bool,
    pub text: String,
    pub ts: String,
    pub client_message_id: Option<String>,
    /// A voice note: its length in ms (0 when unknown) and the words, when known.
    pub voice: Option<(u32, Option<String>)>,
    /// The voice note's waveform strip (0..=255 a byte, at most 64; empty when the server has none).
    pub wave: Vec<u8>,
    /// A reply the server marks sensitive (a code, a password): never drawn on the DSi.
    pub sensitive: bool,
    /// Other attachments, as a count (pictures, files): the card says there is something to see on the phone.
    pub others: u32,
}

fn parse_item(v: &Value) -> Option<Item> {
    let lucina = match v["role"].as_str()? {
        "lucina" => true,
        "user" => false,
        _ => return None,
    };
    let meta = &v["meta"];
    let mut voice = None;
    let mut wave = Vec::new();
    let mut others = 0;
    if let Some(a) = v["attachments"].as_array() {
        for x in a {
            if x["kind"].as_str() == Some("voice") {
                voice = Some((x["durationMs"].as_u64().unwrap_or(0).min(u32::MAX as u64) as u32, s(&x["transcript"])));
                wave = x["waveform"].as_str().and_then(b64_decode).filter(|w| w.len() <= 64).unwrap_or_default();
            } else {
                others += 1;
            }
        }
    }
    let sensitive = meta["sensitive"].as_bool() == Some(true) || v["sensitive"].as_bool() == Some(true);
    Some(Item {
        id: id(&v["id"])?,
        lucina,
        text: if sensitive { String::new() } else { v["text"].as_str().unwrap_or("").into() },
        ts: s(&v["ts"]).unwrap_or_default(),
        client_message_id: s(&v["clientMessageId"]),
        voice,
        wave,
        sensitive,
        others,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Page {
    pub items: Vec<Item>,
    pub cursor: Option<String>,
}

pub fn parse_timeline(body: &[u8]) -> Option<Page> {
    let v = parse(body)?;
    Some(Page {
        items: v["items"].as_array().map(|a| a.iter().filter_map(parse_item).collect()).unwrap_or_default(),
        cursor: id(&v["cursor"]),
    })
}

pub const CHAT: &str = "/v1/chat";

pub fn chat_body(message: &str, session: &str, client_message_id: &str) -> String {
    json!({ "message": message, "sessionId": session, "clientMessageId": client_message_id }).to_string()
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ChatReply {
    pub reply: String,
    pub sensitive: bool,
    pub user_item_id: Option<String>,
    pub lucina_item_id: Option<String>,
}

pub fn parse_chat_reply(body: &[u8]) -> Option<ChatReply> {
    let v = parse(body)?;
    let sensitive = v["sensitive"].as_bool() == Some(true);
    Some(ChatReply {
        reply: if sensitive { String::new() } else { v["reply"].as_str().unwrap_or("").into() },
        sensitive,
        user_item_id: id(&v["userItemId"]),
        lucina_item_id: id(&v["lucinaItemId"]),
    })
}

// ---- voice notes: POST /v1/share ----

pub const SHARE: &str = "/v1/share";

/// The form fields of a voice note before its file part (docs/share.md v1.2), in order.
/// The form fields of a voice note's share; `waveform` (1 to 64 bytes) rides in `meta` like the iPhone's.
pub fn voice_fields(share_id: &str, session: &str, duration_ms: u32, waveform: &[u8]) -> [(&'static str, String); 6] {
    let mut file = json!({"index": 0, "voice": true, "durationMs": duration_ms});
    if (1..=64).contains(&waveform.len()) {
        file["waveform"] = Value::String(b64_encode(waveform));
    }
    [
        ("shareId", share_id.into()),
        ("intent", "ask".into()),
        ("source", SHARE_SOURCE.into()),
        ("channel", "web".into()),
        ("sessionId", session.into()),
        ("meta", json!({ "files": [file] }).to_string()),
    ]
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ShareAccepted {
    pub share_id: Option<String>,
    pub user_item_id: Option<String>,
}

pub fn parse_share_accepted(body: &[u8]) -> Option<ShareAccepted> {
    let v = parse(body)?;
    Some(ShareAccepted { share_id: s(&v["shareId"]), user_item_id: id(&v["userItemId"]) })
}

pub fn share_status_path(share_id: &str) -> String {
    let mut p = String::from("/v1/share/");
    escape(share_id, &mut p);
    p
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ShareStatus {
    /// `received` → `reading` → `thinking` → `done` | `error`
    pub stage: String,
    pub reply: Option<String>,
    pub lucina_item_id: Option<String>,
    pub transcript: Option<String>,
}

impl ShareStatus {
    pub fn finished(&self) -> bool {
        self.stage == "done" || self.stage == "error"
    }
}

pub fn parse_share_status(body: &[u8]) -> Option<ShareStatus> {
    let v = parse(body)?;
    let stage = s(&v["stage"]).or_else(|| s(&v["status"]).map(|x| if x == "processing" { "received".into() } else { x })).unwrap_or_else(|| "received".into());
    let transcript = v["attachments"].as_array().and_then(|a| a.iter().find_map(|x| s(&x["transcript"])));
    Some(ShareStatus { stage, reply: s(&v["reply"]), lucina_item_id: id(&v["lucinaItemId"]), transcript })
}

// ---- Lucina Agents ----

pub fn agents_items_path() -> String {
    let mut p = String::from(AGENTS);
    p.push_str("/items");
    p
}

/// Paseo's state buckets, most urgent first (`lucina-agents::bucket`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Bucket {
    NeedsInput,
    Failed,
    Running,
    Attention,
    Done,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRow {
    pub id: String,
    pub title: String,
    pub provider: String,
    pub status: String,
    pub bucket: Bucket,
    pub updated_at: String,
}

fn bucket(v: &Value) -> Bucket {
    match v.as_str().unwrap_or("") {
        "needs_input" | "needsInput" => Bucket::NeedsInput,
        "failed" => Bucket::Failed,
        "running" => Bucket::Running,
        "attention" => Bucket::Attention,
        _ => Bucket::Done,
    }
}

/// `GET /v1/agents/items` → `{agents: [AgentListItem]}`, most urgent first, then the newest.
pub fn parse_agent_items(body: &[u8]) -> Option<Vec<AgentRow>> {
    let v = parse(body)?;
    let mut rows: Vec<AgentRow> = v["agents"]
        .as_array()?
        .iter()
        .filter(|a| a["archivedAt"].is_null())
        .filter_map(|a| {
            Some(AgentRow {
                id: s(&a["id"])?,
                title: s(&a["title"]).or_else(|| s(&a["shortId"])).unwrap_or_else(|| "Agent".into()),
                provider: s(&a["provider"]).unwrap_or_default(),
                status: s(&a["status"]).unwrap_or_default(),
                bucket: bucket(&a["bucket"]),
                updated_at: s(&a["updatedAt"]).unwrap_or_default(),
            })
        })
        .collect();
    rows.sort_by(|a, b| a.bucket.cmp(&b.bucket).then_with(|| b.updated_at.cmp(&a.updated_at)));
    Some(rows)
}

pub fn agent_path(id: &str, tail: &str) -> String {
    let mut p = String::from(AGENTS);
    p.push('/');
    escape(id, &mut p);
    p.push_str(tail);
    p
}

/// One question of an AskUserQuestion-style request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    pub text: String,
    pub options: Vec<String>,
}

/// A pending permission request or question (`PermissionRequest`).
#[derive(Debug, Clone, PartialEq)]
pub struct Ask {
    pub id: String,
    /// `tool`, `plan`, `question`, `mode`, `other`
    pub kind: String,
    pub title: String,
    pub questions: Vec<Question>,
    input: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Snapshot {
    pub status: String,
    pub title: String,
    pub last_error: Option<String>,
    pub asks: Vec<Ask>,
}

pub fn parse_snapshot(body: &[u8]) -> Option<Snapshot> {
    let v = parse(body)?;
    let v = if v["agent"].is_object() { v["agent"].clone() } else { v };
    let asks = v["pendingPermissions"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|p| {
                    let input = p["input"].as_object().cloned().unwrap_or_default();
                    let questions = input
                        .get("questions")
                        .and_then(Value::as_array)
                        .map(|qs| {
                            qs.iter()
                                .filter_map(|q| {
                                    Some(Question {
                                        text: s(&q["question"])?,
                                        options: q["options"].as_array().map(|o| o.iter().filter_map(|x| s(&x["label"])).collect()).unwrap_or_default(),
                                    })
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    Some(Ask {
                        id: s(&p["id"])?,
                        kind: s(&p["kind"]).unwrap_or_else(|| "other".into()),
                        title: s(&p["title"]).or_else(|| s(&p["description"])).or_else(|| s(&p["name"])).unwrap_or_default(),
                        questions,
                        input,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Some(Snapshot {
        status: s(&v["status"]).unwrap_or_default(),
        title: s(&v["title"]).unwrap_or_default(),
        last_error: s(&v["lastError"]),
        asks,
    })
}

/// `POST /v1/agents/{id}/permissions/{request}`: allow or deny a tool or plan request.
pub fn permission_body(allow: bool) -> String {
    json!({ "behavior": if allow { "allow" } else { "deny" } }).to_string()
}

/// The answer to a question: `allow` with the request's input and `answers` (question → chosen label), which is
/// what Claude's AskUserQuestion and Codex's requestUserInput read (agentd `providers/*`).
pub fn answer_body(ask: &Ask, answers: &[(usize, &str)]) -> String {
    let mut input = ask.input.clone();
    let mut map = Map::new();
    for &(qi, label) in answers {
        if let Some(q) = ask.questions.get(qi) {
            map.insert(q.text.clone(), Value::String(label.into()));
        }
    }
    input.insert("answers".into(), Value::Object(map));
    json!({ "behavior": "allow", "updatedInput": input }).to_string()
}

/// `POST /v1/agents/{id}/send`; while a turn runs, steer it (as the Juno app does).
pub fn send_body(prompt: &str, running: bool) -> String {
    if running {
        json!({ "prompt": prompt, "activeTurnBehavior": "steer" }).to_string()
    } else {
        json!({ "prompt": prompt }).to_string()
    }
}

pub fn timeline_rows_path(id: &str, after_seq: u64, limit: u32) -> String {
    let mut p = agent_path(id, "/timeline?afterSeq=");
    p.push_str(&after_seq.to_string());
    p.push_str("&limit=");
    p.push_str(&limit.to_string());
    p
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowKind {
    User,
    Assistant,
    Tool,
    Error,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub seq: u64,
    pub kind: RowKind,
    /// The words (for a tool call: its name and status), cut to 600 characters.
    pub text: String,
}

fn cut(s: &str, n: usize) -> String {
    match s.char_indices().nth(n) {
        Some((i, _)) => {
            let mut t = String::from(&s[..i]);
            t.push('…');
            t
        }
        None => s.into(),
    }
}

/// `{epoch, rows, nextSeq}`: the rows (reasoning and notifications left out) and the next `afterSeq`.
pub fn parse_rows(body: &[u8]) -> Option<(Vec<Row>, u64)> {
    let v = parse(body)?;
    let rows = v["rows"]
        .as_array()?
        .iter()
        .filter_map(|r| {
            let seq = r["seq"].as_u64()?;
            let it = &r["item"];
            let (kind, text) = match it["type"].as_str()? {
                "user_message" => (RowKind::User, it["text"].as_str().unwrap_or("").to_string()),
                "assistant_message" => (RowKind::Assistant, it["text"].as_str().unwrap_or("").to_string()),
                "tool_call" => {
                    let name = s(&it["name"]).or_else(|| s(&it["detail"]["type"])).unwrap_or_else(|| "tool".into());
                    let status = s(&it["status"]).unwrap_or_default();
                    (RowKind::Tool, if status.is_empty() { name } else { alloc::format!("{name} · {status}") })
                }
                "error" => (RowKind::Error, it["message"].as_str().unwrap_or("").to_string()),
                "reasoning" | "notification" | "compaction" => return None,
                _ => (RowKind::Other, String::new()),
            };
            Some(Row { seq, kind, text: cut(&text, 600) })
        })
        .collect();
    let next = v["nextSeq"].as_u64().unwrap_or(0);
    Some((rows, next))
}

/// Standard base64 with padding (the waveform's encoding).
pub fn b64_encode(b: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(b.len().div_ceil(3) * 4);
    for c in b.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for k in 0..4 {
            out.push(if k <= c.len() { A[(n >> (18 - 6 * k) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

/// Standard base64, padding optional; `None` for anything else.
pub fn b64_decode(s: &str) -> Option<Vec<u8>> {
    let s = s.trim_end_matches('=');
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for ch in s.bytes() {
        let v = match ch {
            b'A'..=b'Z' => ch - b'A',
            b'a'..=b'z' => ch - b'a' + 26,
            b'0'..=b'9' => ch - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32;
        acc = acc << 6 | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    (bits < 6).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_in_answers() {
        let q = parse_qr_start(br#"{"code":"c1","shortCode":"100001","pollToken":"pt","approveUrl":"https://getsecretary.tech/approve?c=c1","expiresIn":120}"#).unwrap();
        assert_eq!(q.poll_token, "pt");
        assert_eq!(qr_poll_path("a+b/="), "/v1/session/qr/poll?t=a%2Bb%2F%3D");
        assert_eq!(parse_qr_poll(br#"{"status":"pending"}"#), Some(PollAnswer::Pending));
        assert_eq!(parse_qr_poll(br#"{"status":"approved","token":"nope"}"#), None);
        let t = "sec_abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNO-_";
        let body = alloc::format!(r#"{{"status":"approved","token":"{t}"}}"#);
        assert_eq!(parse_qr_poll(body.as_bytes()), Some(PollAnswer::Approved(t.into())));
        assert_eq!(alloc::format!("{:?}", PollAnswer::Approved(t.into())), "Approved(..)");
        assert_eq!(qr_start_body(), r#"{"device":{"name":"Fortuna (Nintendo DSi)"}}"#);
        let s = parse_session(br#"{"authenticated":true,"deviceId":"dev_9"}"#).unwrap();
        assert_eq!(s.device_id.as_deref(), Some("dev_9"));
        assert_eq!(session_id("dev_9"), "fortuna-dev_9");
    }

    #[test]
    fn timeline_and_chat() {
        let p = parse_timeline(br#"{"items":[
            {"id":12,"role":"user","text":"hi","ts":"2026-11-08T10:00:00Z","clientMessageId":"c1"},
            {"id":"13","role":"lucina","text":"secret","meta":{"sensitive":true}},
            {"id":"14","role":"system","text":"x"},
            {"id":"15","role":"user","text":"","attachments":[{"kind":"voice","durationMs":4200,"transcript":"ciao","waveform":"AID/"},{"kind":"image"}]}
        ],"cursor":"cur"}"#)
        .unwrap();
        assert_eq!(p.items.len(), 3);
        assert_eq!(p.items[0].id, "12");
        assert!(p.items[1].sensitive && p.items[1].text.is_empty());
        assert_eq!(p.items[2].voice, Some((4200, Some("ciao".into()))));
        assert_eq!(p.items[2].others, 1);
        assert_eq!(p.items[2].wave, [0, 128, 255]);
        assert_eq!(p.cursor.as_deref(), Some("cur"));
        assert_eq!(timeline_path(Some("a b"), 999), "/v1/timeline?limit=200&scope=mine&after=a%20b");
        let r = parse_chat_reply(br#"{"reply":"Ciao!","userItemId":20,"lucinaItemId":"21"}"#).unwrap();
        assert_eq!((r.reply.as_str(), r.user_item_id.as_deref(), r.lucina_item_id.as_deref()), ("Ciao!", Some("20"), Some("21")));
        assert!(chat_body("x\"y", "s", "c").contains(r#""message":"x\"y""#));
    }

    #[test]
    fn base64_both_ways() {
        for n in 0..70usize {
            let b: Vec<u8> = (0..n).map(|i| (i * 37 + 5) as u8).collect();
            assert_eq!(b64_decode(&b64_encode(&b)).as_deref(), Some(&b[..]), "{n} bytes");
        }
        assert_eq!(b64_encode(b"Ma"), "TWE=");
        assert_eq!(b64_decode("TWE"), Some(b"Ma".to_vec()));
        assert_eq!(b64_decode("T!E="), None);
    }

    #[test]
    fn shares() {
        let f = voice_fields("sh1", "fortuna-dev_9", 4200, &[]);
        assert_eq!(f[5].1, r#"{"files":[{"durationMs":4200,"index":0,"voice":true}]}"#);
        let f = voice_fields("sh1", "fortuna-dev_9", 4200, &[0, 128, 255]);
        assert_eq!(f[5].1, r#"{"files":[{"durationMs":4200,"index":0,"voice":true,"waveform":"AID/"}]}"#);
        let a = parse_share_accepted(br#"{"ok":true,"shareId":"sh1","userItemId":"u1","status":"processing"}"#).unwrap();
        assert_eq!(a.user_item_id.as_deref(), Some("u1"));
        let st = parse_share_status(br#"{"ok":true,"stage":"done","reply":"Ok","lucinaItemId":"l1","attachments":[{"index":0,"transcript":"hello"}]}"#).unwrap();
        assert!(st.finished());
        assert_eq!(st.transcript.as_deref(), Some("hello"));
        let st = parse_share_status(br#"{"ok":true,"status":"processing"}"#).unwrap();
        assert_eq!(st.stage, "received");
    }

    #[test]
    fn agents() {
        let rows = parse_agent_items(br#"{"agents":[
            {"id":"a1","title":"Fix build","provider":"claude","status":"idle","bucket":"done","updatedAt":"2026-11-08T10:00:00Z"},
            {"id":"a2","shortId":"a2s","provider":"codex","status":"running","bucket":"running","updatedAt":"2026-11-08T09:00:00Z"},
            {"id":"a3","title":"Ask","provider":"claude","status":"idle","bucket":"needs_input","updatedAt":"2026-11-07T09:00:00Z"},
            {"id":"a4","title":"Old","bucket":"done","archivedAt":"2026-11-01T00:00:00Z"}
        ]}"#)
        .unwrap();
        assert_eq!(rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["a3", "a2", "a1"]);
        assert_eq!(rows[1].title, "a2s");
        let snap = parse_snapshot(br#"{"status":"idle","title":"Ask","pendingPermissions":[
            {"id":"p1","provider":"claude","name":"AskUserQuestion","kind":"question","title":"Questions",
             "input":{"questions":[{"question":"Which rollout?","options":[{"label":"Now"},{"label":"Later"}]}]}}]}"#)
        .unwrap();
        let ask = &snap.asks[0];
        assert_eq!(ask.questions[0].options, ["Now", "Later"]);
        let body: Value = serde_json::from_str(&answer_body(ask, &[(0, "Later")])).unwrap();
        assert_eq!(body["behavior"], "allow");
        assert_eq!(body["updatedInput"]["answers"]["Which rollout?"], "Later");
        assert_eq!(body["updatedInput"]["questions"][0]["question"], "Which rollout?");
        assert_eq!(permission_body(false), r#"{"behavior":"deny"}"#);
        assert_eq!(agent_path("a/1", "/send"), "/v1/agents/a%2F1/send");
        let (rows, next) = parse_rows(br#"{"epoch":"e","rows":[
            {"seq":5,"timestamp":"t","item":{"type":"assistant_message","text":"Done."}},
            {"seq":6,"timestamp":"t","item":{"type":"reasoning","text":"hmm"}},
            {"seq":7,"timestamp":"t","item":{"type":"tool_call","name":"Bash","status":"completed"}}],"nextSeq":7}"#)
        .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].text, "Bash · completed");
        assert_eq!(next, 7);
        assert_eq!(send_body("go", true), r#"{"activeTurnBehavior":"steer","prompt":"go"}"#);
    }
}
