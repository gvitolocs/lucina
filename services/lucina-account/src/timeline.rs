//! Timeline items and the chat's state, on `secretary_chat::merge`: the iPhone's rules, proved in SecretaryIOS
//! `verification/verus/chat_merge.rs`. Everything here comes from the network, so the parsers drop what does not fit.
use std::collections::HashMap;

use secretary_chat::merge::{self, Key, Role, LOCAL_PREFIX};
use secretary_chat::page;
use serde_json::Value;

use crate::rich::Rich;

/// A timeline id: a string, or a number as its decimal text (`docs/server-api.md`).
fn id_of(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// RFC 3339 to epoch milliseconds: `2026-10-22T07:30:00.000Z`, with an offset too.
pub fn parse_rfc3339_ms(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b't' | b' ') || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d, h, mi, se) = (num(0..4)?, num(5..7)?, num(8..10)?, num(11..13)?, num(14..16)?, num(17..19)?);
    let mut i = 19;
    let mut ms = 0;
    if b[i] == b'.' {
        let start = i + 1;
        i = start;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        let digits = s.get(start..i)?;
        ms = format!("{digits:0<3}").get(..3)?.parse::<i64>().ok()?;
    }
    let offset = match *b.get(i)? {
        b'Z' | b'z' => 0,
        sign @ (b'+' | b'-') => {
            let v = num(i + 1..i + 3)? * 3600 + num(i + 4..i + 6)? * 60;
            if sign == b'+' { v } else { -v }
        }
        _ => return None,
    };
    Some((days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + se - offset) * 1000 + ms)
}

fn ts_of(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Value::String(s) => parse_rfc3339_ms(s),
        _ => None,
    }
}

fn opt_str(v: &Value) -> Option<String> {
    v.as_str().filter(|s| !s.is_empty()).map(str::to_owned)
}

/// One message of the timeline.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Item {
    pub id: String,
    pub ts_ms: i64,
    pub channel: String,
    pub lucina: bool,
    pub text: String,
    pub client_message_id: Option<String>,
    pub reply_to: Option<String>,
    /// `[(kind, name)]`: Juno shows the name, not the file.
    pub attachments: Vec<(String, Option<String>)>,
    /// `durationMs` of each attachment, in step with `attachments`.
    pub attachment_ms: Vec<Option<u64>>,
    pub rich: Option<Rich>,
    /// `meta.kind`: `reminder`, `secret_…`, …
    pub kind: Option<String>,
    pub via_mcp_caller: Option<String>,
    pub meta_device: Option<String>,
    pub share_id: Option<String>,
    pub title: Option<String>,
    /// As the server sent it, for the RAM cache.
    raw: Value,
}

impl Item {
    /// An item with a role other than `user` or `lucina`, or with no id, is skipped, as every client does.
    pub fn parse(v: &Value) -> Option<Item> {
        let lucina = match v["role"].as_str()? {
            "lucina" => true,
            "user" => false,
            _ => return None,
        };
        let meta = &v["meta"];
        let attachments = v["attachments"]
            .as_array()
            .map(|a| a.iter().filter_map(|x| Some((x["kind"].as_str()?.to_owned(), opt_str(&x["name"])))).collect())
            .unwrap_or_default();
        let attachment_ms = v["attachments"]
            .as_array()
            .map(|a| a.iter().filter(|x| x["kind"].as_str().is_some()).map(|x| x["durationMs"].as_u64()).collect())
            .unwrap_or_default();
        Some(Item {
            id: id_of(&v["id"])?,
            ts_ms: ts_of(&v["ts"]).unwrap_or(0),
            channel: v["channel"].as_str().unwrap_or("other").to_owned(),
            lucina,
            text: v["text"].as_str().unwrap_or("").to_owned(),
            client_message_id: opt_str(&v["clientMessageId"]),
            reply_to: id_of(&v["replyTo"]),
            attachments,
            attachment_ms,
            rich: Rich::parse(meta),
            kind: opt_str(&meta["kind"]),
            via_mcp_caller: if meta["via"].as_str() == Some("mcp") { Some(meta["caller"].as_str().unwrap_or("").to_owned()) } else { None },
            meta_device: opt_str(&meta["device"]),
            share_id: if meta["isShare"].as_bool() == Some(true) { opt_str(&meta["shareId"]) } else { None },
            title: opt_str(&meta["chatTitle"]),
            raw: v.clone(),
        })
    }

