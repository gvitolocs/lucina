//! A stand-in for the server's note routes, in this process: `crates/secretary-server/src/notes` as `docs/keep.md` and
//! `docs/server-api.md` describe it, for the tests and for showing the app without an account (`LUCINA_APP_DEMO=1`). No real
//! note ever passes through here. `tests/against_server.rs` runs the same scenario against the real `secretary-server`.

use std::sync::{Arc, Mutex};

use lucina_app_support::dates::{now_ms, to_iso};
use lucina_app_support::net::{Method, Response, Transport};
use serde_json::{Value, json};

use crate::model::{Input, Item, Note, clean_label, items_to_content, new_item_id, norm_label};

#[derive(Default)]
pub struct State {
    pub notes: Vec<Note>,
    pub rev: u64,
    /// `{tag, name}`.
    pub labels: Vec<(String, String)>,
    /// Ids deleted forever, with their revision.
    pub tombstones: Vec<(String, u64)>,
    /// The next this many requests get no answer, and whether each arrived anyway first.
    pub drop_next: u32,
    pub drop_after_arriving: bool,
    pub seq: u64,
    pub requests: Vec<String>,
    /// `GET /changes` answers 410 for a cursor at or above this.
    pub expire_cursor: bool,
    pub page: usize,
    /// Answer 401 to everything.
    pub unauthorized: bool,
}

/// Cheap to clone; every clone is the same server. A test keeps one, and edits through it like another client would.
#[derive(Clone)]
pub struct Fake(pub Arc<Mutex<State>>);

fn err(status: u16, code: &str, message: &str) -> Response {
    Response {
        status,
        body: json!({"error": {"code": code, "message": message}}),
        etag: None,
    }
}

fn ok(status: u16, body: Value) -> Response {
    Response {
        status,
        body,
        etag: None,
    }
}

impl Fake {
    pub fn new() -> Fake {
        Fake(Arc::new(Mutex::new(State {
            page: 200,
            ..State::default()
        })))
    }

    /// `n` invented small notes, as a long-used store would hold.
    pub fn fill(&self, n: u32) {
        let mut s = self.0.lock().expect("fake");
        let words = [
            "apples", "budget", "ferry", "garden", "invoice", "kettle", "lantern", "meeting",
            "notebook", "orchard",
        ];
        for i in 0..n {
            let w = words[i as usize % words.len()];
            s.insert(Note {
                title: format!("{w} {i}"),
                content: format!("Something ordinary about the {w}, number {i}, to remember later.\n\n- first\n- second"),
                ..Note::default()
            });
        }
    }

    /// What the demo shows: invented notes, in every shape the screens must handle.
    pub fn demo() -> Fake {
        let f = Fake::new();
        {
            let mut s = f.0.lock().expect("fake");
            let mut add = |title: &str, content: &str, f: &dyn Fn(&mut Note)| {
                let mut n = Note {
                    title: title.into(),
                    content: content.into(),
                    ..Note::default()
                };
                f(&mut n);
                s.insert(n);
            };
            add(
                "Welcome to Notes",
                "Keep and Notes share one store, so this is one app.\n\nPress **Ctrl+N** for a note, **Ctrl+L** for a list, and start typing to search.",
                &|n| {
                    n.pinned = true;
                    n.color = "teal".into();
                    n.source = "lucina".into();
                },
            );
            add("Pasta night", "", &|n| {
                n.pinned = true;
                n.color = "yellow".into();
                n.tags = vec!["cooking".into(), "home".into()];
                n.items = Some(
                    [
                        ("Spaghetti", true),
                        ("San Marzano tomatoes", false),
                        ("Basil", false),
                        ("Parmigiano", false),
                        ("Red wine", false),
                    ]
                    .iter()
                    .map(|(t, c)| Item {
                        id: new_item_id(),
                        text: (*t).into(),
                        checked: *c,
                        indent: 0,
                    })
                    .collect(),
                );
                n.content = items_to_content(n.items.as_ref().expect("items"));
            });
            add(
                "Meeting briefing",
                "# Meeting briefing\n\n## Agenda\n\n1. Where the project stands\n2. What is **blocked**\n3. Next steps\n\n## Notes\n\n- Bring the *printed* plan\n- Ask about the budget\n\n```\nroom 2.14, 10:00\n```\n",
                &|n| {
                    n.source = "chatgpt".into();
                    n.tags = vec!["work".into()];
                },
            );
            add(
                "Rust ideas",
                "A note about memory: keep the search text folded once, not on every key.\n\nhttps://www.rust-lang.org/learn",
                &|n| {
                    n.color = "blue".into();
                    n.tags = vec!["work".into(), "ideas".into()];
                },
            );
            add("Dentist", "Call to move the appointment.", &|n| {
                n.color = "red".into();
                n.reminder = Some(crate::model::Reminder {
                    at: to_iso(now_ms() + 26 * 3_600_000),
                    recurrence: None,
                });
            });
            add(
                "Book list",
                "- Dune\n- Il nome della rosa\n- Солярис\n- Πολιτεία",
                &|n| {
                    n.color = "purple".into();
                    n.tags = vec!["ideas".into()];
                    n.source = "grok".into();
                },
            );
            add("", "A quick thought, with no title at all.", &|_| {});
            add(
                "Wi-Fi at the cabin",
                "Network: cabin-guest. Ask the neighbours for the code.",
                &|n| n.color = "green".into(),
            );
            add("Trip packing", "", &|n| {
                n.color = "orange".into();
                n.tags = vec!["home".into()];
                n.items = Some(
                    ["Passport", "Chargers", "Rain jacket", "Boots"]
                        .iter()
                        .map(|t| Item {
                            id: new_item_id(),
                            text: (*t).into(),
                            checked: false,
                            indent: 0,
                        })
                        .collect(),
                );
                n.content = items_to_content(n.items.as_ref().expect("items"));
            });
            add("Old plans", "Kept for the record.", &|n| n.archived = true);
            add("Scratch", "To throw away.", &|n| {
                n.deleted_at = Some(to_iso(now_ms() - 86_400_000))
            });
            s.labels = [
                ("cooking", "Cooking"),
                ("home", "Home"),
                ("ideas", "Ideas"),
                ("work", "Work"),
            ]
            .iter()
            .map(|(t, n)| ((*t).into(), (*n).into()))
            .collect();
        }
        f
    }
}

