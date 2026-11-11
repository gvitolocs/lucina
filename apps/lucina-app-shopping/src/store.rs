//! Lists, the current list's tiles, the catalog and an offline outbox: `web/src/state/shopping.ts` without promises. The
//! store never waits: [`Store::next_job`] says which request should go next, [`Store::on_reply`] hears how it went.
//!
//! The current list's tiles are always confirmed server state plus pending ops, computed with the server's semantics, so an
//! edit shows at once and survives refetches until the server has acknowledged it. Ops keep their id across retries, which
//! makes replay safe; after a 409 they are rebased on a fresh state with new ids.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use lucina_app_support::dates::{now_ms, to_iso, uuid};
use lucina_app_support::net::{self, ApiError, Job, Method, Reply, enc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::model::{
    Catalog, Ctx, DEFAULT_LIST_ID, Hint, Index, Kind, LANGS, List, ListState, OTHER_SECTION, Op,
    Row, Section, Status, apply_ops, catalog_name, parse_catalog, parse_list, parse_list_state,
    parse_lists, parse_rows, rebase_ops, same_row,
};
use lucina_app_support::text::fold_name as fold;

/// The server's copy of `assets/catalog/catalog.en.json`, shipped so that search works before the first answer. SecretaryIOS
/// 1dd77a2b; replaced by what `GET /v1/shopping/catalog` says when the server is reachable.
pub const CATALOG_EN: &str = include_str!("../catalog/catalog.en.json");
pub const UNDO_FOR: Duration = Duration::from_secs(5);
const MAX_CONFLICT_RETRIES: u32 = 3;
const MAX_OPS: usize = 200;
const RETRY_MIN: Duration = Duration::from_secs(2);
const RETRY_MAX: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    pub list_id: String,
    pub op: Op,
}

#[derive(Clone, Debug)]
pub struct UndoEntry {
    pub message: String,
    pub inverse: Option<Vec<Kind>>,
    pub list_id: String,
    pub until: Instant,
}

/// What the app keeps between launches.
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CacheFile {
    pub current_list_id: String,
    pub lists: Vec<List>,
    pub states: HashMap<String, ListState>,
    pub outbox: Vec<Entry>,
    pub grouped: bool,
    pub item_lang: Option<String>,
    pub section_order: HashMap<String, Vec<String>>,
    pub expanded: HashMap<String, bool>,
}

#[derive(Debug)]
enum Flight {
    Lists,
    State(String),
    Catalog,
    /// The list's state, needed before its ops can name a revision.
    FlushBase(String),
    Ops(String, Vec<Op>),
    /// A fresh state after a 409.
    Conflict(String),
    CreateList,
    PatchList(String),
    DeleteList(String),
}

#[derive(Debug)]
enum Admin {
    Create(String, String),
    Patch(String, Value),
    Delete(String),
}

pub struct Store {
    pub lang: String,
    pub lists: Vec<List>,
    pub current_list_id: String,
    /// Visible rows of the current list, optimistic.
    pub items: Vec<Row>,
    pub loaded: bool,
    pub load_error: Option<String>,
    pub action_error: Option<String>,
    pub online: bool,
    pub signed_out: bool,
    pub unauthorized: bool,
    pub catalog: Catalog,
    pub index: Index,
    pub grouped: bool,
    pub item_lang_pref: Option<String>,
    pub item_lang: String,
    pub section_order: HashMap<String, Vec<String>>,
    pub expanded: HashMap<String, bool>,
    pub undo: Option<UndoEntry>,
    /// Goes up with every change a screen could show.
    pub version: u64,
    pub dirty: bool,
    server: HashMap<String, ListState>,
    outbox: Vec<Entry>,
    flight: Option<(u64, Flight)>,
    next_id: u64,
    admin: Vec<Admin>,
    lists_wanted: bool,
    state_wanted: Option<String>,
    catalog_wanted: bool,
    retry_at: Option<Instant>,
    retry: Duration,
    conflicts: HashMap<String, u32>,
    conflict_job: Option<Job>,
}

