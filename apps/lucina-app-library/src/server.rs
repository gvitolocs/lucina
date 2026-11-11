//! The routes of web/src/api/library.ts behind a trait, and the worker thread. The server has no command engine here: reads,
//! and a `PATCH` that carries the `updatedAt` it was based on and answers 409 if the book has moved on.

use lucina_account::{ApiError, Client};
use serde_json::{Value, json};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServerError {
    Offline(String),
    SignedOut,
    Conflict,
    Refused(String),
}

pub trait Server: Send {
    fn get(&mut self, path: &str) -> Result<Value, ServerError>;
    fn send(&mut self, method: &str, path: &str, body: &Value) -> Result<Value, ServerError>;
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
        ApiError::Http(409, _, _) => ServerError::Conflict,
        ApiError::Http(_, _, m) | ApiError::Protocol(m) => ServerError::Refused(m),
    }
}

impl Server for Account {
    fn get(&mut self, path: &str) -> Result<Value, ServerError> {
        self.client.get_json(path).map_err(from_api)
    }

    fn send(&mut self, method: &str, path: &str, body: &Value) -> Result<Value, ServerError> {
        self.client.send_json(method, path, body).map_err(from_api)
    }
}

pub fn encode(s: &str) -> String {
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

fn text(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or_default().to_owned()
}

fn opt_text(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_owned)
}

fn strings(v: &Value, key: &str) -> Vec<String> {
    v.get(key).and_then(Value::as_array).map_or_else(Vec::new, |a| {
        a.iter().filter_map(|x| x.as_str().map(str::to_owned)).collect()
    })
}

fn array<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v.get(key).and_then(Value::as_array).map_or(&[], Vec::as_slice)
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Progress {
    pub percent: Option<f64>,
    pub location: Option<u64>,
    pub page: Option<u64>,
}

