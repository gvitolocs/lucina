//! The `/v1` routes Juno's apps use (`docs/server-api.md` in SecretaryIOS): blocking, one [`Client`] per thread or shared,
//! with `Authorization: Bearer` when signed in. Errors carry the server's `code` and never a URL, so a poll token cannot
//! reach a log.
use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use ureq::http::Response;
use ureq::Body;

use crate::credential::Token;
use crate::decisions::{Approval, Decision};
use crate::secrets::{SaveSecret, SecretQueue, SecretReceipt};
use crate::sse;
use crate::timeline::{ChatReply, Conversation, Item, Job, TimelinePage};

pub const DEFAULT_BASE: &str = "https://getsecretary.tech";
/// A different server (the tests' and the demos' fake one): `http` is accepted for loopback only.
pub const BASE_ENV: &str = "LUCINA_SECRETARY_URL";
pub const USER_AGENT: &str = concat!("LucinaOS-Juno lucina-account/", env!("CARGO_PKG_VERSION"));
/// The label the server gives a client's messages. Only `web` and `ios` exist; Juno's device id tells it apart.
const CLIENT_CHANNEL: &str = "web";
/// An LLM turn can take the server's 120 s.
const CHAT_TIMEOUT: Duration = Duration::from_secs(130);
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);
const SECRET_TIMEOUT: Duration = Duration::from_secs(60);
/// One stream connection lives this long, then resumes with `Last-Event-ID`. ureq's only read deadline covers the whole
/// body, so this is also how long a silently dead connection can go unnoticed (Lucina pings every 20 s).
const STREAM_TIMEOUT: Duration = Duration::from_secs(120);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(3600);
/// [`Client::post_stream`]: the local model can take minutes to load before its first word.
const LONG_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiError {
    /// 401: revoked, unknown or missing token.
    Unauthorized(String),
    /// 429: try again after this many seconds.
    RateLimited(u64),
    /// Another non-2xx answer: status, code, message.
    Http(u16, String, String),
    /// No answer (DNS, connect, TLS, timeout): the request may or may not have arrived.
    Network(String),
    /// An answer that does not follow the contract.
    Protocol(String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::Unauthorized(m) => write!(f, "Signed out: {m}"),
            ApiError::RateLimited(s) => write!(f, "Too many requests; wait {s} s"),
            ApiError::Http(s, c, m) if m.is_empty() => write!(f, "{c} ({s})"),
            ApiError::Http(_, _, m) => f.write_str(m),
            ApiError::Network(m) => write!(f, "No connection: {m}"),
            ApiError::Protocol(m) => write!(f, "Unexpected answer: {m}"),
        }
    }
}

impl ApiError {
    pub fn status(&self) -> Option<u16> {
        match self {
            ApiError::Unauthorized(_) => Some(401),
            ApiError::RateLimited(_) => Some(429),
            ApiError::Http(s, _, _) => Some(*s),
            _ => None,
        }
    }

    pub fn code(&self) -> Option<&str> {
        match self {
            ApiError::Http(_, c, _) => Some(c),
            _ => None,
        }
    }

    /// `web/src/state/chat.ts` `describeChatError`.
    pub fn chat_text(&self) -> String {
        match self.code() {
            Some("lucina_unreachable") => return "Lucina is offline. Your message was not delivered.".into(),
            Some("lucina_timeout") => return "Lucina took too long to answer.".into(),
            Some("lucina_error") => return "Lucina had a problem with that.".into(),
            _ => {}
        }
        match self {
            ApiError::Network(m) if m.contains("timeout") || m.contains("timed out") => "No answer in time.".into(),
            ApiError::Network(_) => "No connection. Your message was not delivered.".into(),
            ApiError::Unauthorized(_) => "This laptop is signed out.".into(),
            ApiError::RateLimited(_) => "Too many messages; wait a moment.".into(),
            e => e.to_string(),
        }
    }
}

/// 2026-01-01: before this the clock is wrong, not the certificate. Juno starts in 1970 until `lucina-timesync` has run.
const CLOCK_FLOOR_SECS: u64 = 1_767_225_600;

/// Has this machine learned the date yet? Certificates cannot be checked without it.
pub fn clock_is_set() -> bool {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).is_ok_and(|d| d.as_secs() >= CLOCK_FLOOR_SECS)
}

fn network(e: ureq::Error) -> ApiError {
    if !clock_is_set() && e.to_string().contains("certificate") {
        return ApiError::Network("the clock is not set yet, so certificates cannot be checked".into());
    }
    // Everything after a '?' goes: a query can hold a poll token.
    let text = e.to_string();
    let clean = text.split('?').next().unwrap_or("").to_owned();
    ApiError::Network(clean)
}