impl Store {
    pub fn new() -> Store {
        let catalog = serde_json::from_str(CATALOG_EN)
            .map(|v: Value| parse_catalog(&v))
            .unwrap_or_default();
        let index = Index::build(&catalog, "en");
        Store {
            lang: "en".into(),
            lists: Vec::new(),
            current_list_id: DEFAULT_LIST_ID.into(),
            items: Vec::new(),
            loaded: false,
            load_error: None,
            action_error: None,
            online: true,
            signed_out: false,
            unauthorized: false,
            catalog,
            index,
            grouped: false,
            item_lang_pref: None,
            item_lang: "en".into(),
            section_order: HashMap::new(),
            expanded: HashMap::from([("__recent".to_owned(), true)]),
            undo: None,
            version: 1,
            dirty: false,
            server: HashMap::new(),
            outbox: Vec::new(),
            flight: None,
            next_id: 1,
            admin: Vec::new(),
            lists_wanted: false,
            state_wanted: None,
            catalog_wanted: false,
            retry_at: None,
            retry: RETRY_MIN,
            conflicts: HashMap::new(),
            conflict_job: None,
        }
    }

    /// `initShopping`: the cache shows at once, then the outbox and fresh data.
    pub fn start(&mut self, cache: CacheFile) {
        if !cache.current_list_id.is_empty() {
            self.current_list_id = cache.current_list_id;
        }
        self.lists = cache.lists;
        self.server = cache.states;
        self.outbox = cache.outbox;
        self.grouped = cache.grouped;
        self.item_lang_pref = cache.item_lang;
        self.section_order = cache.section_order;
        self.expanded.extend(cache.expanded);
        self.refresh_view();
        self.refresh();
        self.catalog_wanted = true;
    }

    /// `refreshShopping`.
    pub fn refresh(&mut self) {
        self.lists_wanted = true;
        self.state_wanted = Some(self.current_list_id.clone());
    }

    pub fn signed_in(&mut self) {
        self.unauthorized = false;
        self.signed_out = false;
        self.refresh();
        self.catalog_wanted = true;
        self.retry_now();
    }

    pub fn retry_now(&mut self) {
        self.retry_at = None;
        self.retry = RETRY_MIN;
    }

    pub fn retry_at(&self) -> Option<Instant> {
        self.retry_at
    }

    pub fn idle(&self) -> bool {
        self.flight.is_none()
            && self.outbox.is_empty()
            && self.admin.is_empty()
            && !self.lists_wanted
            && self.state_wanted.is_none()
            && !self.catalog_wanted
    }

    pub fn in_flight(&self) -> bool {
        self.flight.is_some()
    }

    pub fn pending(&self) -> usize {
        self.outbox.len()
    }

    pub fn cache_json(&mut self) -> Vec<u8> {
        self.dirty = false;
        let file = CacheFile {
            current_list_id: self.current_list_id.clone(),
            lists: self.lists.clone(),
            states: self.server.clone(),
            outbox: self.outbox.clone(),
            grouped: self.grouped,
            item_lang: self.item_lang_pref.clone(),
            section_order: self.section_order.clone(),
            expanded: self.expanded.clone(),
        };
        serde_json::to_vec(&file).unwrap_or_default()
    }

    pub fn outbox_snapshot(&self) -> &[Entry] {
        &self.outbox
    }

    // ------------------------------------------------------------ the view

    fn hint(catalog: &Catalog, index: &Index, lang: &str, id: &str) -> Option<Hint> {
        let item = catalog.items.get(index.by_id(id)?)?;
        Some(Hint {
            name: catalog_name(item, lang).to_owned(),
            section: item.section.clone(),
            icon: item.icon.clone(),
        })
    }

    fn pending_for(&self, list_id: &str) -> Vec<Op> {
        self.outbox
            .iter()
            .filter(|e| e.list_id == list_id)
            .map(|e| e.op.clone())
            .collect()
    }

    /// `refreshView`: the rows of the current list from server state plus outbox.
    pub fn refresh_view(&mut self) {
        let base = self.server.get(&self.current_list_id);
        let empty = Vec::new();
        let rows = base.map_or(&empty, |b| &b.items);
        let ops = self.pending_for(&self.current_list_id);
        let (catalog, index, lang) = (&self.catalog, &self.index, self.item_lang.clone());
        let now = to_iso(now_ms());
        let hint = |id: &str| Store::hint(catalog, index, &lang, id);
        let mut new_id = uuid;
        let mut ctx = Ctx {
            list_id: &self.current_list_id,
            now: &now,
            by: "web",
            new_id: &mut new_id,
            catalog: &hint,
        };
        let (mut items, ..) = apply_ops(rows, &ops, &mut HashSet::new(), &mut ctx);
        items.retain(Row::active);
        items.sort_by(|a, b| {
            a.position
                .total_cmp(&b.position)
                .then_with(|| a.name.cmp(&b.name))
        });
        self.items = items;
        if base.is_some() {
            self.loaded = true;
        }
        self.update_item_lang();
        self.version += 1;
    }