impl State {
    fn next_rev(&mut self) -> u64 {
        self.rev += 1;
        self.rev
    }

    fn slug_for(&self, n: &Note) -> String {
        let text = if !n.title.is_empty() {
            n.title.clone()
        } else {
            n.content
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("note")
                .split_whitespace()
                .take(6)
                .collect::<Vec<_>>()
                .join(" ")
        };
        let mut base: String = text
            .to_lowercase()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        while base.contains("--") {
            base = base.replace("--", "-");
        }
        let base = base.trim_matches('-').chars().take(64).collect::<String>();
        let base = if base.is_empty() {
            "note".to_owned()
        } else {
            base
        };
        let taken = |s: &str| {
            self.notes
                .iter()
                .any(|x| x.deleted_at.is_none() && x.slug == s)
        };
        if !taken(&base) {
            return base;
        }
        (2..)
            .map(|i| format!("{base}-{i}"))
            .find(|s| !taken(s))
            .unwrap_or(base)
    }

    pub fn insert(&mut self, mut n: Note) -> Note {
        self.seq += 1;
        let ms = 1_793_000_000_000 + self.seq as i64 * 60_000;
        n.id = format!("n_{:020x}", self.seq);
        n.created_at = to_iso(ms);
        n.updated_at = to_iso(ms);
        if n.color.is_empty() {
            n.color = "default".into();
        }
        if n.source.is_empty() {
            n.source = "owner".into();
        }
        n.position = ms as f64;
        n.slug = self.slug_for(&n);
        n.rev = self.next_rev();
        self.notes.push(n.clone());
        n
    }

    fn find(&mut self, key: &str) -> Option<&mut Note> {
        self.notes
            .iter_mut()
            .find(|n| n.deleted_at.is_none() && (n.id == key || n.slug == key))
    }

    fn register(&mut self, names: &[String]) {
        for raw in names {
            let name = clean_label(raw);
            let tag = norm_label(&name);
            if !tag.is_empty() && !self.labels.iter().any(|(t, _)| *t == tag) {
                self.labels.push((tag, name));
            }
        }
    }

    fn apply(&mut self, id: &str, input: &Input, base: Option<&str>) -> Result<Note, Response> {
        if input
            .content
            .as_ref()
            .is_some_and(|c| c.len() > crate::model::NOTE_MAX_BYTES)
        {
            return Err(err(413, "note_too_large", "The note is too large."));
        }
        if let Some(l) = &input.labels {
            self.register(l);
        }
        let rev = self.next_rev();
        let now = to_iso(now_ms());
        let Some(n) = self.find(id) else {
            return Err(err(404, "note_not_found", "No such note."));
        };
        if let Some(b) = base
            && b != n.updated_at
        {
            return Err(err(
                409,
                "conflict",
                "This note changed since you opened it.",
            ));
        }
        let was_list = n.items.is_some();
        let mut changed = crate::model::apply_input(n, input, &now);
        // `content` on a list note is parsed back into items.
        if was_list && input.content.is_some() && input.items.is_none() {
            let items = crate::model::content_to_items(&changed.content);
            changed.content = items_to_content(&items);
            changed.items = Some(items);
        }
        changed.rev = rev;
        *n = changed.clone();
        Ok(changed)
    }

