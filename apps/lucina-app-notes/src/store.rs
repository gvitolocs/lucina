//! The note cache, the sync cursor and the writes: `web/src/state/keep.ts` without promises. The store never waits. It says
//! which request should go next ([`Store::next_job`]) and is told how that went ([`Store::on_reply`]), so everything here
//! runs on the UI thread in microseconds, and the tests can drive it without threads.
//!
//! As on the web: writes apply locally first and go to the server in order, the server's answer replaces the local copy,
//! and a note with a write in flight ignores incoming sync data until its answer arrives.
//!
//! One difference, recorded in `docs/research/juno-apps-notes.md`: when no answer comes, the web reverts the note and says
//! "your change was not saved". This laptop is often offline, so here the write stays queued (in a file, too) and is sent
//! when the network is back, the way Shopping's outbox works. A refusal from the server still reverts, as on the web.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use lucina_app_support::dates::{now_ms, to_iso};
use lucina_app_support::net::{self, ApiError, Job, Method, Reply, enc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::model::{
    Entry, Input, Label, Note, apply_input, clean_label, new_item_id, norm_label, parse_note,
};

/// One queued write.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Op {
    Create {
        tmp: String,
        input: Input,
    },
    /// `guard`: send `baseUpdatedAt`, so that a note changed elsewhere answers 409 instead of being overwritten.
    Patch {
        id: String,
        input: Input,
        guard: bool,
    },
    Set {
        ids: Vec<String>,
        set: Input,
    },
    AddLabel {
        ids: Vec<String>,
        name: String,
    },
    RemoveLabel {
        ids: Vec<String>,
        tag: String,
        name: String,
    },
    Trash {
        ids: Vec<String>,
    },
    Restore {
        ids: Vec<String>,
    },
    DeleteForever {
        ids: Vec<String>,
    },
    EmptyTrash {
        ids: Vec<String>,
    },
    CreateLabel {
        name: String,
    },
    RenameLabel {
        tag: String,
        name: String,
    },
    DeleteLabel {
        tag: String,
    },
}

impl Op {
    fn ids(&self) -> Vec<String> {
        match self {
            Op::Create { tmp, .. } => vec![tmp.clone()],
            Op::Patch { id, .. } => vec![id.clone()],
            Op::Set { ids, .. }
            | Op::AddLabel { ids, .. }
            | Op::RemoveLabel { ids, .. }
            | Op::Trash { ids }
            | Op::Restore { ids }
            | Op::DeleteForever { ids }
            | Op::EmptyTrash { ids } => ids.clone(),
            Op::CreateLabel { .. } | Op::RenameLabel { .. } | Op::DeleteLabel { .. } => Vec::new(),
        }
    }
}

/// What to offer after an action, `web/src/state/keepActions.ts`.
#[derive(Clone, Debug, PartialEq)]
pub enum Undo {
    Unarchive {
        ids: Vec<String>,
        repin: Vec<String>,
    },
    Archive {
        ids: Vec<String>,
    },
    Restore {
        ids: Vec<String>,
    },
    Trash {
        ids: Vec<String>,
    },
    SetPinned {
        ids: Vec<String>,
        pinned: bool,
    },
}

/// A snackbar message: one at a time, as on Keep.
#[derive(Clone, Debug, PartialEq)]
pub struct Notice {
    pub text: String,
    pub undo: Option<Undo>,
    pub until: Instant,
}

const NOTICE_FOR: Duration = Duration::from_millis(6500);
const RETRY_MIN: Duration = Duration::from_secs(2);
const RETRY_MAX: Duration = Duration::from_secs(30);
/// Sync on a clock while the app is open (`startAutoSync`).
pub const SYNC_EVERY: Duration = Duration::from_secs(60);

#[derive(Debug)]
enum Flight {
    Write,
    Changes,
    Labels,
}

/// What survives leaving the app, in two files: the outbox is small and written at once, the cache is large and written on
/// a thread.
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CacheFile {
    pub cursor: String,
    pub labels: Vec<Label>,
    pub notes: Vec<Note>,
}

pub struct Store {
    notes: HashMap<String, Arc<Entry>>,
    registry: Vec<Label>,
    cursor: String,
    outbox: VecDeque<Op>,
    flight: Option<(u64, Flight)>,
    next_job: u64,
    pending: HashMap<String, u32>,
    /// The last copy the server confirmed, while writes are pending: where a refusal goes back to.
    snapshots: HashMap<String, Option<Arc<Entry>>>,
    alias: HashMap<String, String>,
    sync_wanted: bool,
    sync_changed: bool,
    /// More pages of the same sync are coming.
    sync_paging: bool,
    labels_wanted: bool,
    /// The one retry of a 410: the cursor came from another store.
    reloaded: bool,
    retry_at: Option<Instant>,
    retry: Duration,
    /// The note a guarded write was refused for.
    pub conflict: Option<String>,
    /// The sync is running.
    pub syncing: bool,
    pub sync_error: Option<String>,
    /// A cache or a first answer is in: the page can stop saying "Loading".
    pub loaded: bool,
    /// Off after a request got no answer; on after any answer.
    pub online: bool,
    pub signed_out: bool,
    /// The server no longer takes the token: nothing more is sent until the owner has signed in again.
    pub unauthorized: bool,
    pub notice: Option<Notice>,
    /// Goes up with every change a screen could show.
    pub version: u64,
    /// The cache file is behind.
    pub cache_dirty: bool,
    /// The outbox file is behind.
    pub outbox_dirty: bool,
    tmp_seq: u64,
    /// When no answer came to a create, it may have arrived: [`Store::settle_unclear_create`] looks at what is newer than
    /// this revision.
    unclear_create: bool,
    unclear_since: u64,
}

