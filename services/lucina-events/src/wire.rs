// The JSON lines on the two sockets and on the kernel device: what a program sends, what a reader asks, what the hub answers,
// and the kernel's own line format. One JSON value per line, `\n`-terminated.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::rules;

/// Where programs write their `hello`/`emit`/`present` lines.
pub const SOCKET: &str = "/run/lucina/events.sock";
/// Where root readers send `read`/`wait`/`settle`/`stats`.
pub const READ_SOCKET: &str = "/run/lucina/events-read.sock";
/// The kernel's ring device, read line by line.
pub const KERNEL_DEVICE: &str = "/dev/lucina-events";
/// Its time says when a reader last asked for tree events (`node.*`, `view.*`); apps build trees after a paint only within
/// [`TREE_WANTED_FOR`] of it.
pub const TREE_WANTED: &str = "/run/lucina/events-tree";
pub const TREE_WANTED_FOR: std::time::Duration = std::time::Duration::from_secs(120);
/// A longer line is refused, and the connection closed.
pub const MAX_LINE: usize = 16 * 1024;
/// At most this many fields an event keeps.
pub const MAX_FIELDS: usize = 16;
/// Bytes of a string field value; longer ones are cut at a char boundary and get "…".
pub const MAX_VALUE: usize = 1024;

/// The union of two rectangles given as `[x, y, w, h]`. An empty one (`w` or `h` <= 0) is ignored: the union of an empty
/// rect and `r` is `r`.
pub fn union(a: [i32; 4], b: [i32; 4]) -> [i32; 4] {
    let empty = |r: [i32; 4]| r[2] <= 0 || r[3] <= 0;
    if empty(a) {
        return b;
    }
    if empty(b) {
        return a;
    }
    let x = a[0].min(b[0]);
    let y = a[1].min(b[1]);
    let x2 = (a[0] + a[2]).max(b[0] + b[2]);
    let y2 = (a[1] + a[3]).max(b[1] + b[3]);
    [x, y, x2 - x, y2 - y]
}

/// What made an event.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    App,
    Toolkit,
    Shell,
    Kernel,
}

impl Source {
    pub fn code(self) -> u8 {
        match self {
            Source::App => rules::SRC_APP,
            Source::Toolkit => rules::SRC_TOOLKIT,
            Source::Shell => rules::SRC_SHELL,
            Source::Kernel => rules::SRC_KERNEL,
        }
    }

    pub fn from_code(c: u8) -> Option<Source> {
        match c {
            rules::SRC_APP => Some(Source::App),
            rules::SRC_TOOLKIT => Some(Source::Toolkit),
            rules::SRC_SHELL => Some(Source::Shell),
            rules::SRC_KERNEL => Some(Source::Kernel),
            _ => None,
        }
    }
}

/// One event in the queue, as the hub stores it and as a reader gets it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Event {
    pub seq: u64,
    pub src: Source,
    pub src_seq: u64,
    pub t_ms: u64,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub fields: Map<String, Value>,
}

/// A field on its way in: the sender marks its visibility, the hub decides what leaves.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Field {
    pub n: String,
    pub v: Value,
    #[serde(default)]
    pub vis: u8,
}

/// The lines a program sends. The first line of a connection is always `hello`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum In {
    Hello {
        src: Source,
        app: Option<String>,
        /// `"all"`, `"tree"` or `"none"`.
        introspect: Option<String>,
    },
    Emit {
        src_seq: u64,
        kind: String,
        subject: Option<String>,
        fields: Vec<Field>,
    },
    Present {
        frame: u64,
        rects: Vec<[i32; 4]>,
        gpu: Option<u32>,
    },
}

/// What a reader asks for. `filter` defaults to empty, which takes everything.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum Ask {
    Read {
        since: u64,
        #[serde(default)]
        filter: Filter,
        max: Option<u64>,
    },
    Wait {
        since: u64,
        #[serde(default)]
        filter: Filter,
        timeout_ms: u64,
    },
    Settle {
        since: u64,
        settle_ms: u64,
        timeout_ms: u64,
    },
    Stats {},
    State {},
}

/// A reader's filter: every key that is set must match (AND), the `kind` patterns OR among themselves.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct Filter {
    pub kind: Vec<String>,
    pub app: Option<String>,
    pub src: Vec<Source>,
    pub subject_prefix: Option<String>,
}

impl Filter {
    pub fn matches(&self, e: &Event) -> bool {
        if !self.kind.is_empty()
            && !self
                .kind
                .iter()
                .any(|p| rules::kind_matches(p.as_bytes(), e.kind.as_bytes()))
        {
            return false;
        }
        if let Some(app) = &self.app
            && e.app.as_deref() != Some(app.as_str())
        {
            return false;
        }
        if !rules::source_matches(self.mask(), e.src.code()) {
            return false;
        }
        if let Some(p) = &self.subject_prefix {
            let s = e.subject.as_deref().unwrap_or("");
            if !rules::subject_matches(p.as_bytes(), s.as_bytes()) {
                return false;
            }
        }
        true
    }

