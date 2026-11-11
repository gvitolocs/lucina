//! A stand-in for the shopping routes, in this process: `docs/server-api.md` "Shopping v2" and the semantics of
//! `crates/lucina-grocery` (op-id receipts, a list revision per batch that applied anything, a stale `expectedRevision` is a
//! 409 only when an op touches a row changed after it). For the tests and for showing the app without an account. Its data
//! is invented.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use lucina_app_support::dates::{now_ms, to_iso, uuid};
use lucina_app_support::net::{Method, Response, Transport};
use serde_json::{Value, json};

use crate::model::{
    Ctx, DEFAULT_LIST_ID, Kind, List, ListState, Op, Row, Status, apply_ops, parse_catalog,
};

#[derive(Default)]
pub struct State {
    pub lists: Vec<List>,
    pub states: HashMap<String, ListState>,
    pub seen: HashSet<String>,
    /// When each row last changed, by list revision.
    pub changed_at: HashMap<String, u64>,
    pub drop_next: u32,
    pub drop_after_arriving: bool,
    pub requests: Vec<String>,
    pub batches: Vec<usize>,
    pub unauthorized: bool,
    /// Lucina's grocery store is down: 503.
    pub unavailable: bool,
}

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

fn no_hint(_: &str) -> Option<crate::model::Hint> {
    None
}

impl Fake {
    pub fn new() -> Fake {
        let f = Fake(Arc::new(Mutex::new(State::default())));
        f.0.lock()
            .expect("fake")
            .add_list(DEFAULT_LIST_ID, "Home", "home");
        f
    }

    pub fn demo() -> Fake {
        let f = Fake::new();
        {
            let mut s = f.0.lock().expect("fake");
            s.add_list("work", "Office", "work");
            let catalog =
                parse_catalog(&serde_json::from_str(crate::store::CATALOG_EN).unwrap_or_default());
            let by = |id: &str| catalog.items.iter().find(|i| i.id == id);
            for (id, spec, status) in [
                ("banana", "6, ripe", Status::Needed),
                ("milk", "", Status::Needed),
                ("bread", "x2", Status::Needed),
                ("eggs", "", Status::Needed),
                ("coffee", "", Status::Needed),
                ("pasta", "", Status::Recent),
                ("butter", "", Status::Recent),
                ("apple", "", Status::Recent),
            ] {
                let item = by(id);
                let name = item.map_or(id, |i| i.name.as_str()).to_owned();
                s.push_row(
                    "default",
                    &name,
                    item.map(|i| i.id.clone()),
                    spec,
                    item.and_then(|i| i.section.clone()),
                    status,
                );
            }
            s.push_row(
                "default",
                "Sourdough starter",
                None,
                "from Anna",
                None,
                Status::Needed,
            );
            s.push_row("work", "Printer paper", None, "A4", None, Status::Needed);
        }
        f
    }
}

impl State {
    pub fn add_list(&mut self, id: &str, name: &str, theme: &str) {
        let list = List {
            id: id.into(),
            name: name.into(),
            theme: theme.into(),
            ..List::default()
        };
        self.lists.push(list.clone());
        self.states.insert(
            id.into(),
            ListState {
                list,
                revision: 1,
                items: Vec::new(),
            },
        );
    }

    pub fn push_row(
        &mut self,
        list: &str,
        name: &str,
        catalog_id: Option<String>,
        spec: &str,
        section: Option<String>,
        status: Status,
    ) -> String {
        let id = uuid();
        let st = self.states.get_mut(list).expect("list");
        st.revision += 1;
        st.items.push(Row {
            id: id.clone(),
            catalog_id,
            name: name.into(),
            spec: spec.into(),
            section,
            status,
            position: st.items.len() as f64,
            updated_at: to_iso(now_ms()),
            updated_by: "telegram".into(),
            ..Row::default()
        });
        self.changed_at.insert(id.clone(), st.revision);
        id
    }

    fn list_json(&self, l: &List) -> Value {
        let st = &self.states[&l.id];
        let count = |s| st.items.iter().filter(|r| r.status == s).count();
        json!({"id": l.id, "name": l.name, "theme": l.theme, "needed": count(Status::Needed), "recent": count(Status::Recent), "updatedAt": to_iso(now_ms())})
    }

    fn state_json(&self, id: &str) -> Option<Value> {
        let st = self.states.get(id)?;
        Some(
            json!({"list": {"id": id, "name": st.list.name, "theme": st.list.theme}, "revision": st.revision, "items": st.items}),
        )
    }

