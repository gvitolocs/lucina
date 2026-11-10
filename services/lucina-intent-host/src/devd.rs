//! What lucina-devd needs to serve `intents`, `intent` and `undo`: their schemas for `tools`, a [`Host`] over the shell's control
//! socket, and one [`Intents::run`] for the three tools, so that its own `tools.rs` only lists them and passes the call on.

use std::io::{BufRead, BufReader, Write as _};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use lucina_app::manifest::{self, Manifest};
use lucina_intents::journal::Journal;
use lucina_intents::spec::{self, Reply};
use serde_json::{Value, json};

use crate::{Catalog, Host, Runner};

/// How long the apps together have to list their intents.
pub const LISTING_WITHIN: Duration = Duration::from_secs(4);

/// The shell's control socket.
pub const SHELL_SOCKET: &str = "/run/lucina/shell.sock";
/// Where apps write their answers.
pub const REPLY_DIR: &str = "/run/lucina/intents";
/// Lucina's undo journal.
pub const UNDO_DIR: &str = "/data/lucina/undo";

/// The three driver tools, as `tools` lists them.
pub fn specs() -> Vec<Value> {
    vec![
        json!({"name": "intents", "description": "The apps' intents: typed actions that do a whole job in one call, without the screen. Without `app`: the app in front's and the shell's. Use them before clicking.",
            "input_schema": {"type": "object", "required": [], "properties": {"app": {"type": "string"}}, "additionalProperties": false}}),
        json!({"name": "intent", "description": "Runs one intent (`name` is <app>.<verb>, `args` as its schema says). A write she can undo returns an `action`. A destructive or external one answers needs_decision first; call again with `decision_id`.",
            "input_schema": {"type": "object", "required": ["name"], "properties": {"name": {"type": "string"}, "args": {"type": "object"}, "decision_id": {"type": "string"}}, "additionalProperties": false}}),
        json!({"name": "undo", "description": "Undoes one of her own writes on the laptop by its `action`; without `action`, lists those she can still undo (7 days). Refused when the thing changed since.",
            "input_schema": {"type": "object", "required": [], "properties": {"action": {"type": "string"}}, "additionalProperties": false}}),
    ]
}

/// One JSON line out, one back, as `apps/lucina-shell/src/control.rs` speaks.
fn ask(socket: &Path, request: &Value) -> Result<Value, String> {
    let mut s = UnixStream::connect(socket).map_err(|e| format!("the shell does not answer ({e})"))?;
    s.set_read_timeout(Some(Duration::from_secs(10))).map_err(|e| e.to_string())?;
    writeln!(s, "{request}").map_err(|e| format!("the shell: {e}"))?;
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line).map_err(|e| format!("the shell: {e}"))?;
    let v: Value = serde_json::from_str(line.trim()).map_err(|e| format!("the shell's answer: {e}"))?;
    if v["ok"] == json!(false) {
        return Err(v["error"].as_str().unwrap_or("the shell refused").to_string());
    }
    Ok(v)
}

/// lucina-devd's own check of a path she may change (its `places` and off-limits list).
pub type PathCheck = Box<dyn Fn(&Path) -> Result<(), String> + Send + Sync>;

/// The laptop's [`Host`]: the shell's socket, and lucina-devd's own check of the paths she may change.
pub struct ShellHost {
    pub socket: PathBuf,
    pub allowed: PathCheck,
}

impl Host for ShellHost {
    /// From the shell's control socket; where that is not there yet, from the shell's tree (lucina-ax), which names the app in front.
    fn front(&self) -> Option<String> {
        match ask(&self.socket, &json!({"op": "list"})) {
            Ok(v) => v["front"].as_str().map(str::to_owned),
            Err(_) => lucina_ax::client::front(Path::new(lucina_ax::proto::DIR)).ok().map(|(_, app)| app).filter(|a| a != "shell"),
        }
    }
    fn open_for(&self, app: &str, call: &Value, reply: &Path) -> Result<(), String> {
        ask(&self.socket, &json!({"op": "open", "id": app, "intent": call, "reply": reply.display().to_string()})).map(|_| ())
    }
    fn open(&self, app: &str) -> Result<Value, String> {
        ask(&self.socket, &json!({"op": "open", "id": app}))
    }
    fn close(&self) -> Result<Value, String> {
        ask(&self.socket, &json!({"op": "close"}))
    }
    fn apps(&self) -> Result<Value, String> {
        ask(&self.socket, &json!({"op": "list"}))
    }
    fn file_allowed(&self, path: &Path) -> Result<(), String> {
        (self.allowed)(path)
    }
    /// Over the app's own socket (`lucina-ax`), when the shell says it is in front and it has one.
    fn send_running(&self, app: &str, call: &Value, timeout: Duration) -> Option<Result<Reply, String>> {
        send_over_ax(Path::new(lucina_ax::proto::DIR), app, call, timeout)
    }
}