/// A refusal's code and sentence. Most routes answer `{"error": {"code", "message"}}`; the local modules (Tasks) answer
/// `{"ok": false, "error": "<sentence>", "code"}`.
fn error_fields(v: &Value) -> (String, String) {
    let e = &v["error"];
    let code = e["code"].as_str().or(v["code"].as_str()).unwrap_or("error").to_owned();
    let message = e["message"].as_str().or(e.as_str()).unwrap_or("").to_owned();
    (code, message)
}

/// `GET /v1/session/qr` of a device request.
#[derive(Clone, PartialEq, Eq)]
pub struct QrRequest {
    pub approve_url: String,
    pub short_code: Option<String>,
    pub(crate) poll_token: String,
    pub expires_in: u64,
}

impl std::fmt::Debug for QrRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "QrRequest(expires in {} s)", self.expires_in)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum QrPoll {
    Pending,
    Denied,
    /// Expired, already collected or unknown.
    Expired,
    Approved(Token),
}

/// `GET /v1/session`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Session {
    pub authenticated: bool,
    pub device_id: Option<String>,
    pub device_name: Option<String>,
}

/// `GET /v1/timeline`'s filters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TimelineQuery {
    pub before: Option<String>,
    pub after: Option<String>,
    /// `Some(key)`: one conversation; `None`: this device's "mine".
    pub conversation: Option<String>,
    pub limit: u32,
}

#[derive(Clone)]
pub struct Client {
    agent: ureq::Agent,
    stream_agent: ureq::Agent,
    base: Arc<str>,
    token: Option<Token>,
}

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .user_agent(USER_AGENT)
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_global(Some(timeout))
        .build()
        .into()
}

fn loopback(base: &str) -> bool {
    let host = base.strip_prefix("http://").unwrap_or("");
    ["127.0.0.1", "localhost", "[::1]"].iter().any(|h| host == *h || host.starts_with(&format!("{h}:")))
}

fn query_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

impl Client {
    /// `None` for a base that would send a token in clear: only `https`, or `http` to this machine.
    pub fn new(base: &str, token: Option<Token>) -> Option<Client> {
        let base = base.trim_end_matches('/');
        if !base.starts_with("https://") && !loopback(base) {
            return None;
        }
        Some(Client { agent: agent(DEFAULT_TIMEOUT), stream_agent: agent(STREAM_TIMEOUT), base: base.into(), token })
    }

    /// The account server: [`BASE_ENV`] or [`DEFAULT_BASE`].
    pub fn from_env(token: Option<Token>) -> Option<Client> {
        match std::env::var(BASE_ENV) {
            Ok(b) => Client::new(&b, token),
            Err(_) => Client::new(DEFAULT_BASE, token),
        }
    }

    pub fn signed_in(&self) -> bool {
        self.token.is_some()
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// A `/v1` path as a WebSocket address: `https` becomes `wss`, and the loopback `http` that [`Client::new`] allows becomes
    /// `ws`. The voice socket (`/v1/voice/ws`) is opened from this, with [`Client::bearer`] on the upgrade.
    pub fn ws_url(&self, path: &str) -> String {
        let url = self.url(path);
        match url.strip_prefix("https://") {
            Some(rest) => format!("wss://{rest}"),
            None => format!("ws://{}", url.strip_prefix("http://").unwrap_or(&url)),
        }
    }

    /// The token, for the one route that is not HTTP request and response (the voice socket). Handle it as it is: never log it.
    pub fn bearer(&self) -> Option<&Token> {
        self.token.as_ref()
    }

    fn get(&self, path: &str) -> ureq::RequestBuilder<ureq::typestate::WithoutBody> {
        let r = self.agent.get(self.url(path)).header("X-Secretary-Client", CLIENT_CHANNEL);
        match &self.token {
            Some(t) => r.header("Authorization", format!("Bearer {}", t.expose())),
            None => r,
        }
    }

    fn post(&self, path: &str) -> ureq::RequestBuilder<ureq::typestate::WithBody> {
        let r = self.agent.post(self.url(path)).header("X-Secretary-Client", CLIENT_CHANNEL);
        match &self.token {
            Some(t) => r.header("Authorization", format!("Bearer {}", t.expose())),
            None => r,
        }
    }

    fn json(resp: Result<Response<Body>, ureq::Error>) -> Result<Value, ApiError> {
        let mut resp = resp.map_err(network)?;
        let status = resp.status().as_u16();
        let retry = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok());
        let text = resp.body_mut().read_to_string().map_err(network)?;
        let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        if (200..300).contains(&status) {
            return if value.is_null() { Err(ApiError::Protocol("not JSON".into())) } else { Ok(value) };
        }
        let (code, message) = error_fields(&value);
        Err(match status {
            401 => ApiError::Unauthorized(message),
            429 => ApiError::RateLimited(retry.unwrap_or(60)),
            _ => ApiError::Http(status, code, message),
        })
    }