    /// `itemLang`: the menu's choice, else the language most of the list's catalog rows are named in, else the UI's.
    fn update_item_lang(&mut self) {
        let lang = self.item_lang_pref.clone().unwrap_or_else(|| {
            let mut votes: HashMap<&str, u32> = HashMap::new();
            for row in &self.items {
                let Some(cat) = row
                    .catalog_id
                    .as_deref()
                    .and_then(|c| self.index.by_id(c))
                    .and_then(|i| self.catalog.items.get(i))
                else {
                    continue;
                };
                let name = fold(&row.name);
                for l in LANGS {
                    let n = cat
                        .names
                        .get(l)
                        .or((l == self.catalog.lang).then_some(&cat.name));
                    if n.is_some_and(|n| fold(n) == name) {
                        *votes.entry(l).or_default() += 1;
                    }
                }
            }
            let mut best = self.lang.as_str();
            let mut most = votes.get(best).copied().unwrap_or(0);
            for l in LANGS {
                let n = votes.get(l).copied().unwrap_or(0);
                if n > most {
                    best = l;
                    most = n;
                }
            }
            best.to_owned()
        });
        if lang != self.item_lang {
            self.item_lang = lang;
            self.index = Index::build(&self.catalog, &self.item_lang);
        }
    }

    /// `orderedSections`: the list's store-walk order; sections it has not placed keep catalog order, after.
    pub fn ordered_sections(&self) -> Vec<Section> {
        let order = self
            .section_order
            .get(&self.current_list_id)
            .cloned()
            .unwrap_or_default();
        let rank = |id: &str| order.iter().position(|o| o == id);
        let mut out = self.catalog.sections.clone();
        let catalog_rank: HashMap<String, usize> = out
            .iter()
            .enumerate()
            .map(|(i, s)| (s.id.clone(), i))
            .collect();
        out.sort_by_key(|s| rank(&s.id).unwrap_or(order.len() + catalog_rank[&s.id]));
        out
    }

    /// `moveSection`.
    pub fn move_section(&mut self, id: &str, delta: i32) {
        let mut ids: Vec<String> = self.ordered_sections().into_iter().map(|s| s.id).collect();
        let Some(i) = ids.iter().position(|s| s == id) else {
            return;
        };
        let j = i as i32 + delta;
        if j < 0 || j as usize >= ids.len() {
            return;
        }
        ids.swap(i, j as usize);
        self.section_order.insert(self.current_list_id.clone(), ids);
        self.dirty = true;
        self.version += 1;
    }

    pub fn reset_section_order(&mut self) {
        self.section_order.remove(&self.current_list_id);
        self.dirty = true;
        self.version += 1;
    }

    fn row_catalog(&self, row: &Row) -> Option<usize> {
        match &row.catalog_id {
            Some(c) => self.index.by_id(c),
            None => self.index.resolve(&row.name),
        }
    }

    /// `itemSection`.
    pub fn item_section(&self, row: &Row) -> String {
        if let Some(s) = &row.section {
            return s.clone();
        }
        self.row_catalog(row)
            .and_then(|i| self.catalog.items[i].section.clone())
            .unwrap_or_else(|| OTHER_SECTION.to_owned())
    }

    /// The section a to-buy tile walks under: one the catalog knows, else "other".
    pub fn walk_section(&self, row: &Row) -> String {
        let id = self.item_section(row);
        if self.catalog.sections.iter().any(|s| s.id == id) {
            id
        } else {
            OTHER_SECTION.to_owned()
        }
    }

    /// `needed`: to buy, in the order of a walk through the store.
    pub fn needed(&self) -> Vec<Row> {
        let order = self.ordered_sections();
        let rank = |id: &str| order.iter().position(|s| s.id == id).unwrap_or(usize::MAX);
        let mut rows: Vec<(usize, usize, Row)> = self
            .items
            .iter()
            .filter(|r| r.status == Status::Needed)
            .enumerate()
            .map(|(i, r)| (rank(&self.walk_section(r)), i, r.clone()))
            .collect();
        rows.sort_by_key(|a| (a.0, a.1));
        rows.into_iter().map(|r| r.2).collect()
    }

    /// `recent`: newest first.
    pub fn recent(&self) -> Vec<Row> {
        let mut rows: Vec<Row> = self
            .items
            .iter()
            .filter(|r| r.status == Status::Recent)
            .cloned()
            .collect();
        rows.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        rows
    }