fn iso_now() -> String {
    to_iso(now_ms())
}

impl Store {
    pub fn new() -> Store {
        Store {
            notes: HashMap::new(),
            registry: Vec::new(),
            cursor: "0".into(),
            outbox: VecDeque::new(),
            flight: None,
            next_job: 1,
            pending: HashMap::new(),
            snapshots: HashMap::new(),
            alias: HashMap::new(),
            sync_wanted: true,
            sync_changed: false,
            sync_paging: false,
            labels_wanted: true,
            reloaded: false,
            retry_at: None,
            retry: RETRY_MIN,
            conflict: None,
            syncing: false,
            sync_error: None,
            loaded: false,
            online: true,
            signed_out: false,
            unauthorized: false,
            notice: None,
            version: 1,
            cache_dirty: false,
            outbox_dirty: false,
            tmp_seq: 0,
            unclear_create: false,
            unclear_since: 0,
        }
    }

    // ------------------------------------------------------------ reading

    pub fn all(&self) -> impl Iterator<Item = &Arc<Entry>> {
        self.notes.values()
    }

    pub fn note(&self, id: &str) -> Option<&Arc<Entry>> {
        self.notes
            .get(self.alias.get(id).map_or(id, String::as_str))
    }

    /// The id a note has now, after its temporary one.
    pub fn resolve<'a>(&'a self, id: &'a str) -> &'a str {
        self.alias.get(id).map_or(id, String::as_str)
    }

    /// `labels()`: the registry plus labels in use, with live counts, alphabetical.
    pub fn labels(&self) -> Vec<Label> {
        let mut counts: HashMap<&str, u32> = HashMap::new();
        for e in self.notes.values() {
            if e.note.deleted_at.is_none() {
                for t in &e.note.tags {
                    *counts.entry(t).or_default() += 1;
                }
            }
        }
        let mut out: Vec<Label> = self
            .registry
            .iter()
            .map(|l| Label {
                tag: l.tag.clone(),
                name: l.name.clone(),
                count: counts.get(l.tag.as_str()).copied().unwrap_or(0),
            })
            .collect();
        for (tag, count) in counts {
            if !out.iter().any(|l| l.tag == tag) {
                out.push(Label {
                    tag: tag.to_owned(),
                    name: tag.to_owned(),
                    count,
                });
            }
        }
        out.sort_by_cached_key(|l| (l.name.to_lowercase(), l.tag.clone()));
        out
    }

    pub fn label_name(&self, tag: &str) -> String {
        self.registry
            .iter()
            .find(|l| l.tag == tag)
            .map_or_else(|| tag.to_owned(), |l| l.name.clone())
    }

    /// Writes not yet answered, for the status line.
    pub fn waiting(&self) -> usize {
        self.outbox.len()
    }

    pub fn idle(&self) -> bool {
        self.flight.is_none() && !self.sync_wanted && !self.labels_wanted && self.outbox.is_empty()
    }

    pub fn in_flight(&self) -> bool {
        self.flight.is_some()
    }

    pub fn outbox(&self) -> impl Iterator<Item = &Op> {
        self.outbox.iter()
    }

    // ------------------------------------------------------------ the cache

    fn insert(&mut self, n: Note) {
        self.notes.insert(n.id.clone(), Entry::new(n));
    }

    fn bump(&mut self) {
        self.version += 1;
        self.cache_dirty = true;
    }

    /// Render the cache at once: what the cache file held, with the writes still queued on top.
    pub fn load_cache(&mut self, cache: CacheFile, outbox: Vec<Op>) {
        for n in cache.notes {
            if !self.notes.contains_key(&n.id) {
                self.insert(n);
            }
        }
        if !cache.cursor.is_empty() {
            self.cursor = cache.cursor;
        }
        self.registry = cache.labels;
        for op in outbox {
            for id in op.ids() {
                self.begin(&id);
            }
            self.apply_local(&op);
            self.outbox.push_back(op);
        }
        self.loaded = true;
        self.version += 1;
    }