    /// Any other `GET` under `/v1` (`path` starts with it), for an app with its own routes: the body as JSON, errors as above.
    /// Blocking: call it on a thread (`tx.spawn`), never in `view`.
    pub fn get_json(&self, path: &str) -> Result<Value, ApiError> {
        Self::json(self.get(path).call())
    }

    /// [`Self::get_json`] with its own time limit instead of the usual 15 s, for a long poll that the server holds open.
    pub fn get_json_within(&self, path: &str, timeout: Duration) -> Result<Value, ApiError> {
        Self::json(self.get(path).config().timeout_global(Some(timeout)).build().call())
    }

    /// A file's answer that is not the file: the error it names. A 429 says how long to wait, as for JSON.
    fn file_status(resp: &mut Response<Body>) -> Result<(), ApiError> {
        let status = resp.status().as_u16();
        if (200..300).contains(&status) {
            return Ok(());
        }
        let retry = resp.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<u64>().ok());
        let text = resp.body_mut().read_to_string().unwrap_or_default();
        let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        let (code, message) = error_fields(&value);
        Err(match status {
            401 => ApiError::Unauthorized(message),
            429 => ApiError::RateLimited(retry.unwrap_or(60)),
            _ => ApiError::Http(status, code, message),
        })
    }

    /// A `GET` whose answer is a file, up to `limit` bytes. Blocking, like the others.
    pub fn get_bytes(&self, path: &str, limit: u64) -> Result<Vec<u8>, ApiError> {
        let mut resp = self.get(path).call().map_err(network)?;
        Self::file_status(&mut resp)?;
        resp.body_mut().with_config().limit(limit).read_to_vec().map_err(network)
    }

    /// A `GET` whose answer is a file, up to `limit` bytes, with one of its response headers (`/v1/vault/file` sends the
    /// file's revision in `X-Vault-Revision`). Blocking, like the others.
    pub fn get_file_with(&self, path: &str, limit: u64, header: &str) -> Result<(Vec<u8>, Option<String>), ApiError> {
        let mut r = agent(DOWNLOAD_TIMEOUT).get(self.url(path)).header("X-Secretary-Client", CLIENT_CHANNEL);
        if let Some(t) = &self.token {
            r = r.header("Authorization", format!("Bearer {}", t.expose()));
        }
        let mut resp = r.call().map_err(network)?;
        Self::file_status(&mut resp)?;
        let value = resp.headers().get(header).and_then(|v| v.to_str().ok()).map(str::to_owned);
        let bytes = resp.body_mut().with_config().limit(limit).read_to_vec().map_err(network)?;
        Ok((bytes, value))
    }

    /// `method` (`PUT` or `POST`) with raw bytes as the body and a JSON answer, under the same rules as [`Client::post_json`]: a
    /// refusal comes back as [`ApiError::Http`] with its status (`409` for a compare-and-swap that lost). Blocking.
    pub fn send_bytes(&self, method: &str, path: &str, bytes: &[u8]) -> Result<Value, ApiError> {
        let m = ureq::http::Method::from_bytes(method.as_bytes()).map_err(|_| ApiError::Protocol("bad method".into()))?;
        let mut req = ureq::http::Request::builder()
            .method(m)
            .uri(self.url(path))
            .header("X-Secretary-Client", CLIENT_CHANNEL)
            .header("Content-Type", "application/octet-stream");
        if let Some(t) = &self.token {
            req = req.header("Authorization", format!("Bearer {}", t.expose()));
        }
        let req = req.body(bytes.to_vec()).map_err(|_| ApiError::Protocol("bad request".into()))?;
        Self::json(agent(DOWNLOAD_TIMEOUT).run(req))
    }

    /// A `GET` whose answer may be large: written to `out` a piece at a time, so it costs no memory. `progress` gets the bytes so
    /// far, and ends it by answering false. Blocking, like the others, and with an hour to finish: a film is not done in a minute.
    pub fn download(&self, path: &str, out: &mut dyn std::io::Write, progress: &mut dyn FnMut(u64) -> bool) -> Result<u64, ApiError> {
        let mut r = agent(DOWNLOAD_TIMEOUT).get(self.url(path)).header("X-Secretary-Client", CLIENT_CHANNEL);
        if let Some(t) = &self.token {
            r = r.header("Authorization", format!("Bearer {}", t.expose()));
        }
        let mut resp = r.call().map_err(network)?;
        Self::file_status(&mut resp)?;
        let mut reader = resp.body_mut().with_config().limit(u64::MAX).reader();
        let mut buf = vec![0u8; 64 << 10];
        let mut total = 0u64;
        loop {
            let n = std::io::Read::read(&mut reader, &mut buf).map_err(|e| ApiError::Network(e.to_string()))?;
            if n == 0 {
                return Ok(total);
            }
            out.write_all(&buf[..n]).map_err(|e| ApiError::Protocol(e.to_string()))?;
            total += n as u64;
            if !progress(total) {
                return Err(ApiError::Protocol("stopped".into()));
            }
        }
    }

    /// A chunk of a resumable upload (`web/src/api/memos.ts`): `PUT` with `Upload-Offset`. Gives the server's byte count, also from the `409` it
    /// answers a wrong offset with, so that the caller goes on where the server is. Blocking, like the others.
    pub fn put_upload(&self, path: &str, offset: u64, chunk: &[u8]) -> Result<u64, ApiError> {
        let mut req = ureq::http::Request::builder()
            .method(ureq::http::Method::PUT)
            .uri(self.url(path))
            .header("X-Secretary-Client", CLIENT_CHANNEL)
            .header("Upload-Offset", offset.to_string())
            .header("Content-Type", "application/octet-stream");
        if let Some(t) = &self.token {
            req = req.header("Authorization", format!("Bearer {}", t.expose()));
        }
        let req = req.body(chunk.to_vec()).map_err(|_| ApiError::Protocol("bad request".into()))?;
        let mut resp = self.agent.run(req).map_err(network)?;
        let status = resp.status().as_u16();
        let text = resp.body_mut().read_to_string().map_err(network)?;
        let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        if (200..300).contains(&status) || status == 409 {
            if let Some(received) = value["receivedBytes"].as_u64() {
                return Ok(received);
            }
        }
        let (code, message) = error_fields(&value);
        Err(match status {
            401 => ApiError::Unauthorized(message),
            429 => ApiError::RateLimited(60),
            _ => ApiError::Http(status, code, message),
        })
    }

    /// A piece of a file: up to `len` bytes at `start`, with the file's whole size. Blocking, like the others.
    pub fn get_range(&self, path: &str, start: u64, len: u64) -> Result<(Vec<u8>, u64), ApiError> {
        let range = format!("bytes={start}-{}", start + len.saturating_sub(1));
        let mut resp = self.get(path).header("Range", range).call().map_err(network)?;
        let status = resp.status().as_u16();
        if status == 416 {
            return Ok((Vec::new(), start));
        }
        if !(200..300).contains(&status) {
            let text = resp.body_mut().read_to_string().unwrap_or_default();
            let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
            let code = value["error"]["code"].as_str().unwrap_or("error").to_owned();
            let message = value["error"]["message"].as_str().unwrap_or("").to_owned();
            return Err(if status == 401 { ApiError::Unauthorized(message) } else { ApiError::Http(status, code, message) });
        }
        let total = resp.headers().get("content-range").and_then(|v| v.to_str().ok()).and_then(|v| v.rsplit('/').next()?.parse::<u64>().ok());
        let bytes = resp.body_mut().with_config().limit(len.max(1)).read_to_vec().map_err(network)?;
        // A server that ignored the range sent the whole file from its start: not this function's answer to give.
        if status == 200 && start > 0 {
            return Err(ApiError::Protocol("the server does not read parts of files".into()));
        }
        let total = total.unwrap_or(start + bytes.len() as u64);
        Ok((bytes, total))
    }

    /// Any other `POST` with a JSON body, under the same rules.
    pub fn post_json(&self, path: &str, body: &Value) -> Result<Value, ApiError> {
        Self::json(self.post(path).send_json(body))
    }

    /// [`Self::post_json`] with its own time limit, for a body that takes long to send (a screenshot over a slow link).
    pub fn post_json_within(&self, path: &str, body: &Value, timeout: Duration) -> Result<Value, ApiError> {
        Self::json(self.post(path).config().timeout_global(Some(timeout)).build().send_json(body))
    }

    /// A `POST` that may take minutes, such as a model writing a reply: each server-sent event goes to `on_event` as it arrives
    /// (event name, data), and the answer is `None`; a server that answers plain JSON instead gives `Some`. Blocking, like the others.
    pub fn post_stream(&self, path: &str, body: &Value, on_event: &mut dyn FnMut(&str, &str)) -> Result<Option<Value>, ApiError> {
        let mut r = agent(LONG_TIMEOUT).post(self.url(path)).header("X-Secretary-Client", CLIENT_CHANNEL);
        r = r.header("Accept", "text/event-stream, application/json");
        if let Some(t) = &self.token {
            r = r.header("Authorization", format!("Bearer {}", t.expose()));
        }
        let resp = r.send_json(body).map_err(network)?;
        let streaming = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|c| c.contains("text/event-stream"));
        if !streaming || !(200..300).contains(&resp.status().as_u16()) {
            return Self::json(Ok(resp)).map(Some);
        }
        let mut reader = resp.into_body().into_reader();
        let mut parser = sse::Parser::default();
        let mut buf = [0u8; 8192];
        loop {
            let n = reader.read(&mut buf).map_err(|e| ApiError::Network(e.kind().to_string()))?;
            if n == 0 {
                return Ok(None);
            }
            for block in parser.push(&buf[..n]) {
                on_event(&block.event, &block.data);
            }
        }
    }

    /// `PUT`, `PATCH` or `DELETE`, with a JSON body, under the same rules. A `204` with no body answers `Null`.
    pub fn send_json(&self, method: &str, path: &str, body: &Value) -> Result<Value, ApiError> {
        let m = ureq::http::Method::from_bytes(method.as_bytes()).map_err(|_| ApiError::Protocol("bad method".into()))?;
        let mut req = ureq::http::Request::builder()
            .method(m)
            .uri(self.url(path))
            .header("X-Secretary-Client", CLIENT_CHANNEL)
            .header("Content-Type", "application/json");
        if let Some(t) = &self.token {
            req = req.header("Authorization", format!("Bearer {}", t.expose()));
        }
        let req = req.body(body.to_string()).map_err(|_| ApiError::Protocol("bad request".into()))?;
        match self.agent.run(req) {
            Ok(r) if r.status().as_u16() == 204 => Ok(Value::Null),
            other => Self::json(other),
        }
    }

    /// `GET /v1/health`. Public: the first check that DNS, TLS and the clock are right.
    pub fn health(&self) -> Result<Value, ApiError> {
        Self::json(self.get("/v1/health").call())
    }

    /// `GET /v1/session`: is this token still good, and which device is this.
    pub fn session(&self) -> Result<Session, ApiError> {
        let v = Self::json(self.get("/v1/session").call())?;
        Ok(Session {
            authenticated: v["authenticated"].as_bool() == Some(true),
            device_id: v["deviceId"].as_str().map(str::to_owned),
            device_name: v["deviceName"].as_str().map(str::to_owned),
        })
    }

    /// `POST /v1/session/qr` as a device. Counts as a login attempt of this address.
    pub fn qr_start(&self, device_name: &str) -> Result<QrRequest, ApiError> {
        let v = Self::json(self.post("/v1/session/qr").send_json(json!({ "device": { "name": device_name } })))?;
        let field = |k: &str| v[k].as_str().map(str::to_owned).ok_or_else(|| ApiError::Protocol(format!("no {k}")));
        Ok(QrRequest {
            approve_url: field("approveUrl")?,
            short_code: v["shortCode"].as_str().map(str::to_owned),
            poll_token: field("pollToken")?,
            expires_in: v["expiresIn"].as_u64().unwrap_or(120),
        })
    }

    /// `GET /v1/session/qr/poll`. The answer that says `approved` is the only one that ever holds this token.
    pub fn qr_poll(&self, poll_token: &str) -> Result<QrPoll, ApiError> {
        let v = Self::json(self.get(&format!("/v1/session/qr/poll?t={}", query_escape(poll_token))).call())?;
        match v["status"].as_str() {
            Some("pending") => Ok(QrPoll::Pending),
            Some("denied") => Ok(QrPoll::Denied),
            Some("expired") => Ok(QrPoll::Expired),
            Some("approved") => v["token"]
                .as_str()
                .and_then(Token::parse)
                .map(QrPoll::Approved)
                .ok_or_else(|| ApiError::Protocol("approved without a device token".into())),
            _ => Err(ApiError::Protocol("unknown sign-in status".into())),
        }
    }

    /// `GET /v1/timeline`. A 410 means the cursor expired; [`crate::engine`] then starts a fresh window.
    pub fn timeline(&self, q: &TimelineQuery) -> Result<TimelinePage, ApiError> {
        let mut path = format!("/v1/timeline?limit={}", q.limit.clamp(1, 200));
        match &q.conversation {
            Some(c) => path += &format!("&conversation={}", query_escape(c)),
            None => path += "&scope=mine",
        }
        if let Some(b) = &q.before {
            path += &format!("&before={}", query_escape(b));
        }
        if let Some(a) = &q.after {
            path += &format!("&after={}", query_escape(a));
        }
        Ok(TimelinePage::parse(&Self::json(self.get(&path).call())?))
    }

    /// `GET /v1/timeline/conversations`.
    pub fn conversations(&self) -> Result<Vec<Conversation>, ApiError> {
        let v = Self::json(self.get("/v1/timeline/conversations").call())?;
        Ok(v["items"].as_array().map(|a| a.iter().filter_map(Conversation::parse).collect()).unwrap_or_default())
    }

    /// `POST /v1/chat`. A retry with the same `client_message_id` within 10 minutes returns the first answer.
    pub fn send_chat(
        &self,
        message: &str,
        session_id: &str,
        client_message_id: &str,
        reply_to: Option<&str>,
    ) -> Result<ChatReply, ApiError> {
        let mut body = json!({ "message": message, "sessionId": session_id, "clientMessageId": client_message_id });
        if let Some(r) = reply_to {
            body["replyTo"] = r.into();
        }
        let resp = self
            .post("/v1/chat")
            .config()
            .timeout_global(Some(CHAT_TIMEOUT))
            .build()
            .send_json(body);
        Ok(ChatReply::parse(&Self::json(resp)?))
    }

    /// `GET /v1/jobs/{id}`.
    pub fn job(&self, id: &str) -> Result<Job, ApiError> {
        Ok(Job::parse(&Self::json(self.get(&format!("/v1/jobs/{}", query_escape(id))).call())?))
    }

    /// Opens `GET /v1/timeline/stream`, resuming after `last_id`. Errors before the stream starts are ordinary ones.
    pub fn stream(&self, last_id: Option<&str>) -> Result<TimelineStream, ApiError> {
        let mut r = self.stream_agent.get(self.url("/v1/timeline/stream")).header("Accept", "text/event-stream");
        r = r.header("X-Secretary-Client", CLIENT_CHANNEL);
        if let Some(t) = &self.token {
            r = r.header("Authorization", format!("Bearer {}", t.expose()));
        }
        if let Some(id) = last_id {
            r = r.header("Last-Event-ID", id);
        }
        let resp = r.call().map_err(network)?;
        if resp.status().as_u16() != 200 {
            return Err(Self::json(Ok(resp)).err().unwrap_or(ApiError::Protocol("no stream".into())));
        }
        Ok(TimelineStream { body: Box::new(resp.into_body().into_reader()), parser: sse::Parser::default(), queue: Vec::new() })
    }

    /// Opens any other server-sent stream under `/v1`, such as `/v1/agents/stream`, resuming after `last_id`.
    pub fn sse(&self, path: &str, last_id: Option<&str>) -> Result<SseStream, ApiError> {
        let mut r = self.stream_agent.get(self.url(path)).header("Accept", "text/event-stream");
        r = r.header("X-Secretary-Client", CLIENT_CHANNEL);
        if let Some(t) = &self.token {
            r = r.header("Authorization", format!("Bearer {}", t.expose()));
        }
        if let Some(id) = last_id {
            r = r.header("Last-Event-ID", id);
        }
        let resp = r.call().map_err(network)?;
        if resp.status().as_u16() != 200 {
            return Err(Self::json(Ok(resp)).err().unwrap_or(ApiError::Protocol("no stream".into())));
        }
        Ok(SseStream { body: Box::new(resp.into_body().into_reader()), parser: sse::Parser::default(), queue: std::collections::VecDeque::new() })
    }

    /// `GET /v1/decisions?status=&limit=`.
    pub fn decisions(&self, status: Option<&str>, limit: u32) -> Result<Vec<Decision>, ApiError> {
        let mut path = format!("/v1/decisions?limit={limit}");
        if let Some(s) = status {
            path += &format!("&status={}", query_escape(s));
        }
        let v = Self::json(self.get(&path).call())?;
        Ok(v["items"].as_array().map(|a| a.iter().filter_map(Decision::parse).collect()).unwrap_or_default())
    }

    /// `GET /v1/decisions/{id}`.
    pub fn decision(&self, id: &str) -> Result<Decision, ApiError> {
        let v = Self::json(self.get(&format!("/v1/decisions/{}", query_escape(id))).call())?;
        Decision::parse(&v).ok_or_else(|| ApiError::Protocol("not a decision".into()))
    }

    /// `GET /v1/approvals?status=pending`: what this device may decide now.
    pub fn approvals(&self) -> Result<Vec<Approval>, ApiError> {
        Ok(Approval::parse_list(&Self::json(self.get("/v1/approvals?status=pending").call())?))
    }

    /// `GET /v1/approvals/{id}`, answered or expired too.
    pub fn approval(&self, id: &str) -> Result<Approval, ApiError> {
        let v = Self::json(self.get(&format!("/v1/approvals/{}", query_escape(id))).call())?;
        Approval::parse(&v).ok_or_else(|| ApiError::Protocol("not an approval".into()))
    }

    /// `POST /v1/approvals/{id}/decide`. `payload_hash` is the one shown: the answer counts only for what was shown.
    pub fn approval_decide(&self, id: &str, approve: bool, payload_hash: &str) -> Result<(), ApiError> {
        let decision = if approve { "approve" } else { "deny" };
        let path = format!("/v1/approvals/{}/decide", query_escape(id));
        Self::json(self.post(&path).send_json(json!({ "decision": decision, "payloadHash": payload_hash }))).map(|_| ())
    }

    /// `GET /v1/secrets/catalog`: the projects. Names only.
    pub fn secrets_catalog(&self) -> Result<Vec<crate::secrets::Project>, ApiError> {
        Ok(crate::secrets::Project::parse_list(&Self::json(self.get("/v1/secrets/catalog").call())?))
    }

    /// `GET /v1/secrets/requests`: Lucina's queue. Names only.
    pub fn secret_requests(&self) -> Result<SecretQueue, ApiError> {
        Ok(SecretQueue::parse(&Self::json(self.get("/v1/secrets/requests").call())?))
    }

    /// `POST /v1/secrets/requests/{id}/dismiss`.
    pub fn secret_dismiss(&self, id: &str) -> Result<(), ApiError> {
        let path = format!("/v1/secrets/requests/{}/dismiss", query_escape(id));
        Self::json(self.post(&path).send_json(json!({}))).map(|_| ())
    }

    /// `GET /v1/secrets/names`: a folder's sub-folders and secret names. Never a value.
    pub fn secret_names(&self, project: &str, environment: &str, path: &str) -> Result<crate::secrets::Folder, ApiError> {
        let q = format!("/v1/secrets/names?project={}&environment={}&path={}", query_escape(project), query_escape(environment), query_escape(path));
        Ok(crate::secrets::Folder::parse(&Self::json(self.get(&q).call())?))
    }

    /// `POST /v1/secrets/projects`: a new project, which no agent or service can reach until the owner says so.
    pub fn secret_create_project(&self, name: &str) -> Result<crate::secrets::Project, ApiError> {
        let v = Self::json(self.post("/v1/secrets/projects").send_json(json!({ "name": name, "confirmed": true })))?;
        crate::secrets::Project::parse(&v["project"]).ok_or_else(|| ApiError::Protocol("not a project".into()))
    }

    /// `POST /v1/secrets`: the one way a value leaves this machine. The request's bytes are wiped once sent; the value itself
    /// when `save` is dropped.
    pub fn secret_save(&self, save: &SaveSecret) -> Result<SecretReceipt, ApiError> {
        let mut body = save.body();
        let resp = self
            .post("/v1/secrets")
            .config()
            .timeout_global(Some(SECRET_TIMEOUT))
            .build()
            .content_type("application/json")
            .send(&body[..]);
        zeroize::Zeroize::zeroize(&mut body);
        Ok(SecretReceipt::parse(&Self::json(resp)?))
    }
}

