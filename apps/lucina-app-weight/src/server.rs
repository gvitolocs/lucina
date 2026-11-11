//! The routes of web/src/api/body.ts behind a trait, and the worker thread that sends one command at a time: the
//! queue and the conflict retry of web/src/state/engine.ts.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use lucina_account::engine::new_uuid;
use lucina_account::{ApiError, Client};
use secretary_body::{Command, Engine, Response, View};
use serde_json::{Value, json};

/// The view and the unit chosen for the form, in RAM beside the token.
pub const CACHE_FILE: &str = "weight-cache.json";
const CONFLICT: &str = "This changed somewhere else. Refreshed. Please try again.";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServerError {
    /// No answer, or 502/503/504. The command may or may not have arrived.
    Offline(String),
    /// 401: the device token is revoked.
    SignedOut,
    /// The server looked at it and said no.
    Refused { status: u16, code: String, message: String },
}

pub trait Server: Send {
    /// `GET /v1/body/state`.
    fn state(&mut self, tz: &str) -> Result<View, ServerError>;
    /// `POST /v1/body/command`.
    fn command(&mut self, body: &Value) -> Result<Response, ServerError>;
}

/// The real server, over `lucina-account`.
pub struct Account {
    pub client: Client,
}

fn from_api(e: ApiError) -> ServerError {
    match e {
        ApiError::Network(m) => ServerError::Offline(m),
        ApiError::RateLimited(s) => ServerError::Offline(format!("too many requests; wait {s} s")),
        ApiError::Http(502..=504, _, m) => ServerError::Offline(m),
        ApiError::Unauthorized(_) => ServerError::SignedOut,
        ApiError::Http(status, code, message) => ServerError::Refused { status, code, message },
        ApiError::Protocol(message) => ServerError::Refused { status: 0, code: "protocol".into(), message },
    }
}

fn unexpected(e: serde_json::Error) -> ServerError {
    ServerError::Refused { status: 0, code: "protocol".into(), message: format!("unexpected answer: {e}") }
}

/// `timezone` in a query. Zone names are ASCII letters, digits, `/`, `_`, `+` and `-`.
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

impl Server for Account {
    fn state(&mut self, tz: &str) -> Result<View, ServerError> {
        let v = self.client.get_json(&format!("/v1/body/state?timezone={}", encode(tz))).map_err(from_api)?;
        // `normalizeBodyView`: the engine's `{ok, view}`, or a bare view.
        let view = v.get("view").cloned().unwrap_or(v);
        serde_json::from_value(view).map_err(unexpected)
    }

    fn command(&mut self, body: &Value) -> Result<Response, ServerError> {
        let v = self.client.post_json("/v1/body/command", body).map_err(from_api)?;
        serde_json::from_value(v).map_err(unexpected)
    }
}

/// The server's engine in this process: sample mode and the tests. The same JSON both ways, and the status of
/// `status_for` in secretary-server's engines.rs.
pub struct Fake {
    pub engine: Arc<Mutex<Engine>>,
    /// While set, every call fails as a lost connection does.
    pub offline: Arc<AtomicBool>,
    /// While set, a command is carried out and its answer lost.
    pub lose_answers: Arc<AtomicBool>,
    pub now_ms: u64,
}

impl Fake {
    pub fn new(engine: Engine, now_ms: u64) -> Fake {
        Fake {
            engine: Arc::new(Mutex::new(engine)),
            offline: Arc::new(AtomicBool::new(false)),
            lose_answers: Arc::new(AtomicBool::new(false)),
            now_ms,
        }
    }

    fn handle(&self, body: &str) -> Result<Response, ServerError> {
        let r = self.engine.lock().map_err(|_| ServerError::Offline("poisoned".into()))?.handle_at(body, self.now_ms);
        let wire = serde_json::to_string(&r).map_err(unexpected)?;
        let r: Response = serde_json::from_str(&wire).map_err(unexpected)?;
        if r.ok {
            return Ok(r);
        }
        let code = r.code.unwrap_or_else(|| "error".into());
        let status = match code.as_str() {
            "conflict" => 409,
            "storage_error" => 500,
            _ => 422,
        };
        Err(ServerError::Refused { status, code, message: r.error.unwrap_or_default() })
    }
}

impl Server for Fake {
    fn state(&mut self, tz: &str) -> Result<View, ServerError> {
        if self.offline.load(Ordering::SeqCst) {
            return Err(ServerError::Offline("no connection".into()));
        }
        let body = json!({"timezone": tz, "command": {"type": "snapshot"}}).to_string();
        self.handle(&body)?.view.ok_or_else(|| ServerError::Offline("no view".into()))
    }

    fn command(&mut self, body: &Value) -> Result<Response, ServerError> {
        if self.offline.load(Ordering::SeqCst) {
            return Err(ServerError::Offline("no connection".into()));
        }
        let r = self.handle(&body.to_string());
        if self.lose_answers.load(Ordering::SeqCst) {
            return Err(ServerError::Offline("no connection".into()));
        }
        r
    }
}

/// Which form a command came from, to tell it how it went.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Tag {
    Log,
    Edit,
    Delete,
    Settings,
}

pub enum Job {
    Refresh,
    Run(Tag, Command),
    /// A weigh-in typed in `unit`. The engine parses text in its display unit, so for the other unit it is asked
    /// to parse first (`submitLog` in web/src/views/Weight.tsx).
    Log { text: String, unit: String, note: Option<String> },
}