    /// The source mask: bit `code` set for each listed source, 0 when none is listed.
    pub fn mask(&self) -> u8 {
        let mut m = 0u8;
        for s in &self.src {
            m |= 1 << s.code();
        }
        m
    }

    /// A pattern is one kind exactly, or a valid kind followed by `.*`; anything else is refused.
    pub fn check(&self) -> Result<(), String> {
        for p in &self.kind {
            let b = p.as_bytes();
            let ok = rules::valid_kind(b)
                || (b.len() >= 2
                    && &b[b.len() - 2..] == b".*"
                    && rules::valid_kind(&b[..b.len() - 2]));
            if !ok {
                return Err(format!("bad kind pattern: {p}"));
            }
        }
        Ok(())
    }
}

/// A `read` answer: the events the filter took, where the cursor stands now, what it lost, whether the queue restarted.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Batch {
    pub ok: bool,
    pub events: Vec<Event>,
    pub next: u64,
    pub lost: u64,
    pub reset: bool,
}

/// A `wait` answer: the first matching event, or the timeout.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Waited {
    pub ok: bool,
    pub event: Option<Event>,
    pub timeout: bool,
    pub next: u64,
    pub lost: u64,
}

/// A `settle` answer: whether the screen settled, and the union of what was presented since the reader's cursor.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Settled {
    pub ok: bool,
    pub settled: bool,
    pub present_seq: u64,
    pub app: Option<String>,
    pub rect: Option<[i32; 4]>,
}

/// A `state` answer: the sticky events the hub keeps outside the queue, sorted by seq.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub ok: bool,
    pub events: Vec<Event>,
}

/// The error answer: `{"ok": false, "error": "..."}`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Failure {
    pub ok: bool,
    pub error: String,
}

/// One kernel line, parsed: `<src_seq> <t_ms> <kind> <subject or -> [key=value]...`.
#[derive(Clone, Debug, PartialEq)]
pub struct KernelEvent {
    pub src_seq: u64,
    pub t_ms: u64,
    pub kind: String,
    pub subject: Option<String>,
    pub fields: Map<String, Value>,
}

fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            if i + 2 >= b.len() {
                return None;
            }
            let h = |c: u8| -> Option<u8> {
                match c {
                    b'0'..=b'9' => Some(c - b'0'),
                    b'a'..=b'f' => Some(c - b'a' + 10),
                    b'A'..=b'F' => Some(c - b'A' + 10),
                    _ => None,
                }
            };
            out.push(h(b[i + 1])? * 16 + h(b[i + 2])?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'%' => out.push_str("%25"),
            b' ' => out.push_str("%20"),
            b'=' => out.push_str("%3D"),
            b'\n' => out.push_str("%0A"),
            0x21..=0x7e => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn kernel_value(raw: &str) -> Option<Value> {
    let d = percent_decode(raw)?;
    if let Ok(n) = d.parse::<i64>() {
        return Some(Value::from(n));
    }
    match d.as_str() {
        "true" => Some(Value::Bool(true)),
        "false" => Some(Value::Bool(false)),
        _ => Some(Value::String(d)),
    }
}

/// Parse one kernel line; a bad line is `None`.
pub fn parse_kernel(line: &str) -> Option<KernelEvent> {
    let mut parts = line.split(' ');
    let src_seq: u64 = parts.next()?.parse().ok()?;
    let t_ms: u64 = parts.next()?.parse().ok()?;
    let kind = percent_decode(parts.next()?)?;
    if !rules::valid_kind(kind.as_bytes()) {
        return None;
    }
    let subject = match parts.next()? {
        "-" => None,
        s => Some(percent_decode(s)?),
    };
    let mut fields = Map::new();
    for kv in parts {
        if kv.is_empty() {
            return None;
        }
        let (k, v) = kv.split_once('=')?;
        fields.insert(k.to_string(), kernel_value(v)?);
    }
    Some(KernelEvent {
        src_seq,
        t_ms,
        kind,
        subject,
        fields,
    })
}

/// Encode one kernel line, for tests; the kernel side writes the same format.
pub fn encode_kernel(
    src_seq: u64,
    t_ms: u64,
    kind: &str,
    subject: Option<&str>,
    fields: &[(String, Value)],
) -> String {
    let mut line = format!("{src_seq} {t_ms} {} ", percent_encode(kind));
    match subject {
        Some(s) => line.push_str(&percent_encode(s)),
        None => line.push('-'),
    }
    for (k, v) in fields {
        let vs = match v {
            Value::String(s) => percent_encode(s),
            other => other.to_string(),
        };
        line.push_str(&format!(" {}={vs}", percent_encode(k)));
    }
    line
}
