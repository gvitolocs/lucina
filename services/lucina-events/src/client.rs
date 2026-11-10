// What programs call: [`emit`] and [`present`] over one process-wide connection to the hub, and [`Reader`] for root readers
// (lucina-devd) on the read socket.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::rules;
use crate::wire::{self, Batch, Field, Filter, Settled, Snapshot, Source, Waited};

/// The source this process says it is; App unless [`set_source`] was called.
static SOURCE: AtomicU8 = AtomicU8::new(rules::SRC_APP);
/// Numbered per process, so the hub sees a gap when lines were lost.
static SRC_SEQ: AtomicU64 = AtomicU64::new(0);

struct ClientState {
    stream: Option<UnixStream>,
    last_fail: Option<Instant>,
}

static STATE: Mutex<ClientState> = Mutex::new(ClientState {
    stream: None,
    last_fail: None,
});

/// Say this process is the shell or the toolkit, not an app.
pub fn set_source(src: Source) {
    SOURCE.store(src.code(), Ordering::Relaxed);
}

fn current_source() -> Source {
    Source::from_code(SOURCE.load(Ordering::Relaxed)).unwrap_or(Source::App)
}

fn introspect_env() -> u8 {
    match std::env::var("LUCINA_APP_INTROSPECT").as_deref() {
        Ok("tree") => rules::INTROSPECT_TREE,
        Ok("none") => rules::INTROSPECT_NONE,
        _ => rules::INTROSPECT_ALL,
    }
}

fn socket_path() -> String {
    std::env::var("LUCINA_EVENTS_SOCKET").unwrap_or_else(|_| wire::SOCKET.to_string())
}

/// Send one line; a write that would block or fails drops the line and closes the connection.
fn send_line(line: &str) {
    let mut st = STATE.lock().unwrap();
    if st.stream.is_none() {
        let failed_recently = st
            .last_fail
            .is_some_and(|t| t.elapsed() < Duration::from_secs(1));
        if failed_recently {
            return;
        }
        match UnixStream::connect(socket_path()) {
            Ok(mut s) => {
                let _ = s.set_nonblocking(true);
                let hello = wire::In::Hello {
                    src: current_source(),
                    app: std::env::var("LUCINA_APP_ID").ok(),
                    introspect: Some(
                        match introspect_env() {
                            rules::INTROSPECT_TREE => "tree",
                            rules::INTROSPECT_NONE => "none",
                            _ => "all",
                        }
                        .to_string(),
                    ),
                };
                let mut hello_line = serde_json::to_string(&hello).unwrap();
                hello_line.push('\n');
                if s.write_all(hello_line.as_bytes()).is_err() {
                    st.last_fail = Some(Instant::now());
                    return;
                }
                st.stream = Some(s);
            }
            Err(_) => {
                st.last_fail = Some(Instant::now());
                return;
            }
        }
    }
    let mut line = line.to_string();
    line.push('\n');
    let stream = st.stream.as_mut().unwrap();
    if stream.write_all(line.as_bytes()).is_err() {
        st.stream = None;
    }
}

/// Whether a reader asked for tree events lately ([`wire::TREE_WANTED`]): the toolkit builds a tree after a paint only then. One
/// file check a second at most.
pub fn tree_wanted() -> bool {
    static LAST: Mutex<Option<(Instant, bool)>> = Mutex::new(None);
    let mut last = LAST.lock().unwrap_or_else(|p| p.into_inner());
    if let Some((at, wanted)) = *last
        && at.elapsed() < Duration::from_secs(1)
    {
        return wanted;
    }
    let wanted = std::fs::metadata(wire::TREE_WANTED)
        .and_then(|m| m.modified())
        // A clock that moved back (the wall clock is set after boot) counts as just now.
        .map(|t| t.elapsed().map_or(true, |age| age < wire::TREE_WANTED_FOR))
        .unwrap_or(false);
    *last = Some((Instant::now(), wanted));
    wanted
}

/// Start an event: `emit("media.ended").subject("track1").field("codec", "opus").send()`.
pub fn emit(kind: &str) -> Emit {
    Emit {
        kind: kind.to_string(),
        subject: None,
        fields: Vec::new(),
    }
}

pub struct Emit {
    kind: String,
    subject: Option<String>,
    fields: Vec<Field>,
}

impl Emit {
    pub fn subject(mut self, s: &str) -> Self {
        self.subject = Some(s.to_string());
        self
    }

