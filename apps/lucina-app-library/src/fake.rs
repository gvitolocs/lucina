//! The routes of secretary-server's library.rs, in this process: sample mode and the tests. `tests.rs` checks what this
//! serves against `ink-library`'s own types.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use crate::server::{Server, ServerError};

#[derive(Default)]
pub struct Data {
    pub books: Vec<Value>,
    pub highlights: Vec<(String, Value)>,
    pub clock: u64,
    /// Every request, to count them.
    pub requests: Vec<String>,
}

pub struct Fake {
    pub data: Arc<Mutex<Data>>,
    pub offline: Arc<AtomicBool>,
    pub dead: Arc<AtomicBool>,
    pub writes: Arc<AtomicU64>,
}

fn stamp(n: u64) -> String {
    format!("2026-10-{:02}T08:{:02}:00Z", 1 + n / 60 % 28, n % 60)
}

/// One book as `GET /v1/library/books` and `/books/{id}` send it.
pub fn book(id: &str, title: &str, author: &str, status: &str, percent: Option<f64>, on_kindle: bool) -> Value {
    json!({
        "id": id, "title": title, "authors": [author], "kind": "book", "status": status,
        "progress": percent.map(|p| json!({"percent": p, "location": null, "page": null, "source": "kindle", "at": stamp(0)})),
        "onKindle": on_kindle.then(|| json!({"path": format!("documents/{id}.mobi"), "sizeBytes": 1000, "lastSeenAt": stamp(0)})),
        "owned": true, "formats": ["mobi"], "drm": "none", "updatedAt": stamp(0),
        "isbn": null, "doi": null, "asin": null, "removedFromKindleAt": null, "mentionedOnly": false,
        "tags": [], "briefs": [], "relatedNotes": [], "highlightCount": 0, "lastHighlightAt": null,
    })
}

pub fn highlight(id: &str, text: &str, start: u64) -> Value {
    json!({
        "id": id, "kind": "highlight", "text": text, "note": null,
        "location": {"start": start, "end": start + 4}, "page": null,
        "addedAt": null, "addedAtRaw": "Monday, 5 October 2026 21:14:02", "source": "kindle",
    })
}

impl Fake {
    pub fn new(data: Data) -> Fake {
        Fake {
            data: Arc::new(Mutex::new(data)),
            offline: Arc::new(AtomicBool::new(false)),
            dead: Arc::new(AtomicBool::new(false)),
            writes: Arc::new(AtomicU64::new(0)),
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
}

fn query(path: &str, key: &str) -> Option<String> {
    let q = path.split_once('?')?.1;
    q.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == key).then(|| decode(v))
    })
}

fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() + 0 {
            if let Ok(n) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(n);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn book_id(path: &str) -> String {
    let tail = path.split('?').next().unwrap_or("").trim_start_matches("/v1/library/books/");
    decode(tail.split('/').next().unwrap_or(""))
}

impl Server for Fake {
    fn get(&mut self, path: &str) -> Result<Value, ServerError> {
        self.up()?;
        let mut d = self.data.lock().map_err(|_| ServerError::Offline("poisoned".into()))?;
        d.requests.push(format!("GET {path}"));
        let route = path.split('?').next().unwrap_or("");
        match route {
            "/v1/library/books" => {
                let q = query(path, "q").unwrap_or_default().to_lowercase();
                let status = query(path, "status");
                let kindle = query(path, "onKindle").is_some();
                let books: Vec<Value> = d
                    .books
                    .iter()
                    .filter(|b| {
                        let id = b["id"].as_str().unwrap_or("");
                        let hay = format!("{} {}", b["title"], b["authors"]).to_lowercase();
                        let in_highlights = d.highlights.iter().any(|(bid, h)| {
                            bid == id && h["text"].as_str().unwrap_or("").to_lowercase().contains(&q)
                        });
                        (q.is_empty() || hay.contains(&q) || in_highlights)
                            && status.as_ref().is_none_or(|s| b["status"] == json!(s))
                            && (!kindle || b["onKindle"].is_object())
                    })
                    .map(|b| {
                        let mut b = b.clone();
                        let id = b["id"].as_str().unwrap_or("").to_owned();
                        b["highlightCount"] = json!(d.highlights.iter().filter(|(bid, _)| *bid == id).count());
                        b
                    })
                    .collect();
                Ok(json!({"books": books}))
            }
            "/v1/library/search" => {
                let q = query(path, "q").unwrap_or_default().to_lowercase();
                let results: Vec<Value> = d
                    .highlights
                    .iter()
                    .filter(|(_, h)| h["text"].as_str().unwrap_or("").to_lowercase().contains(&q))
                    .map(|(bid, h)| json!({"bookId": bid, "highlightId": h["id"], "snippet": h["text"]}))
                    .collect();
                Ok(json!({"results": results}))
            }
            "/v1/library/stats" => {
                let mut by = serde_json::Map::new();
                for b in &d.books {
                    let s = b["status"].as_str().unwrap_or("unknown").to_owned();
                    let n = by.get(&s).and_then(Value::as_u64).unwrap_or(0);
                    by.insert(s, json!(n + 1));
                }
                Ok(json!({"byStatus": by}))
            }
            "/v1/library/sync" => Ok(json!({"imports": [{"time": stamp(0)}]})),
            "/v1/library/recent" => {
                let hs: Vec<&Value> = d.highlights.iter().map(|(_, h)| h).collect();
                Ok(json!({"highlights": hs, "readingSessions": "unavailable_on_stock_kindle"}))
            }
            _ => {
                let id = book_id(path);
                let book = d.books.iter().find(|b| b["id"] == json!(id)).cloned();
                let Some(book) = book else { return Err(ServerError::Refused("not found".into())) };
                let hs: Vec<&Value> = d.highlights.iter().filter(|(b, _)| *b == id).map(|(_, h)| h).collect();
                Ok(json!({"book": book, "highlights": hs, "sessions": []}))
            }
        }
    }

    fn send(&mut self, method: &str, path: &str, body: &Value) -> Result<Value, ServerError> {
        self.up()?;
        self.writes.fetch_add(1, Ordering::SeqCst);
        let mut d = self.data.lock().map_err(|_| ServerError::Offline("poisoned".into()))?;
        d.requests.push(format!("{method} {path}"));
        d.clock += 1;
        let clock = d.clock;
        let id = book_id(path);
        let highlight_count = d.highlights.iter().filter(|(b, _)| *b == id).count();
        let Some(b) = d.books.iter_mut().find(|b| b["id"] == json!(id)) else {
            return Err(ServerError::Refused("not found".into()));
        };
        if path.ends_with("/brief") {
            if highlight_count == 0 {
                return Err(ServerError::Refused("no highlights".into()));
            }
            let brief = json!({"id": format!("b{clock}"), "kind": "extractive",
                "text": format!("{highlight_count} passages stand out in this book."),
                "generatedAt": stamp(clock), "model": null, "basedOn": []});
            b["briefs"] = json!([brief]);
            b["updatedAt"] = json!(stamp(clock));
            return Ok(brief);
        }
        if body.get("baseUpdatedAt").is_some_and(|base| *base != b["updatedAt"]) {
            return Err(ServerError::Conflict);
        }
        for key in ["status", "tags"] {
            if let Some(v) = body.get(key) {
                b[key] = v.clone();
            }
        }
        b["updatedAt"] = json!(stamp(clock));
        Ok(b.clone())
    }
}

/// Made-up books and passages, for sample mode.
pub fn sample() -> Data {
    let mut d = Data { clock: 100, ..Data::default() };
    d.books = vec![
        book("b1", "The Long Way Round", "A. Marsh", "reading", Some(42.5), true),
        book("b2", "Notes on Quiet Machines", "L. Okafor", "completed", Some(100.0), true),
        book("b3", "A Field Guide to Small Systems", "R. Lindqvist", "reading", Some(12.0), false),
        book("b4", "Rivers of the North", "M. Haugen", "not_started", None, true),
        book("b5", "Unfinished Letters", "C. Duarte", "abandoned", Some(8.0), false),
        book("b6", "The Clockmaker's Year", "S. Ito", "unknown", None, false),
    ];
    d.highlights = vec![
        ("b1".into(), highlight("h1", "Every road is only the shortest way to somewhere you did not mean to go.", 120)),
        ("b1".into(), highlight("h2", "He kept the map folded, as if not looking at it could make it true.", 480)),
        ("b2".into(), highlight("h3", "A quiet machine is one that has already answered before you finish asking.", 77)),
        ("b2".into(), highlight("h4", "Nothing wears out faster than a thing that waits with its motor running.", 410)),
        ("b2".into(), highlight("h5", "Measure it, then leave it alone.", 902)),
        ("b3".into(), highlight("h6", "Small systems fail in ways you can hold in one hand.", 33)),
    ];
    d
}