    fn create(&mut self, body: &Value) -> Result<Note, Response> {
        let input: Input = serde_json::from_value(body.clone())
            .map_err(|_| err(400, "bad_request", "Bad note."))?;
        if let Some(l) = &input.labels {
            self.register(l);
        }
        let title = input.title.clone().unwrap_or_default();
        let content = input.content.clone().unwrap_or_default();
        if title.trim().is_empty()
            && content.trim().is_empty()
            && input.items.as_ref().is_none_or(Vec::is_empty)
        {
            return Err(err(400, "bad_request", "title: must not be empty"));
        }
        let base = Note {
            color: "default".into(),
            source: "owner".into(),
            ..Note::default()
        };
        let n = crate::model::apply_input(&base, &input, "");
        Ok(self.insert(n))
    }

    fn changes(&mut self, since: &str, limit: usize) -> Response {
        let Ok(since) = since.parse::<u64>() else {
            return err(400, "bad_request", "since");
        };
        if since > self.rev || self.expire_cursor {
            self.expire_cursor = false;
            return err(410, "cursor_expired", "Load again from the start.");
        }
        let mut stream: Vec<(u64, Value, bool)> = self
            .notes
            .iter()
            .filter(|n| n.rev > since)
            .map(|n| (n.rev, json!(n), false))
            .chain(
                self.tombstones
                    .iter()
                    .filter(|(_, r)| *r > since)
                    .map(|(id, r)| (*r, json!(id), true)),
            )
            .collect();
        stream.sort_by_key(|x| x.0);
        let has_more = stream.len() > limit;
        stream.truncate(limit);
        let cursor = stream.last().map_or(self.rev, |x| x.0);
        let (mut notes, mut removed) = (Vec::new(), Vec::new());
        for (_, v, gone) in stream {
            if gone { removed.push(v) } else { notes.push(v) }
        }
        ok(
            200,
            json!({"notes": notes, "removed": removed, "cursor": cursor.to_string(), "hasMore": has_more}),
        )
    }

    fn labels_json(&self) -> Value {
        let mut out: Vec<Value> = self
            .labels
            .iter()
            .map(|(tag, name)| {
                let count = self
                    .notes
                    .iter()
                    .filter(|n| n.deleted_at.is_none() && n.tags.contains(tag))
                    .count();
                json!({"tag": tag, "name": name, "count": count})
            })
            .collect();
        out.sort_by_key(|l| l["name"].as_str().unwrap_or("").to_lowercase());
        json!({ "labels": out })
    }

    fn forget(&mut self, ids: &[String]) -> Vec<String> {
        let mut gone = Vec::new();
        for id in ids {
            if self
                .notes
                .iter()
                .any(|n| n.id == *id && n.deleted_at.is_some())
            {
                self.notes.retain(|n| n.id != *id);
                let rev = self.next_rev();
                self.tombstones.push((id.clone(), rev));
                gone.push(id.clone());
            }
        }
        gone
    }