/// [`ShellHost::send_running`] in the folder `dir` of app sockets (tests use their own).
pub fn send_over_ax(dir: &Path, app: &str, call: &Value, timeout: Duration) -> Option<Result<Reply, String>> {
    let (sock, front) = lucina_ax::client::front(dir).ok()?;
    if front != app {
        return None;
    }
    let reply = lucina_ax::proto::request(&sock, &json!({"op": "intent", "call": call}), timeout)
        .map_err(|e| format!("{app} did not answer on its socket: {e}"));
    Some(reply.and_then(|v| serde_json::from_value(v).map_err(|e| format!("{app}'s answer is not readable: {e}"))))
}

/// Where the shell mounts the EFI partition with the apps' sets (`lucina_remote::appset::APPS_MOUNT`).
pub const APPS_MOUNT: &str = "/mnt/lucina-apps";
/// Apps installed on /data (`lucina-shell`'s `DATA_APPS`).
pub const DATA_APPS: &str = "/data/lucina/apps";

/// The folders the shell reads its apps from, in its order: the set in use on the disk (`<mount>/lucinaos/apps/<CURRENT>`),
/// the image's manifests, then the apps on /data, whose ids already taken are dropped.
pub fn laptop_manifest_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let sets = Path::new(APPS_MOUNT).join("lucinaos/apps");
    if let Ok(current) = std::fs::read_to_string(sets.join("CURRENT")) {
        let current = current.trim();
        if current.starts_with("g-") && current.len() <= 32 && current.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
            dirs.push(sets.join(current));
        }
    }
    dirs.push(PathBuf::from(manifest::MANIFEST_DIR));
    dirs.push(PathBuf::from(DATA_APPS));
    dirs
}

/// The manifests in `dirs`, the first of an id winning. In a set's folder or on /data the program is `<id>.bin` beside the
/// manifest, as the shell has it; in the image it is the manifest's `exec`.
pub fn manifests(dirs: &[PathBuf]) -> Vec<Manifest> {
    let mut out: Vec<Manifest> = Vec::new();
    for d in dirs {
        let beside = d.as_path() != Path::new(manifest::MANIFEST_DIR);
        for mut m in manifest::scan(d, &mut |e| eprintln!("lucina-devd: {e}")) {
            if beside {
                m.exec = d.join(format!("{}.bin", m.id)).to_string_lossy().into_owned();
            }
            if !out.iter().any(|o| o.id == m.id) {
                out.push(m);
            }
        }
    }
    out
}

/// lucina-devd's intents: the catalog (read again after a minute, so an app installed since shows up), the journal and the host.
pub struct Intents<H: Host> {
    pub host: H,
    pub journal: Journal,
    pub manifest_dirs: Vec<PathBuf>,
    pub reply_dir: PathBuf,
    pub timeout: Duration,
    catalog: Mutex<Option<(Instant, Catalog)>>,
}

impl<H: Host> Intents<H> {
    pub fn new(host: H, journal: Journal, manifest_dirs: Vec<PathBuf>, reply_dir: PathBuf, timeout: Duration) -> Intents<H> {
        Intents { host, journal, manifest_dirs, reply_dir, timeout, catalog: Mutex::new(None) }
    }

    fn catalog(&self) -> Catalog {
        let mut g = self.catalog.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((at, c)) = g.as_ref()
            && at.elapsed() < Duration::from_secs(60)
        {
            return c.clone();
        }
        // Every app at once, 4 s for all of them; a slow one keeps its last listing.
        let previous = g.as_ref().map(|(_, c)| c.clone());
        let c = Catalog::load_keeping(&manifests(&self.manifest_dirs), LISTING_WITHIN, previous.as_ref());
        *g = Some((Instant::now(), c.clone()));
        c
    }