    pub fn current_list_name(&self) -> String {
        self.lists
            .iter()
            .find(|l| l.id == self.current_list_id)
            .map(|l| l.name.clone())
            .or_else(|| {
                self.server
                    .get(&self.current_list_id)
                    .map(|s| s.list.name.clone())
            })
            .unwrap_or_default()
    }

    // ------------------------------------------------------------ what to send next

    fn job(&mut self, method: Method, path: String, body: Option<Value>, flight: Flight) -> Job {
        let id = self.next_id;
        self.next_id += 1;
        self.flight = Some((id, flight));
        Job {
            id,
            method,
            path,
            body,
        }
    }

    /// The next request, if none is on its way: ops first (one list at a time), then list administration, then reads.
    pub fn next_job(&mut self, now: Instant) -> Option<Job> {
        if self.flight.is_some() || self.unauthorized {
            return None;
        }
        if self.retry_at.is_some_and(|t| now < t) {
            return None;
        }
        self.retry_at = None;
        if let Some(first) = self.outbox.first() {
            let list = first.list_id.clone();
            let Some(base) = self.server.get(&list) else {
                let path = format!("/v1/shopping/lists/{}", enc(&list));
                return Some(self.job(Method::Get, path, None, Flight::FlushBase(list)));
            };
            let revision = base.revision;
            let mut sent = self.pending_for(&list);
            sent.truncate(MAX_OPS);
            let path = format!("/v1/shopping/lists/{}/ops", enc(&list));
            let body = json!({"expectedRevision": revision, "ops": sent});
            return Some(self.job(Method::Post, path, Some(body), Flight::Ops(list, sent)));
        }
        if !self.admin.is_empty() {
            let a = self.admin.remove(0);
            return Some(match a {
                Admin::Create(name, theme) => self.job(
                    Method::Post,
                    "/v1/shopping/lists".into(),
                    Some(json!({"name": name, "theme": theme})),
                    Flight::CreateList,
                ),
                Admin::Patch(id, body) => {
                    let path = format!("/v1/shopping/lists/{}", enc(&id));
                    self.job(Method::Patch, path, Some(body), Flight::PatchList(id))
                }
                Admin::Delete(id) => {
                    let path = format!("/v1/shopping/lists/{}", enc(&id));
                    self.job(Method::Delete, path, None, Flight::DeleteList(id))
                }
            });
        }
        if let Some(list) = self.state_wanted.take() {
            let path = format!("/v1/shopping/lists/{}", enc(&list));
            return Some(self.job(Method::Get, path, None, Flight::State(list)));
        }
        if self.lists_wanted {
            self.lists_wanted = false;
            return Some(self.job(
                Method::Get,
                "/v1/shopping/lists".into(),
                None,
                Flight::Lists,
            ));
        }
        if self.catalog_wanted {
            self.catalog_wanted = false;
            let path = format!("/v1/shopping/catalog?lang={}", self.lang);
            return Some(self.job(Method::Get, path, None, Flight::Catalog));
        }
        None
    }

    fn back_off(&mut self) {
        self.retry_at = Some(Instant::now() + self.retry);
        self.retry = (self.retry * 2).min(RETRY_MAX);
    }

    /// `storeServerState`: a GET that raced a newer ops response must not roll the list back.
    fn store_state(&mut self, list_id: &str, mut state: ListState) {
        if self
            .server
            .get(list_id)
            .is_some_and(|k| state.revision < k.revision)
        {
            return;
        }
        state.items.retain(Row::active);
        self.server.insert(list_id.to_owned(), state);
    }

    fn error_text(&self, e: &ApiError) -> String {
        if e.is_network() {
            "Can't reach the server.".into()
        } else if e.code == "lucina_unreachable" || e.code == "grocery_unavailable" {
            "Lucina is offline.".into()
        } else if e.message.is_empty() {
            "Something went wrong.".into()
        } else {
            e.message.clone()
        }
    }

