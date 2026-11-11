//! The routes of web/src/api/gym.ts (and body.ts for the summary on top) behind a trait, and the worker thread that
//! sends one command at a time: the queue and the conflict retry of web/src/state/engine.ts.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use lucina_account::engine::new_uuid;
use lucina_account::{ApiError, Client};
use secretary_gym::{Clock, Engine, Response, View};
use serde_json::{Value, json};

/// The view, in RAM beside the token. The service deletes it on sign-out.
pub const CACHE_FILE: &str = "training-cache.json";
const CONFLICT: &str = "This changed somewhere else. Refreshed. Please try again.";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServerError {
    /// No answer, or 502/503/504. The command may or may not have arrived.
    Offline(String),
    /// 401.
    SignedOut,
    /// The server looked at it and said no.
    Refused { status: u16, code: String, message: String },
}

pub trait Server: Send {
    /// `GET /v1/gym/state`.
    fn state(&mut self, tz: &str) -> Result<View, ServerError>;
    /// `POST /v1/gym/command`.
    fn command(&mut self, body: &Value) -> Result<Response, ServerError>;
    /// `GET /v1/gym/clock`.
    fn clock(&mut self, tz: &str) -> Result<Clock, ServerError>;
    /// `GET /v1/body/state`, for the summary on top.
    fn body(&mut self, tz: &str) -> Result<secretary_body::View, ServerError>;
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
        let v = self.client.get_json(&format!("/v1/gym/state?timezone={}", encode(tz))).map_err(from_api)?;
        // `normalizeGymView`: the engine's `{ok, view}`, or a bare view.
        let view = v.get("view").cloned().unwrap_or(v);
        serde_json::from_value(view).map_err(unexpected)
    }

    fn command(&mut self, body: &Value) -> Result<Response, ServerError> {
        let v = self.client.post_json("/v1/gym/command", body).map_err(from_api)?;
        serde_json::from_value(v).map_err(unexpected)
    }

    fn clock(&mut self, tz: &str) -> Result<Clock, ServerError> {
        let v = self.client.get_json(&format!("/v1/gym/clock?timezone={}", encode(tz))).map_err(from_api)?;
        let clock = v.get("clock").cloned().unwrap_or(v);
        serde_json::from_value(clock).map_err(unexpected)
    }

    fn body(&mut self, tz: &str) -> Result<secretary_body::View, ServerError> {
        let v = self.client.get_json(&format!("/v1/body/state?timezone={}", encode(tz))).map_err(from_api)?;
        let view = v.get("view").cloned().unwrap_or(v);
        serde_json::from_value(view).map_err(unexpected)
    }
}

/// The server's engines in this process: sample mode and the tests. The same JSON both ways, and the status of
/// `status_for` in secretary-server's engines.rs.
pub struct Fake {
    pub engine: Arc<Mutex<Engine>>,
    pub body: Arc<Mutex<secretary_body::Engine>>,
    /// While set, every call fails as a lost connection does.
    pub offline: Arc<AtomicBool>,
    /// While set, a command is carried out and its answer lost.
    pub lose_answers: Arc<AtomicBool>,
    /// While set, the body route fails and the gym's does not.
    pub body_down: Arc<AtomicBool>,
    /// A dead token.
    pub dead: Arc<AtomicBool>,
    /// A fixed time for the tests; 0 is the real clock, for sample mode, whose timers count.
    pub now_ms: u64,
}

impl Fake {
    pub fn new(engine: Engine, body: secretary_body::Engine, now_ms: u64) -> Fake {
        Fake {
            engine: Arc::new(Mutex::new(engine)),
            body: Arc::new(Mutex::new(body)),
            offline: Arc::new(AtomicBool::new(false)),
            lose_answers: Arc::new(AtomicBool::new(false)),
            body_down: Arc::new(AtomicBool::new(false)),
            dead: Arc::new(AtomicBool::new(false)),
            now_ms,
        }
    }

    fn up(&self) -> Result<(), ServerError> {
        if self.dead.load(Ordering::SeqCst) {
            return Err(ServerError::SignedOut);
        }
        if self.offline.load(Ordering::SeqCst) {
            return Err(ServerError::Offline("no connection".into()));
        }
        Ok(())
    }