    /// One of the three tools. `decided` is true when the server checked `decision_id` for this call. `now_ms` is wall time.
    /// The answer has `text` for the model, like every driver tool, and carries what the audit needs: `target` (the intent's
    /// name) and `undo` (the action id), so lucina-devd's audit line gets them.
    pub fn run(&self, tool: &str, args: &Value, call_id: &str, decided: bool, now_ms: u64) -> Result<Value, String> {
        let catalog = self.catalog();
        let runner = Runner { catalog: &catalog, journal: &self.journal, host: &self.host, reply_dir: self.reply_dir.clone(), timeout: self.timeout };
        let mut v = match tool {
            "intents" => {
                let front = self.host.front();
                let mut v = catalog.list(args["app"].as_str(), front.as_deref());
                v["text"] = json!(intents_text(&v, front.as_deref(), args["app"].as_str()));
                v
            }
            "intent" => {
                let name = args["name"].as_str().ok_or("intent: name is needed")?;
                let mut v = runner.call(call_id, name, &args["args"], decided, now_ms)?;
                v["target"] = json!(name);
                v["undo"] = v["action"].clone();
                v["text"] = json!(intent_text(&v));
                v
            }
            "undo" => {
                let action = args["action"].as_str();
                let mut v = runner.undo(action, call_id, now_ms)?;
                if action.is_some() {
                    v["target"] = json!("undo");
                    v["undo"] = v["undid"].clone();
                } else {
                    v["text"] = json!(undo_list_text(&v));
                }
                v
            }
            other => return Err(format!("{other} is not an intent tool")),
        };
        if v.get("text").is_none() {
            v["text"] = json!("");
        }
        Ok(v)
    }
}

// What the model reads. A client may forward only `text` (lucina-rs did, 2026-11-04: she never saw an intent's name), so the text
// carries everything she needs to act: names, arguments, effects, entities, the action to undo. The other fields stay for the
// audit and for clients that read them.