fn progress(v: &Value) -> Option<Progress> {
    let p = v.get("progress").filter(|p| p.is_object())?;
    Some(Progress {
        percent: p.get("percent").and_then(Value::as_f64),
        location: p.get("location").and_then(Value::as_u64),
        page: p.get("page").and_then(Value::as_u64),
    })
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct BookRow {
    pub id: String,
    pub title: String,
    pub authors: Vec<String>,
    pub kind: String,
    pub status: String,
    pub progress: Option<Progress>,
    pub on_kindle: bool,
    pub protected: bool,
    pub highlight_count: u64,
}

pub fn parse_row(v: &Value) -> BookRow {
    BookRow {
        id: text(v, "id"),
        title: text(v, "title"),
        authors: strings(v, "authors"),
        kind: text(v, "kind"),
        status: text(v, "status"),
        progress: progress(v),
        on_kindle: v.get("onKindle").is_some_and(|k| k.is_object()),
        protected: text(v, "drm") == "protected",
        highlight_count: v.get("highlightCount").and_then(Value::as_u64).unwrap_or(0),
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Highlight {
    pub id: String,
    pub text: String,
    pub note: Option<String>,
    pub location: Option<(u64, u64)>,
    pub page: Option<String>,
    /// As supplied: a Kindle's date has no timezone, and converting it would invent one.
    pub added: Option<String>,
    pub source: String,
}

pub fn parse_highlight(v: &Value) -> Highlight {
    let loc = v.get("location").filter(|l| l.is_object()).and_then(|l| {
        Some((l.get("start")?.as_u64()?, l.get("end")?.as_u64()?))
    });
    Highlight {
        id: text(v, "id"),
        text: text(v, "text"),
        note: opt_text(v, "note"),
        location: loc,
        page: opt_text(v, "page"),
        added: opt_text(v, "addedAtRaw").or_else(|| opt_text(v, "addedAt")),
        source: text(v, "source"),
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Detail {
    pub row: BookRow,
    pub updated_at: String,
    pub owned: bool,
    pub formats: Vec<String>,
    pub ids: Vec<(&'static str, String)>,
    pub removed_at: Option<String>,
    pub mentioned_only: bool,
    pub tags: Vec<String>,
    pub briefs: Vec<(String, String)>,
    pub related: Vec<(String, String)>,
    pub highlights: Vec<Highlight>,
}

/// `{book, highlights, sessions}`, or a bare book as `PATCH` answers, with `highlights` kept from before.
pub fn parse_book(b: &Value, highlights: Vec<Highlight>) -> Detail {
    let mut row = parse_row(b);
    row.highlight_count = highlights.len() as u64;
    Detail {
        row,
        updated_at: text(b, "updatedAt"),
        owned: b.get("owned").and_then(Value::as_bool).unwrap_or(false),
        formats: strings(b, "formats"),
        ids: [("ISBN", "isbn"), ("DOI", "doi"), ("ASIN", "asin")]
            .into_iter()
            .filter_map(|(label, key)| opt_text(b, key).map(|v| (label, v)))
            .collect(),
        removed_at: opt_text(b, "removedFromKindleAt"),
        mentioned_only: b.get("mentionedOnly").and_then(Value::as_bool).unwrap_or(false),
        tags: strings(b, "tags"),
        briefs: array(b, "briefs").iter().map(|x| (text(x, "text"), text(x, "generatedAt"))).collect(),
        related: array(b, "relatedNotes")
            .iter()
            .map(|x| (text(x, "title"), format!("{}: {}", text(x, "vault"), text(x, "path"))))
            .collect(),
        highlights,
    }
}

pub fn parse_detail(v: &Value) -> Detail {
    let highlights = array(v, "highlights").iter().map(parse_highlight).collect();
    parse_book(v.get("book").unwrap_or(v), highlights)
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Hit {
    pub book_id: String,
    pub highlight: bool,
    pub snippet: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Activity {
    pub last_import: Option<String>,
    pub by_status: Vec<(String, u64)>,
    pub week: usize,
    pub month: usize,
    pub capped: bool,
    pub no_sessions: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Filters {
    pub q: String,
    pub status: Option<&'static str>,
    pub on_kindle: bool,
}

pub const LIST_LIMIT: usize = 500;

pub fn list_path(f: &Filters) -> String {
    let mut p = format!("/v1/library/books?limit={LIST_LIMIT}");
    if !f.q.is_empty() {
        p += &format!("&q={}", encode(&f.q));
    }
    if let Some(s) = f.status {
        p += &format!("&status={s}");
    }
    if f.on_kindle {
        p += "&onKindle=true";
    }
    p
}

pub fn book_path(id: &str) -> String {
    format!("/v1/library/books/{}", encode(id))
}

pub enum Job {
    List(u64, Filters),
    Detail(String),
    Patch { id: String, patch: Value, base: String },
    Brief(String),
    Activity,
}

pub enum Msg {
    Changed,
    SignedIn,
    SignedOut,
    Listed { seq: u64, books: Vec<BookRow>, hits: Vec<Hit>, capped: bool },
    ListFailed { seq: u64, message: String },
    Detail(String, Box<Detail>),
    DetailFailed(String, String),
    /// The book as the server now has it. `conflict`: it had moved on, and this is the reload.
    Saved { id: String, detail: Box<Detail>, conflict: bool },
    SaveFailed(String),
    Activity(Box<Activity>),
    ActivityFailed(String),
}

const LOAD_FAILED: &str = "Could not load or save the library. Please try again.";

fn say(e: &ServerError) -> String {
    match e {
        ServerError::Offline(m) => format!("{m}. Please try again."),
        _ => LOAD_FAILED.to_owned(),
    }
}

pub struct Worker {
    server: Box<dyn Server>,
}

impl Worker {
    pub fn new(server: Box<dyn Server>) -> Worker {
        Worker { server }
    }

    pub fn handle(&mut self, job: Job) -> Vec<Msg> {
        let mut out = Vec::new();
        let r = match job {
            Job::List(seq, f) => self.list(seq, &f, &mut out),
            Job::Detail(id) => self.detail(&id, &mut out),
            Job::Patch { id, patch, base } => self.patch(&id, patch, &base, &mut out),
            Job::Brief(id) => self.brief(&id, &mut out),
            Job::Activity => self.activity(&mut out),
        };
        if let Err(ServerError::SignedOut) = r {
            out.push(Msg::SignedOut);
        }
        out
    }

    fn list(&mut self, seq: u64, f: &Filters, out: &mut Vec<Msg>) -> Result<(), ServerError> {
        let got = self.server.get(&list_path(f)).and_then(|books| {
            let hits = if f.q.is_empty() {
                Value::Null
            } else {
                self.server.get(&format!("/v1/library/search?q={}&limit={LIST_LIMIT}", encode(&f.q)))?
            };
            Ok((books, hits))
        });
        match got {
            Ok((books, hits)) => {
                let all = array(&books, "books");
                let capped = all.len() >= LIST_LIMIT;
                // Dictionaries are the Kindle's own, not reading.
                let books = all.iter().map(parse_row).filter(|b| b.kind != "dictionary").collect();
                let hits = array(&hits, "results")
                    .iter()
                    .map(|h| Hit {
                        book_id: text(h, "bookId"),
                        highlight: opt_text(h, "highlightId").is_some(),
                        snippet: text(h, "snippet"),
                    })
                    .collect();
                out.push(Msg::Listed { seq, books, hits, capped });
                Ok(())
            }
            Err(ServerError::SignedOut) => Err(ServerError::SignedOut),
            Err(e) => {
                out.push(Msg::ListFailed { seq, message: say(&e) });
                Ok(())
            }
        }
    }

    fn detail(&mut self, id: &str, out: &mut Vec<Msg>) -> Result<(), ServerError> {
        match self.server.get(&book_path(id)) {
            Ok(v) => out.push(Msg::Detail(id.to_owned(), Box::new(parse_detail(&v)))),
            Err(ServerError::SignedOut) => return Err(ServerError::SignedOut),
            Err(e) => out.push(Msg::DetailFailed(id.to_owned(), say(&e))),
        }
        Ok(())
    }

    /// `save`: on 409 the latest version is fetched and shown, and the message says so.
    fn patch(&mut self, id: &str, mut patch: Value, base: &str, out: &mut Vec<Msg>) -> Result<(), ServerError> {
        patch["baseUpdatedAt"] = json!(base);
        match self.server.send("PATCH", &book_path(id), &patch) {
            Ok(_) => self.saved(id, false, out),
            Err(ServerError::Conflict) => self.saved(id, true, out),
            Err(ServerError::SignedOut) => Err(ServerError::SignedOut),
            Err(e) => {
                out.push(Msg::SaveFailed(say(&e)));
                Ok(())
            }
        }
    }

    fn saved(&mut self, id: &str, conflict: bool, out: &mut Vec<Msg>) -> Result<(), ServerError> {
        match self.server.get(&book_path(id)) {
            Ok(v) => out.push(Msg::Saved { id: id.to_owned(), detail: Box::new(parse_detail(&v)), conflict }),
            Err(ServerError::SignedOut) => return Err(ServerError::SignedOut),
            Err(e) => out.push(Msg::SaveFailed(say(&e))),
        }
        Ok(())
    }

    fn brief(&mut self, id: &str, out: &mut Vec<Msg>) -> Result<(), ServerError> {
        match self.server.send("POST", &format!("{}/brief", book_path(id)), &json!({})) {
            Ok(_) => self.saved(id, false, out),
            Err(ServerError::SignedOut) => Err(ServerError::SignedOut),
            Err(e) => {
                out.push(Msg::SaveFailed(say(&e)));
                Ok(())
            }
        }
    }

    fn activity(&mut self, out: &mut Vec<Msg>) -> Result<(), ServerError> {
        let got = (|| {
            Ok::<_, ServerError>((
                self.server.get("/v1/library/stats")?,
                self.server.get("/v1/library/sync")?,
                self.server.get("/v1/library/recent?days=7&limit=500")?,
                self.server.get("/v1/library/recent?days=30&limit=500")?,
            ))
        })();
        match got {
            Ok((stats, sync, week, month)) => {
                let by = stats.get("byStatus");
                let count = |s: &str| by.and_then(|b| b.get(s)).and_then(Value::as_u64).unwrap_or(0);
                let (w, m) = (array(&week, "highlights").len(), array(&month, "highlights").len());
                let unavailable = |v: &Value| text(v, "readingSessions") == "unavailable_on_stock_kindle";
                out.push(Msg::Activity(Box::new(Activity {
                    last_import: array(&sync, "imports").last().map(|i| text(i, "time")),
                    by_status: crate::app::STATUSES.iter().map(|s| ((*s).to_owned(), count(s))).collect(),
                    week: w,
                    month: m,
                    capped: w >= 500 || m >= 500,
                    no_sessions: unavailable(&week) || unavailable(&month),
                })));
                Ok(())
            }
            Err(ServerError::SignedOut) => Err(ServerError::SignedOut),
            Err(e) => {
                out.push(Msg::ActivityFailed(say(&e)));
                Ok(())
            }
        }
    }
}
