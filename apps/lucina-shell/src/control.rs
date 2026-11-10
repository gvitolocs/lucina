//! `/run/lucina/shell.sock`: what `lucina-devd` asks the shell for, so that Lucina can open and close apps as the owner does. One
//! JSON line in, one JSON line out, then the connection ends. Mode 0600: root only.
//!
//! ```text
//! {"op":"list"}                          {"ok":true,"apps":[{"id","title"}],"front":"video"|null}
//! {"op":"open","id":"video","args":[…]}  {"ok":true,"opening":"video","closing":"player"|null}
//! {"op":"open","id":"notes","intent":{…},"reply":"/run/lucina/intents/<call>.json"}   the same, started for an app intent
//! {"op":"close"}                         {"ok":true,"closing":"video"|null}
//! {"op":"notify","title":"…","body":"…"} {"ok":true}
//! {"op":"notices"}                       {"ok":true,"notice":"…"|null,"system":"…"|null}
//! {"op":"inhibit","what":"sleep"|"idle","who":"…","why":"…"}  {"ok":true,"id":N}, then the connection stays open: the hold lasts until it closes
//! {"op":"holds"}  {"ok":true, …the shell's own holds…, "inhibitors":[{"id","what","who","why","for_s"}]}
//! ```
//!
//! Everything is non-blocking and read inside the shell's own poll loop: nothing here waits, and nothing touches the UI from
//! another thread. Opening is what activating the app's tile does; nothing here runs a command or touches the disk otherwise.