    pub fn on_reply(&mut self, reply: Reply) {
        let Some((id, kind)) = self.flight.take() else {
            return;
        };
        if id != reply.id {
            self.flight = Some((id, kind));
            return;
        }
        if matches!(&reply.result, Ok(r) if r.status == 401) {
            // Nothing queued is lost: it goes after the next sign-in.
            self.unauthorized = true;
            if let Flight::State(l) = &kind {
                self.state_wanted = Some(l.clone());
            }
            self.version += 1;
            return;
        }
        self.signed_out = matches!(&reply.result, Err(m) if m == net::NOT_SIGNED_IN);
        let result = net::check(&reply.result).cloned();
        match kind {
            Flight::Lists => match result {
                Ok(v) => {
                    self.online = true;
                    let lists = parse_lists(&v);
                    if !lists.is_empty() && !lists.iter().any(|l| l.id == self.current_list_id) {
                        let next = lists
                            .iter()
                            .find(|l| l.id == DEFAULT_LIST_ID)
                            .map_or_else(|| lists[0].id.clone(), |l| l.id.clone());
                        self.lists = lists;
                        self.select_list(&next);
                    } else {
                        self.lists = lists;
                    }
                }
                Err(e) if e.is_network() => {
                    self.online = false;
                    self.back_off();
                }
                Err(_) => {}
            },
            Flight::State(list) => match result {
                Ok(v) => {
                    self.online = true;
                    self.store_state(&list, parse_list_state(&v, &list));
                    if list == self.current_list_id {
                        self.load_error = None;
                    }
                }
                Err(e) => {
                    if list == self.current_list_id {
                        self.load_error = Some(self.error_text(&e));
                    }
                    if e.is_network() {
                        self.online = false;
                        self.back_off();
                    }
                }
            },
            Flight::Catalog => {
                // Search keeps the shipped catalog when this fails.
                if let Ok(v) = result {
                    let c = parse_catalog(&v);
                    if !c.items.is_empty() && c != self.catalog {
                        self.index = Index::build(&c, &self.item_lang);
                        self.catalog = c;
                    }
                }
            }
            Flight::FlushBase(list) => match result {
                Ok(v) => self.store_state(&list, parse_list_state(&v, &list)),
                Err(e) if e.is_transient() => {
                    self.online = !e.is_network();
                    self.back_off();
                }
                Err(e) => {
                    self.outbox.retain(|o| o.list_id != list);
                    self.action_error = Some(self.error_text(&e));
                }
            },
            Flight::Ops(list, sent) => match result {
                Ok(v) => self.ops_done(&list, &sent, &v),
                Err(e) if e.status == 409 => {
                    let n = self.conflicts.entry(list.clone()).or_default();
                    *n += 1;
                    let path = format!("/v1/shopping/lists/{}", enc(&list));
                    self.conflict_job =
                        Some(self.job(Method::Get, path, None, Flight::Conflict(list)));
                }
                Err(e) if e.is_transient() => {
                    self.online = !e.is_network();
                    self.back_off();
                }
                Err(e) => {
                    // Refused for good (400, 404, 422): drop these ops and show the server's view.
                    let drop: HashSet<&str> = sent.iter().map(|o| o.op_id.as_str()).collect();
                    self.outbox.retain(|o| !drop.contains(o.op.op_id.as_str()));
                    self.action_error = Some(self.error_text(&e));
                    if e.status == 404 {
                        self.server.remove(&list);
                    } else {
                        self.state_wanted = Some(list);
                    }
                }
            },
            Flight::Conflict(list) => match result {
                Ok(v) => {
                    let mut fresh = parse_list_state(&v, &list);
                    fresh.items.retain(Row::active);
                    let n = self.conflicts.get(&list).copied().unwrap_or(0);
                    if n > MAX_CONFLICT_RETRIES {
                        // Give up on this batch rather than loop: the fresh state wins.
                        self.outbox.retain(|e| e.list_id != list);
                        self.action_error = Some("Something went wrong.".into());
                        self.conflicts.remove(&list);
                    } else {
                        let pending = self.pending_for(&list);
                        let now = to_iso(now_ms());
                        let (catalog, index, lang) =
                            (&self.catalog, &self.index, self.item_lang.clone());
                        let hint = |id: &str| Store::hint(catalog, index, &lang, id);
                        let mut new_id = uuid;
                        let mut ctx = Ctx {
                            list_id: &list,
                            now: &now,
                            by: "web",
                            new_id: &mut new_id,
                            catalog: &hint,
                        };
                        let rebased = rebase_ops(&fresh.items, &pending, &mut ctx);
                        self.outbox.retain(|e| e.list_id != list);
                        self.outbox.extend(rebased.into_iter().map(|op| Entry {
                            list_id: list.clone(),
                            op,
                        }));
                    }
                    self.server.insert(list, fresh);
                }
                Err(e) => {
                    // The refetch failed: keep everything queued.
                    self.online = !e.is_network();
                    self.back_off();
                }
            },
            Flight::CreateList => match result {
                Ok(v) => {
                    if let Some(list) = parse_list(&v).or_else(|| parse_list(&v["list"])) {
                        self.lists.retain(|l| l.id != list.id);
                        self.lists.push(list.clone());
                        self.server.insert(
                            list.id.clone(),
                            ListState {
                                list: list.clone(),
                                revision: 0,
                                items: Vec::new(),
                            },
                        );
                        self.select_list(&list.id);
                    }
                }
                Err(e) => self.action_error = Some(self.error_text(&e)),
            },
            Flight::PatchList(id) => match result {
                Ok(v) => {
                    if let Some(p) = parse_list(&v).or_else(|| parse_list(&v["list"])) {
                        for l in self.lists.iter_mut().filter(|l| l.id == id) {
                            l.name = p.name.clone();
                            l.theme = p.theme.clone();
                        }
                    }
                }
                Err(e) => self.action_error = Some(self.error_text(&e)),
            },
            Flight::DeleteList(id) => match result {
                Ok(_) => {
                    self.lists.retain(|l| l.id != id);
                    self.server.remove(&id);
                    self.outbox.retain(|e| e.list_id != id);
                    if self.current_list_id == id {
                        let next = self
                            .lists
                            .iter()
                            .find(|l| l.id == DEFAULT_LIST_ID)
                            .map(|l| l.id.clone())
                            .or_else(|| self.lists.first().map(|l| l.id.clone()))
                            .unwrap_or_else(|| DEFAULT_LIST_ID.to_owned());
                        self.select_list(&next);
                    }
                }
                Err(e) => self.action_error = Some(self.error_text(&e)),
            },
        }
        self.dirty = true;
        self.refresh_view();
    }