    fn batch(&mut self, b: &Value) -> Response {
        let ids: Vec<String> = b["ids"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect();
        let flag = |k: &str| b[k].as_bool() == Some(true);
        if flag("deleteForever") {
            let gone = self.forget(&ids);
            return ok(200, json!({"notes": [], "removed": gone}));
        }
        let now = to_iso(now_ms());
        let mut changed = Vec::new();
        let labels = |k: &str| -> Vec<String> {
            b[k].as_array()
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        };
        let (add, remove) = (labels("addLabels"), labels("removeLabels"));
        self.register(&add);
        let set: Input = serde_json::from_value(b["set"].clone()).unwrap_or_default();
        for id in &ids {
            let rev = self.rev + 1;
            let Some(n) = self.notes.iter_mut().find(|n| n.id == *id) else {
                continue;
            };
            let before = n.clone();
            if flag("trash") && n.deleted_at.is_none() {
                n.deleted_at = Some(now.clone());
            } else if flag("restore") && n.deleted_at.is_some() {
                n.deleted_at = None;
            } else if n.deleted_at.is_none() {
                *n = crate::model::apply_input(n, &set, &n.updated_at.clone());
                for a in &add {
                    let t = norm_label(a);
                    if !n.tags.contains(&t) {
                        n.tags.push(t);
                    }
                }
                for r in &remove {
                    let t = norm_label(r);
                    n.tags.retain(|x| *x != t);
                }
            }
            if *n != before {
                n.updated_at = now.clone();
                n.rev = rev;
                self.rev = rev;
                changed.push(n.clone());
            }
        }
        ok(200, json!({ "notes": changed }))
    }

    fn route(&mut self, method: Method, path: &str, body: Option<&Value>) -> Response {
        let (path, query) = path.split_once('?').unwrap_or((path, ""));
        let param = |k: &str| {
            query
                .split('&')
                .find_map(|p| p.strip_prefix(k).and_then(|r| r.strip_prefix('=')))
                .unwrap_or("")
                .to_owned()
        };
        let empty = Value::Null;
        let b = body.unwrap_or(&empty);
        let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
        match (method, parts.as_slice()) {
            (Method::Get, ["v1", "notes", "changes"]) => {
                let limit = param("limit").parse().unwrap_or(self.page).min(self.page);
                let since = param("since");
                self.changes(if since.is_empty() { "0" } else { &since }, limit)
            }
            (Method::Get, ["v1", "notes", "labels"]) => ok(200, self.labels_json()),
            (Method::Post, ["v1", "notes", "labels"]) => {
                let name = clean_label(b["name"].as_str().unwrap_or(""));
                let tag = norm_label(&name);
                if self.labels.iter().any(|(t, _)| *t == tag) {
                    return err(409, "label_exists", "That label exists.");
                }
                self.labels.push((tag.clone(), name.clone()));
                ok(201, json!({"tag": tag, "name": name, "count": 0}))
            }
            (Method::Patch, ["v1", "notes", "labels", tag]) => {
                let name = clean_label(b["name"].as_str().unwrap_or(""));
                let new = norm_label(&name);
                if !self.labels.iter().any(|(t, _)| t == tag) {
                    return err(404, "label_not_found", "No such label.");
                }
                if new != *tag {
                    let rev = self.next_rev();
                    for n in &mut self.notes {
                        if n.tags.iter().any(|t| t == tag) {
                            for t in &mut n.tags {
                                if t == tag {
                                    *t = new.clone();
                                }
                            }
                            n.rev = rev;
                        }
                    }
                    self.labels.retain(|(t, _)| t != tag);
                }
                if !self.labels.iter().any(|(t, _)| *t == new) {
                    self.labels.push((new.clone(), name.clone()));
                } else if new == *tag {
                    for l in &mut self.labels {
                        if l.0 == new {
                            l.1 = name.clone();
                        }
                    }
                }
                ok(200, json!({"tag": new, "name": name, "count": 0}))
            }
            (Method::Delete, ["v1", "notes", "labels", tag]) => {
                let rev = self.next_rev();
                let mut removed = 0;
                for n in &mut self.notes {
                    if n.tags.iter().any(|t| t == tag) {
                        n.tags.retain(|t| t != tag);
                        n.rev = rev;
                        removed += 1;
                    }
                }
                self.labels.retain(|(t, _)| t != tag);
                ok(200, json!({"ok": true, "removed": removed}))
            }
            (Method::Post, ["v1", "notes", "batch"]) => self.batch(b),
            (Method::Delete, ["v1", "notes", "trash"]) => {
                let ids: Vec<String> = self
                    .notes
                    .iter()
                    .filter(|n| n.deleted_at.is_some())
                    .map(|n| n.id.clone())
                    .collect();
                let gone = self.forget(&ids);
                ok(200, json!({"ok": true, "removed": gone.len()}))
            }
            (Method::Post, ["v1", "notes"]) => match self.create(b) {
                Ok(n) => ok(201, json!(n)),
                Err(e) => e,
            },
            (Method::Patch, ["v1", "notes", key]) => {
                let Ok(input) = serde_json::from_value::<Input>(b.clone()) else {
                    return err(400, "bad_request", "Bad note.");
                };
                let base = b["baseUpdatedAt"].as_str().map(str::to_owned);
                match self.apply(key, &input, base.as_deref()) {
                    Ok(n) => ok(200, json!(n)),
                    Err(e) => e,
                }
            }
            (Method::Get, ["v1", "notes", key]) => match self.find(key) {
                Some(n) => ok(200, json!(n)),
                None => err(404, "note_not_found", "No such note."),
            },
            _ => err(404, "not_found", "No such route."),
        }
    }
}

impl Transport for Fake {
    fn call(
        &mut self,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Response, String> {
        let mut s = self.0.lock().map_err(|_| "fake poisoned".to_owned())?;
        s.requests.push(format!("{} {path}", method.as_str()));
        if s.unauthorized {
            return Ok(err(401, "unauthorized", "Signed out."));
        }
        if s.drop_next > 0 {
            s.drop_next -= 1;
            if s.drop_after_arriving {
                s.route(method, path, body);
            }
            return Err("connection timed out".into());
        }
        Ok(s.route(method, path, body))
    }
}