/// `GET /v1/timeline/stream`'s `item` events. An `Err` or `Ok(None)` means reconnect with the last id seen.
pub struct TimelineStream {
    body: Box<dyn Read + Send>,
    parser: sse::Parser,
    queue: Vec<Item>,
}

impl TimelineStream {
    /// Blocks until the next item. `Ok(None)`: the server ended the response, which it does on its own.
    pub fn next_item(&mut self) -> Result<Option<Item>, ApiError> {
        let mut buf = [0u8; 8192];
        loop {
            if !self.queue.is_empty() {
                return Ok(Some(self.queue.remove(0)));
            }
            let n = self.body.read(&mut buf).map_err(|e| ApiError::Network(e.kind().to_string()))?;
            if n == 0 {
                return Ok(None);
            }
            for block in self.parser.push(&buf[..n]) {
                if block.event != "item" {
                    continue;
                }
                if let Some(item) = serde_json::from_str::<Value>(&block.data).ok().as_ref().and_then(Item::parse) {
                    self.queue.push(item);
                }
            }
        }
    }
}

/// Any server-sent stream. An `Err` or `Ok(None)` means reconnect with the last id seen.
pub struct SseStream {
    body: Box<dyn Read + Send>,
    parser: sse::Parser,
    queue: std::collections::VecDeque<sse::Block>,
}