    /// Everything the cache file needs, cheap to take: the notes are shared, and serialized on the saver's thread.
    pub fn cache_snapshot(&mut self) -> (String, Vec<Label>, Vec<Arc<Entry>>) {
        self.cache_dirty = false;
        // A note still being written is saved as confirmed: the outbox replays the write on top after a restart.
        let notes = self
            .notes
            .iter()
            .filter(|(id, _)| !id.starts_with("tmp_"))
            .filter_map(|(id, e)| match self.snapshots.get(id) {
                Some(snap) => snap.clone(),
                None => Some(e.clone()),
            })
            .collect();
        (self.cursor.clone(), self.registry.clone(), notes)
    }

    pub fn outbox_json(&mut self) -> Vec<u8> {
        self.outbox_dirty = false;
        serde_json::to_vec(&self.outbox).unwrap_or_default()
    }

    // ------------------------------------------------------------ what to send next

    /// A sync, on the clock, on opening, or after a write.
    pub fn want_sync(&mut self) {
        self.sync_wanted = true;
    }

    /// Signed in again.
    pub fn signed_in(&mut self) {
        self.unauthorized = false;
        self.signed_out = false;
        self.sync_wanted = true;
        self.labels_wanted = true;
        self.retry_now();
    }

    /// The network came back, or the owner asked: stop waiting out the retry delay.
    pub fn retry_now(&mut self) {
        self.retry_at = None;
        self.retry = RETRY_MIN;
    }

    /// When [`Store::next_job`] will next have something, if nothing else happens.
    pub fn retry_at(&self) -> Option<Instant> {
        self.retry_at
    }

    fn job(&mut self, method: Method, path: String, body: Option<Value>, flight: Flight) -> Job {
        let id = self.next_job;
        self.next_job += 1;
        self.flight = Some((id, flight));
        Job {
            id,
            method,
            path,
            body,
        }
    }

    /// The next request, if none is on its way: writes first, in order, then the sync. One at a time, so the answers can
    /// never overtake each other.
    pub fn next_job(&mut self, now: Instant) -> Option<Job> {
        if self.flight.is_some() || self.unauthorized {
            return None;
        }
        if self.retry_at.is_some_and(|t| now < t) {
            return None;
        }
        self.retry_at = None;
        // A create that may have arrived: look before sending it again.
        if self.unclear_create {
            self.sync_wanted = true;
        } else if let Some(op) = self.outbox.front().cloned() {
            let (method, path, body) = self.request(&op);
            return Some(self.job(method, path, body, Flight::Write));
        }
        if self.sync_wanted {
            self.sync_wanted = false;
            if !self.sync_paging {
                self.sync_changed = false;
            }
            self.syncing = true;
            let path = format!("/v1/notes/changes?since={}&limit=200", enc(&self.cursor));
            return Some(self.job(Method::Get, path, None, Flight::Changes));
        }
        if self.labels_wanted {
            self.labels_wanted = false;
            return Some(self.job(Method::Get, "/v1/notes/labels".into(), None, Flight::Labels));
        }
        None
    }

    fn real_ids(&self, ids: &[String]) -> Vec<String> {
        ids.iter().map(|i| self.resolve(i).to_owned()).collect()
    }

    fn request(&self, op: &Op) -> (Method, String, Option<Value>) {
        let batch = |body: Value| (Method::Post, "/v1/notes/batch".to_owned(), Some(body));
        match op {
            Op::Create { input, .. } => (Method::Post, "/v1/notes".into(), Some(input.to_json())),
            Op::Patch { id, input, guard } => {
                let real = self.resolve(id);
                let mut body = input.to_json();
                if *guard {
                    let base = match self.snapshots.get(real) {
                        Some(Some(s)) => Some(s.note.updated_at.clone()),
                        Some(None) => None,
                        None => self.note(real).map(|e| e.note.updated_at.clone()),
                    };
                    if let Some(b) = base {
                        body["baseUpdatedAt"] = json!(b);
                    }
                }
                (
                    Method::Patch,
                    format!("/v1/notes/{}", enc(real)),
                    Some(body),
                )
            }
            Op::Set { ids, set } => {
                let mut s = set.to_json();
                if let Some(m) = s.as_object_mut() {
                    m.retain(|k, _| matches!(k.as_str(), "color" | "pinned" | "archived"));
                }
                batch(json!({"ids": self.real_ids(ids), "set": s}))
            }
            Op::AddLabel { ids, name } => {
                batch(json!({"ids": self.real_ids(ids), "addLabels": [name]}))
            }
            Op::RemoveLabel { ids, name, .. } => {
                batch(json!({"ids": self.real_ids(ids), "removeLabels": [name]}))
            }
            Op::Trash { ids } => batch(json!({"ids": self.real_ids(ids), "trash": true})),
            Op::Restore { ids } => batch(json!({"ids": self.real_ids(ids), "restore": true})),
            Op::DeleteForever { ids } => {
                batch(json!({"ids": self.real_ids(ids), "deleteForever": true}))
            }
            Op::EmptyTrash { .. } => (Method::Delete, "/v1/notes/trash".into(), None),
            Op::CreateLabel { name } => (
                Method::Post,
                "/v1/notes/labels".into(),
                Some(json!({"name": name})),
            ),
            Op::RenameLabel { tag, name } => (
                Method::Patch,
                format!("/v1/notes/labels/{}", enc(tag)),
                Some(json!({"name": name})),
            ),
            Op::DeleteLabel { tag } => (
                Method::Delete,
                format!("/v1/notes/labels/{}", enc(tag)),
                None,
            ),
        }
    }

