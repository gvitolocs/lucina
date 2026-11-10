//! lucina-devd's side of app intents and Lucina's undo journal (docs/research/juno-intents.md): which intents the laptop has,
//! whether a call runs, waits on a decision or is refused, how it reaches its app, what is journaled, and how an undo is done.
//! lucina-devd serves the driver tools `intents`, `intent` and `undo` with [`Catalog`] and [`Runner`]; what only lucina-devd
//! and the shell know (the shell's socket, the off-limits paths, settings) comes in through [`Host`].

use std::io::Read as _;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use lucina_app::manifest::Manifest;
use lucina_intents::journal::Journal;
use lucina_intents::rules::{self, Effect, Gate, State};
use lucina_intents::spec::{self, Call, IntentInfo, Reply, Undo, UndoEntry};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub mod devd;

/// What lucina-devd and the shell provide.
pub trait Host {
    /// The app in front, if any.
    fn front(&self) -> Option<String>;
    /// Ask the shell to start `app` for `call` (the shell's `open` with `intent` and `reply`).
    fn open_for(&self, app: &str, call: &Value, reply: &Path) -> Result<(), String>;
    /// The shell's `open`, `close` and `list`, for the shell's own intents.
    fn open(&self, app: &str) -> Result<Value, String>;
    fn close(&self) -> Result<Value, String>;
    fn apps(&self) -> Result<Value, String>;
    /// A call to an app that is running, over its socket (stage B). `None` when it has none.
    fn send_running(&self, _app: &str, _call: &Value, _timeout: Duration) -> Option<Result<Reply, String>> {
        None
    }
    /// Whether Lucina may change this file (lucina-devd's places and off-limits paths).
    fn file_allowed(&self, path: &Path) -> Result<(), String>;
    /// Put a setting back.
    fn set_setting(&self, key: &str, _value: &Value) -> Result<(), String> {
        Err(format!("cannot put the setting {key} back on this generation"))
    }
}

/// One intent the laptop has, with the program that answers it.
#[derive(Clone, Debug, PartialEq)]
pub struct Offer {
    pub info: IntentInfo,
    /// The app's program; empty for the shell's own intents.
    pub exec: String,
}

/// The shell's own intents: they need no app.
fn shell_offers() -> Vec<Offer> {
    let open = json!({"type": "object", "required": ["app"], "additionalProperties": false,
        "properties": {"app": {"type": "string", "maxLength": 128, "pattern": "^[A-Za-z0-9_.:-]+$", "x-kind": "id", "description": "The app's id, as shell.apps lists it."}}});
    let none = json!({"type": "object", "required": [], "additionalProperties": false, "properties": {}});
    let mk = |name: &str, description: &str, schema: &Value, effect: Effect, shows: bool| Offer {
        info: IntentInfo {
            app: "shell".into(),
            name: name.into(),
            description: description.into(),
            args: spec::args_of(schema).unwrap_or_default(),
            schema: schema.clone(),
            effect,
            shows,
            inverse: None,
        },
        exec: String::new(),
    };
    vec![
        mk("shell.apps", "The apps on the laptop (id, title) and which one is in front.", &none, Effect::Read, false),
        mk("shell.open", "Opens an app as its tile does; the app in front is closed first.", &open, Effect::Write, true),
        mk("shell.close", "Closes the app in front, as F1 does, and goes Home.", &none, Effect::Write, false),
    ]
}

/// The intents of the apps installed, read once from their manifests and programs.
#[derive(Clone, Debug, Default)]
pub struct Catalog {
    offers: Vec<Offer>,
    /// Apps whose listing could not be read or did not match their manifest: said in `intents`, never fatal.
    pub problems: Vec<String>,
}

impl Catalog {
    /// Each manifest with `intents` names its intents; `<exec> --intents` gives their schemas. An intent the manifest names and the
    /// program does not list (or the other way round) is left out and reported.
    pub fn load(manifests: &[Manifest], timeout: Duration) -> Catalog {
        Catalog::load_keeping(manifests, timeout, None)
    }