impl SseStream {
    /// Blocks until the next block. `Ok(None)`: the server ended the response.
    pub fn next_block(&mut self) -> Result<Option<sse::Block>, ApiError> {
        let mut buf = [0u8; 8192];
        loop {
            if let Some(b) = self.queue.pop_front() {
                return Ok(Some(b));
            }
            let n = self.body.read(&mut buf).map_err(|e| ApiError::Network(e.kind().to_string()))?;
            if n == 0 {
                return Ok(None);
            }
            self.queue.extend(self.parser.push(&buf[..n]));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_never_goes_in_clear_to_another_machine() {
        assert!(Client::new("http://example.com", None).is_none());
        assert!(Client::new("http://127.0.0.1.example.com", None).is_none());
        assert!(Client::new("ftp://x", None).is_none());
        assert!(Client::new("http://127.0.0.1:9", None).is_some());
        assert!(Client::new("https://getsecretary.tech/", None).is_some());
    }

    #[test]
    fn the_voice_socket_address_follows_the_base() {
        let live = Client::new("https://getsecretary.tech/", None).unwrap();
        assert_eq!(live.ws_url("/v1/voice/ws"), "wss://getsecretary.tech/v1/voice/ws");
        let local = Client::new("http://127.0.0.1:8801", None).unwrap();
        assert_eq!(local.ws_url("/v1/voice/ws"), "ws://127.0.0.1:8801/v1/voice/ws");
        assert!(local.bearer().is_none());
    }

    #[test]
    fn both_refusal_shapes_keep_the_servers_sentence() {
        let nested = json!({"error": {"code": "not_found", "message": "No such thing"}});
        assert_eq!(error_fields(&nested), ("not_found".to_owned(), "No such thing".to_owned()));
        let flat = json!({"ok": false, "error": "That task is gone", "code": "invalid_command"});
        assert_eq!(error_fields(&flat), ("invalid_command".to_owned(), "That task is gone".to_owned()));
        assert_eq!(error_fields(&json!({"ok": false, "error": "No code"})), ("error".to_owned(), "No code".to_owned()));
        assert_eq!(error_fields(&Value::Null), ("error".to_owned(), String::new()));
    }

    #[test]
    fn a_flat_refusal_reaches_the_caller_over_the_wire() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            // All of it, body too: closing with bytes unread makes the kernel send a reset, which can overtake the answer.
            let mut request = Vec::new();
            let mut chunk = [0u8; 2048];
            let whole = |r: &[u8]| {
                let text = String::from_utf8_lossy(r);
                text.split_once("\r\n\r\n").is_some_and(|(head, rest)| {
                    let length = head.lines().find_map(|l| {
                        let (name, value) = l.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length").then(|| value.trim().parse::<usize>().ok())?
                    });
                    rest.len() >= length.unwrap_or(0)
                })
            };
            while !whole(&request) {
                let n = conn.read(&mut chunk).unwrap();
                assert!(n > 0, "the request ended early");
                request.extend_from_slice(&chunk[..n]);
            }
            let body = r#"{"ok":false,"error":"That task is gone","code":"invalid_command"}"#;
            write!(conn, "HTTP/1.1 422 X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        });
        let e = Client::new(&base, None).unwrap().post_json("/v1/tasks/command", &json!({})).unwrap_err();
        assert_eq!(e, ApiError::Http(422, "invalid_command".into(), "That task is gone".into()));
        assert_eq!(e.to_string(), "That task is gone");
    }

    /// A server that answers `{"ok":true}` to each request after `delay`.
    fn slow_server(delay: Duration) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for mut conn in listener.incoming().flatten() {
                let mut chunk = [0u8; 4096];
                let _ = conn.read(&mut chunk);
                std::thread::sleep(delay);
                let body = r#"{"ok":true}"#;
                let _ = write!(conn, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            }
        });
        base
    }

    #[test]
    fn a_request_can_wait_longer_or_shorter_than_the_usual_limit() {
        let base = slow_server(Duration::from_millis(1500));
        let c = Client::new(&base, None).unwrap();
        assert!(matches!(c.get_json_within("/v1/x", Duration::from_millis(500)), Err(ApiError::Network(_))));
        assert_eq!(c.get_json_within("/v1/x", Duration::from_secs(5)).unwrap(), json!({"ok": true}));
        assert!(matches!(c.post_json_within("/v1/x", &json!({}), Duration::from_millis(500)), Err(ApiError::Network(_))));
        assert_eq!(c.post_json_within("/v1/x", &json!({}), Duration::from_secs(5)).unwrap(), json!({"ok": true}));
    }

    #[test]
    fn query_values_are_escaped() {
        assert_eq!(query_escape("device:dev_1/x y"), "device%3Adev_1%2Fx%20y");
    }
}