/// The listing as lines she can call from: `- notes.create(body*: text<=20000, title: text<=1000) [write; undo: notes.trash]: …`.
fn intents_text(v: &Value, front: Option<&str>, app: Option<&str>) -> String {
    let list = v["intents"].as_array().cloned().unwrap_or_default();
    let mut t = match (app, front) {
        (Some(a), _) => format!("{} intents of {a}:", list.len()),
        (None, Some(f)) => format!("{} intents ({f} is in front; others with intents {{app}}):", list.len()),
        (None, None) => format!("{} intents (the home screen is in front; others with intents {{app}}):", list.len()),
    };
    for i in &list {
        let args: Vec<String> = spec::args_of(&i["args"])
            .map(|a| {
                let mut a = a;
                a.sort_by(|x, y| y.required.cmp(&x.required).then(x.name.cmp(&y.name)));
                a.iter()
                    .map(|a| {
                        let kind = match a.kind {
                            lucina_intents::Kind::Text => format!("text<={}", a.max_len),
                            lucina_intents::Kind::Path => "absolute path".into(),
                            lucina_intents::Kind::Id => "id".into(),
                            lucina_intents::Kind::Int => format!("{}..{}", a.min, a.max),
                            lucina_intents::Kind::Bool => "true/false".into(),
                        };
                        format!("{}{}: {kind}", a.name, if a.required { "*" } else { "" })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut tags = vec![i["effect"].as_str().unwrap_or("?").to_string()];
        if i["shows"] == true {
            tags.push("opens the app".into());
        }
        if let Some(inv) = i["inverse"].as_str() {
            tags.push(format!("undo: {inv}"));
        }
        t += &format!("\n- {}({}) [{}]: {}", i["name"].as_str().unwrap_or("?"), args.join(", "), tags.join("; "), i["description"].as_str().unwrap_or(""));
    }
    if let Some(apps) = v["apps_with_intents"].as_array() {
        let names: Vec<&str> = apps.iter().filter_map(Value::as_str).collect();
        t += &format!("\nApps with intents: {}.", names.join(", "));
    }
    if let Some(p) = v["problems"].as_array().filter(|p| !p.is_empty()) {
        let p: Vec<&str> = p.iter().filter_map(Value::as_str).collect();
        t += &format!("\nNot listed (their program did not answer): {}.", p.join("; "));
    }
    t += "\n* = needed. Call: intent {name, args}.";
    t
}

/// An intent's answer: the app's sentence, what it names, its value, and how to undo it.
fn intent_text(v: &Value) -> String {
    if v["needs_decision"] == true {
        return v["text"].as_str().unwrap_or("it needs a decision first").to_string();
    }
    let mut t = v["text"].as_str().unwrap_or("").to_string();
    if let Some(es) = v["entities"].as_array().filter(|e| !e.is_empty()) {
        let es: Vec<String> = es
            .iter()
            .map(|e| format!("{}:{} {:?}", e["type"].as_str().unwrap_or(""), e["id"].as_str().unwrap_or(""), e["title"].as_str().unwrap_or("")))
            .collect();
        t += &format!("\nEntities: {}.", es.join("; "));
    }
    if !v["value"].is_null() {
        let value = v["value"].to_string();
        let cut: String = value.chars().take(4000).collect();
        t += &format!("\nValue: {cut}{}", if cut.len() < value.len() { " (cut)" } else { "" });
    }
    match (v["undoable"] == true, v["action"].as_str(), v["effect"].as_str()) {
        (true, Some(a), _) => t += &format!("\nUndo with: undo {{action: {a}}}."),
        (false, _, Some("read")) => {}
        _ => t += "\nThis cannot be undone by undo.",
    }
    t
}

/// The actions she can still undo, one a line.
fn undo_list_text(v: &Value) -> String {
    let list = v["actions"].as_array().cloned().unwrap_or_default();
    if list.is_empty() {
        return "Nothing to undo: none of her writes on this laptop in the last 7 days can still be undone.".into();
    }
    let mut t = format!("{} actions she can undo, newest first:", list.len());
    for a in &list {
        t += &format!(
            "\n- {}: {} on {}",
            a["action"].as_str().unwrap_or(""),
            a["intent"].as_str().unwrap_or(""),
            a["target"].as_str().unwrap_or("")
        );
    }
    t += "\nCall: undo {action}.";
    t
}

/// For tests and a host with no shell: answers nothing and allows nothing.
pub struct NoShell;

impl Host for NoShell {
    fn front(&self) -> Option<String> {
        None
    }
    fn open_for(&self, _: &str, _: &Value, _: &Path) -> Result<(), String> {
        Err("the shell is not running".into())
    }
    fn open(&self, _: &str) -> Result<Value, String> {
        Err("the shell is not running".into())
    }
    fn close(&self) -> Result<Value, String> {
        Err("the shell is not running".into())
    }
    fn apps(&self) -> Result<Value, String> {
        Err("the shell is not running".into())
    }
    fn file_allowed(&self, p: &Path) -> Result<(), String> {
        Err(format!("{}: no places here", p.display()))
    }
    fn send_running(&self, _: &str, _: &Value, _: Duration) -> Option<Result<Reply, String>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    #[test]
    fn the_shell_host_speaks_the_control_socket() {
        let d = std::env::temp_dir().join(format!("lucina-intent-host-devd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let sock = d.join("shell.sock");
        let l = UnixListener::bind(&sock).unwrap();
        let t = std::thread::spawn(move || {
            let mut got = Vec::new();
            for _ in 0..2 {
                let (s, _) = l.accept().unwrap();
                let mut line = String::new();
                BufReader::new(&s).read_line(&mut line).unwrap();
                let v: Value = serde_json::from_str(&line).unwrap();
                let answer = if v["op"] == "list" { json!({"ok": true, "apps": [], "front": "notes"}) } else { json!({"ok": false, "error": "no such app"}) };
                writeln!(&s, "{answer}").unwrap();
                got.push(v);
            }
            got
        });
        let h = ShellHost { socket: sock, allowed: Box::new(|_| Ok(())) };
        assert_eq!(h.front().as_deref(), Some("notes"));
        let e = h.open_for("ghost", &json!({"call": "c1"}), Path::new("/run/lucina/intents/c1.json")).unwrap_err();
        assert_eq!(e, "no such app");
        let got = t.join().unwrap();
        assert_eq!(got[1], json!({"op": "open", "id": "ghost", "intent": {"call": "c1"}, "reply": "/run/lucina/intents/c1.json"}));
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_set_or_data_app_runs_its_bin_and_the_first_of_an_id_wins() {
        let d = std::env::temp_dir().join(format!("lucina-intent-host-devd3-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        for (sub, title) in [("set", "From the set"), ("data", "From data")] {
            std::fs::create_dir_all(d.join(sub)).unwrap();
            std::fs::write(d.join(sub).join("notes.app"), format!("id = notes\ntitle = {title}\nintents = notes.create\n")).unwrap();
        }
        std::fs::write(d.join("data").join("memo.app"), "id = memo\ntitle = Memo\n").unwrap();
        let m = manifests(&[d.join("set"), d.join("data")]);
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].title, "From the set");
        assert_eq!(m[0].exec, d.join("set/notes.bin").to_string_lossy());
        assert_eq!(m[1].exec, d.join("data/memo.bin").to_string_lossy());
        assert!(laptop_manifest_dirs().contains(&PathBuf::from(manifest::MANIFEST_DIR)));
        std::fs::remove_dir_all(&d).ok();
    }

    /// A client that forwards only `text` (lucina-rs did) must still give her what she needs: names and arguments, entities, the
    /// action to undo, the list of undoable actions.
    #[test]
    fn the_text_alone_is_enough_to_act_on() {
        use std::os::unix::fs::PermissionsExt;
        let d = std::env::temp_dir().join(format!("lucina-intent-host-devd4-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("apps")).unwrap();
        let listing = r#"[{"name":"memo.create","description":"A new memo.","effect":"write","shows":false,"inverse":"memo.trash","args":{"type":"object","required":["body"],"additionalProperties":false,"properties":{"body":{"type":"string","maxLength":100,"x-kind":"text"},"title":{"type":"string","maxLength":50,"x-kind":"text"}}}},{"name":"memo.trash","description":"To the trash.","effect":"destructive","shows":false,"inverse":null,"args":{"type":"object","required":["id"],"additionalProperties":false,"properties":{"id":{"type":"string","maxLength":128,"x-kind":"id"}}}}]"#;
        let reply = r#"{"ok":true,"text":"Made memo m_1","value":{"id":"m_1"},"entities":[{"type":"memo","id":"m_1","title":"Milk"}],"write":{"kind":"create","target":"memo:m_1","before":{"exists":false,"stamp":"0"},"after":{"exists":true,"stamp":"7"},"undo":{"kind":"inverse","intent":"memo.trash","args":{"id":"m_1"}}}}"#;
        std::fs::write(d.join("listing.json"), listing).unwrap();
        std::fs::write(d.join("reply.json"), reply).unwrap();
        let script = format!(
            "#!/bin/sh\nif [ \"$1\" = --intents ]; then cat {}; exit 0; fi\ncat {} > \"$LUCINA_INTENT_REPLY\"\n",
            d.join("listing.json").display(),
            d.join("reply.json").display()
        );
        std::fs::write(d.join("apps/memo.bin"), script).unwrap();
        std::fs::set_permissions(d.join("apps/memo.bin"), std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(d.join("apps/memo.app"), "id = memo\ntitle = Memo\nintents = memo.create, memo.trash\n").unwrap();
        let i = Intents::new(NoShell, Journal::at(&d.join("undo")), vec![d.join("apps")], d.join("replies"), Duration::from_secs(5));
        let t = i.run("intents", &json!({"app": "memo"}), "c1", false, 1).unwrap()["text"].as_str().unwrap().to_owned();
        assert!(t.contains("- memo.create(body*: text<=100, title: text<=50) [write; undo: memo.trash]: A new memo."), "{t}");
        assert!(t.contains("- memo.trash(id*: id) [destructive]"), "{t}");
        let t = i.run("intent", &json!({"name": "memo.create", "args": {"body": "milk"}}), "c2", false, 2).unwrap()["text"].as_str().unwrap().to_owned();
        assert!(t.starts_with("Made memo m_1"), "{t}");
        assert!(t.contains("Entities: memo:m_1 \"Milk\"."), "{t}");
        assert!(t.contains(r#"Value: {"id":"m_1"}"#), "{t}");
        assert!(t.contains("Undo with: undo {action: act_1_2}."), "{t}");
        let t = i.run("undo", &json!({}), "c3", false, 3).unwrap()["text"].as_str().unwrap().to_owned();
        assert!(t.contains("- act_1_2: memo.create on memo:m_1"), "{t}");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn the_three_tools_answer_with_text_target_and_undo() {
        let d = std::env::temp_dir().join(format!("lucina-intent-host-devd2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let i = Intents::new(NoShell, Journal::at(&d.join("undo")), vec![d.join("apps")], d.join("replies"), Duration::from_secs(1));
        let v = i.run("intents", &json!({}), "c1", false, 1).unwrap();
        let text = v["text"].as_str().unwrap();
        assert!(text.starts_with("3 intents (the home screen is in front"), "{text}");
        assert!(text.contains("- shell.open(app*: id) [write; opens the app]: "), "{text}");
        assert!(i.run("intent", &json!({"name": "shell.open", "args": {"app": "notes"}}), "c2", false, 1).unwrap_err().contains("shell is not running"));
        let v = i.run("undo", &json!({}), "c3", false, 1).unwrap();
        assert!(v["text"].as_str().unwrap().starts_with("Nothing to undo"), "{v}");
        assert!(i.run("undo", &json!({"action": "act_9_9"}), "c4", false, 1).unwrap_err().contains("no action act_9_9"));
        assert!(i.run("bash", &json!({}), "c5", false, 1).is_err());
        assert_eq!(specs().len(), 3);
        std::fs::remove_dir_all(&d).ok();
    }
}