    /// [`Catalog::load`] with every program asked at once and `within` for all of them together: one call never waits on the
    /// apps one by one (on the laptop a listing took 69 s that way, and devd, which runs one call at a time, was busy meanwhile).
    /// An app that has not answered in time, or answered nothing usable, keeps its intents from `previous`, the last catalog,
    /// and is named in `problems`.
    pub fn load_keeping(manifests: &[Manifest], within: Duration, previous: Option<&Catalog>) -> Catalog {
        let asked: Vec<&Manifest> = manifests.iter().filter(|m| !m.intents.is_empty()).collect();
        let (tx, rx) = std::sync::mpsc::channel();
        for (n, m) in asked.iter().enumerate() {
            let (tx, exec, id) = (tx.clone(), m.exec.clone(), m.id.clone());
            // Each child is killed at `within` by `listing` itself, so no thread outlives the wait for long.
            std::thread::spawn(move || {
                let _ = tx.send((n, listing(&exec, &id, within)));
            });
        }
        drop(tx);
        let mut answers: Vec<Option<Result<Value, String>>> = asked.iter().map(|_| None).collect();
        let end = Instant::now() + within + Duration::from_millis(200);
        while answers.iter().any(Option::is_none) {
            let left = end.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left) {
                Ok((n, r)) => answers[n] = Some(r),
                Err(_) => break,
            }
        }
        let mut c = Catalog { offers: shell_offers(), problems: Vec::new() };
        for (m, a) in asked.iter().zip(answers) {
            let why = match a {
                Some(Ok(list)) => {
                    c.add(m, &list);
                    continue;
                }
                Some(Err(e)) => e,
                None => format!("did not list its intents within {} ms", within.as_millis()),
            };
            let kept: Vec<Offer> = previous.map_or_else(Vec::new, |p| p.offers.iter().filter(|o| o.info.app == m.id).cloned().collect());
            if kept.is_empty() {
                c.problems.push(format!("{}: {why}", m.id));
            } else {
                c.problems.push(format!("{}: {why}; its last listing is kept", m.id));
                c.offers.extend(kept);
            }
        }
        c
    }

    /// The catalog from listings already in hand (tests, and a cached copy).
    pub fn from_listings(apps: &[(Manifest, Value)]) -> Catalog {
        let mut c = Catalog { offers: shell_offers(), problems: Vec::new() };
        for (m, list) in apps {
            c.add(m, list);
        }
        c
    }

    fn add(&mut self, m: &Manifest, list: &Value) {
        let Some(items) = list.as_array() else {
            self.problems.push(format!("{}: its listing is not a list", m.id));
            return;
        };
        let mut seen = Vec::new();
        for item in items {
            match IntentInfo::from_json(item) {
                Ok(info) if info.app != m.id => self.problems.push(format!("{}: {} is not one of its own", m.id, info.name)),
                Ok(info) if !m.intents.contains(&info.name) => {
                    self.problems.push(format!("{}: {} is not in its manifest", m.id, info.name))
                }
                Ok(info) => {
                    seen.push(info.name.clone());
                    self.offers.push(Offer { info, exec: m.exec.clone() });
                }
                Err(e) => self.problems.push(format!("{}: {e}", m.id)),
            }
        }
        for name in m.intents.iter().filter(|n| !seen.contains(n)) {
            self.problems.push(format!("{}: {name} is in its manifest but the program does not offer it", m.id));
        }
    }

    pub fn find(&self, name: &str) -> Option<&Offer> {
        self.offers.iter().find(|o| o.info.name == name)
    }

    /// What `intents` lists: one app's, or with none named the app in front's and the shell's, so her tool list stays short.
    pub fn list(&self, app: Option<&str>, front: Option<&str>) -> Value {
        let shown: Vec<Value> = self
            .offers
            .iter()
            .filter(|o| match app {
                Some(a) => o.info.app == a,
                None => o.info.app == "shell" || Some(o.info.app.as_str()) == front,
            })
            .map(|o| o.info.to_json())
            .collect();
        let mut apps: Vec<&str> = self.offers.iter().map(|o| o.info.app.as_str()).collect();
        apps.dedup();
        json!({"intents": shown, "apps_with_intents": apps, "problems": self.problems})
    }
}

/// Where a program from the EFI partition is copied to run, as the shell copies it (`lucina-shell` `prepare`): the partition
/// can be unmounted by a tool that writes to it.
const RUN_COPIES: &str = "/tmp/lucina-intent-apps";