    // ------------------------------------------------------------ answers

    pub fn on_reply(&mut self, reply: Reply) {
        let Some((id, kind)) = self.flight.take() else {
            return;
        };
        if id != reply.id {
            self.flight = Some((id, kind));
            return;
        }
        // A dead token refuses nothing the owner did: the writes stay queued for after the next sign-in.
        if matches!(&reply.result, Ok(r) if r.status == 401) {
            self.unauthorized = true;
            self.syncing = false;
            self.sync_paging = false;
            self.sync_wanted = true;
            self.version += 1;
            return;
        }
        let answered = reply.result.is_ok();
        if answered {
            self.online = true;
            self.signed_out = false;
            self.retry = RETRY_MIN;
        } else {
            self.online = false;
            self.signed_out = matches!(&reply.result, Err(m) if m == net::NOT_SIGNED_IN);
        }
        match kind {
            Flight::Changes => self.on_changes(&reply),
            Flight::Labels => {
                if let Ok(v) = net::check(&reply.result) {
                    self.set_registry(v);
                }
            }
            Flight::Write => self.on_write(&reply),
        }
    }

    fn set_registry(&mut self, v: &Value) {
        let labels: Vec<Label> = v["labels"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|l| serde_json::from_value(l.clone()).ok())
            .filter(|l: &Label| !l.tag.is_empty())
            .map(|mut l| {
                if l.name.is_empty() {
                    l.name = l.tag.clone();
                }
                l
            })
            .collect();
        if labels != self.registry {
            self.registry = labels;
            self.bump();
        }
    }

    fn on_changes(&mut self, reply: &Reply) {
        match net::check(&reply.result) {
            Ok(v) => {
                self.loaded = true;
                self.reloaded = false;
                self.sync_error = None;
                let notes = v["notes"].as_array().map_or(&[][..], Vec::as_slice);
                let removed = v["removed"].as_array().map_or(&[][..], Vec::as_slice);
                let changed = self.merge(notes, removed);
                self.sync_changed |= changed;
                self.cursor = match &v["cursor"] {
                    Value::Number(n) => n.to_string(),
                    Value::String(s) if !s.is_empty() => s.clone(),
                    _ => "0".into(),
                };
                self.cache_dirty = true;
                self.settle_unclear_create();
                if v["hasMore"].as_bool() == Some(true) {
                    self.sync_wanted = true;
                    self.sync_paging = true;
                } else {
                    self.syncing = false;
                    self.sync_paging = false;
                    if self.sync_changed {
                        self.labels_wanted = true;
                    }
                }
                self.version += 1;
            }
            Err(e) if e.status == 410 && !self.reloaded => {
                // A cursor from another store (a restore from backup): `dropCache`, and load from 0.
                self.reloaded = true;
                let pending = self.pending.clone();
                self.notes.retain(|id, _| {
                    id.starts_with("tmp_") || pending.get(id).copied().unwrap_or(0) > 0
                });
                self.cursor = "0".into();
                self.sync_wanted = true;
                self.bump();
            }
            Err(e) => {
                self.syncing = false;
                self.loaded = true;
                self.sync_paging = false;
                if e.is_network() {
                    // The writes wait the same delay; the clock tries the sync again. A create with no answer stays
                    // unclear until a sync can look for it.
                    self.back_off();
                } else {
                    self.settle_unclear_create();
                }
                self.sync_error = Some(e.message);
                self.version += 1;
            }
        }
    }

    fn back_off(&mut self) {
        self.retry_at = Some(Instant::now() + self.retry);
        self.retry = (self.retry * 2).min(RETRY_MAX);
    }

    /// `merge`: a note with a write in flight ignores what the sync saw, and so does an older copy.
    fn merge(&mut self, notes: &[Value], removed: &[Value]) -> bool {
        let mut changed = false;
        for v in notes {
            let Some(n) = parse_note(v) else {
                eprintln!("lucina-app-notes: a note in the wrong shape was skipped");
                continue;
            };
            if self.pending.get(&n.id).copied().unwrap_or(0) > 0 {
                continue;
            }
            if self.notes.get(&n.id).is_some_and(|l| l.note.rev > n.rev) {
                continue;
            }
            self.insert(n);
            changed = true;
        }
        for r in removed {
            let Some(id) = r.as_str() else { continue };
            if self.pending.get(id).copied().unwrap_or(0) > 0 {
                continue;
            }
            changed |= self.notes.remove(id).is_some();
        }
        if changed {
            self.bump();
        }
        changed
    }