    /// `docs/server-api.md` "Conversation keys".
    pub fn conversation_key(&self) -> String {
        if let Some(caller) = &self.via_mcp_caller {
            let clean: String = caller
                .to_lowercase()
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
                .collect();
            return format!("agent:{}", if clean.is_empty() { "mcp" } else { &clean });
        }
        match self.channel.as_str() {
            "web" | "ios" => match &self.meta_device {
                Some(d) => format!("device:{d}"),
                None => format!("legacy:{}", self.channel),
            },
            c @ ("telegram" | "discord" | "alexa" | "voice") => c.to_owned(),
            _ => "lucina".to_owned(),
        }
    }

    /// "Mine" for this device on Juno's channel: its own conversation, the old web items and Lucina's own messages.
    pub fn is_mine(&self, device_id: Option<&str>) -> bool {
        let key = self.conversation_key();
        key == "lucina" || key == "legacy:web" || device_id.is_some_and(|d| key == format!("device:{d}"))
    }

    fn key(&self) -> Key {
        Key {
            id: self.id.clone(),
            ts_us: self.ts_ms.saturating_mul(1000),
            role: if self.lucina { Role::Lucina } else { Role::User },
            client_message_id: self.client_message_id.clone(),
            reply_to: self.reply_to.clone(),
            share_id: self.share_id.clone(),
        }
    }
}

/// `GET /v1/timeline`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TimelinePage {
    pub items: Vec<Item>,
    pub next_before: Option<String>,
    pub cursor: Option<String>,
    pub has_more: bool,
}

impl TimelinePage {
    pub fn parse(v: &Value) -> TimelinePage {
        TimelinePage {
            items: v["items"].as_array().map(|a| a.iter().filter_map(Item::parse).collect()).unwrap_or_default(),
            next_before: id_of(&v["nextBefore"]),
            cursor: id_of(&v["cursor"]),
            has_more: v["hasMore"].as_bool() == Some(true),
        }
    }
}

/// `GET /v1/timeline/conversations`: the All chats list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conversation {
    pub key: String,
    pub title: String,
    pub count: u64,
    pub last_ts_ms: i64,
    pub last_text: String,
}

impl Conversation {
    pub fn to_json(&self) -> Value {
        serde_json::json!({"key": self.key, "title": self.title, "count": self.count, "lastTs": self.last_ts_ms, "lastText": self.last_text})
    }

    pub fn parse(v: &Value) -> Option<Conversation> {
        let key = opt_str(&v["key"])?;
        Some(Conversation {
            title: opt_str(&v["title"]).unwrap_or_else(|| key.clone()),
            key,
            count: v["count"].as_u64().unwrap_or(0),
            last_ts_ms: ts_of(&v["lastTs"]).unwrap_or(0),
            last_text: v["lastText"].as_str().unwrap_or("").to_owned(),
        })
    }
}

/// `POST /v1/chat`.
#[derive(Clone, PartialEq, Default)]
pub struct ChatReply {
    pub reply: String,
    pub action: Option<String>,
    pub job: Option<Job>,
    pub user_item_id: Option<String>,
    pub lucina_item_id: Option<String>,
    /// A private OTP: only in RAM, gone at `expires_at_ms`.
    pub sensitive: bool,
    pub expires_at_ms: Option<i64>,
    pub rich: Option<Rich>,
}

impl std::fmt::Debug for ChatReply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ChatReply({} chars, sensitive {})", self.reply.chars().count(), self.sensitive)
    }
}

impl ChatReply {
    pub fn parse(v: &Value) -> ChatReply {
        let sensitive = v["sensitive"].as_bool() == Some(true);
        ChatReply {
            reply: v["reply"].as_str().unwrap_or("").to_owned(),
            action: opt_str(&v["action"]),
            job: if v["job"].is_object() && !v["job"]["id"].is_null() { Some(Job::parse(&v["job"])) } else { None },
            user_item_id: id_of(&v["userItemId"]),
            lucina_item_id: id_of(&v["lucinaItemId"]),
            sensitive,
            expires_at_ms: ts_of(&v["expiresAt"]),
            rich: if sensitive { None } else { Rich::parse(v) },
        }
    }
}