    /// A conflict's refetch, decided in `on_reply` and sent by the app on its next `pump`.
    pub fn take_conflict_job(&mut self) -> Option<Job> {
        self.conflict_job.take()
    }

    fn ops_done(&mut self, list: &str, sent: &[Op], v: &Value) {
        self.online = true;
        self.retry = RETRY_MIN;
        self.conflicts.remove(list);
        let done: HashSet<&str> = sent.iter().map(|o| o.op_id.as_str()).collect();
        self.outbox.retain(|o| !done.contains(o.op.op_id.as_str()));
        if v["items"].is_array() {
            let items = parse_rows(&v["items"]);
            self.remap_ids(list, sent, &items);
            let base = self
                .server
                .get(list)
                .map(|s| s.list.clone())
                .unwrap_or_default();
            let revision = v["revision"].as_u64().unwrap_or(0);
            self.store_state(
                list,
                ListState {
                    list: base,
                    revision,
                    items,
                },
            );
        } else {
            self.state_wanted = Some(list.to_owned());
        }
    }

    /// `remapIds`: rows made with our own id may come back under a server id. Queued ops follow them.
    fn remap_ids(&mut self, list: &str, sent: &[Op], items: &[Row]) {
        let mut remap: HashMap<String, String> = HashMap::new();
        for op in sent {
            if let Kind::Add {
                item_id: Some(i),
                catalog_id,
                name,
                ..
            } = &op.kind
                && !items.iter().any(|r| r.id == *i)
                && let Some(m) = items
                    .iter()
                    .find(|r| r.active() && same_row(r, catalog_id.as_deref(), name))
            {
                remap.insert(i.clone(), m.id.clone());
            }
        }
        for e in self.outbox.iter_mut().filter(|e| e.list_id == list) {
            if let Some(to) = e.op.kind.target().and_then(|t| remap.get(t)).cloned() {
                e.op.kind.retarget(&to);
            }
        }
    }

    // ------------------------------------------------------------ actions

    /// `enqueue`: ops for a list show at once; a flush follows.
    pub fn enqueue(&mut self, list_id: &str, kinds: Vec<Kind>) {
        for kind in kinds {
            self.outbox.push(Entry {
                list_id: list_id.to_owned(),
                op: Op {
                    op_id: uuid(),
                    kind,
                },
            });
        }
        self.dirty = true;
        self.refresh_view();
    }

    fn offer_undo(&mut self, message: String, inverse: Option<Vec<Kind>>) {
        self.undo = Some(UndoEntry {
            message,
            inverse,
            list_id: self.current_list_id.clone(),
            until: Instant::now() + UNDO_FOR,
        });
        self.version += 1;
    }

    pub fn undo_last(&mut self) {
        if let Some(e) = self.undo.take()
            && let Some(inv) = e.inverse
        {
            self.enqueue(&e.list_id, inv);
        }
        self.version += 1;
    }

    pub fn dismiss_undo(&mut self) {
        self.undo = None;
        self.version += 1;
    }

    fn list(&self) -> String {
        self.current_list_id.clone()
    }