    /// After a create got no answer: if the sync now shows that note, it did arrive, and sending it again would make a copy.
    fn settle_unclear_create(&mut self) {
        if !self.unclear_create {
            return;
        }
        self.unclear_create = false;
        let Some(Op::Create { tmp, .. }) = self.outbox.front().cloned() else {
            return;
        };
        let Some(local) = self.notes.get(&tmp).cloned() else {
            return;
        };
        let found = self
            .notes
            .values()
            .find(|e| {
                !e.note.id.starts_with("tmp_")
                    && e.note.title == local.note.title
                    && e.note.content == local.note.content
                    && e.note.rev > self.unclear_since
            })
            .cloned();
        if let Some(real) = found {
            self.outbox.pop_front();
            self.outbox_dirty = true;
            self.replace_tmp(&tmp, real.note.clone());
        }
    }

    fn replace_tmp(&mut self, tmp: &str, real: Note) {
        let local = self.notes.remove(tmp);
        let queued = self.pending.remove(tmp).unwrap_or(1).saturating_sub(1);
        self.snapshots.remove(tmp);
        self.alias.insert(tmp.to_owned(), real.id.clone());
        for op in &mut self.outbox {
            rename_id(op, tmp, &real.id);
        }
        if queued > 0 {
            // Edits made while the create was in flight are queued behind it: keep them on screen.
            let mut shown = local.map_or_else(|| real.clone(), |l| l.note.clone());
            shown.id = real.id.clone();
            shown.slug = real.slug.clone();
            shown.rev = real.rev;
            shown.created_at = real.created_at.clone();
            self.pending.insert(real.id.clone(), queued);
            self.snapshots
                .insert(real.id.clone(), Some(Entry::new(real)));
            self.insert(shown);
        } else {
            self.insert(real);
        }
        self.bump();
    }

    fn on_write(&mut self, reply: &Reply) {
        let Some(op) = self.outbox.front().cloned() else {
            return;
        };
        match net::check(&reply.result) {
            Ok(v) => {
                self.outbox.pop_front();
                self.outbox_dirty = true;
                self.write_done(&op, v);
            }
            Err(e) if e.is_transient() => {
                // No answer, or a server that is not itself: the write stays first in the queue.
                if matches!(op, Op::Create { .. }) && e.is_network() && !self.signed_out {
                    self.unclear_create = true;
                    self.unclear_since = self.cursor.parse().unwrap_or(0);
                }
                self.back_off();
                self.version += 1;
            }
            Err(e) => {
                self.outbox.pop_front();
                self.outbox_dirty = true;
                self.refused(&op, &e);
            }
        }
    }

    fn accept(&mut self, id: &str, saved: Note) {
        if self.finish(id) {
            self.snapshots.remove(id);
            self.insert(saved);
        } else {
            // Newer local edits are queued behind this one; they build on this copy.
            self.snapshots
                .insert(id.to_owned(), Some(Entry::new(saved)));
        }
    }