/// `GET /v1/jobs/{id}`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Job {
    pub id: String,
    pub status: String,
    pub action: Option<String>,
    pub result: Option<String>,
    pub error: Option<String>,
}

impl Job {
    pub fn parse(v: &Value) -> Job {
        Job {
            id: id_of(&v["id"]).unwrap_or_default(),
            status: v["status"].as_str().unwrap_or("queued").to_owned(),
            action: opt_str(&v["action"]),
            result: opt_str(&v["result"]),
            error: opt_str(&v["error"]),
        }
    }

    pub fn finished(&self) -> bool {
        self.status == "done" || self.status == "error"
    }
}

/// What the owner sees of a message he is sending.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    /// From the server, or answered.
    Sent,
    Sending,
    Failed(String),
}

/// A message on screen: an [`Item`], plus what only this device knows about it.
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub item: Item,
    pub delivery: Delivery,
    /// Lucina's button this message's row of buttons was answered with.
    pub chosen: Option<String>,
    /// An OTP reply: never on disk, replaced when it expires.
    pub sensitive_until_ms: Option<i64>,
    pub job: Option<Job>,
}

impl Message {
    fn new(item: Item) -> Message {
        Message { item, delivery: Delivery::Sent, chosen: None, sensitive_until_ms: None, job: None }
    }
}

/// `secretary_chat::page`'s page size for the first paint and the older pages.
pub const PAGE: u32 = 50;

/// The chat's state: messages by id, in the merge's order, with the sync cursor and the older-page cursor.
#[derive(Debug, Default)]
pub struct Timeline {
    by_id: HashMap<String, Message>,
    order: Vec<String>,
    pub cursor: Option<String>,
    older: Option<String>,
    has_older: bool,
}

impl Timeline {
    /// Oldest first.
    pub fn messages(&self) -> impl DoubleEndedIterator<Item = &Message> {
        self.order.iter().filter_map(|id| self.by_id.get(id))
    }