use std::io::{ErrorKind, Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

pub const SOCKET: &str = "/run/lucina/shell.sock";
const MAX_LINE: usize = 16 << 10;
const MAX_CLIENTS: usize = 4;
/// A client that has not sent its line by then is dropped.
const CLIENT_TIME: Duration = Duration::from_secs(2);
pub const MAX_ARGS: usize = 16;
const MAX_ARG: usize = 4096;
const MAX_NOTICE: usize = 300;

/// Where an app started for an intent writes its answer (lucina-devd makes it, mode 0700).
pub const INTENT_DIR: &str = "/run/lucina/intents";

/// An app intent to start the app for (docs/research/juno-intents.md): the call as compact JSON for `LUCINA_INTENT`, and the
/// file for `LUCINA_INTENT_REPLY`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Intent {
    pub call: String,
    pub reply: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum What {
    Sleep,
    Idle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    List,
    Open {
        id: String,
        args: Vec<String>,
        intent: Option<Intent>,
    },
    Close,
    Notify {
        title: String,
        body: String,
    },
    Notices,
    Inhibit {
        what: What,
        who: String,
        why: String,
    },
    Holds,
}

fn text_field(v: &Value, key: &str, max: usize) -> Result<String, String> {
    let s = v[key]
        .as_str()
        .ok_or_else(|| format!("{key} must be a string"))?;
    if s.chars().count() > max {
        return Err(format!("{key} is longer than {max} characters"));
    }
    Ok(s.to_owned())
}

/// The request, or why it is not one.
pub fn parse(line: &str) -> Result<Request, String> {
    let v: Value = serde_json::from_str(line.trim()).map_err(|_| "not JSON".to_string())?;
    if !v.is_object() {
        return Err("not a JSON object".into());
    }
    match v["op"].as_str() {
        Some("list") => Ok(Request::List),
        Some("close") => Ok(Request::Close),
        Some("notices") => Ok(Request::Notices),
        Some("open") => {
            let id = text_field(&v, "id", 64)?;
            if id.is_empty()
                || !id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
            {
                return Err("id must be an app's id".into());
            }
            let args = match &v["args"] {
                Value::Null => Vec::new(),
                Value::Array(a) => {
                    if a.len() > MAX_ARGS {
                        return Err(format!("at most {MAX_ARGS} args"));
                    }
                    a.iter()
                        .map(|x| match x.as_str() {
                            Some(s) if s.len() <= MAX_ARG && !s.contains('\0') => Ok(s.to_owned()),
                            Some(_) => Err("an arg is too long or holds a NUL".to_string()),
                            None => Err("args must be strings".to_string()),
                        })
                        .collect::<Result<_, _>>()?
                }
                _ => return Err("args must be a list of strings".into()),
            };
            let intent = match (&v["intent"], &v["reply"]) {
                (Value::Null, Value::Null) => None,
                (Value::Object(call), Value::String(reply)) => {
                    if !args.is_empty() {
                        return Err("args and intent do not go together".into());
                    }
                    if !call.get("name").is_some_and(Value::is_string) {
                        return Err("the intent has no name".into());
                    }
                    let reply = reply_path(reply)?;
                    Some(Intent {
                        call: Value::Object(call.clone()).to_string(),
                        reply,
                    })
                }
                (Value::Object(_), _) => return Err("an intent needs its reply file".into()),
                (Value::Null, _) => return Err("a reply file without an intent".into()),
                _ => return Err("the intent must be a JSON object".into()),
            };
            Ok(Request::Open { id, args, intent })
        }
        Some("notify") => {
            let title = text_field(&v, "title", MAX_NOTICE)?;
            let body = text_field(&v, "body", MAX_NOTICE)?;
            if title.trim().is_empty() && body.trim().is_empty() {
                return Err("a notice needs a title or a body".into());
            }
            Ok(Request::Notify { title, body })
        }
        Some("inhibit") => {
            // Missing is sleep: the screen may go off, the machine stays awake. "a:b" in either order asks for both.
            let what = match v["what"].as_str() {
                None => What::Sleep,
                Some("sleep") => What::Sleep,
                Some("idle" | "idle:sleep" | "sleep:idle") => What::Idle,
                Some(_) => return Err("what must be sleep or idle".into()),
            };
            let who = v["who"]
                .as_str()
                .ok_or_else(|| "who must be 1 to 64 characters".to_string())?
                .trim();
            if !matches!(who.chars().count(), 1..=64) {
                return Err("who must be 1 to 64 characters".into());
            }
            let why = match &v["why"] {
                Value::Null => String::new(),
                _ => text_field(&v, "why", 300)?,
            };
            Ok(Request::Inhibit {
                what,
                who: who.to_owned(),
                why,
            })
        }
        Some("holds") => Ok(Request::Holds),
        Some(op) => Err(format!("unknown op {op:?}")),
        None => Err("no op".into()),
    }
}

/// A reply file must be one plain name in [`INTENT_DIR`]: no other folder, no `..`, nothing hidden.
fn reply_path(p: &str) -> Result<String, String> {
    let name = p
        .strip_prefix(INTENT_DIR)
        .and_then(|r| r.strip_prefix('/'))
        .ok_or_else(|| format!("the reply goes in {INTENT_DIR}"))?;
    let stem = name.strip_suffix(".json").unwrap_or("");
    if stem.is_empty()
        || stem.len() > 128
        || stem.starts_with('.')
        || !stem
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.:-".contains(&b))
        || stem.contains("..")
    {
        return Err("the reply file must be <call id>.json".into());
    }
    Ok(format!("{INTENT_DIR}/{name}"))
}

/// The reply folder as it must be when an app is started for an intent: a real folder (not a link), open to root alone. And the
/// reply file, if one is there already, not a link: the app would write through it.
pub fn reply_dir_ok(dir: &std::path::Path, reply: &std::path::Path) -> Result<(), String> {
    let m = std::fs::symlink_metadata(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    if !m.is_dir() || m.permissions().mode() & 0o077 != 0 {
        return Err(format!("{} must be a folder of mode 0700", dir.display()));
    }
    if std::fs::symlink_metadata(reply).is_ok_and(|m| !m.is_file()) {
        return Err(format!("{} is not a plain file", reply.display()));
    }
    Ok(())
}

/// The one line the home screen shows: `title: body`, on one line.
pub fn notice_line(title: &str, body: &str) -> String {
    let one = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    match (one(title), one(body)) {
        (t, b) if t.is_empty() => b,
        (t, b) if b.is_empty() => t,
        (t, b) => format!("{t}: {b}"),
    }
}

pub fn refusal(why: &str) -> Value {
    json!({"ok": false, "error": why})
}

struct Client {
    stream: UnixStream,
    since: Instant,
    buf: Vec<u8>,
}

/// A sleep or idle hold over the socket: it lasts exactly as long as the connection that asked for it stays open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HoldInfo {
    pub id: u64,
    pub what: What,
    pub who: String,
    pub why: String,
    pub since: Instant,
}

/// The holds that a `pump` began or ended: the shell takes them into standby.
#[derive(Debug, Default)]
pub struct Pumped {
    pub began: Vec<HoldInfo>,
    pub ended: Vec<HoldInfo>,
}

/// The listening socket, the connections waiting for their line, and the connections that hold the sleep off.
pub struct Control {
    listener: UnixListener,
    clients: Vec<Client>,
    holds: Vec<(UnixStream, HoldInfo)>,
    next_id: u64,
}

pub const MAX_HOLDS: usize = 32;

impl Control {
    /// Binds `path` with mode 0600, replacing a socket left by an earlier shell.
    pub fn bind(path: &str) -> std::io::Result<Control> {
        if std::fs::symlink_metadata(path).is_ok_and(|m| !m.is_dir()) {
            std::fs::remove_file(path)?;
        }
        // The socket must never exist with a wider mode, even for a moment.
        // SAFETY: umask only changes this process's mask; it is put back at once.
        let old = unsafe { libc::umask(0o177) };
        let bound = UnixListener::bind(path);
        // SAFETY: as above.
        unsafe { libc::umask(old) };
        let listener = bound?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        Ok(Control {
            listener,
            clients: Vec::new(),
            holds: Vec::new(),
            next_id: 1,
        })
    }

    /// What the poll waits on: the listener, the waiting clients, and the held connections, so the shell wakes when one closes.
    pub fn fds(&self) -> Vec<RawFd> {
        std::iter::once(self.listener.as_raw_fd())
            .chain(self.clients.iter().map(|c| c.stream.as_raw_fd()))
            .chain(self.holds.iter().map(|(s, _)| s.as_raw_fd()))
            .collect()
    }

    /// When a waiting client runs out of time.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.clients.iter().map(|c| c.since + CLIENT_TIME).min()
    }

    /// One client's answer, then the connection ends. A short answer fits the socket's buffer; a client that does not read
    /// it loses it, and the shell goes on.
    fn reply_and_drop(&mut self, i: usize, v: Value) {
        let c = self.clients.swap_remove(i);
        let _ = c.stream.set_nonblocking(false);
        let _ = c.stream.set_write_timeout(Some(Duration::from_millis(100)));
        let mut line = v.to_string();
        line.push('\n');
        let _ = (&c.stream).write_all(line.as_bytes());
    }

    /// A hold: the connection is answered at once and then kept, non-blocking, until it closes. When there are too many,
    /// it is refused and the connection ends instead.
    fn take_hold(
        &mut self,
        i: usize,
        what: What,
        who: String,
        why: String,
        now: Instant,
        pumped: &mut Pumped,
    ) {
        if self.holds.len() >= MAX_HOLDS {
            self.reply_and_drop(i, refusal("too many holds"));
            return;
        }
        let id = self.next_id;
        self.next_id += 1;
        let c = self.clients.swap_remove(i);
        let _ = c.stream.set_nonblocking(false);
        let _ = c.stream.set_write_timeout(Some(Duration::from_millis(100)));
        let info = HoldInfo {
            id,
            what,
            who,
            why,
            since: now,
        };
        let mut line = json!({"ok": true, "id": id}).to_string();
        line.push('\n');
        if (&c.stream).write_all(line.as_bytes()).is_ok() {
            let _ = c.stream.set_nonblocking(true);
            self.holds.push((c.stream, info.clone()));
            pumped.began.push(info);
        }
    }

    /// New connections and the lines that have come, without waiting. A complete line goes to `answer`, and what it gives
    /// back is written to that client before its connection ends — except `inhibit`, answered here: its connection stays
    /// open as the hold. Every held connection is read without blocking: what comes is thrown away, and the hold ends when
    /// the peer closes it.
    pub fn pump(
        &mut self,
        now: Instant,
        answer: &mut dyn FnMut(Result<Request, String>) -> Value,
    ) -> Pumped {
        let mut pumped = Pumped::default();
        loop {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    if self.clients.len() >= MAX_CLIENTS || stream.set_nonblocking(true).is_err() {
                        continue;
                    }
                    self.clients.push(Client {
                        stream,
                        since: now,
                        buf: Vec::new(),
                    });
                }
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        let mut i = 0;
        while i < self.clients.len() {
            let c = &mut self.clients[i];
            let mut chunk = [0u8; 4096];
            let mut done = false;
            loop {
                match c.stream.read(&mut chunk) {
                    Ok(0) => {
                        done = true;
                        break;
                    }
                    Ok(n) => {
                        c.buf.extend_from_slice(&chunk[..n]);
                        if c.buf.len() > MAX_LINE || c.buf.contains(&b'\n') {
                            break;
                        }
                    }
                    Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                    Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                    Err(_) => {
                        done = true;
                        break;
                    }
                }
            }
            let line_end = c.buf.iter().position(|&b| b == b'\n');
            if c.buf.len() > MAX_LINE && line_end.is_none() {
                self.reply_and_drop(i, refusal("the request is too long"));
            } else if line_end.is_some() || (done && !c.buf.is_empty()) {
                let line = match line_end {
                    Some(end) => String::from_utf8_lossy(&c.buf[..end]).into_owned(),
                    None => String::from_utf8_lossy(&c.buf).into_owned(),
                };
                let request = parse(&line);
                if let Ok(Request::Inhibit { what, who, why }) = &request {
                    self.take_hold(i, *what, who.clone(), why.clone(), now, &mut pumped);
                } else {
                    let holds = matches!(request, Ok(Request::Holds));
                    let mut v = answer(request);
                    if holds && v.is_object() && v["ok"].as_bool() == Some(true) {
                        v["inhibitors"] = Value::Array(
                            self.holds
                                .iter()
                                .map(|(_, h)| {
                                    json!({
                                        "id": h.id,
                                        "what": match h.what { What::Sleep => "sleep", What::Idle => "idle" },
                                        "who": h.who,
                                        "why": h.why,
                                        "for_s": now.saturating_duration_since(h.since).as_secs(),
                                    })
                                })
                                .collect(),
                        );
                    }
                    self.reply_and_drop(i, v);
                }
            } else if done || now.duration_since(c.since) >= CLIENT_TIME {
                self.clients.swap_remove(i);
            } else {
                i += 1;
            }
        }
        let mut i = 0;
        while i < self.holds.len() {
            let (stream, _) = &mut self.holds[i];
            let mut chunk = [0u8; 4096];
            let mut open = false;
            loop {
                match stream.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(_) => continue,
                    Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                    Err(e) if e.kind() == ErrorKind::WouldBlock => {
                        open = true;
                        break;
                    }
                    Err(_) => break,
                }
            }
            if open {
                i += 1;
            } else {
                let (_, info) = self.holds.swap_remove(i);
                pumped.ended.push(info);
            }
        }
        pumped
    }

    /// The holds now, in the order they began.
    #[allow(dead_code)]
    pub fn holds(&self) -> impl Iterator<Item = &HoldInfo> {
        self.holds.iter().map(|(_, h)| h)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_five_requests() {
        assert_eq!(parse(r#"{"op":"list"}"#), Ok(Request::List));
        assert_eq!(parse(r#"{"op":"close"}"#), Ok(Request::Close));
        assert_eq!(parse(r#"{"op":"notices"}"#), Ok(Request::Notices));
        assert_eq!(
            parse(r#" {"op":"open","id":"video"} "#),
            Ok(Request::Open {
                id: "video".into(),
                args: vec![],
                intent: None
            })
        );
        assert_eq!(
            parse(r#"{"op":"open","id":"video","args":["/data/home/a b.mp4"]}"#),
            Ok(Request::Open {
                id: "video".into(),
                args: vec!["/data/home/a b.mp4".into()],
                intent: None
            })
        );
        assert_eq!(
            parse(r#"{"op":"notify","title":"Hi","body":""}"#),
            Ok(Request::Notify {
                title: "Hi".into(),
                body: "".into()
            })
        );
    }

    #[test]
    fn open_for_an_intent_carries_the_call_and_a_reply_file_in_the_intents_folder() {
        let r = parse(
            r#"{"op":"open","id":"notes","intent":{"call":"c1","name":"notes.open","args":{"id":"n_1"}},"reply":"/run/lucina/intents/c1.json"}"#,
        );
        let Ok(Request::Open {
            id,
            args,
            intent: Some(i),
        }) = r
        else {
            panic!("{r:?}")
        };
        assert_eq!((id.as_str(), args.len()), ("notes", 0));
        assert_eq!(i.reply, "/run/lucina/intents/c1.json");
        let call: Value = serde_json::from_str(&i.call).unwrap();
        assert_eq!(call["name"], "notes.open");
        assert_eq!(call["args"]["id"], "n_1");
        for (line, why) in [
            (
                r#"{"op":"open","id":"n","intent":{"name":"n.o"},"args":["a"],"reply":"/run/lucina/intents/c.json"}"#,
                "do not go together",
            ),
            (
                r#"{"op":"open","id":"n","intent":{"name":"n.o"}}"#,
                "needs its reply",
            ),
            (
                r#"{"op":"open","id":"n","reply":"/run/lucina/intents/c.json"}"#,
                "without an intent",
            ),
            (
                r#"{"op":"open","id":"n","intent":"x","reply":"/run/lucina/intents/c.json"}"#,
                "JSON object",
            ),
            (
                r#"{"op":"open","id":"n","intent":{"call":"c"},"reply":"/run/lucina/intents/c.json"}"#,
                "no name",
            ),
            (
                r#"{"op":"open","id":"n","intent":{"name":"n.o"},"reply":"/tmp/c.json"}"#,
                "goes in",
            ),
            (
                r#"{"op":"open","id":"n","intent":{"name":"n.o"},"reply":"/run/lucina/intents/../shell.sock"}"#,
                "<call id>.json",
            ),
            (
                r#"{"op":"open","id":"n","intent":{"name":"n.o"},"reply":"/run/lucina/intents/a/b.json"}"#,
                "<call id>.json",
            ),
            (
                r#"{"op":"open","id":"n","intent":{"name":"n.o"},"reply":"/run/lucina/intents/.json"}"#,
                "<call id>.json",
            ),
            (
                r#"{"op":"open","id":"n","intent":{"name":"n.o"},"reply":"/run/lucina/intents/c.txt"}"#,
                "<call id>.json",
            ),
            (
                r#"{"op":"open","id":"n","intent":{"name":"n.o"},"reply":"/run/lucina/intentsx/c.json"}"#,
                "goes in",
            ),
        ] {
            assert!(
                parse(line).unwrap_err().contains(why),
                "{line}: {:?}",
                parse(line)
            );
        }
    }

    #[test]
    fn the_reply_folder_must_be_roots_alone_and_the_file_no_link() {
        let d = std::env::temp_dir().join(format!("lucina-shell-intents-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(reply_dir_ok(&d, &d.join("c.json")).is_err());
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(reply_dir_ok(&d, &d.join("c.json")).is_ok());
        std::os::unix::fs::symlink("/etc/passwd", d.join("c.json")).unwrap();
        assert!(
            reply_dir_ok(&d, &d.join("c.json"))
                .unwrap_err()
                .contains("not a plain file")
        );
        let link = d.with_extension("link");
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&d, &link).unwrap();
        assert!(reply_dir_ok(&link, &link.join("x.json")).is_err());
        std::fs::remove_file(&link).ok();
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn what_is_not_a_request_says_why() {
        assert_eq!(parse("open video"), Err("not JSON".into()));
        assert_eq!(parse("[1]"), Err("not a JSON object".into()));
        assert_eq!(parse("{}"), Err("no op".into()));
        assert_eq!(
            parse(r#"{"op":"reboot"}"#),
            Err("unknown op \"reboot\"".into())
        );
        assert!(parse(r#"{"op":"open"}"#).unwrap_err().contains("id"));
        assert!(
            parse(r#"{"op":"open","id":"../bin/sh"}"#)
                .unwrap_err()
                .contains("app's id")
        );
        assert!(parse(r#"{"op":"open","id":""}"#).is_err());
        assert!(
            parse(r#"{"op":"open","id":"v","args":"x"}"#)
                .unwrap_err()
                .contains("list")
        );
        assert!(
            parse(r#"{"op":"open","id":"v","args":[1]}"#)
                .unwrap_err()
                .contains("strings")
        );
        let many = format!(
            r#"{{"op":"open","id":"v","args":[{}]}}"#,
            vec!["\"a\""; MAX_ARGS + 1].join(",")
        );
        assert!(parse(&many).unwrap_err().contains("at most"));
        let long = format!(
            r#"{{"op":"open","id":"v","args":["{}"]}}"#,
            "a".repeat(MAX_ARG + 1)
        );
        assert!(parse(&long).is_err());
        assert!(parse(r#"{"op":"open","id":"v","args":["a\u0000b"]}"#).is_err());
        assert!(
            parse(r#"{"op":"notify","title":"","body":"  "}"#)
                .unwrap_err()
                .contains("needs")
        );
        assert!(parse(r#"{"op":"notify","title":1,"body":"x"}"#).is_err());
        let long = format!(
            r#"{{"op":"notify","title":"{}","body":"x"}}"#,
            "a".repeat(MAX_NOTICE + 1)
        );
        assert!(parse(&long).is_err());
    }

    #[test]
    fn a_notice_is_one_line() {
        assert_eq!(
            notice_line("Lucina", "The film is\nready"),
            "Lucina: The film is ready"
        );
        assert_eq!(notice_line("", "Body only"), "Body only");
        assert_eq!(notice_line(" Title ", ""), "Title");
    }

    #[test]
    fn a_client_gets_its_answer_and_a_silent_one_is_dropped() {
        let path =
            std::env::temp_dir().join(format!("lucina-shell-control-{}.sock", std::process::id()));
        let path = path.to_str().unwrap();
        let mut control = Control::bind(path).unwrap();
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let mut asking = UnixStream::connect(path).unwrap();
        let silent = UnixStream::connect(path).unwrap();
        asking.write_all(b"{\"op\":\"list\"}\n").unwrap();
        let mut seen = Vec::new();
        let t0 = Instant::now();
        control.pump(t0, &mut |r| {
            seen.push(r.clone());
            json!({"ok": true, "apps": []})
        });
        assert_eq!(seen, vec![Ok(Request::List)]);
        let mut reply = String::new();
        asking.read_to_string(&mut reply).unwrap();
        assert_eq!(reply, "{\"apps\":[],\"ok\":true}\n");
        assert_eq!(control.fds().len(), 2);
        control.pump(t0 + CLIENT_TIME, &mut |_| unreachable!());
        assert_eq!(control.fds().len(), 1);
        drop(silent);
        // A second shell replaces the socket.
        drop(control);
        assert!(Control::bind(path).is_ok());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_line_without_its_end_is_still_read_when_the_client_closes() {
        let path = std::env::temp_dir().join(format!(
            "lucina-shell-control-eof-{}.sock",
            std::process::id()
        ));
        let path = path.to_str().unwrap();
        let mut control = Control::bind(path).unwrap();
        let mut c = UnixStream::connect(path).unwrap();
        c.write_all(b"{\"op\":\"close\"}").unwrap();
        c.shutdown(std::net::Shutdown::Write).unwrap();
        let mut seen = Vec::new();
        control.pump(Instant::now(), &mut |r| {
            seen.push(r);
            json!({"ok": true})
        });
        assert_eq!(seen, vec![Ok(Request::Close)]);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_client_that_never_reads_does_not_hold_the_shell() {
        let path = std::env::temp_dir().join(format!(
            "lucina-shell-control-deaf-{}.sock",
            std::process::id()
        ));
        let path = path.to_str().unwrap();
        let mut control = Control::bind(path).unwrap();
        let mut deaf = UnixStream::connect(path).unwrap();
        deaf.write_all(b"{\"op\":\"list\"}\n").unwrap();
        let t0 = Instant::now();
        // An answer far larger than the socket's buffer.
        control.pump(t0, &mut |_| json!({"ok": true, "big": "x".repeat(4 << 20)}));
        assert!(t0.elapsed() < Duration::from_secs(1), "{:?}", t0.elapsed());
        let mut next = UnixStream::connect(path).unwrap();
        next.write_all(b"{\"op\":\"close\"}\n").unwrap();
        control.pump(Instant::now(), &mut |_| json!({"ok": true}));
        let mut reply = String::new();
        next.read_to_string(&mut reply).unwrap();
        assert_eq!(reply, "{\"ok\":true}\n");
        drop(deaf);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn an_inhibit_is_a_hold_and_holds_lists_them() {
        let inhibits = |what: &str, who: &str, why: Option<&str>| {
            let line = format!(
                r#"{{"op":"inhibit","what":"{what}","who":"{who}"{}}}"#,
                why.map(|w| format!(",\"why\":\"{w}\"")).unwrap_or_default()
            );
            parse(&line)
        };
        assert_eq!(
            inhibits("sleep", "agent X", Some("running a test")),
            Ok(Request::Inhibit {
                what: What::Sleep,
                who: "agent X".into(),
                why: "running a test".into()
            })
        );
        assert_eq!(
            parse(r#"{"op":"inhibit","what":"idle","who":"a"}"#),
            Ok(Request::Inhibit {
                what: What::Idle,
                who: "a".into(),
                why: "".into()
            })
        );
        // Missing asks for sleep only; both in either order asks for the screen too.
        assert_eq!(
            parse(r#"{"op":"inhibit","who":"a"}"#),
            Ok(Request::Inhibit {
                what: What::Sleep,
                who: "a".into(),
                why: "".into()
            })
        );
        for both in ["idle:sleep", "sleep:idle"] {
            assert_eq!(
                inhibits(both, "a", None),
                Ok(Request::Inhibit {
                    what: What::Idle,
                    who: "a".into(),
                    why: "".into()
                })
            );
        }
        assert_eq!(
            parse(r#"{"op":"inhibit","who":"  spaced  ","why":" w "}"#),
            Ok(Request::Inhibit {
                what: What::Sleep,
                who: "spaced".into(),
                why: " w ".into()
            })
        );
        assert_eq!(
            parse(&format!(r#"{{"op":"inhibit","who":"{}"}}"#, "a".repeat(64))),
            Ok(Request::Inhibit {
                what: What::Sleep,
                who: "a".repeat(64),
                why: "".into()
            })
        );
        for (line, why) in [
            (r#"{"op":"inhibit","what":"sleep"}"#, "who must be 1 to 64"),
            (
                r#"{"op":"inhibit","what":"sleep","who":1}"#,
                "who must be 1 to 64",
            ),
            (
                r#"{"op":"inhibit","what":"sleep","who":""}"#,
                "who must be 1 to 64",
            ),
            (
                r#"{"op":"inhibit","what":"sleep","who":"   "}"#,
                "who must be 1 to 64",
            ),
            (
                &format!(
                    r#"{{"op":"inhibit","what":"sleep","who":"{}"}}"#,
                    "a".repeat(65)
                ),
                "who must be 1 to 64",
            ),
            (
                r#"{"op":"inhibit","what":"screen","who":"a"}"#,
                "what must be sleep or idle",
            ),
            (
                r#"{"op":"inhibit","what":"idle:screens","who":"a"}"#,
                "what must be sleep or idle",
            ),
            (
                r#"{"op":"inhibit","what":"sleep:screen","who":"a"}"#,
                "what must be sleep or idle",
            ),
        ] {
            assert!(
                parse(line).unwrap_err().contains(why),
                "{line}: {:?}",
                parse(line)
            );
        }
        assert!(
            parse(&format!(
                r#"{{"op":"inhibit","what":"sleep","who":"a","why":"{}"}}"#,
                "a".repeat(301)
            ))
            .is_err()
        );
        assert_eq!(parse(r#"{"op":"holds"}"#), Ok(Request::Holds));
    }

    /// Pumps until `s` has a whole reply line (at most 2 s), and returns it as JSON.
    fn reply_of(control: &mut Control, s: &mut UnixStream) -> Value {
        s.set_nonblocking(true).unwrap();
        let until = Instant::now() + Duration::from_secs(2);
        let mut got = Vec::new();
        while !got.contains(&b'\n') {
            assert!(
                Instant::now() < until,
                "no reply line: {:?}",
                String::from_utf8_lossy(&got)
            );
            control.pump(Instant::now(), &mut |_| json!({"ok": true}));
            let mut chunk = [0u8; 512];
            if let Ok(n) = s.read(&mut chunk) {
                got.extend_from_slice(&chunk[..n]);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let end = got.iter().position(|&b| b == b'\n').unwrap();
        serde_json::from_slice(&got[..end]).unwrap()
    }

    /// Pumps until a hold begins or ends (at most 2 s).
    fn pump_until(control: &mut Control, ended: bool) -> Pumped {
        let until = Instant::now() + Duration::from_secs(2);
        loop {
            let p = control.pump(Instant::now(), &mut |_| json!({"ok": true}));
            if (ended && !p.ended.is_empty()) || (!ended && !p.began.is_empty()) {
                return p;
            }
            assert!(Instant::now() < until, "nothing began or ended");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn socket_path(name: &str) -> String {
        let p =
            std::env::temp_dir().join(format!("lucina-shell-{name}-{}.sock", std::process::id()));
        p.to_str().unwrap().to_owned()
    }

    #[test]
    fn a_hold_lasted_while_the_connection_stood() {
        let path = socket_path("holds");
        let mut control = Control::bind(&path).unwrap();
        let mut holder = UnixStream::connect(&path).unwrap();
        holder
            .write_all(b"{\"op\":\"inhibit\",\"what\":\"sleep\",\"who\":\"agent X\",\"why\":\"running a test\"}\n")
            .unwrap();
        let p = pump_until(&mut control, false);
        assert_eq!(p.began.len(), 1);
        let b = &p.began[0];
        assert_eq!(
            (b.id, b.what, b.who.as_str(), b.why.as_str()),
            (1, What::Sleep, "agent X", "running a test")
        );
        assert_eq!(
            reply_of(&mut control, &mut holder),
            json!({"ok": true, "id": 1})
        );
        // A second connection asks what holds: it sees the one on the first.
        let mut asking = UnixStream::connect(&path).unwrap();
        asking.write_all(b"{\"op\":\"holds\"}\n").unwrap();
        let v = reply_of(&mut control, &mut asking);
        assert_eq!(v["ok"], true);
        let inhibitors = v["inhibitors"].as_array().unwrap();
        assert_eq!(inhibitors.len(), 1);
        assert_eq!(inhibitors[0]["id"], 1);
        assert_eq!(inhibitors[0]["what"], "sleep");
        assert_eq!(inhibitors[0]["who"], "agent X");
        assert_eq!(inhibitors[0]["why"], "running a test");
        assert!(inhibitors[0]["for_s"].is_u64());
        // When the first goes away, the hold ends with it.
        drop(holder);
        let p = pump_until(&mut control, true);
        assert_eq!(
            (p.ended.len(), p.ended[0].id, p.ended[0].who.as_str()),
            (1, 1, "agent X")
        );
        assert_eq!(control.holds().count(), 0);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn more_than_max_holds_is_refused() {
        let path = socket_path("holds-max");
        let mut control = Control::bind(&path).unwrap();
        // One at a time, as programs come: at most MAX_CLIENTS connections may wait for their line at once.
        let mut streams = Vec::new();
        for n in 0..MAX_HOLDS {
            let mut s = UnixStream::connect(&path).unwrap();
            s.write_all(
                format!("{{\"op\":\"inhibit\",\"what\":\"idle\",\"who\":\"h{n}\"}}\n").as_bytes(),
            )
            .unwrap();
            let p = pump_until(&mut control, false);
            assert_eq!((p.began[0].id, p.began[0].what), (n as u64 + 1, What::Idle));
            streams.push(s);
        }
        assert_eq!(control.holds().count(), MAX_HOLDS);
        let mut extra = UnixStream::connect(&path).unwrap();
        extra
            .write_all(b"{\"op\":\"inhibit\",\"what\":\"sleep\",\"who\":\"one too many\"}\n")
            .unwrap();
        let v = reply_of(&mut control, &mut extra);
        assert_eq!(v["ok"], false);
        assert!(v["error"].as_str().unwrap().contains("too many holds"));
        assert_eq!(control.holds().count(), MAX_HOLDS);
        drop(streams);
        let p = pump_until(&mut control, true);
        assert!(!p.ended.is_empty());
        let _ = std::fs::remove_file(&path);
    }
}