    fn now(&self) -> u64 {
        if self.now_ms == 0 { secretary_gym::now_ms() } else { self.now_ms }
    }

    fn handle(&self, body: &str) -> Result<Response, ServerError> {
        let now = self.now();
        let r = self.engine.lock().map_err(|_| ServerError::Offline("poisoned".into()))?.handle_at(body, now);
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
        self.up()?;
        let body = json!({"timezone": tz, "command": {"type": "snapshot"}}).to_string();
        self.handle(&body)?.view.ok_or_else(|| ServerError::Offline("no view".into()))
    }

    fn command(&mut self, body: &Value) -> Result<Response, ServerError> {
        self.up()?;
        let r = self.handle(&body.to_string());
        if self.lose_answers.load(Ordering::SeqCst) {
            return Err(ServerError::Offline("no connection".into()));
        }
        r
    }

    fn clock(&mut self, tz: &str) -> Result<Clock, ServerError> {
        self.up()?;
        let body = json!({"timezone": tz, "command": {"type": "clock"}}).to_string();
        self.handle(&body)?.clock.ok_or_else(|| ServerError::Offline("no clock".into()))
    }

    fn body(&mut self, tz: &str) -> Result<secretary_body::View, ServerError> {
        self.up()?;
        if self.body_down.load(Ordering::SeqCst) {
            return Err(ServerError::Offline("no body route".into()));
        }
        let body = json!({"timezone": tz, "command": {"type": "snapshot"}}).to_string();
        let r = self.body.lock().map_err(|_| ServerError::Offline("poisoned".into()))?.handle_at(&body, self.now());
        r.view.ok_or_else(|| ServerError::Offline("no view".into()))
    }
}

/// What a command was for, so that the screen knows where to go when it went through.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Tag {
    Start,
    Finish,
    Discard,
    DeleteWorkout,
    SaveRoutine,
    SaveRoutineFromActive,
    CreateExercise(String),
    /// Weight, reps and so on of this set.
    Edit(String),
    Complete(String),
    Settings,
    Measurement,
    Rename,
    Other,
}

pub enum Job {
    Refresh,
    Clock,
    Body,
    Run(Tag, Value),
    /// One after the other, stopping at the first that fails: `toggleDone` saves what was typed, then completes.
    Sequence(Tag, Vec<Value>),
    /// `exportAs`: the engine's text, to this file.
    Export { json: bool, dest: PathBuf },
    /// `chooseFile`: is this a backup at all?
    ReadBackup { path: PathBuf },
    /// `restore`.
    Restore { name: String, text: String },
}

pub enum Msg {
    /// The sign-in screen changed.
    Changed,
    SignedIn,
    View(Box<View>),
    Clock(Box<Clock>),
    Body(Box<secretary_body::View>),
    LoadFailed(String),
    Done(Tag, Result<(), String>),
    /// What became of an export or a restore, for the Settings screen.
    Data(Result<String, String>),
    /// A backup that may be restored: its name and text.
    BackupRead(Result<(String, String), String>),
    SignedOut,
}

pub struct Worker {
    server: Box<dyn Server>,
    tz: String,
    revision: Option<u64>,
    /// A command that got no answer, with its op_id. Sent again, it keeps the op_id: the engine answers with its
    /// receipt if the first one did arrive, so a lost answer never applies twice.
    unanswered: Option<(String, String)>,
}

impl Worker {
    pub fn new(server: Box<dyn Server>, tz: String) -> Worker {
        Worker { server, tz, revision: None, unanswered: None }
    }