    pub fn field(mut self, name: &str, v: impl Into<Value>) -> Self {
        self.fields.push(Field {
            n: name.to_string(),
            v: v.into(),
            vis: rules::VIS_PUBLIC,
        });
        self
    }

    /// A field with an explicit visibility mark.
    pub fn field_vis(mut self, name: &str, v: impl Into<Value>, vis: u8) -> Self {
        self.fields.push(Field {
            n: name.to_string(),
            v: v.into(),
            vis,
        });
        self
    }

    /// Documents that a field was deliberately left out; adds nothing.
    pub fn private(self, _name: &str) -> Self {
        self
    }

    pub fn send(self) {
        let src = current_source();
        if !rules::valid_kind(self.kind.as_bytes())
            || !rules::may_record(src.code(), introspect_env())
        {
            return;
        }
        let src_seq = SRC_SEQ.fetch_add(1, Ordering::Relaxed);
        let vis: Vec<u8> = self.fields.iter().map(|f| f.vis).collect();
        let keep = rules::exposed_fields(&vis, rules::content_free(self.kind.as_bytes()));
        let fields: Vec<Field> = keep.into_iter().map(|i| self.fields[i].clone()).collect();
        let line = wire::In::Emit {
            src_seq,
            kind: self.kind,
            subject: self.subject,
            fields,
        };
        send_line(&serde_json::to_string(&line).unwrap());
    }
}

/// Present a frame: one `Present` line per call, no throttle. The rects go as given when there are at most 16,
/// else their union as one rect.
pub fn present(frame: u64, rects: &[[i32; 4]], gpu: u32) {
    let rects = if rects.len() <= 16 {
        rects.to_vec()
    } else {
        vec![
            rects
                .iter()
                .copied()
                .reduce(wire::union)
                .unwrap_or([0, 0, 0, 0]),
        ]
    };
    let line = wire::In::Present {
        frame,
        rects,
        gpu: Some(gpu),
    };
    send_line(&serde_json::to_string(&line).unwrap());
}

/// A root reader on the read socket.
pub struct Reader {
    stream: UnixStream,
}

impl Reader {
    pub fn connect(path: &Path) -> io::Result<Reader> {
        Ok(Reader {
            stream: UnixStream::connect(path)?,
        })
    }

    fn ask<T: DeserializeOwned>(&mut self, ask: &wire::Ask, timeout: Duration) -> io::Result<T> {
        self.stream.set_read_timeout(Some(timeout))?;
        let mut line = serde_json::to_string(ask)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        line.push('\n');
        self.stream.write_all(line.as_bytes())?;
        let mut buf: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let n = self.stream.read(&mut chunk)?;
            if n == 0 {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "hub closed"));
            }
            buf.extend_from_slice(&chunk[..n]);
            if let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                buf.truncate(pos);
                break;
            }
            if buf.len() > wire::MAX_LINE {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "line too long"));
            }
        }
        let v: Value = serde_json::from_slice(&buf)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        if v.get("ok") == Some(&Value::Bool(false)) {
            let msg = v
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("refused")
                .to_string();
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, msg));
        }
        serde_json::from_value(v).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    pub fn read(&mut self, since: u64, filter: &Filter, max: Option<u64>) -> io::Result<Batch> {
        self.ask(
            &wire::Ask::Read {
                since,
                filter: filter.clone(),
                max,
            },
            Duration::from_secs(10),
        )
    }

    pub fn wait(&mut self, since: u64, filter: &Filter, timeout_ms: u64) -> io::Result<Waited> {
        self.ask(
            &wire::Ask::Wait {
                since,
                filter: filter.clone(),
                timeout_ms,
            },
            Duration::from_millis(timeout_ms) + Duration::from_secs(2),
        )
    }

    pub fn settle(
        &mut self,
        since: u64,
        settle_ms: u64,
        timeout_ms: u64,
    ) -> io::Result<Settled> {
        self.ask(
            &wire::Ask::Settle {
                since,
                settle_ms,
                timeout_ms,
            },
            Duration::from_millis(timeout_ms) + Duration::from_secs(2),
        )
    }

    pub fn stats(&mut self) -> io::Result<Value> {
        self.ask(&wire::Ask::Stats {}, Duration::from_secs(10))
    }

    pub fn state(&mut self) -> io::Result<Snapshot> {
        self.ask(&wire::Ask::State {}, Duration::from_secs(10))
    }
}