    pub fn get(&self, id: &str) -> Option<&Message> {
        self.by_id.get(id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut Message> {
        self.by_id.get_mut(id)
    }

    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// Merges server items in by the proved rules. A local bubble an incoming copy replaces hands over what only this device
    /// knows.
    pub fn merge(&mut self, incoming: Vec<Item>) {
        let current: Vec<Key> = self.messages().map(|m| m.item.key()).collect();
        let keys: Vec<Key> = incoming.iter().map(Item::key).collect();
        let result = merge::merge(&current, &keys);
        // A body in the result is the last incoming copy of its id, users before Lucina (the merge's rule).
        let (users, lucina): (Vec<Item>, Vec<Item>) = incoming.into_iter().partition(|i| !i.lucina);
        for item in users.into_iter().chain(lucina) {
            match self.by_id.get_mut(&item.id) {
                Some(m) => {
                    if m.delivery != Delivery::Sent {
                        m.delivery = Delivery::Sent;
                    }
                    m.item = item;
                }
                None => {
                    self.by_id.insert(item.id.clone(), Message::new(item));
                }
            }
        }
        for r in &result.replaced {
            if let Some(local) = self.by_id.remove(&r.local) {
                if let Some(m) = self.by_id.get_mut(&r.server) {
                    m.chosen = m.chosen.take().or(local.chosen);
                    m.job = m.job.take().or(local.job);
                    m.sensitive_until_ms = m.sensitive_until_ms.or(local.sensitive_until_ms);
                }
            }
        }
        for u in &result.reply_to_updates {
            if let Some(m) = self.by_id.get_mut(&u.id) {
                m.item.reply_to = Some(u.reply_to.clone());
            }
        }
        let kept: std::collections::HashSet<&String> = result.ids.iter().collect();
        self.by_id.retain(|id, _| kept.contains(id));
        self.order = result.ids;
    }

    /// What can be shown again at once next time the app opens: the newest server items as the server sent them, and the
    /// cursor. Not an OTP, and not what is still being sent.
    pub fn snapshot(&self, max: usize) -> Value {
        let mut items: Vec<&Value> = self
            .messages()
            .filter(|m| !m.item.id.starts_with(LOCAL_PREFIX) && m.sensitive_until_ms.is_none() && !m.item.raw.is_null())
            .map(|m| &m.item.raw)
            .collect();
        let start = items.len().saturating_sub(max);
        items.drain(..start);
        let next_before = if start > 0 || self.has_older() { items.first().and_then(|i| id_of(&i["id"])) } else { None };
        serde_json::json!({ "items": items, "cursor": self.cursor, "nextBefore": next_before })
    }

    /// Takes a [`Timeline::snapshot`] in, before anything else is known.
    pub fn restore(&mut self, v: &Value) {
        let page = TimelinePage::parse(v);
        self.merge(page.items);
        self.cursor = page.cursor;
        self.note_first_page(page.next_before);
    }

    /// The iOS store's reset: drop old server items and the cursor, keep what is still being sent.
    pub fn reset_server_items(&mut self) {
        self.by_id.retain(|id, _| id.starts_with(LOCAL_PREFIX));
        self.order.retain(|id| id.starts_with(LOCAL_PREFIX));
        self.cursor = None;
        self.older = None;
        self.has_older = false;
    }

    /// A page of the newest window: `next_before` says where older ones start.
    pub fn note_first_page(&mut self, next_before: Option<String>) {
        if self.older.is_none() {
            self.has_older = next_before.is_some();
            self.older = next_before;
        }
    }

    pub fn has_older(&self) -> bool {
        page::has_older(0, 1, self.older.as_deref()) || self.has_older
    }

    /// Where the next older page starts.
    pub fn older_cursor(&self) -> Option<String> {
        page::older_cursor(self.order.iter().map(String::as_str), self.older.as_deref())
    }

    pub fn note_older_page(&mut self, next_before: Option<String>) {
        self.has_older = next_before.is_some();
        self.older = Some(next_before.unwrap_or_default());
    }

    /// The optimistic bubble: never above what is already on screen, even with a slow clock (`state/chat.ts`). Returns its id.
    pub fn add_local(&mut self, text: &str, client_message_id: &str, reply_to: Option<String>, now_ms: i64) -> String {
        let ts = self.messages().next_back().map_or(now_ms, |m| now_ms.max(m.item.ts_ms + 1));
        let id = format!("{LOCAL_PREFIX}{client_message_id}");
        let item = Item {
            id: id.clone(),
            ts_ms: ts,
            channel: "web".into(),
            text: text.to_owned(),
            client_message_id: Some(client_message_id.to_owned()),
            reply_to,
            ..Item::default()
        };
        let mut m = Message::new(item);
        m.delivery = Delivery::Sending;
        self.by_id.insert(id.clone(), m);
        self.order.push(id.clone());
        id
    }

    pub fn mark_failed(&mut self, local_id: &str, error: String) {
        if let Some(m) = self.by_id.get_mut(local_id) {
            m.delivery = Delivery::Failed(error);
        }
    }

    pub fn mark_sending(&mut self, local_id: &str) {
        if let Some(m) = self.by_id.get_mut(local_id) {
            m.delivery = Delivery::Sending;
        }
    }

    /// Removes a failed bubble the owner gave up on. Only a local bubble can go.
    pub fn discard_local(&mut self, local_id: &str) {
        if local_id.starts_with(LOCAL_PREFIX) {
            self.by_id.remove(local_id);
            self.order.retain(|x| x != local_id);
        }
    }

    /// Takes in what `POST /v1/chat` answered. Lucina's copy may already be here from the stream, in which case it only gets
    /// the job; otherwise a local reply bubble stands in until the timeline's copy replaces it.
    pub fn apply_reply(&mut self, local_id: &str, client_message_id: &str, reply: ChatReply, now_ms: i64) -> Option<String> {
        if let Some(m) = self.by_id.get_mut(local_id) {
            m.delivery = Delivery::Sent;
        }
        let user_ts = self.by_id.get(local_id).map(|m| m.item.ts_ms);
        // Adopt the server's id for the owner's bubble now, so a reply quoting it finds it.
        if let Some(user_id) = &reply.user_item_id {
            if let Some(m) = self.by_id.get(local_id) {
                let mut item = m.item.clone();
                item.id = user_id.clone();
                self.merge(vec![item]);
            }
        }
        let existing = reply
            .lucina_item_id
            .as_ref()
            .filter(|id| self.by_id.contains_key(*id))
            .cloned()
            .or_else(|| {
                let user_id = reply.user_item_id.as_deref();
                self.messages()
                    .find(|m| {
                        m.item.lucina
                            && !m.item.id.starts_with(LOCAL_PREFIX)
                            && (m.item.client_message_id.as_deref() == Some(client_message_id)
                                || (user_id.is_some() && m.item.reply_to.as_deref() == user_id))
                    })
                    .map(|m| m.item.id.clone())
            });
        if let Some(id) = existing {
            if let Some(m) = self.by_id.get_mut(&id) {
                m.job = reply.job;
                if m.item.text.is_empty() {
                    m.item.text = reply.reply;
                }
                if m.item.rich.is_none() {
                    m.item.rich = reply.rich;
                }
            }
            return Some(id);
        }
        if reply.reply.is_empty() && reply.job.is_none() {
            return None;
        }
        let id = reply.lucina_item_id.clone().unwrap_or_else(|| format!("{LOCAL_PREFIX}r-{client_message_id}"));
        let ts = user_ts.map_or(now_ms, |t| now_ms.max(t + 1));
        let item = Item {
            id: id.clone(),
            ts_ms: ts,
            channel: "web".into(),
            lucina: true,
            text: reply.reply,
            client_message_id: Some(client_message_id.to_owned()),
            reply_to: reply.user_item_id,
            rich: reply.rich,
            ..Item::default()
        };
        self.merge(vec![item]);
        if let Some(m) = self.by_id.get_mut(&id) {
            m.job = reply.job;
            if reply.sensitive {
                m.sensitive_until_ms = Some(reply.expires_at_ms.unwrap_or(now_ms + 120_000));
            }
        }
        Some(id)
    }

    /// A finished job's result is the reply's text (`state/chat.ts`).
    pub fn apply_job(&mut self, job: Job) -> bool {
        let Some(m) = self.by_id.values_mut().find(|m| m.job.as_ref().is_some_and(|j| j.id == job.id)) else {
            return false;
        };
        if job.status == "done" {
            if let Some(r) = &job.result {
                if m.item.text.is_empty() {
                    m.item.text = r.clone();
                }
            }
        }
        m.job = Some(job);
        true
    }

    /// Unfinished jobs, for the poller.
    pub fn pending_jobs(&self) -> Vec<String> {
        self.by_id.values().filter_map(|m| m.job.as_ref()).filter(|j| !j.finished()).map(|j| j.id.clone()).collect()
    }

    /// An OTP's time is up: its text goes from memory too. True if anything changed.
    pub fn expire_sensitive(&mut self, now_ms: i64) -> bool {
        let mut changed = false;
        for m in self.by_id.values_mut() {
            if m.sensitive_until_ms.is_some_and(|t| now_ms >= t) {
                zeroize::Zeroize::zeroize(&mut m.item.text);
                m.item.text = "OTP expired.".into();
                m.sensitive_until_ms = None;
                changed = true;
            }
        }
        changed
    }

    /// `docs/lucina-message-format.md`: a later owner message equal to a button's text counts as choosing it, so a choice
    /// survives reloads and other devices. The button's id, if so.
    pub fn choice_of(&self, message_id: &str) -> Option<String> {
        let m = self.by_id.get(message_id)?;
        if let Some(c) = &m.chosen {
            return Some(c.clone());
        }
        let rich = m.item.rich.as_ref()?;
        let pos = self.order.iter().position(|x| x == message_id)?;
        let next_user = self.order[pos + 1..].iter().filter_map(|id| self.by_id.get(id)).find(|m| !m.item.lucina)?;
        let said = next_user.item.text.trim().to_lowercase();
        rich.actions.iter().find(|a| a.text().is_some_and(|t| t.trim().to_lowercase() == said)).map(|a| a.id.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn attachment_durations_follow_the_attachments() {
        let v = serde_json::json!({"id": "a1", "role": "user", "text": "", "attachments": [
            {"kind": "voice", "name": "voice-note.wav", "durationMs": 5000}, {"kind": "pdf", "name": "r.pdf"}]});
        let item = Item::parse(&v).unwrap();
        assert_eq!(item.attachments.len(), 2);
        assert_eq!(item.attachment_ms, vec![Some(5000), None]);
    }

    fn server(id: &str, ts: i64, lucina: bool, text: &str) -> Item {
        Item { id: id.into(), ts_ms: ts, channel: "web".into(), lucina, text: text.into(), ..Item::default() }
    }

    fn texts(t: &Timeline) -> Vec<String> {
        t.messages().map(|m| m.item.text.clone()).collect()
    }

    #[test]
    fn parse_takes_numbers_iso_times_and_skips_other_roles() {
        let i = Item::parse(&json!({"id": 12, "ts": "2026-10-22T07:30:00.000Z", "role": "user", "channel": "ios", "text": "x"})).unwrap();
        assert_eq!(i.id, "12");
        assert_eq!(i.ts_ms, parse_rfc3339_ms("2026-10-22T09:30:00+02:00").unwrap());
        assert_eq!(parse_rfc3339_ms("2026-01-01T00:00:00Z"), Some(1_767_225_600_000));
        assert!(Item::parse(&json!({"id": "a", "role": "system"})).is_none());
        assert!(Item::parse(&json!({"role": "user"})).is_none());
    }

    #[test]
    fn conversation_keys_follow_the_contract() {
        let item = |v: Value| Item::parse(&v).unwrap().conversation_key();
        assert_eq!(item(json!({"id":"a","role":"user","channel":"web","meta":{"via":"mcp","caller":"Claude Code!"}})), "agent:claudecode");
        assert_eq!(item(json!({"id":"a","role":"user","channel":"ios","meta":{"device":"dev_1"}})), "device:dev_1");
        assert_eq!(item(json!({"id":"a","role":"user","channel":"web"})), "legacy:web");
        assert_eq!(item(json!({"id":"a","role":"user","channel":"telegram"})), "telegram");
        assert_eq!(item(json!({"id":"a","role":"lucina","channel":"other"})), "lucina");
        let mine = Item::parse(&json!({"id":"a","role":"user","channel":"web","meta":{"device":"dev_1"}})).unwrap();
        assert!(mine.is_mine(Some("dev_1")));
        assert!(!mine.is_mine(Some("dev_2")));
    }

    #[test]
    fn the_server_copy_replaces_the_local_bubble_whichever_comes_first() {
        // The stream's copy first, then the POST's answer.
        let mut t = Timeline::default();
        let local = t.add_local("ciao", "c1", None, 1000);
        let mut user = server("tl_1", 1000, false, "ciao");
        user.client_message_id = Some("c1".into());
        let mut lucina = server("tl_2", 1001, true, "ciao!");
        lucina.reply_to = Some("tl_1".into());
        t.merge(vec![user.clone(), lucina.clone()]);
        assert_eq!(texts(&t), ["ciao", "ciao!"]);
        assert!(t.get(&local).is_none());
        let reply = ChatReply { reply: "ciao!".into(), user_item_id: Some("tl_1".into()), lucina_item_id: Some("tl_2".into()), ..ChatReply::default() };
        assert_eq!(t.apply_reply(&local, "c1", reply, 2000).as_deref(), Some("tl_2"));
        assert_eq!(texts(&t), ["ciao", "ciao!"]);

        // The POST's answer first, then the stream's copy.
        let mut t = Timeline::default();
        let local = t.add_local("ciao", "c1", None, 1000);
        let reply = ChatReply { reply: "ciao!".into(), user_item_id: Some("tl_1".into()), lucina_item_id: Some("tl_2".into()), ..ChatReply::default() };
        t.apply_reply(&local, "c1", reply, 2000);
        assert_eq!(texts(&t), ["ciao", "ciao!"]);
        t.merge(vec![user, lucina]);
        assert_eq!(texts(&t), ["ciao", "ciao!"]);
        assert!(t.messages().all(|m| m.delivery == Delivery::Sent));
    }

    #[test]
    fn a_reply_without_ids_stands_in_until_the_timeline_copy_comes() {
        let mut t = Timeline::default();
        let local = t.add_local("ciao", "c1", None, 1000);
        t.apply_reply(&local, "c1", ChatReply { reply: "ciao!".into(), ..ChatReply::default() }, 2000);
        assert_eq!(texts(&t), ["ciao", "ciao!"]);
        let mut user = server("tl_1", 1000, false, "ciao");
        user.client_message_id = Some("c1".into());
        let mut lucina = server("tl_2", 1500, true, "ciao!");
        lucina.client_message_id = Some("c1".into());
        t.merge(vec![user, lucina]);
        assert_eq!(texts(&t), ["ciao", "ciao!"]);
        assert_eq!(t.len(), 2);
    }

    #[test]
    fn a_bubble_never_sorts_above_what_is_on_screen() {
        let mut t = Timeline::default();
        t.merge(vec![server("tl_1", 5000, true, "later")]);
        t.add_local("now", "c1", None, 1000);
        assert_eq!(texts(&t), ["later", "now"]);
    }

    #[test]
    fn failed_stays_with_its_text_and_can_go() {
        let mut t = Timeline::default();
        let l = t.add_local("x", "c1", None, 1);
        t.mark_failed(&l, "No connection".into());
        assert_eq!(t.get(&l).unwrap().delivery, Delivery::Failed("No connection".into()));
        t.merge(vec![server("tl_9", 2, true, "other")]);
        assert_eq!(t.len(), 2);
        t.discard_local(&l);
        assert_eq!(texts(&t), ["other"]);
        t.discard_local("tl_9");
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn an_otp_goes_from_memory_when_it_expires() {
        let mut t = Timeline::default();
        let l = t.add_local("code?", "c1", None, 1);
        let reply = ChatReply { reply: "123456".into(), sensitive: true, expires_at_ms: Some(500), ..ChatReply::default() };
        let id = t.apply_reply(&l, "c1", reply, 100).unwrap();
        assert!(!t.expire_sensitive(499));
        assert_eq!(t.get(&id).unwrap().item.text, "123456");
        assert!(t.expire_sensitive(500));
        assert_eq!(t.get(&id).unwrap().item.text, "OTP expired.");
    }

    #[test]
    fn older_pages_go_by_the_stored_cursor() {
        let mut t = Timeline::default();
        t.merge(vec![server("tl_5", 5, true, "e"), server("tl_6", 6, true, "f")]);
        t.note_first_page(Some("tl_5".into()));
        assert!(t.has_older());
        assert_eq!(t.older_cursor().as_deref(), Some("tl_5"));
        t.merge(vec![server("tl_4", 4, true, "d")]);
        t.note_older_page(None);
        assert!(!t.has_older());
        assert_eq!(texts(&t), ["d", "e", "f"]);
    }

    #[test]
    fn a_snapshot_comes_back_without_otps_or_unsent_bubbles() {
        let raw = |id: &str, ts: i64| Item::parse(&json!({"id": id, "ts": ts, "role": "lucina", "channel": "other", "text": id})).unwrap();
        let mut t = Timeline::default();
        t.merge(vec![raw("tl_1", 1), raw("tl_2", 2), raw("tl_3", 3)]);
        t.cursor = Some("tl_3".into());
        let l = t.add_local("unsent", "c1", None, 10);
        let otp = t.apply_reply(&l, "c1", ChatReply { reply: "123456".into(), sensitive: true, expires_at_ms: Some(99), ..ChatReply::default() }, 11).unwrap();
        assert!(t.get(&otp).is_some());
        let mut back = Timeline::default();
        back.restore(&t.snapshot(2));
        assert_eq!(texts(&back), ["tl_2", "tl_3"]);
        assert_eq!(back.cursor.as_deref(), Some("tl_3"));
        assert_eq!(back.older_cursor().as_deref(), Some("tl_2"));
        assert!(!t.snapshot(9).to_string().contains("123456"));
    }

    #[test]
    fn a_job_result_fills_the_reply() {
        let mut t = Timeline::default();
        let l = t.add_local("draw", "c1", None, 1);
        let job = Job { id: "j1".into(), status: "queued".into(), ..Job::default() };
        let id = t.apply_reply(&l, "c1", ChatReply { job: Some(job), ..ChatReply::default() }, 2).unwrap();
        assert_eq!(t.pending_jobs(), ["j1"]);
        assert!(t.apply_job(Job { id: "j1".into(), status: "done".into(), result: Some("Here".into()), ..Job::default() }));
        assert_eq!(t.get(&id).unwrap().item.text, "Here");
        assert!(t.pending_jobs().is_empty());
    }
}