    fn ops(&mut self, list: &str, b: &Value) -> Response {
        if !self.states.contains_key(list) {
            return err(404, "list_not_found", "No such list.");
        }
        let ops: Vec<Op> = match b["ops"].as_array().map(|a| {
            a.iter()
                .map(|o| serde_json::from_value(o.clone()))
                .collect()
        }) {
            Some(Ok(v)) => v,
            _ => return err(400, "bad_request", "ops"),
        };
        if ops.is_empty() || ops.len() > 200 {
            return err(400, "bad_request", "ops: 1-200");
        }
        self.batches.push(ops.len());
        let rev = self.states[list].revision;
        if let Some(expected) = b["expectedRevision"].as_u64()
            && expected < rev
        {
            let stale = ops
                .iter()
                .filter(|o| !self.seen.contains(&o.op_id))
                .any(|o| {
                    o.kind
                        .target()
                        .is_some_and(|t| self.changed_at.get(t).is_some_and(|c| *c > expected))
                });
            if stale {
                return err(409, "revision_conflict", "The list changed.");
            }
        }
        let before = self.states[list].items.clone();
        let now = to_iso(now_ms());
        let mut new_id = uuid;
        let mut ctx = Ctx {
            list_id: list,
            now: &now,
            by: "web",
            new_id: &mut new_id,
            catalog: &no_hint,
        };
        let mut seen = std::mem::take(&mut self.seen);
        let (mut items, applied, duplicates) = apply_ops(&before, &ops, &mut seen, &mut ctx);
        self.seen = seen;
        // `move` puts the row in the other list.
        for o in &ops {
            if let Kind::Move {
                item_id,
                to_list_id,
            } = &o.kind
                && let (Some(r), Some(to)) = (
                    before.iter().find(|r| r.id == *item_id),
                    self.states.get_mut(to_list_id),
                )
            {
                to.items.push(Row {
                    id: uuid(),
                    ..r.clone()
                });
            }
        }
        let st = self.states.get_mut(list).expect("list");
        if !applied.is_empty() {
            st.revision += 1;
        }
        let revision = st.revision;
        for r in &items {
            if before.iter().find(|x| x.id == r.id) != Some(r) {
                self.changed_at.insert(r.id.clone(), revision);
            }
        }
        items.retain(Row::active);
        st.items = items;
        ok(
            200,
            json!({"revision": revision, "applied": applied, "duplicates": duplicates, "items": st.items}),
        )
    }

    fn route(&mut self, method: Method, path: &str, body: Option<&Value>) -> Response {
        if self.unavailable {
            return err(
                503,
                "grocery_unavailable",
                "The grocery store is not available.",
            );
        }
        let (path, _) = path.split_once('?').unwrap_or((path, ""));
        let empty = Value::Null;
        let b = body.unwrap_or(&empty);
        let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
        match (method, parts.as_slice()) {
            (Method::Get, ["v1", "shopping", "catalog"]) => ok(
                200,
                serde_json::from_str(crate::store::CATALOG_EN).unwrap_or_default(),
            ),
            (Method::Get, ["v1", "shopping", "lists"]) => {
                let v: Vec<Value> = self.lists.iter().map(|l| self.list_json(l)).collect();
                ok(200, json!(v))
            }
            (Method::Post, ["v1", "shopping", "lists"]) => {
                let name = b["name"].as_str().unwrap_or("").trim().to_owned();
                if name.is_empty() {
                    return err(400, "bad_request", "name");
                }
                let id = format!("l{}", self.lists.len());
                self.add_list(&id, &name, b["theme"].as_str().unwrap_or("other"));
                let l = self.lists.last().expect("just added").clone();
                ok(201, self.list_json(&l))
            }
            (Method::Get, ["v1", "shopping", "lists", id]) => match self.state_json(id) {
                Some(v) => ok(200, v),
                None => err(404, "list_not_found", "No such list."),
            },
            (Method::Patch, ["v1", "shopping", "lists", id]) => {
                let Some(l) = self.lists.iter_mut().find(|l| l.id == *id) else {
                    return err(404, "list_not_found", "No such list.");
                };
                if let Some(n) = b["name"].as_str() {
                    l.name = n.to_owned();
                }
                if let Some(t) = b["theme"].as_str() {
                    l.theme = t.to_owned();
                }
                let l = l.clone();
                ok(200, self.list_json(&l))
            }
            (Method::Delete, ["v1", "shopping", "lists", id]) => {
                if *id == DEFAULT_LIST_ID {
                    return err(422, "default_list", "The main list can't be deleted.");
                }
                self.lists.retain(|l| l.id != *id);
                self.states.remove(*id);
                ok(200, json!({"ok": true}))
            }
            (Method::Post, ["v1", "shopping", "lists", id, "ops"]) => self.ops(id, b),
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