    /// `tapItem`: to-buy becomes bought; recent goes back on the list.
    pub fn tap_item(&mut self, id: &str) {
        let Some(row) = self.items.iter().find(|r| r.id == id).cloned() else {
            return;
        };
        let item_id = id.to_owned();
        if row.status == Status::Needed {
            self.enqueue(
                &self.list(),
                vec![Kind::Purchase {
                    item_id: item_id.clone(),
                }],
            );
            self.offer_undo(
                format!("{} bought", row.name),
                Some(vec![Kind::Need { item_id }]),
            );
        } else {
            self.enqueue(
                &self.list(),
                vec![Kind::Need {
                    item_id: item_id.clone(),
                }],
            );
            self.offer_undo(
                format!("{} added", row.name),
                Some(vec![Kind::Purchase { item_id }]),
            );
        }
    }

    fn find_active(&self, catalog_id: Option<&str>, name: &str) -> Option<Row> {
        self.items
            .iter()
            .find(|r| same_row(r, catalog_id, name))
            .cloned()
    }

    /// `addCatalogItem`. Returns the row id it ended up on.
    pub fn add_catalog(&mut self, at: usize, spec: Option<String>) -> Option<String> {
        let cat = self.catalog.items.get(at)?.clone();
        let name = catalog_name(&cat, &self.item_lang).to_owned();
        if let Some(existing) = self.find_active(Some(&cat.id), &name) {
            if existing.status == Status::Needed && spec.is_none() {
                return Some(existing.id);
            }
            let was_recent = existing.status == Status::Recent;
            let add = Kind::Add {
                item_id: Some(existing.id.clone()),
                catalog_id: Some(cat.id.clone()),
                name: name.clone(),
                spec,
                quantity: None,
                unit: None,
                section: None,
            };
            self.enqueue(&self.list(), vec![add]);
            if was_recent {
                self.offer_undo(
                    format!("{name} added"),
                    Some(vec![Kind::Purchase {
                        item_id: existing.id.clone(),
                    }]),
                );
            }
            return Some(existing.id);
        }
        let item_id = uuid();
        let add = Kind::Add {
            item_id: Some(item_id.clone()),
            catalog_id: Some(cat.id),
            name: name.clone(),
            spec: spec.filter(|s| !s.is_empty()),
            quantity: None,
            unit: None,
            section: cat.section,
        };
        self.enqueue(&self.list(), vec![add]);
        self.offer_undo(
            format!("{name} added"),
            Some(vec![Kind::Remove {
                item_id: item_id.clone(),
            }]),
        );
        Some(item_id)
    }

    /// `addText`: a catalog item when it matches exactly, else a custom one.
    pub fn add_text(&mut self, text: &str, spec: Option<String>) -> Option<String> {
        let name: String = text.trim().chars().take(200).collect();
        if name.is_empty() {
            return None;
        }
        if let Some(at) = self.index.exact(&name) {
            return self.add_catalog(at, spec);
        }
        if let Some(existing) = self.find_active(None, &name) {
            if spec.is_some() {
                let was_recent = existing.status == Status::Recent;
                let add = Kind::Add {
                    item_id: Some(existing.id.clone()),
                    catalog_id: None,
                    name: existing.name.clone(),
                    spec,
                    quantity: None,
                    unit: None,
                    section: None,
                };
                self.enqueue(&self.list(), vec![add]);
                if was_recent {
                    self.offer_undo(
                        format!("{} added", existing.name),
                        Some(vec![Kind::Purchase {
                            item_id: existing.id.clone(),
                        }]),
                    );
                }
            } else if existing.status == Status::Recent {
                self.tap_item(&existing.id);
            }
            return Some(existing.id);
        }
        let item_id = uuid();
        let add = Kind::Add {
            item_id: Some(item_id.clone()),
            catalog_id: None,
            name: name.clone(),
            spec: spec.filter(|s| !s.is_empty()),
            quantity: None,
            unit: None,
            section: None,
        };
        self.enqueue(&self.list(), vec![add]);
        self.offer_undo(
            format!("{name} added"),
            Some(vec![Kind::Remove {
                item_id: item_id.clone(),
            }]),
        );
        Some(item_id)
    }