    fn write_done(&mut self, op: &Op, v: &Value) {
        match op {
            Op::Create { tmp, .. } => {
                if let Some(real) = parse_note(v) {
                    self.replace_tmp(tmp, real);
                }
            }
            Op::Patch { id, .. } => {
                let id = self.resolve(id).to_owned();
                match parse_note(v) {
                    Some(saved) => self.accept(&id, saved),
                    None => {
                        self.finish_clean(&id);
                        self.sync_wanted = true;
                    }
                }
            }
            Op::Set { ids, .. }
            | Op::AddLabel { ids, .. }
            | Op::RemoveLabel { ids, .. }
            | Op::Trash { ids }
            | Op::Restore { ids } => {
                let saved: Vec<Note> = v["notes"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(parse_note)
                    .collect();
                for id in self.real_ids(ids) {
                    match saved.iter().find(|s| s.id == id) {
                        Some(s) => self.accept(&id, s.clone()),
                        None => self.finish_clean(&id),
                    }
                }
                if saved.is_empty() {
                    self.sync_wanted = true;
                }
            }
            Op::DeleteForever { ids } | Op::EmptyTrash { ids } => {
                for id in self.real_ids(ids) {
                    self.finish_clean(&id);
                }
            }
            Op::CreateLabel { .. } | Op::RenameLabel { .. } | Op::DeleteLabel { .. } => {
                self.labels_wanted = true;
                self.sync_wanted = true;
            }
        }
        self.bump();
    }

    fn finish(&mut self, id: &str) -> bool {
        let n = self.pending.get(id).copied().unwrap_or(1) - 1;
        if n == 0 {
            self.pending.remove(id);
            true
        } else {
            self.pending.insert(id.to_owned(), n);
            false
        }
    }

    fn finish_clean(&mut self, id: &str) {
        if self.finish(id) {
            self.snapshots.remove(id);
        }
    }

    /// `mutate`'s failure path: the earlier copy comes back and the owner is told.
    fn refused(&mut self, op: &Op, err: &ApiError) {
        if let (
            Op::Patch {
                id, guard: true, ..
            },
            409,
        ) = (op, err.status)
        {
            self.conflict = Some(self.resolve(id).to_owned());
        }
        let ids = op.ids();
        // Later writes to these notes build on what was just refused, and go the same way.
        let mut dropped = Vec::new();
        self.outbox.retain(|o| {
            let hit = o.ids().iter().any(|i| ids.contains(i));
            if hit {
                dropped.push(o.clone());
            }
            !hit
        });
        let mut all = vec![op.clone()];
        all.extend(dropped);
        for o in &all {
            for id in o.ids() {
                let id = self.resolve(&id).to_owned();
                let last = self.finish(&id);
                if !last {
                    continue;
                }
                let snap = self.snapshots.remove(&id).flatten();
                match snap {
                    Some(s) if err.status != 404 => {
                        self.notes.insert(id, s);
                    }
                    _ => {
                        self.notes.remove(&id);
                    }
                }
            }
        }
        match op {
            Op::CreateLabel { .. } if err.code == "label_exists" => {}
            _ => self.notify(err.message.clone(), None),
        }
        self.labels_wanted |= matches!(
            op,
            Op::CreateLabel { .. } | Op::RenameLabel { .. } | Op::DeleteLabel { .. }
        );
        self.sync_wanted |= err.status == 404;
        self.bump();
    }

    // ------------------------------------------------------------ writes

    fn begin(&mut self, id: &str) {
        let n = self.pending.get(id).copied().unwrap_or(0);
        if n == 0 {
            self.snapshots
                .insert(id.to_owned(), self.notes.get(id).cloned());
        }
        self.pending.insert(id.to_owned(), n + 1);
    }

    fn apply_local(&mut self, op: &Op) {
        let now = iso_now();
        let map = |s: &mut Store, ids: &[String], f: &dyn Fn(&Note) -> Note| {
            for id in ids {
                if let Some(e) = s.notes.get(id).cloned() {
                    s.insert(f(&e.note));
                }
            }
        };
        match op {
            Op::Create { tmp, input } => {
                let ms = now_ms();
                let base = Note {
                    id: tmp.clone(),
                    color: "default".into(),
                    position: ms as f64,
                    source: "juno".into(),
                    created_at: to_iso(ms),
                    updated_at: to_iso(ms),
                    ..Note::default()
                };
                let n = apply_input(&base, input, &now);
                self.insert(n);
            }
            Op::Patch { id, input, .. } => map(self, std::slice::from_ref(id), &|n| {
                apply_input(n, input, &now)
            }),
            Op::Set { ids, set } => map(self, ids, &|n| apply_input(n, set, &now)),
            Op::AddLabel { ids, name } => {
                let tag = norm_label(name);
                map(self, ids, &|n| {
                    let mut c = n.clone();
                    if !c.tags.contains(&tag) {
                        c.tags.push(tag.clone());
                        c.updated_at = now.clone();
                    }
                    c
                });
            }
            Op::RemoveLabel { ids, tag, .. } => map(self, ids, &|n| {
                let mut c = n.clone();
                c.tags.retain(|t| t != tag);
                c.updated_at = now.clone();
                c
            }),
            Op::Trash { ids } => map(self, ids, &|n| {
                let mut c = n.clone();
                c.deleted_at = Some(now.clone());
                c
            }),
            Op::Restore { ids } => map(self, ids, &|n| {
                let mut c = n.clone();
                c.deleted_at = None;
                c
            }),
            Op::DeleteForever { ids } | Op::EmptyTrash { ids } => {
                for id in ids {
                    self.notes.remove(id);
                }
            }
            Op::CreateLabel { name } => {
                let tag = norm_label(name);
                if !self.registry.iter().any(|l| l.tag == tag) {
                    self.registry.push(Label {
                        tag,
                        name: name.clone(),
                        count: 0,
                    });
                }
            }
            Op::RenameLabel { tag, name } => {
                for l in &mut self.registry {
                    if &l.tag == tag {
                        l.name = name.clone();
                    }
                }
            }
            Op::DeleteLabel { tag } => {
                self.registry.retain(|l| &l.tag != tag);
                let touched: Vec<String> = self
                    .notes
                    .values()
                    .filter(|e| e.note.tags.contains(tag))
                    .map(|e| e.note.id.clone())
                    .collect();
                map(self, &touched, &|n| {
                    let mut c = n.clone();
                    c.tags.retain(|t| t != tag);
                    c
                });
            }
        }
    }

    fn write(&mut self, op: Op) {
        for id in op.ids() {
            self.begin(&id);
        }
        self.apply_local(&op);
        self.outbox.push_back(op);
        self.outbox_dirty = true;
        self.bump();
    }

    /// Labels typed into a note keep their display case before the registry is fetched again.
    fn register_names(&mut self, names: &[String]) {
        for raw in names {
            let name = clean_label(raw);
            let tag = norm_label(&name);
            if !tag.is_empty() && !self.registry.iter().any(|l| l.tag == tag) {
                self.registry.push(Label {
                    tag,
                    name,
                    count: 0,
                });
            }
        }
    }

    /// `create`: the note shows at once under a temporary id, which the server's answer replaces.
    pub fn create(&mut self, input: Input) -> String {
        if let Some(l) = &input.labels {
            self.register_names(&l.clone());
        }
        self.tmp_seq += 1;
        let tmp = format!("tmp_{:x}_{:x}", now_ms(), self.tmp_seq);
        self.write(Op::Create {
            tmp: tmp.clone(),
            input,
        });
        tmp
    }

    /// `update`. `guard` is the Notes view's conflict check.
    pub fn update(&mut self, id: &str, input: Input, guard: bool) {
        let id = self.resolve(id).to_owned();
        if !self.notes.contains_key(&id) {
            return;
        }
        if let Some(l) = &input.labels {
            self.register_names(&l.clone());
        }
        let in_hand = self
            .flight
            .as_ref()
            .is_some_and(|(_, f)| matches!(f, Flight::Write)) as usize;
        // A later edit to the note last in the queue travels with it: typing is one PATCH, not one per pause.
        if self.outbox.len() > in_hand
            && let Some(Op::Patch {
                id: last,
                input: queued,
                guard: g,
            }) = self.outbox.back_mut()
            && *last == id
            && *g == guard
        {
            queued.merge(input.clone());
            self.apply_local(&Op::Patch { id, input, guard });
            self.outbox_dirty = true;
            self.bump();
            return;
        }
        self.write(Op::Patch { id, input, guard });
    }

    fn live(&self, ids: &[String]) -> Vec<String> {
        ids.iter()
            .map(|i| self.resolve(i).to_owned())
            .filter(|i| self.notes.contains_key(i))
            .collect()
    }

    fn notify(&mut self, text: String, undo: Option<Undo>) {
        self.notice = Some(Notice {
            text,
            undo,
            until: Instant::now() + NOTICE_FOR,
        });
        self.version += 1;
    }

    pub fn dismiss_notice(&mut self) {
        self.notice = None;
        self.version += 1;
    }

    fn plural(n: usize, one: &str, many: &str) -> String {
        if n == 1 {
            one.to_owned()
        } else {
            format!("{n} {many}")
        }
    }

    /// `setMany`: the batch route takes colour, pin and archive together.
    fn set(&mut self, ids: &[String], set: Input) {
        if !ids.is_empty() {
            self.write(Op::Set {
                ids: ids.to_vec(),
                set,
            });
        }
    }

    /// `archive`: Keep unpins what it archives; undo pins it again.
    pub fn archive(&mut self, ids: &[String]) {
        let ids = self.live(ids);
        if ids.is_empty() {
            return;
        }
        let repin: Vec<String> = ids
            .iter()
            .filter(|i| self.note(i).is_some_and(|e| e.note.pinned))
            .cloned()
            .collect();
        self.set(
            &ids,
            Input {
                archived: Some(true),
                pinned: Some(false),
                ..Input::default()
            },
        );
        self.notify(
            Self::plural(ids.len(), "Note archived", "notes archived"),
            Some(Undo::Unarchive { ids, repin }),
        );
    }

    pub fn unarchive(&mut self, ids: &[String]) {
        let ids = self.live(ids);
        if ids.is_empty() {
            return;
        }
        self.set(
            &ids,
            Input {
                archived: Some(false),
                ..Input::default()
            },
        );
        self.notify(
            Self::plural(ids.len(), "Note unarchived", "notes unarchived"),
            Some(Undo::Archive { ids }),
        );
    }

    pub fn trash(&mut self, ids: &[String]) {
        let ids = self.live(ids);
        if ids.is_empty() {
            return;
        }
        self.write(Op::Trash { ids: ids.clone() });
        self.notify(
            Self::plural(ids.len(), "Note in the trash", "notes in the trash"),
            Some(Undo::Restore { ids }),
        );
    }

    pub fn restore(&mut self, ids: &[String]) {
        let ids = self.live(ids);
        if ids.is_empty() {
            return;
        }
        self.write(Op::Restore { ids: ids.clone() });
        self.notify(
            Self::plural(ids.len(), "Note restored", "notes restored"),
            Some(Undo::Trash { ids }),
        );
    }

    pub fn delete_forever(&mut self, ids: &[String]) {
        let ids = self.live(ids);
        if ids.is_empty() {
            return;
        }
        self.write(Op::DeleteForever { ids: ids.clone() });
        self.notify(
            Self::plural(ids.len(), "Note deleted", "notes deleted"),
            None,
        );
    }

    pub fn empty_trash(&mut self) {
        let ids: Vec<String> = self
            .notes
            .values()
            .filter(|e| e.note.deleted_at.is_some())
            .map(|e| e.note.id.clone())
            .collect();
        if !ids.is_empty() {
            self.write(Op::EmptyTrash { ids });
        }
    }

    /// `setPinned`: pinning also takes a note out of the archive.
    pub fn set_pinned(&mut self, ids: &[String], pinned: bool) {
        let ids = self.live(ids);
        let changed: Vec<String> = ids
            .iter()
            .filter(|i| self.note(i).is_some_and(|e| e.note.pinned != pinned))
            .cloned()
            .collect();
        let set = if pinned {
            Input {
                pinned: Some(true),
                archived: Some(false),
                ..Input::default()
            }
        } else {
            Input {
                pinned: Some(false),
                ..Input::default()
            }
        };
        self.set(&ids, set);
        let undo = (!changed.is_empty()).then_some(Undo::SetPinned {
            ids: changed,
            pinned: !pinned,
        });
        self.notify(
            if pinned {
                "Note pinned"
            } else {
                "Note unpinned"
            }
            .to_owned(),
            undo,
        );
    }

    pub fn set_color(&mut self, ids: &[String], color: &str) {
        let ids = self.live(ids);
        self.set(
            &ids,
            Input {
                color: Some(color.to_owned()),
                ..Input::default()
            },
        );
    }

    pub fn add_label(&mut self, id: &str, name: &str) {
        let clean = clean_label(name);
        if norm_label(&clean).is_empty() {
            return;
        }
        self.register_names(std::slice::from_ref(&clean));
        let ids = self.live(&[id.to_owned()]);
        if !ids.is_empty() {
            self.write(Op::AddLabel { ids, name: clean });
        }
    }

    pub fn remove_label(&mut self, id: &str, tag: &str) {
        let ids = self.live(&[id.to_owned()]);
        if !ids.is_empty() {
            let name = self.label_name(tag);
            self.write(Op::RemoveLabel {
                ids,
                tag: tag.to_owned(),
                name,
            });
        }
    }

    pub fn create_label(&mut self, name: &str) {
        let clean = clean_label(name);
        if !norm_label(&clean).is_empty() {
            self.write(Op::CreateLabel { name: clean });
        }
    }

    pub fn rename_label(&mut self, tag: &str, name: &str) {
        let clean = clean_label(name);
        if !norm_label(&clean).is_empty() {
            self.write(Op::RenameLabel {
                tag: tag.to_owned(),
                name: clean,
            });
        }
    }

    pub fn delete_label(&mut self, tag: &str) {
        self.write(Op::DeleteLabel {
            tag: tag.to_owned(),
        });
    }

    /// `duplicate`, "Make a copy": same text, list, colour and labels, as a new note on top. Attachments need their bytes
    /// fetched and uploaded again, which waits for attachment support.
    pub fn duplicate(&mut self, id: &str) -> Option<String> {
        let src = self.note(id)?.note.clone();
        let input = Input {
            title: Some(if src.title.is_empty() {
                String::new()
            } else {
                format!("Copy of {}", src.title)
            }),
            content: src.items.is_none().then(|| src.content.clone()),
            items: src.items.as_ref().map(|items| {
                items
                    .iter()
                    .map(|i| crate::model::Item {
                        id: new_item_id(),
                        ..i.clone()
                    })
                    .collect()
            }),
            color: Some(src.color.clone()),
            labels: Some(src.tags.iter().map(|t| self.label_name(t)).collect()),
            ..Input::default()
        };
        let made = self.create(input);
        self.notify("Note copied".into(), None);
        Some(made)
    }

    pub fn undo(&mut self, u: Undo) {
        match u {
            Undo::Unarchive { ids, repin } => {
                self.set(
                    &ids,
                    Input {
                        archived: Some(false),
                        ..Input::default()
                    },
                );
                self.set(
                    &repin,
                    Input {
                        pinned: Some(true),
                        ..Input::default()
                    },
                );
            }
            Undo::Archive { ids } => self.set(
                &ids,
                Input {
                    archived: Some(true),
                    ..Input::default()
                },
            ),
            Undo::Restore { ids } => self.write(Op::Restore { ids }),
            Undo::Trash { ids } => self.write(Op::Trash { ids }),
            Undo::SetPinned { ids, pinned } => self.set(
                &ids,
                Input {
                    pinned: Some(pinned),
                    ..Input::default()
                },
            ),
        }
        self.dismiss_notice();
    }

    /// The Notes view's conflict: forget the refusal.
    pub fn clear_conflict(&mut self) {
        self.conflict = None;
    }
}

fn rename_id(op: &mut Op, from: &str, to: &str) {
    let swap = |i: &mut String| {
        if i == from {
            *i = to.to_owned();
        }
    };
    match op {
        Op::Patch { id, .. } => swap(id),
        Op::Set { ids, .. }
        | Op::AddLabel { ids, .. }
        | Op::RemoveLabel { ids, .. }
        | Op::Trash { ids }
        | Op::Restore { ids }
        | Op::DeleteForever { ids }
        | Op::EmptyTrash { ids } => ids.iter_mut().for_each(swap),
        _ => {}
    }
}
