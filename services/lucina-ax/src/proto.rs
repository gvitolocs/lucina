//! The socket: an app listens on `/run/lucina/ax/<pid>.sock`, lucina-devd connects, sends one JSON line and reads one back.
//! `{"op":"tree"}` and `{"op":"act","id":"<16 hex>","action":"click","value":…,"version":"…"}`; see
//! docs/research/juno-ax.md for the replies.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::rules::MAX_REPLY_BYTES;

pub const DIR: &str = "/run/lucina/ax";
/// A request is a line; nothing Lucina sends is longer.
const MAX_REQUEST: u64 = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Tree,
    Act {
        /// The node id (16 hex digits). `node` is the same, as stage F's Servo socket names it.
        #[serde(alias = "node")]
        id: String,
        action: String,
        #[serde(default)]
        value: Option<String>,
        version: String,
        /// Who asks; only `lucina` today. Kept for the audit.
        #[serde(default)]
        source: Option<String>,
    },
    /// An app intent (stage C, docs/research/juno-intents.md): `call` is a `lucina_intents::spec::Call`, the reply its `Reply`.
    /// It may come after frames have run, through [`Server::take_request`] and [`Waiting::reply`].
    Intent { call: Value },
}

pub fn socket_path(pid: u32) -> PathBuf {
    Path::new(DIR).join(format!("{pid}.sock"))
}

/// A refusal reply; `tree` is the tree now, sent with it so that she need not ask again.
pub fn refusal(error: &str, why: &str, tree: Option<Value>) -> Value {
    let mut v = match tree {
        Some(Value::Object(m)) => Value::Object(m),
        _ => json!({}),
    };
    v["ok"] = json!(false);
    v["error"] = json!(error);
    v["why"] = json!(why);
    v
}

/// The listening side, in the app.
pub struct Server {
    listener: UnixListener,
    path: PathBuf,
}

impl Server {
    /// Listens on this process's socket. `Ok(None)` when there is no `/run/lucina` (tests, a host build): no tree, no cost.
    pub fn bind() -> std::io::Result<Option<Server>> {
        if !Path::new("/run/lucina").is_dir() {
            return Ok(None);
        }
        Server::bind_at(&socket_path(std::process::id())).map(Some)
    }

    pub fn bind_at(path: &Path) -> std::io::Result<Server> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
            let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        }
        let _ = std::fs::remove_file(path);
        let listener = UnixListener::bind(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        Ok(Server { listener, path: path.to_owned() })
    }

    /// For the app's `poll`: readable when someone connects.
    pub fn fd(&self) -> RawFd {
        self.listener.as_raw_fd()
    }

    /// Answers whoever is waiting, one request each, and returns how many. `answer` runs on the app's own thread, between passes.
    pub fn pump(&mut self, mut answer: impl FnMut(Result<Request, String>) -> Value) -> usize {
        let mut n = 0;
        while let Some((waiting, request)) = self.take_request() {
            n += 1;
            waiting.reply(&answer(request));
        }
        n
    }

    /// The next request, if someone is waiting, and where its reply goes: for an app that must run frames before it can
    /// answer (egui), so that the server is not borrowed meanwhile.
    pub fn take_request(&mut self) -> Option<(Waiting, Result<Request, String>)> {
        loop {
            let (stream, _) = self.listener.accept().ok()?;
            match read_request(&stream) {
                Ok(request) => return Some((Waiting { stream }, request)),
                // A client that hung up or sent nothing in time: the next one.
                Err(_) => continue,
            }
        }
    }
}

/// A request read, its reply not sent yet.
pub struct Waiting {
    stream: UnixStream,
}

impl Waiting {
    pub fn reply(self, reply: &Value) {
        let mut bytes = serde_json::to_vec(reply).unwrap_or_default();
        if bytes.len() as u64 > MAX_REPLY_BYTES {
            bytes = serde_json::to_vec(&refusal("too_big", "the tree is larger than a reply may be", None)).unwrap_or_default();
        }
        bytes.push(b'\n');
        let _ = (&self.stream).write_all(&bytes);
    }
}

fn read_request(stream: &UnixStream) -> std::io::Result<Result<Request, String>> {
    stream.set_nonblocking(false)?;
    // devd writes its line at once; an app never waits long on a client that does not.
    stream.set_read_timeout(Some(Duration::from_millis(300)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut line = String::new();
    BufReader::new(stream.take(MAX_REQUEST)).read_line(&mut line)?;
    Ok(serde_json::from_str::<Request>(line.trim()).map_err(|e| format!("bad request: {e}")))
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// The asking side, in lucina-devd: one request, one reply.
pub fn request(path: &Path, req: &Value, timeout: Duration) -> std::io::Result<Value> {
    let stream = UnixStream::connect(path)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    let mut line = serde_json::to_vec(req)?;
    line.push(b'\n');
    (&stream).write_all(&line)?;
    let mut reply = String::new();
    BufReader::new((&stream).take(MAX_REPLY_BYTES + 2)).read_line(&mut reply)?;
    serde_json::from_str(reply.trim()).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}