pub enum Msg {
    /// The sign-in screen changed.
    Changed,
    SignedIn,
    View(Box<View>),
    LoadFailed(String),
    Done(Tag, Result<(), String>),
    SignedOut,
}

pub struct Worker {
    server: Box<dyn Server>,
    tz: String,
    revision: Option<u64>,
    unit: Option<String>,
    /// A command that got no answer, with its op_id. Sent again, it keeps the op_id: the engine answers with its
    /// receipt if the first one did arrive, so a lost answer never logs a weigh-in twice.
    unanswered: Option<(String, String)>,
}

impl Worker {
    pub fn new(server: Box<dyn Server>, tz: String) -> Worker {
        Worker { server, tz, revision: None, unit: None, unanswered: None }
    }

    pub fn handle(&mut self, job: Job) -> Vec<Msg> {
        let mut out = Vec::new();
        match job {
            Job::Refresh => {
                if let Err(Failed::SignedOut) = self.refresh(&mut out) {
                    out.push(Msg::SignedOut);
                }
            }
            Job::Run(tag, command) => {
                let result = self.execute(command, &mut out).map(|_| ());
                self.finish(tag, result, &mut out);
            }
            Job::Log { text, unit, note } => {
                let result = self.log(text, unit, note, &mut out);
                self.finish(Tag::Log, result, &mut out);
            }
        }
        out
    }

    fn finish(&self, tag: Tag, result: Result<(), Failed>, out: &mut Vec<Msg>) {
        match result {
            Ok(()) => out.push(Msg::Done(tag, Ok(()))),
            Err(Failed::Said(m)) => out.push(Msg::Done(tag, Err(m))),
            Err(Failed::SignedOut) => out.push(Msg::SignedOut),
        }
    }

    fn log(&mut self, text: String, unit: String, note: Option<String>, out: &mut Vec<Msg>) -> Result<(), Failed> {
        if self.revision.is_none() {
            self.refresh(out)?;
        }
        let (weight, weight_grams) = if self.unit.as_deref() == Some(unit.as_str()) {
            (Some(text), None)
        } else {
            (None, Some(self.parse(text, unit)?))
        };
        self.execute(Command::LogEntry { weight_grams, weight, measured_ms: None, note }, out).map(|_| ())
    }

    /// `parseWeightOnServer`: a query, so nothing to retry.
    fn parse(&mut self, value: String, unit: String) -> Result<u64, Failed> {
        let body = json!({
            "op_id": new_uuid(),
            "expected_revision": self.revision.unwrap_or(0),
            "timezone": self.tz,
            "command": Command::ParseWeight { value, unit: Some(unit) },
        });
        let r = self.server.command(&body).map_err(Failed::from)?;
        match r.weight_grams {
            Some(g) if r.ok => Ok(g),
            _ => Err(Failed::Said(r.error.unwrap_or_else(|| "Enter a valid weight.".into()))),
        }
    }

    fn refresh(&mut self, out: &mut Vec<Msg>) -> Result<(), Failed> {
        match self.server.state(&self.tz) {
            Ok(view) => {
                self.seen(view, out);
                Ok(())
            }
            Err(ServerError::SignedOut) => Err(Failed::SignedOut),
            Err(ServerError::Offline(m)) | Err(ServerError::Refused { message: m, .. }) => {
                out.push(Msg::LoadFailed(m.clone()));
                Err(Failed::Said(m))
            }
        }
    }

    fn seen(&mut self, view: View, out: &mut Vec<Msg>) {
        self.revision = Some(view.revision);
        self.unit = Some(view.settings.unit.clone());
        out.push(Msg::View(Box::new(view)));
    }

    /// `execute` in web/src/state/engine.ts: on conflict, fetch again and send with a new op_id, once.
    fn execute(&mut self, command: Command, out: &mut Vec<Msg>) -> Result<Response, Failed> {
        if self.revision.is_none() {
            self.refresh(out)?;
        }
        let sent = serde_json::to_string(&command).unwrap_or_default();
        let mut op_id = match &self.unanswered {
            Some((c, op)) if *c == sent => op.clone(),
            _ => new_uuid(),
        };
        for attempt in 0..2 {
            let body = json!({
                "op_id": op_id,
                "expected_revision": self.revision.unwrap_or(0),
                "timezone": self.tz,
                "command": command,
            });
            match self.server.command(&body) {
                Ok(mut r) => {
                    self.unanswered = None;
                    if let Some(view) = r.view.take() {
                        self.seen(view, out);
                    }
                    return Ok(r);
                }
                Err(ServerError::Refused { status, code, .. }) if status == 409 || code == "conflict" => {
                    self.unanswered = None;
                    if attempt == 1 {
                        return Err(Failed::Said(CONFLICT.into()));
                    }
                    self.refresh(out)?;
                    op_id = new_uuid();
                }
                Err(e @ ServerError::Offline(_)) => {
                    self.unanswered = Some((sent, op_id));
                    return Err(Failed::from(e));
                }
                Err(e) => {
                    self.unanswered = None;
                    return Err(Failed::from(e));
                }
            }
        }
        Err(Failed::Said(CONFLICT.into()))
    }
}

enum Failed {
    Said(String),
    SignedOut,
}

impl From<ServerError> for Failed {
    fn from(e: ServerError) -> Failed {
        match e {
            ServerError::SignedOut => Failed::SignedOut,
            ServerError::Offline(m) => Failed::Said(format!("Not saved: {m}. Try again.")),
            ServerError::Refused { message, .. } => Failed::Said(message),
        }
    }
}