/// The program to start for `exec`, and its `LUCINA_APP_DIR`, as the shell gives them.
fn runnable(exec: &str, id: &str) -> Result<(PathBuf, String), String> {
    let p = Path::new(exec);
    if exec.starts_with("/mnt/lucina-apps/") {
        std::fs::create_dir_all(RUN_COPIES).map_err(|e| format!("{RUN_COPIES}: {e}"))?;
        let copy = Path::new(RUN_COPIES).join(id);
        let _ = std::fs::remove_file(&copy);
        std::fs::copy(p, &copy).map_err(|e| format!("{exec}: {e}"))?;
        std::fs::set_permissions(&copy, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
        let dir = p.parent().map_or(String::new(), |d| d.to_string_lossy().into_owned());
        return Ok((copy, dir));
    }
    let dir = if exec.starts_with("/data/lucina/apps/") { "/data/lucina/apps" } else { lucina_app::manifest::MANIFEST_DIR };
    Ok((p.to_path_buf(), dir.to_string()))
}

/// The framebuffer an app is given when it must not draw (`LUCINA_FB`, which the toolkit and Video read): a path that is never there.
pub const NO_SCREEN: &str = "/run/lucina/no-screen-for-intent-listing";

/// `<exec> --intents`, at most `timeout`.
fn listing(exec: &str, id: &str, timeout: Duration) -> Result<Value, String> {
    let (exec_path, _) = runnable(exec, id)?;
    let mut cmd = Command::new(&exec_path);
    // A program that does not know `--intents` (an older build, an app that reads its own arguments first) must not take the screen
    // from the app in front: the screen it would open is not there.
    cmd.arg("--intents")
        .env("LUCINA_FB", NO_SCREEN)
        .env_remove("LUCINA_INTENT")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = spawn(&mut cmd).map_err(|e| format!("{exec}: {e}"))?;
    let start = Instant::now();
    loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(status) if status.success() => break,
            Some(status) => return Err(format!("{exec} --intents: {status}")),
            None if start.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{exec} --intents: no answer in {} ms", timeout.as_millis()));
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    let mut out = String::new();
    child.stdout.take().ok_or("no output")?.read_to_string(&mut out).map_err(|e| e.to_string())?;
    serde_json::from_str(out.trim()).map_err(|e| format!("{exec} --intents: {e}"))
}

/// Start a program. A program written a moment ago can be "busy" while another thread's fork still holds the writer's file
/// (ETXTBSY): that passes in milliseconds, so try again a few times.
fn spawn(cmd: &mut Command) -> std::io::Result<std::process::Child> {
    let mut tries = 0;
    loop {
        match cmd.spawn() {
            Err(e) if e.raw_os_error() == Some(libc_etxtbsy()) && tries < 10 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(20));
            }
            r => return r,
        }
    }
}

const fn libc_etxtbsy() -> i32 {
    26
}