    pub fn handle(&mut self, job: Job) -> Vec<Msg> {
        let mut out = Vec::new();
        match job {
            Job::Refresh => {
                if let Err(Failed::SignedOut) = self.refresh(&mut out) {
                    out.push(Msg::SignedOut);
                }
            }
            // Clock ticks stay quiet (`tickClock`): the next view or tick corrects the display.
            Job::Clock => {
                if let Ok(c) = self.server.clock(&self.tz) {
                    out.push(Msg::Clock(Box::new(c)));
                }
            }
            // The summary on top never gets in the way of the training screen.
            Job::Body => {
                if let Ok(v) = self.server.body(&self.tz) {
                    out.push(Msg::Body(Box::new(v)));
                }
            }
            Job::Run(tag, command) => {
                let result = self.execute(command, &mut out);
                self.finish(tag, result, &mut out);
            }
            Job::Sequence(tag, commands) => {
                let mut result = Ok(());
                for command in commands {
                    result = self.execute(command, &mut out);
                    if result.is_err() {
                        break;
                    }
                }
                self.finish(tag, result, &mut out);
            }
            Job::Export { json, dest } => {
                let command = json!({"type": if json { "export_json" } else { "export_csv" }});
                match self.answer(command, &mut out) {
                    Ok(r) => out.push(Msg::Data(match r.text {
                        Some(text) => write_whole(&dest, text.as_bytes()).map(|()| {
                            format!("Exported {}.", dest.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned()))
                        }),
                        None => Err(EXPORT_FAILED.into()),
                    })),
                    Err(Failed::SignedOut) => out.push(Msg::SignedOut),
                    Err(Failed::Said(_)) => out.push(Msg::Data(Err(EXPORT_FAILED.into()))),
                }
            }
            Job::ReadBackup { path } => out.push(Msg::BackupRead(read_backup(&path))),
            Job::Restore { name, text } => {
                match self.answer(json!({"type": "restore_json", "backup": text}), &mut out) {
                    Ok(_) => out.push(Msg::Data(Ok(format!("Backup restored from {name}.")))),
                    Err(Failed::SignedOut) => out.push(Msg::SignedOut),
                    // The engine's reason ("Invalid backup: …").
                    Err(Failed::Said(m)) => out.push(Msg::Data(Err(if m.is_empty() { "The backup could not be restored.".into() } else { m }))),
                }
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

    fn refresh(&mut self, out: &mut Vec<Msg>) -> Result<(), Failed> {
        match self.server.state(&self.tz) {
            Ok(view) => {
                self.revision = Some(view.state.revision);
                out.push(Msg::View(Box::new(view)));
                Ok(())
            }
            Err(ServerError::SignedOut) => Err(Failed::SignedOut),
            Err(ServerError::Offline(m)) | Err(ServerError::Refused { message: m, .. }) => {
                out.push(Msg::LoadFailed(m.clone()));
                Err(Failed::Said(m))
            }
        }
    }

    fn execute(&mut self, command: Value, out: &mut Vec<Msg>) -> Result<(), Failed> {
        self.answer(command, out).map(|_| ())
    }

    /// `execute` in web/src/state/engine.ts: on conflict, fetch again and send with a new op_id, once.
    fn answer(&mut self, command: Value, out: &mut Vec<Msg>) -> Result<Response, Failed> {
        if self.revision.is_none() {
            self.refresh(out)?;
        }
        let sent = command.to_string();
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
                        self.revision = Some(view.state.revision);
                        out.push(Msg::View(Box::new(view)));
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

const EXPORT_FAILED: &str = "The export could not be created. Please try again.";
/// `GYM_MAX_INPUT_BYTES`.
const MAX_BACKUP: u64 = 16 << 20;

/// Whole or not at all.
fn write_whole(dest: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = dest.with_extension("part");
    std::fs::write(&tmp, bytes).and_then(|()| std::fs::rename(&tmp, dest)).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("{}: {e}", dest.display())
    })
}

/// `chooseFile`: size first, then text, then JSON. What it means is the engine's to say.
pub fn read_backup(path: &Path) -> Result<(String, String), String> {
    const UNREADABLE: &str = "That file could not be read.";
    let meta = std::fs::metadata(path).map_err(|_| UNREADABLE.to_owned())?;
    if meta.len() > MAX_BACKUP {
        return Err("This backup exceeds the 16 MB import limit.".into());
    }
    let text = std::fs::read_to_string(path).map_err(|_| UNREADABLE.to_owned())?;
    if serde_json::from_str::<Value>(&text).is_err() {
        return Err("That file is not a Lucina Gym JSON backup.".into());
    }
    let name = path.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned());
    Ok((name, text))
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