    /// `editItem`: only the fields that changed become ops.
    pub fn edit_item(
        &mut self,
        id: &str,
        name: &str,
        spec: &str,
        quantity: Option<f64>,
        unit: &str,
        section: Option<String>,
    ) {
        let Some(row) = self.items.iter().find(|r| r.id == id).cloned() else {
            return;
        };
        let item_id = id.to_owned();
        let mut ops = Vec::new();
        if !name.trim().is_empty() && name.trim() != row.name {
            ops.push(Kind::Rename {
                item_id: item_id.clone(),
                name: name.trim().to_owned(),
            });
        }
        if spec.trim() != row.spec {
            ops.push(Kind::SetSpec {
                item_id: item_id.clone(),
                spec: spec.trim().to_owned(),
            });
        }
        let unit = (!unit.trim().is_empty()).then(|| unit.trim().to_owned());
        if quantity != row.quantity || unit != row.unit {
            ops.push(Kind::SetQuantity {
                item_id: item_id.clone(),
                quantity,
                unit,
            });
        }
        if let Some(s) = section
            && s != self.item_section(&row)
        {
            ops.push(Kind::SetSection {
                item_id,
                section: Some(s),
            });
        }
        if !ops.is_empty() {
            self.enqueue(&self.list(), ops);
        }
    }

    /// `deleteItem`: a removed row never comes back, so undo adds it again as a new row.
    pub fn delete_item(&mut self, id: &str) {
        let Some(row) = self.items.iter().find(|r| r.id == id).cloned() else {
            return;
        };
        self.enqueue(
            &self.list(),
            vec![Kind::Remove {
                item_id: id.to_owned(),
            }],
        );
        let new_id = uuid();
        let mut inverse = vec![Kind::Add {
            item_id: Some(new_id.clone()),
            catalog_id: row.catalog_id.clone(),
            name: row.name.clone(),
            spec: Some(row.spec.clone()),
            quantity: row.quantity,
            unit: row.unit.clone(),
            section: row.section.clone(),
        }];
        if row.status == Status::Recent {
            inverse.push(Kind::Purchase { item_id: new_id });
        }
        self.offer_undo(format!("{} deleted", row.name), Some(inverse));
    }

    pub fn move_item(&mut self, id: &str, to: &str) {
        let Some(row) = self.items.iter().find(|r| r.id == id).cloned() else {
            return;
        };
        if to == self.current_list_id {
            return;
        }
        self.enqueue(
            &self.list(),
            vec![Kind::Move {
                item_id: id.to_owned(),
                to_list_id: to.to_owned(),
            }],
        );
        self.offer_undo(format!("{} moved", row.name), None);
        // The target list's counts change on the server.
        self.lists_wanted = true;
    }

    pub fn remove_items(&mut self, ids: &[String]) {
        let present: Vec<Kind> = ids
            .iter()
            .filter(|id| {
                self.items
                    .iter()
                    .any(|r| r.id == **id && r.status == Status::Recent)
            })
            .map(|id| Kind::Remove {
                item_id: id.clone(),
            })
            .collect();
        if present.is_empty() {
            return;
        }
        let n = present.len();
        self.enqueue(&self.list(), present);
        self.offer_undo(format!("{n} removed from recently used"), None);
    }

    pub fn clear_recent(&mut self) {
        if self.items.iter().any(|r| r.status == Status::Recent) {
            self.enqueue(&self.list(), vec![Kind::ClearRecent]);
            self.offer_undo("Recently used cleared".into(), None);
        }
    }

    pub fn select_list(&mut self, id: &str) {
        if id == self.current_list_id {
            return;
        }
        self.undo = None;
        self.current_list_id = id.to_owned();
        self.loaded = self.server.contains_key(id);
        self.load_error = None;
        self.state_wanted = Some(id.to_owned());
        self.dirty = true;
        self.refresh_view();
    }

    pub fn create_list(&mut self, name: &str, theme: &str) {
        let clean: String = name.trim().chars().take(80).collect();
        if !clean.is_empty() {
            self.admin.push(Admin::Create(clean, theme.to_owned()));
        }
    }

    pub fn update_list(&mut self, id: &str, name: Option<&str>, theme: Option<&str>) {
        let mut body = serde_json::Map::new();
        if let Some(n) = name {
            body.insert("name".into(), json!(n.trim()));
        }
        if let Some(t) = theme {
            body.insert("theme".into(), json!(t));
        }
        if !body.is_empty() {
            self.admin
                .push(Admin::Patch(id.to_owned(), Value::Object(body)));
        }
    }

    pub fn remove_list(&mut self, id: &str) {
        if id != DEFAULT_LIST_ID {
            self.admin.push(Admin::Delete(id.to_owned()));
        }
    }

    pub fn set_item_lang(&mut self, lang: Option<String>) {
        self.item_lang_pref = lang;
        self.dirty = true;
        self.refresh_view();
    }
}