/// The hash a decision is bound to: the same on the server, which files the decision, and here.
pub fn call_hash(name: &str, args: &Value) -> String {
    let canonical = serde_json::to_string(&json!({"tool": "intent", "name": name, "args": args})).unwrap_or_default();
    let mut s = String::with_capacity(64);
    for b in Sha256::digest(canonical.as_bytes()) {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Runs calls and undos.
pub struct Runner<'a, H: Host> {
    pub catalog: &'a Catalog,
    pub journal: &'a Journal,
    pub host: &'a H,
    /// Where apps write their answers: `/run/lucina/intents`.
    pub reply_dir: PathBuf,
    /// How long an app has to answer.
    pub timeout: Duration,
}

/// What a call answers to lucina-devd's caller.
fn answer(reply: &Reply, effect: Effect, action: Option<&str>, undoable: bool) -> Value {
    json!({
        "ok": reply.ok,
        "text": reply.text,
        "value": reply.value,
        "entities": reply.entities,
        "effect": spec::effect_name(effect),
        "action": action,
        "undoable": undoable,
    })
}

impl<H: Host> Runner<'_, H> {
    /// One call of an intent. `decided` is true only when the server checked a decision for this very call (its hash, see
    /// [`call_hash`]). Writes and above are journaled; those that can be undone get an action id.
    pub fn call(&self, call_id: &str, name: &str, args: &Value, decided: bool, now_ms: u64) -> Result<Value, String> {
        let offer = self.catalog.find(name).ok_or_else(|| format!("no intent {name} on this laptop"))?;
        offer.info.check(args)?;
        let effect = offer.info.effect;
        match rules::gate(effect, effect, decided) {
            Gate::Run => {}
            Gate::NeedsDecision => {
                return Ok(json!({"ok": false, "needs_decision": true, "effect": spec::effect_name(effect), "call_hash": call_hash(name, args),
                    "text": format!("{name} is {}: it needs a decision first", spec::effect_name(effect))}));
            }
            Gate::Refuse => return Err(format!("{name} is the owner's own: Lucina never calls it")),
        }
        let call = Call { call: call_id.into(), name: name.into(), args: args.clone(), undo: None };
        let reply = self.deliver(offer, &call)?;
        Ok(self.journal_call(call_id, &offer.info, effect, reply, now_ms))
    }

    fn journal_call(&self, call_id: &str, info: &IntentInfo, effect: Effect, mut reply: Reply, now_ms: u64) -> Value {
        if !reply.ok || effect == Effect::Read {
            reply.write = None;
            return answer(&reply, effect, None, false);
        }
        match self.journal.record(now_ms, call_id, &info.name, effect, reply.write.take()) {
            Ok(line) => answer(&reply, effect, line.undoable.then_some(line.action.as_str()), line.undoable),
            Err(e) => {
                reply.text = format!("{} (not journaled, so it cannot be undone: {e})", reply.text);
                answer(&reply, effect, None, false)
            }
        }
    }

    /// Her recent undoable actions (`action` None), or undo one.
    pub fn undo(&self, action: Option<&str>, call_id: &str, now_ms: u64) -> Result<Value, String> {
        let Some(action) = action else {
            let recent = self.journal.recent(20, now_ms)?;
            let list: Vec<Value> = recent
                .iter()
                .map(|l| json!({"action": l.action, "intent": l.intent, "target": l.target, "ts": l.ts}))
                .collect();
            return Ok(json!({"ok": true, "actions": list}));
        };
        let (line, entry) = self.journal.undoable(action, now_ms)?;
        let write = line.write.as_ref().ok_or("nothing to undo")?;
        let result = match &write.undo {
            Undo::Inverse { intent, args } => self.undo_by_intent(call_id, intent, args, entry),
            Undo::File { path, before } => self.undo_file(Path::new(path), before.as_deref(), entry),
            Undo::Setting { key, old } => self.host.set_setting(key, old).map(|()| Reply::done(format!("{key} is back to {old}"))),
        };
        let ok = matches!(&result, Ok(r) if r.ok);
        let undo_line = self.journal.record_undo(now_ms, action, ok)?;
        let reply = result?;
        Ok(json!({
            "ok": reply.ok,
            "text": reply.text,
            "undid": if ok { Some(action) } else { None },
            "action": undo_line.map(|l| l.action),
            "effect": spec::effect_name(rules::undo_effect(entry.op)),
        }))
    }

    fn undo_by_intent(&self, call_id: &str, intent: &str, args: &Value, entry: rules::Entry) -> Result<Reply, String> {
        let offer = self.catalog.find(intent).ok_or_else(|| format!("no intent {intent} on this laptop to undo it with"))?;
        offer.info.check(args)?;
        let call = Call { call: call_id.into(), name: intent.into(), args: args.clone(), undo: Some(UndoEntry::from(entry)) };
        self.deliver(offer, &call)
    }

    fn undo_file(&self, path: &Path, before: Option<&str>, entry: rules::Entry) -> Result<Reply, String> {
        self.host.file_allowed(path)?;
        let now = match std::fs::symlink_metadata(path) {
            Ok(m) if m.is_file() => spec::there(&std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?),
            Ok(_) => return Err(format!("{} is not a file now", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => spec::GONE,
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        let Some(target) = rules::undo_to(now, entry) else {
            return Ok(Reply::failed(format!("{} changed since Lucina wrote it, so it stays as it is", path.display())));
        };
        put_file(self.journal, path, target, before)?;
        Ok(Reply::done(if target.exists {
            format!("{} has its old content back", path.display())
        } else {
            format!("{} is removed again", path.display())
        }))
    }

    /// Get the call to its app: over its socket when it is running, through the shell when it shows, or with no screen.
    fn deliver(&self, offer: &Offer, call: &Call) -> Result<Reply, String> {
        let info = &offer.info;
        if info.app == "shell" {
            return self.shell(info, &call.args);
        }
        let call_json = serde_json::to_value(call).map_err(|e| e.to_string())?;
        let front = self.host.front();
        if front.as_deref() == Some(info.app.as_str())
            && let Some(r) = self.host.send_running(&info.app, &call_json, self.timeout)
        {
            return r;
        }
        let reply = self.reply_path(&call.call)?;
        let _ = std::fs::remove_file(&reply);
        if info.shows {
            self.host.open_for(&info.app, &call_json, &reply)?;
            return wait_reply(&reply, self.timeout, None);
        }
        let (exec, app_dir) = runnable(&offer.exec, &info.app)?;
        let mut cmd = Command::new(&exec);
        cmd.env("LUCINA_INTENT", call_json.to_string())
            .env("LUCINA_APP_ID", &info.app)
            .env("LUCINA_APP_DIR", &app_dir)
            .env("LUCINA_INTENT_REPLY", &reply)
            .env("LUCINA_INTENT_HEADLESS", "1")
            .env("LUCINA_FB", NO_SCREEN)
            .env("LUCINA_INTENT_TIMEOUT_MS", self.timeout.as_millis().to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null());
        let mut child = spawn(&mut cmd).map_err(|e| format!("{}: {e}", offer.exec))?;
        let r = wait_reply(&reply, self.timeout + Duration::from_secs(2), Some(&mut child));
        let _ = child.kill();
        let _ = child.wait();
        r
    }

    fn shell(&self, info: &IntentInfo, args: &Value) -> Result<Reply, String> {
        let v = match info.name.as_str() {
            "shell.apps" => self.host.apps()?,
            "shell.open" => self.host.open(args["app"].as_str().unwrap_or_default())?,
            "shell.close" => self.host.close()?,
            other => return Err(format!("no shell intent {other}")),
        };
        let text = match info.name.as_str() {
            "shell.open" => format!("Opening {}", args["app"].as_str().unwrap_or_default()),
            "shell.close" => "Closing the app in front".to_string(),
            _ => "The apps".to_string(),
        };
        Ok(Reply::done(text).with_value(v))
    }

    fn reply_path(&self, call_id: &str) -> Result<PathBuf, String> {
        if !rules::id_ok(call_id.as_bytes()) {
            return Err(format!("{call_id:?} is not a call id"));
        }
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&self.reply_dir).map_err(|e| format!("{}: {e}", self.reply_dir.display()))?;
        std::fs::set_permissions(&self.reply_dir, std::fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
        Ok(self.reply_dir.join(format!("{call_id}.json")))
    }
}

/// Wait for an app's answer in `path`. A headless `child` that ends without one ends the wait.
fn wait_reply(path: &Path, timeout: Duration, mut child: Option<&mut std::process::Child>) -> Result<Reply, String> {
    let start = Instant::now();
    // An answer that does not parse yet may still be being written (the toolkit renames a whole file into place, a program of
    // its own may not): wait for it, until the program ends or the time is up.
    let take = |path: &Path| -> Option<Result<Reply, String>> {
        let bytes = std::fs::read(path).ok()?;
        match serde_json::from_slice::<Reply>(&bytes) {
            Ok(r) => {
                let _ = std::fs::remove_file(path);
                Some(Ok(r))
            }
            Err(e) => Some(Err(format!("the app's answer is not readable: {e}"))),
        }
    };
    loop {
        if let Some(Ok(r)) = take(path) {
            return Ok(r);
        }
        if let Some(c) = child.as_deref_mut()
            && let Ok(Some(status)) = c.try_wait()
        {
            return match take(path) {
                Some(r) => r,
                None => Err(format!("the app ended ({status}) without answering")),
            };
        }
        if start.elapsed() > timeout
            && let Some(Err(e)) = take(path)
        {
            return Err(e);
        }
        if start.elapsed() > timeout {
            return Err(format!("no answer from the app in {} s", timeout.as_secs()));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Bring a file to `target`: away when it was not there, or its old bytes back from the journal's blob.
fn put_file(journal: &Journal, path: &Path, target: State, before: Option<&str>) -> Result<(), String> {
    if !target.exists {
        return std::fs::remove_file(path).map_err(|e| format!("{}: {e}", path.display()));
    }
    let name = before.ok_or("the old content was not kept")?;
    let bytes = journal.blob(name)?;
    if spec::there(&bytes) != target {
        return Err(format!("{name} is not what {} held", path.display()));
    }
    let tmp = path.with_extension("lucina-undo");
    std::fs::write(&tmp, &bytes).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

/// What a file write of Lucina's (lucina-devd's `write_file`/`delete_file`) records for its undo: call before changing the
/// file. Returns the state before and the undo; after the change, make the [`spec::Write`] with the state after.
pub fn before_file_write(journal: &Journal, path: &Path) -> Result<(State, Undo), String> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_file() => {
            let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
            let blob = journal.keep_blob(&bytes)?;
            Ok((spec::there(&bytes), Undo::File { path: path.display().to_string(), before: Some(blob) }))
        }
        Ok(_) => Err(format!("{} is not a file", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Ok((spec::GONE, Undo::File { path: path.display().to_string(), before: None }))
        }
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

#[cfg(test)]
mod tests;
