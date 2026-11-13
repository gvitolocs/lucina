//! lucina-app-shopping: Shopping on Juno. What it copies from `web/src/views/Shopping.tsx` and `web/src/views/shopping/`, and
//! what it leaves out, is in `docs/research/juno-apps-notes.md`.
//!
//! The screen is the to-buy tiles in store-walk order, recently used beside them, and the catalog under them. Everything shows
//! what is in memory; [`store::Store`] says what to send, and a worker thread sends it.

// Juno with no C library: Eyra is the libc and the program start, in Rust (docs/research/juno-pure-rust.md).
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
extern crate eyra;

mod fake;
mod model;
mod store;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lucina_account::ui::SignInView;
use lucina_app::keys;
use lucina_app::{App, Face, Harness, Image, ListState, Rect, Rgb, Sender, TextEdit, Theme, Ui};
use lucina_app_support::dates::{now_ms, parse_iso};
use lucina_app_support::files::{Saver, data_dir, write_atomic};
use lucina_app_support::net::{self, Reply, Transport, Worker};
use lucina_app_support::text::fold_name as fold;

use model::{
    DEFAULT_LIST_ID, LANGS, OTHER_SECTION, Row as ShopRow, SPEC_CHIPS_EN, Status, THEMES,
    catalog_name, item_key, join_spec, parse_query,
};
use store::{CacheFile, Store};

pub enum Msg {
    Reply(Reply),
    Cache(Box<CacheFile>),
    /// The sign-in screen has something new to show.
    Changed,
    /// The sign-in screen has kept a token.
    SignedIn,
    /// Pictures decoded on a thread.
    Art(Vec<(String, Image)>),
}

const TILE_W: f32 = 104.0;
const TILE_H: f32 = 96.0;
const TILE_GAP: f32 = 8.0;

/// `components/Tile`'s roles: to buy in the accent, recent muted, catalog a plain card, custom outlined.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TileKind {
    Need,
    Recent,
    Catalog,
    Custom,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Tap {
    /// A row of the list.
    Row(String),
    /// A catalog entry.
    Catalog(usize),
    /// From the search: free text.
    Custom,
    /// From the search: a row the owner typed once.
    History(String, String),
    /// From the search: a catalog entry.
    Found(usize),
    /// Recently used in edit mode: flip the selection.
    Select(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct TileData {
    pub key: String,
    pub kind: TileKind,
    pub name: String,
    pub spec: String,
    pub tap: Tap,
    pub details: Option<String>,
    pub checked: Option<bool>,
}

/// A row of the scrolling list.
#[derive(Clone, Debug, PartialEq)]
enum Line {
    Heading(String),
    Hint(String),
    Section {
        id: String,
        name: String,
        open: bool,
        count: usize,
    },
    Tiles(usize, usize),
}

/// A dialog.
pub enum Dialog {
    Details(Details),
    Lists(ListsDialog),
    Order(ListState),
    Menu,
}

pub struct Details {
    pub id: String,
    pub name: TextEdit,
    pub spec: TextEdit,
    pub quantity: TextEdit,
    pub unit: TextEdit,
    pub section: Option<String>,
    picking_section: bool,
    picking_list: bool,
    confirm: bool,
    sections: ListState,
}

pub struct ListsDialog {
    list: ListState,
    new_name: TextEdit,
    theme: usize,
    /// Settings of this list.
    editing: Option<String>,
    edit_name: TextEdit,
    edit_theme: usize,
    confirm_delete: bool,
}

pub struct Shopping {
    pub store: Store,
    worker: Option<Worker>,
    deliver: Arc<dyn Fn(Reply) + Send + Sync>,
    tx: Option<Sender<Msg>>,
    pub signin: Option<SignInView>,
    pub needs_signin: bool,
    saver: Saver,
    dir: Option<PathBuf>,
    pub search: TextEdit,
    pub chips: Vec<String>,
    pub tiles: Vec<TileData>,
    lines: Vec<Line>,
    rail_tiles: Vec<TileData>,
    rail_lines: Vec<(usize, usize)>,
    built: (u64, String, usize, usize, usize, bool, usize),
    list: ListState,
    rail: ListState,
    pub editing: bool,
    pub selected: Vec<String>,
    pub dialog: Option<Dialog>,
    pub sel: Option<usize>,
    want_search: u8,
    search_focused: bool,
    art: HashMap<&'static str, &'static [u8]>,
    /// Tile key to catalog id, for the tiles that have a picture.
    tile_art: HashMap<String, String>,
    images: HashMap<String, Image>,
    asked_art: std::collections::HashSet<String>,
    art_px: u32,
    /// The face of each tile of the list, by tile index: the rect `draw_tiles_list` hit-tests, so that
    /// a test clicks from the layout rather than from numbers copied beside it. `None` off screen.
    pub tile_rects: Vec<Option<Rect>>,
    /// The details corner of each tile that has one, by tile index.
    pub tile_corners: Vec<Option<Rect>>,
    quit_saved: bool,
    next_refresh: Instant,
}

const REFRESH_EVERY: Duration = Duration::from_secs(60);

/// SecretaryIOS's original item illustrations (`assets/icons/items`, its licence, MIT; `examples/bake_art.rs`). The bytes stay
/// mapped in the program and only the pictures on screen are decoded.
const ART: &[u8] = include_bytes!("../art/items.bin");

fn art_index() -> HashMap<&'static str, &'static [u8]> {
    let mut out = HashMap::new();
    let mut b = ART;
    while let Some((&n, rest)) = b.split_first() {
        let (Some(id), Some(len)) = (rest.get(..n as usize), rest.get(n as usize..n as usize + 4))
        else {
            break;
        };
        let len = u32::from_le_bytes([len[0], len[1], len[2], len[3]]) as usize;
        let start = n as usize + 4;
        let Some(png) = rest.get(start..start + len) else {
            break;
        };
        if let Ok(id) = std::str::from_utf8(id) {
            out.insert(id, png);
        }
        b = &rest[start + len..];
    }
    out
}

fn fill_of(kind: TileKind, t: &Theme) -> Rgb {
    match kind {
        TileKind::Need => t.accent,
        TileKind::Recent => t.panel_hi,
        TileKind::Catalog => t.panel,
        TileKind::Custom => t.bg,
    }
}

impl Shopping {
    pub fn new(
        transport: Option<Box<dyn Transport>>,
        deliver: impl Fn(Reply) + Send + Sync + 'static,
        dir: Option<PathBuf>,
        tx: Option<Sender<Msg>>,
    ) -> Shopping {
        let deliver: Arc<dyn Fn(Reply) + Send + Sync> = Arc::new(deliver);
        let d = deliver.clone();
        let needs_signin = transport.is_none();
        Shopping {
            store: Store::new(),
            worker: Some(Worker::spawn(transport, move |r| d(r))),
            deliver,
            tx,
            signin: None,
            needs_signin,
            saver: Saver::spawn(),
            dir,
            search: TextEdit::new().placeholder("What do you need?"),
            chips: Vec::new(),
            tiles: Vec::new(),
            lines: Vec::new(),
            rail_tiles: Vec::new(),
            rail_lines: Vec::new(),
            built: (0, String::new(), 0, 0, 0, false, 0),
            list: ListState::new(),
            rail: ListState::new(),
            editing: false,
            selected: Vec::new(),
            dialog: None,
            sel: None,
            want_search: 3,
            search_focused: false,
            art: art_index(),
            tile_art: HashMap::new(),
            images: HashMap::new(),
            asked_art: Default::default(),
            art_px: 56,
            tile_rects: Vec::new(),
            tile_corners: Vec::new(),
            quit_saved: false,
            next_refresh: Instant::now() + REFRESH_EVERY,
        }
    }

    fn ask_sign_in(&mut self) {
        self.needs_signin = true;
        if self.signin.is_some() {
            return;
        }
        let Some(tx) = self.tx.clone() else {
            return;
        };
        let t2 = tx.clone();
        self.signin = Some(SignInView::start(
            Arc::new(move || tx.send(Msg::Changed)),
            Arc::new(move || t2.send(Msg::SignedIn)),
        ));
    }

    fn signed_in(&mut self) {
        let d = self.deliver.clone();
        self.worker = Some(Worker::spawn(net::connect(), move |r| d(r)));
        self.signin = None;
        self.needs_signin = false;
        self.store.signed_in();
    }

    pub fn pump(&mut self) {
        let now = Instant::now();
        if self.store.unauthorized && !self.needs_signin {
            net::forget_token();
            self.ask_sign_in();
        }
        if let Some(j) = self.store.take_conflict_job()
            && let Some(w) = &self.worker
        {
            w.send(j);
        }
        while let Some(job) = self.store.next_job(now) {
            if let Some(w) = &self.worker {
                w.send(job);
            }
        }
        if self.store.dirty {
            let bytes = self.store.cache_json();
            if let Some(dir) = &self.dir {
                let path = dir.join("cache.json");
                self.saver.save(move || Some((path, bytes)));
            }
        }
    }

    fn tick(&mut self, ui: &mut Ui) {
        let now = Instant::now();
        if self.store.undo.as_ref().is_some_and(|u| u.until <= now) {
            self.store.dismiss_undo();
        }
        // The web refreshes on focus and when the tab shows; here the app is open or it is not.
        if now >= self.next_refresh {
            self.next_refresh = now + REFRESH_EVERY;
            self.store.refresh();
        }
        self.pump();
        let mut next = self.next_refresh;
        for d in [
            self.store.undo.as_ref().map(|u| u.until),
            self.store.retry_at(),
        ]
        .into_iter()
        .flatten()
        {
            next = next.min(d);
        }
        ui.wake_after(
            next.saturating_duration_since(now)
                .max(Duration::from_millis(5)),
        );
    }

    fn query(&self) -> (String, String) {
        let p = parse_query(&self.search.text);
        let spec = join_spec(&p.spec, &self.chips);
        (p.text, spec)
    }

    pub fn searching(&self) -> bool {
        !self.search.text.trim().is_empty()
    }

    fn row_tile(&self, r: &ShopRow) -> TileData {
        let qty = r
            .quantity
            .map(|q| {
                let n = if q.fract() == 0.0 {
                    format!("{}", q as i64)
                } else {
                    format!("{q}")
                };
                match &r.unit {
                    Some(u) => format!("{n} {u}"),
                    None => n,
                }
            })
            .unwrap_or_default();
        let spec = [qty.as_str(), r.spec.as_str()]
            .iter()
            .filter(|s| !s.is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join(" · ");
        let editing = self.editing && r.status == Status::Recent;
        TileData {
            key: r.id.clone(),
            kind: if r.status == Status::Needed {
                TileKind::Need
            } else {
                TileKind::Recent
            },
            name: r.name.clone(),
            spec,
            tap: if editing {
                Tap::Select(r.id.clone())
            } else {
                Tap::Row(r.id.clone())
            },
            details: (!editing).then(|| r.id.clone()),
            checked: editing.then(|| self.selected.contains(&r.id)),
        }
    }

    /// `Shopping.tsx`'s memos: what the screen shows, again only when something it depends on changed.
    fn rebuild(&mut self, cols: usize, rail_cols: usize) {
        let key = (
            self.store.version,
            format!("{}|{}", self.search.text, self.chips.join(",")),
            cols,
            rail_cols,
            self.selected.len(),
            self.editing,
            self.store.expanded.values().filter(|v| **v).count(),
        );
        if key == self.built {
            return;
        }
        self.built = key;
        let cols = cols.max(1);
        let mut tiles: Vec<TileData> = Vec::new();
        let mut lines: Vec<Line> = Vec::new();
        let push_tiles = |tiles: &mut Vec<TileData>, lines: &mut Vec<Line>, more: Vec<TileData>| {
            let from = tiles.len();
            tiles.extend(more);
            let mut i = from;
            while i < tiles.len() {
                let end = (i + cols).min(tiles.len());
                lines.push(Line::Tiles(i, end));
                i = end;
            }
        };
        let needed = self.store.needed();
        if self.searching() {
            let (text, spec) = self.query();
            lines.push(Line::Heading("Suggestions".into()));
            let more = self.search_tiles(&text, &spec);
            if more.is_empty() {
                lines.push(Line::Hint("Nothing in the catalog.".into()));
            }
            push_tiles(&mut tiles, &mut lines, more);
        } else {
            lines.push(Line::Heading(format!("To buy ({})", needed.len())));
            if needed.is_empty() {
                let hint = if !self.store.loaded {
                    self.store
                        .load_error
                        .clone()
                        .unwrap_or_else(|| "Loading your list…".into())
                } else {
                    "Nothing to buy! Tap an item below or search for it to add it. Lucina can do it for you, too.".into()
                };
                lines.push(Line::Hint(hint));
            } else if self.store.grouped {
                // `groups`: the same tiles with a header per section.
                let mut i = 0;
                while i < needed.len() {
                    let sec = self.store.walk_section(&needed[i]);
                    let mut j = i;
                    while j < needed.len() && self.store.walk_section(&needed[j]) == sec {
                        j += 1;
                    }
                    let name = self
                        .store
                        .catalog
                        .sections
                        .iter()
                        .find(|s| s.id == sec)
                        .map_or("Other".to_owned(), |s| s.name.clone());
                    lines.push(Line::Hint(name));
                    let more: Vec<TileData> =
                        needed[i..j].iter().map(|r| self.row_tile(r)).collect();
                    push_tiles(&mut tiles, &mut lines, more);
                    i = j;
                }
            } else {
                let more: Vec<TileData> = needed.iter().map(|r| self.row_tile(r)).collect();
                push_tiles(&mut tiles, &mut lines, more);
            }
            // The catalogue, in store-walk order.
            lines.push(Line::Heading("Catalog".into()));
            let mut counts: std::collections::HashMap<String, usize> = Default::default();
            for r in &needed {
                *counts.entry(self.store.walk_section(r)).or_default() += 1;
            }
            let by_key: std::collections::HashMap<String, &ShopRow> = self
                .store
                .items
                .iter()
                .map(|r| (item_key(r.catalog_id.as_deref(), &r.name), r))
                .collect();
            for sec in self.store.ordered_sections() {
                let items: Vec<(usize, &model::CatalogItem)> = self
                    .store
                    .catalog
                    .items
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| c.section.as_deref().unwrap_or(OTHER_SECTION) == sec.id)
                    .collect();
                if items.is_empty() {
                    continue;
                }
                let open = self.store.expanded.get(&sec.id).copied().unwrap_or(false);
                lines.push(Line::Section {
                    id: sec.id.clone(),
                    name: sec.name.clone(),
                    open,
                    count: counts.get(&sec.id).copied().unwrap_or(0),
                });
                if open {
                    let more: Vec<TileData> = items
                        .iter()
                        .map(|(at, c)| {
                            let name = catalog_name(c, &self.store.item_lang).to_owned();
                            let row = by_key.get(&item_key(Some(&c.id), &name));
                            let on = row.is_some_and(|r| r.status == Status::Needed);
                            TileData {
                                key: format!("catalog:{}", c.id),
                                kind: if on {
                                    TileKind::Need
                                } else {
                                    TileKind::Catalog
                                },
                                name,
                                spec: if on {
                                    row.map_or(String::new(), |r| r.spec.clone())
                                } else {
                                    String::new()
                                },
                                tap: match row.filter(|r| r.status == Status::Needed) {
                                    Some(r) => Tap::Row(r.id.clone()),
                                    None => Tap::Catalog(*at),
                                },
                                details: None,
                                checked: None,
                            }
                        })
                        .collect();
                    push_tiles(&mut tiles, &mut lines, more);
                }
            }
        }
        self.tiles = tiles;
        self.lines = lines;
        self.list.invalidate();
        self.rail_tiles = self
            .store
            .recent()
            .iter()
            .map(|r| self.row_tile(r))
            .collect();
        self.find_art();

        self.rail_lines.clear();
        let rail_cols = rail_cols.max(1);
        let mut i = 0;
        while i < self.rail_tiles.len() {
            let end = (i + rail_cols).min(self.rail_tiles.len());
            self.rail_lines.push((i, end));
            i = end;
        }
        self.rail.invalidate();
        if self.sel.is_some_and(|s| s >= self.tiles.len()) {
            self.sel = self.tiles.len().checked_sub(1);
        }
    }

    /// `itemVisual`, as far as pictures go: a tile's catalog entry, by id, or by its row's name as the server resolves it.
    fn art_key(&self, d: &TileData) -> Option<String> {
        let s = &self.store;
        let at = match &d.tap {
            Tap::Catalog(at) | Tap::Found(at) => Some(*at),
            Tap::Row(id) | Tap::Select(id) => {
                let row = s.items.iter().find(|r| r.id == *id)?;
                match &row.catalog_id {
                    Some(c) => return Some(c.clone()),
                    None => s.index.resolve(&row.name),
                }
            }
            Tap::History(_, name) => s.index.resolve(name),
            Tap::Custom => s.index.resolve(&d.name),
        }?;
        s.catalog.items.get(at).map(|c| c.id.clone())
    }

    /// Which pictures the tiles need; those not decoded yet go to a thread, and until they arrive a tile shows its letter.
    fn find_art(&mut self) {
        let mut map = HashMap::new();
        for d in self.tiles.iter().chain(&self.rail_tiles) {
            if let Some(k) = self
                .art_key(d)
                .filter(|k| self.art.contains_key(k.as_str()))
            {
                map.insert(d.key.clone(), k);
            }
        }
        let wanted: Vec<(String, &'static [u8])> = map
            .values()
            .filter(|k| self.asked_art.insert((*k).clone()))
            .map(|k| (k.clone(), self.art[k.as_str()]))
            .collect();
        self.tile_art = map;
        if wanted.is_empty() {
            return;
        }
        let px = self.art_px;
        let decode = move || -> Vec<(String, Image)> {
            wanted
                .into_iter()
                .filter_map(|(k, png)| Some((k, Image::decode(png).ok()?.fit_within(px, px))))
                .collect()
        };
        match &self.tx {
            Some(tx) => tx.spawn(move || Msg::Art(decode())),
            None => self.images.extend(decode()),
        }
    }

    /// `SearchResults`: a custom tile, the owner's own items, then the catalog.
    fn search_tiles(&self, text: &str, spec: &str) -> Vec<TileData> {
        if text.is_empty() {
            return Vec::new();
        }
        let s = &self.store;
        let q = fold(text);
        let recent = s.recent();
        let rank: std::collections::HashMap<&str, i32> = recent
            .iter()
            .enumerate()
            .filter_map(|(i, r)| Some((r.catalog_id.as_deref()?, 100 - i as i32)))
            .collect();
        let found = s.index.search(text, 40, &|at| {
            s.catalog
                .items
                .get(at)
                .and_then(|c| rank.get(c.id.as_str()))
                .copied()
                .unwrap_or(0)
        });
        let mut history: Vec<&ShopRow> = s
            .items
            .iter()
            .filter(|r| r.catalog_id.is_none() && fold(&r.name).contains(&q))
            .collect();
        history.sort_by(|a, b| {
            fold(&b.name)
                .starts_with(&q)
                .cmp(&fold(&a.name).starts_with(&q))
                .then_with(|| a.name.cmp(&b.name))
        });
        history.truncate(12);
        let exact = s.index.exact(text).is_some();
        let exact_row = history.iter().any(|r| fold(&r.name) == q);
        let mut out = Vec::new();
        if !exact && !exact_row {
            out.push(TileData {
                key: format!("custom:{text}"),
                kind: TileKind::Custom,
                name: text.to_owned(),
                spec: spec.to_owned(),
                tap: Tap::Custom,
                details: None,
                checked: None,
            });
        }
        for r in history {
            let on = r.status == Status::Needed;
            out.push(TileData {
                key: format!("hist:{}", r.id),
                kind: if on { TileKind::Need } else { TileKind::Recent },
                name: r.name.clone(),
                spec: if spec.is_empty() {
                    r.spec.clone()
                } else {
                    spec.to_owned()
                },
                tap: Tap::History(r.id.clone(), r.name.clone()),
                details: None,
                checked: None,
            });
        }
        let by_key: std::collections::HashMap<String, &ShopRow> = s
            .items
            .iter()
            .map(|r| (item_key(r.catalog_id.as_deref(), &r.name), r))
            .collect();
        for at in found {
            let c = &s.catalog.items[at];
            let name = catalog_name(c, &s.item_lang).to_owned();
            let row = by_key.get(&item_key(Some(&c.id), &name));
            let on = row.is_some_and(|r| r.status == Status::Needed);
            out.push(TileData {
                key: format!("cat:{}", c.id),
                kind: if on {
                    TileKind::Need
                } else {
                    TileKind::Catalog
                },
                name,
                spec: if !spec.is_empty() {
                    spec.to_owned()
                } else if on {
                    row.map_or(String::new(), |r| r.spec.clone())
                } else {
                    String::new()
                },
                tap: Tap::Found(at),
                details: None,
                checked: None,
            });
        }
        out
    }

    /// `SearchField`'s `addTyped`: the exact catalog item or the owner's own, the only suggestion, else a new custom item.
    pub fn add_typed(&mut self, ui: &mut Ui) {
        let (text, spec) = self.query();
        if text.is_empty() {
            return;
        }
        let spec = (!spec.is_empty()).then_some(spec);
        let s = &self.store;
        let q = fold(&text);
        let history: Vec<&ShopRow> = s
            .items
            .iter()
            .filter(|r| r.catalog_id.is_none() && fold(&r.name).contains(&q))
            .collect();
        let exact_row = history
            .iter()
            .find(|r| fold(&r.name) == q)
            .map(|r| r.name.clone());
        let found = s.index.search(&text, 40, &|_| 0);
        let only = (found.len() == 1 && history.is_empty()).then(|| found[0]);
        let cat = s
            .index
            .exact(&text)
            .or(if exact_row.is_some() { None } else { only });
        let name = exact_row.unwrap_or_else(|| text.clone());
        self.clear_search();
        match cat {
            Some(at) => {
                self.store.add_catalog(at, spec);
            }
            None => {
                self.store.add_text(&name, spec);
            }
        }
        ui.focus("search");
    }

    fn clear_search(&mut self) {
        self.search.clear();
        self.chips.clear();
    }

    fn tap(&mut self, tap: Tap, ui: &mut Ui) {
        let (text, spec) = self.query();
        let spec_opt = (!spec.is_empty()).then_some(spec);
        match tap {
            Tap::Row(id) => self.store.tap_item(&id),
            Tap::Catalog(at) => {
                self.store.add_catalog(at, None);
            }
            Tap::Custom => {
                self.clear_search();
                self.store.add_text(&text, spec_opt);
                ui.focus("search");
            }
            Tap::History(id, name) => {
                self.clear_search();
                let on = self
                    .store
                    .items
                    .iter()
                    .any(|r| r.id == id && r.status == Status::Needed);
                if on && spec_opt.is_none() {
                    self.store.tap_item(&id);
                } else {
                    self.store.add_text(&name, spec_opt);
                }
                ui.focus("search");
            }
            Tap::Found(at) => {
                self.clear_search();
                let name = self
                    .store
                    .catalog
                    .items
                    .get(at)
                    .map(|c| catalog_name(c, &self.store.item_lang).to_owned());
                let row = name.and_then(|n| {
                    let c = &self.store.catalog.items[at];
                    self.store
                        .items
                        .iter()
                        .find(|r| model::same_row(r, Some(&c.id), &n))
                        .cloned()
                });
                match row {
                    Some(r) if r.status == Status::Needed && spec_opt.is_none() => {
                        self.store.tap_item(&r.id)
                    }
                    _ => {
                        self.store.add_catalog(at, spec_opt);
                    }
                }
                ui.focus("search");
            }
            Tap::Select(id) => {
                if let Some(p) = self.selected.iter().position(|x| *x == id) {
                    self.selected.remove(p);
                } else {
                    self.selected.push(id);
                }
            }
        }
    }

    pub fn open_details(&mut self, id: &str) {
        let Some(r) = self.store.items.iter().find(|r| r.id == id).cloned() else {
            return;
        };
        let qty = r.quantity.map_or(String::new(), |q| format!("{q}"));
        self.dialog = Some(Dialog::Details(Details {
            id: id.to_owned(),
            name: TextEdit::with_text(&r.name),
            spec: TextEdit::with_text(&r.spec).placeholder("Add a specification"),
            quantity: TextEdit::with_text(&qty).placeholder("Quantity"),
            unit: TextEdit::with_text(r.unit.as_deref().unwrap_or("")).placeholder("Unit"),
            section: Some(self.store.item_section(&r)),
            picking_section: false,
            picking_list: false,
            confirm: false,
            sections: ListState::new(),
        }));
    }

    fn draw_head(&mut self, ui: &mut Ui) {
        let t = ui.theme();
        let row = ui.cut_top(t.px(34.0));
        let gap = t.px(8.0);
        let name = self.store.current_list_name();
        let label = format!("{} ▾", if name.is_empty() { "List" } else { &name });
        let w = ui.measure(Face::Bold, t.pt(15.0), &label) + 2 * t.px(14.0);
        if ui.button_at(Rect::new(row.x, row.y, w, row.h), "lists", &label) {
            self.dialog = Some(Dialog::Lists(ListsDialog {
                list: ListState::new(),
                new_name: TextEdit::new().placeholder("List name"),
                theme: 0,
                editing: None,
                edit_name: TextEdit::new(),
                edit_theme: 0,
                confirm_delete: false,
            }));
        }
        let mut right = row.right();
        let mut take = |ui: &mut Ui, label: &str, id: &str| -> bool {
            let bw = ui.measure(Face::Sans, t.pt(15.0), label) + 2 * t.px(12.0);
            right -= bw;
            let r = Rect::new(right, row.y, bw, row.h);
            right -= gap;
            ui.button_at(r, id, label)
        };
        if take(ui, "•••", "menu") {
            self.dialog = Some(Dialog::Menu);
        }
        if take(ui, "Store order", "order") {
            self.dialog = Some(Dialog::Order(ListState::new()));
        }
        let needed = self
            .store
            .items
            .iter()
            .filter(|r| r.status == Status::Needed)
            .count();
        let mut context = format!("{needed} to buy");
        let (mut c, s) = (t.dim, &self.store);
        if s.signed_out {
            context.push_str(" · Not signed in");
            c = t.danger;
        } else if !s.online {
            context.push_str(&format!(" · Offline: {} changes waiting", s.pending()));
            c = t.danger;
        } else if s.pending() > 0 {
            context.push_str(" · Syncing…");
        }
        if s.loaded && s.load_error.is_some() {
            context.push_str(" · Showing the last copy.");
        }
        let x = row.x + w + t.px(14.0);
        ui.text_in(
            Rect::new(x, row.y, right - x, row.h),
            Face::Sans,
            t.pt(14.0),
            c,
            &context,
        );
        ui.space(8.0);
    }

    fn draw_search(&mut self, ui: &mut Ui) {
        let t = ui.theme();
        let row = ui.cut_top(t.px(34.0));
        let add = t.px(70.0);
        let f = Rect::new(row.x, row.y, row.w - add - t.px(8.0), row.h);
        // With nothing typed, Left and Right have no use in the field: they walk the tiles.
        let walk = self.search_focused
            && self.search.text.is_empty()
            && ui.press().is_some_and(|p| {
                matches!(p.code, keys::KEY_LEFT | keys::KEY_RIGHT) && !p.mods.ctrl && !p.mods.shift
            });
        let mut walked = None;
        if walk {
            walked = ui.press();
            ui.take_press();
        }
        // The keyboard belongs here: when the app starts, and after a dialog, for the passes it takes the screen behind it to
        // answer again. Asked before the field, so that the frame already shows it.
        if self.want_search > 0 {
            self.want_search -= 1;
            ui.focus("search");
        }
        let res = ui.text_field_at(f, "search", &mut self.search);
        self.search_focused = res.focused;
        if let Some(p) = walked {
            self.walk(if p.code == keys::KEY_RIGHT { 1 } else { -1 });
        }
        let mut submit = res.submitted && self.searching();
        submit |= ui.button_at(
            Rect::new(f.right() + t.px(8.0), row.y, add, row.h),
            "add",
            "Add",
        );
        if submit {
            self.add_typed(ui);
        } else if res.submitted
            && let Some(s) = self.sel
        {
            // Enter with nothing typed: the current tile, as a tap.
            if let Some(tile) = self.tiles.get(s) {
                let tap = tile.tap.clone();
                self.tap(tap, ui);
            }
        }
        if self.searching() {
            ui.space(6.0);
            ui.row(34.0, |ui| {
                for c in SPEC_CHIPS_EN {
                    let on = self.chips.iter().any(|x| x == c);
                    let clicked = if on {
                        ui.primary_button(c)
                    } else {
                        ui.button_id(("chip", model_hash(c)), c)
                    };
                    if clicked {
                        if on {
                            self.chips.retain(|x| x != c);
                        } else {
                            self.chips.push(c.to_owned());
                        }
                    }
                }
            });
        }
        ui.space(6.0);
    }

    fn draw_tiles_list(&mut self, ui: &mut Ui, r: Rect, cols: usize) {
        let t = ui.theme();
        let gap = t.px(TILE_GAP);
        let w = (r.w - t.px(8.0) - gap * (cols as i32 - 1)) / cols.max(1) as i32;
        let (band, head, hint) = (t.px(TILE_H) + gap, t.px(32.0), t.px(30.0));
        let lines = std::mem::take(&mut self.lines);
        let tiles = std::mem::take(&mut self.tiles);
        let (tile_art, images) = (&self.tile_art, &self.images);
        let sel = self.sel;
        let (mut tap, mut details, mut toggle) = (None, None, None);
        let (mut rects, mut corners) = (vec![None; tiles.len()], vec![None; tiles.len()]);
        ui.list_measured(
            "tiles",
            &mut self.list,
            r,
            lines.len(),
            |_, i, _| match &lines[i] {
                Line::Heading(_) => head,
                Line::Hint(_) => hint,
                Line::Section { .. } => t.px(40.0),
                Line::Tiles(..) => band,
            },
            |ui, i, _| {
                let a = ui.area();
                ui.fill(a, t.bg);
                match &lines[i] {
                    Line::Heading(s) => ui.text_in(
                        Rect::new(a.x + t.px(2.0), a.y, a.w, a.h),
                        Face::Bold,
                        t.pt(14.0),
                        t.text,
                        s,
                    ),
                    Line::Hint(s) => ui.text_in(
                        Rect::new(a.x + t.px(2.0), a.y, a.w, a.h),
                        Face::Sans,
                        t.pt(13.0),
                        t.dim,
                        s,
                    ),
                    Line::Section {
                        id,
                        name,
                        open,
                        count,
                    } => {
                        let b = Rect::new(a.x, a.y + t.px(3.0), a.w, a.h - t.px(6.0));
                        ui.fill_round(b, t.px(6.0), t.panel);
                        if ui.hit(("section", model_hash(id)), b).clicked {
                            toggle = Some(id.clone());
                        }
                        let label = format!("{} {name}", if *open { "▾" } else { "▸" });
                        ui.text_in(
                            Rect::new(b.x + t.px(10.0), b.y, b.w - t.px(60.0), b.h),
                            Face::Sans,
                            t.pt(14.0),
                            t.text,
                            &label,
                        );
                        if *count > 0 {
                            let s = count.to_string();
                            let cw = ui.measure(Face::Bold, t.pt(13.0), &s);
                            ui.text_in(
                                Rect::new(b.right() - cw - t.px(12.0), b.y, cw + 2, b.h),
                                Face::Bold,
                                t.pt(13.0),
                                t.accent,
                                &s,
                            );
                        }
                    }
                    Line::Tiles(from, to) => {
                        for k in *from..*to {
                            let x = a.x + (k - from) as i32 * (w + gap);
                            let rect = Rect::new(x, a.y, w, a.h - gap);
                            rects[k] = Some(rect);
                            corners[k] = tiles[k].details.is_some().then(|| tile_corner(&t, rect));
                            let (tapped, more) = tile(
                                ui,
                                &tiles[k],
                                tile_art.get(&tiles[k].key).and_then(|c| images.get(c)),
                                rect,
                                sel == Some(k),
                            );
                            if tapped {
                                tap = Some(tiles[k].tap.clone());
                            }
                            if more {
                                details.clone_from(&tiles[k].details);
                            }
                        }
                    }
                }
            },
        );
        self.lines = lines;
        self.tiles = tiles;
        self.tile_rects = rects;
        self.tile_corners = corners;
        if let Some(id) = toggle {
            let v = self.store.expanded.entry(id).or_insert(false);
            *v = !*v;
            self.store.dirty = true;
        }
        if let Some(x) = tap {
            self.tap(x, ui);
        }
        if let Some(id) = details {
            self.open_details(&id);
        }
    }

    fn draw_rail(&mut self, ui: &mut Ui, r: Rect, cols: usize) {
        let t = ui.theme();
        ui.fill_round(r, t.px(8.0), t.bar);
        let inner = r.inset(t.px(10.0));
        let (mut tap, mut details) = (None, None);
        let (mut edit, mut clear, mut cancel, mut remove) = (false, false, false, false);
        let n = self.selected.len();
        let rest = ui.within(inner, |ui| {
            ui.heading("Recently used");
            if self.rail_tiles.is_empty() {
                ui.dim("What you buy shows up here.");
                return ui.area();
            }
            ui.row(34.0, |ui| {
                if self.editing {
                    cancel = ui.button_id("rec-cancel", "Cancel");
                    remove = ui.button_id(
                        "rec-remove",
                        &if n > 0 {
                            format!("Remove ({n})")
                        } else {
                            "Remove".to_owned()
                        },
                    );
                } else {
                    edit = ui.button_id("rec-edit", "Edit");
                    clear = ui.button_id("rec-clear", "Clear all");
                }
            });
            ui.area()
        });
        if !self.rail_tiles.is_empty() {
            let gap = t.px(TILE_GAP);
            let w = (rest.w - t.px(8.0) - gap * (cols as i32 - 1)) / cols.max(1) as i32;
            let band = t.px(TILE_H) + gap;
            let (lines, tiles) = (&self.rail_lines, &self.rail_tiles);
            let (tile_art, images) = (&self.tile_art, &self.images);
            ui.list_measured(
                "recent",
                &mut self.rail,
                rest,
                lines.len(),
                |_, _, _| band,
                |ui, i, _| {
                    let a = ui.area();
                    ui.fill(a, t.bar);
                    let (from, to) = lines[i];
                    for (col, d) in tiles[from..to].iter().enumerate() {
                        let rect = Rect::new(a.x + col as i32 * (w + gap), a.y, w, a.h - gap);
                        let (tapped, more) = tile(
                            ui,
                            d,
                            tile_art.get(&d.key).and_then(|c| images.get(c)),
                            rect,
                            false,
                        );
                        if tapped {
                            tap = Some(d.tap.clone());
                        }
                        if more {
                            details.clone_from(&d.details);
                        }
                    }
                },
            );
        }
        if edit {
            self.editing = true;
            self.selected.clear();
        }
        if cancel {
            self.editing = false;
            self.selected.clear();
        }
        if remove {
            let ids = std::mem::take(&mut self.selected);
            self.store.remove_items(&ids);
            self.editing = false;
        }
        if clear {
            self.store.clear_recent();
        }
        if let Some(x) = tap {
            self.tap(x, ui);
        }
        if let Some(id) = details {
            self.open_details(&id);
        }
    }

    fn draw_snack(&mut self, ui: &mut Ui, r: Rect) {
        let t = ui.theme();
        let message = self.store.action_error.clone();
        let undo = self
            .store
            .undo
            .as_ref()
            .map(|u| (u.message.clone(), u.inverse.is_some()));
        let (text, has_undo, is_error) = match (message, undo) {
            (Some(m), _) => (m, false, true),
            (None, Some((m, i))) => (m, i, false),
            _ => return,
        };
        let w = t.px(440.0).min(r.w);
        let bar = Rect::new(r.x, r.bottom() - t.px(44.0), w, t.px(44.0));
        ui.fill_round(
            bar,
            t.px(8.0),
            if is_error { t.danger } else { t.selection },
        );
        let close = Rect::new(
            bar.right() - t.px(36.0),
            bar.y + t.px(6.0),
            t.px(30.0),
            t.px(32.0),
        );
        if ui.button_at(close, "snack-close", "×") {
            if is_error {
                self.store.action_error = None;
            } else {
                self.store.dismiss_undo();
            }
        }
        let mut right = close.x - t.px(4.0);
        if has_undo {
            let b = Rect::new(
                right - t.px(70.0),
                bar.y + t.px(6.0),
                t.px(70.0),
                t.px(32.0),
            );
            right = b.x - t.px(4.0);
            if ui.button_at(b, "snack-undo", "Undo") {
                self.store.undo_last();
            }
        }
        ui.text_in(
            Rect::new(bar.x + t.px(14.0), bar.y, right - bar.x - t.px(14.0), bar.h),
            Face::Sans,
            t.pt(14.0),
            t.text,
            &text,
        );
    }

    fn draw_dialog(&mut self, ui: &mut Ui) {
        let Some(mut d) = self.dialog.take() else {
            return;
        };
        let keep = match &mut d {
            Dialog::Details(x) => self.draw_details(ui, x),
            Dialog::Lists(x) => self.draw_lists(ui, x),
            Dialog::Order(x) => self.draw_order(ui, x),
            Dialog::Menu => self.draw_menu(ui),
        };
        if keep && !ui.key(keys::KEY_ESC) {
            self.dialog = Some(d);
        } else {
            self.want_search = 3;
        }
    }

    fn draw_details(&mut self, ui: &mut Ui, d: &mut Details) -> bool {
        let Some(row) = self.store.items.iter().find(|r| r.id == d.id).cloned() else {
            return false;
        };
        let t = ui.theme();
        let mut keep = true;
        let (mut done, mut del) = (false, false);
        let mut move_to = None;
        let sections: Vec<(String, String)> = {
            let mut v: Vec<(String, String)> = self
                .store
                .ordered_sections()
                .into_iter()
                .map(|s| (s.id, s.name))
                .collect();
            if !v.iter().any(|s| s.0 == OTHER_SECTION) {
                v.push((OTHER_SECTION.to_owned(), "Other".to_owned()));
            }
            v
        };
        let lists: Vec<model::List> = self
            .store
            .lists
            .iter()
            .filter(|l| l.id != self.store.current_list_id)
            .cloned()
            .collect();
        ui.modal(560.0, 620.0, |ui| {
            ui.heading(&row.name);
            ui.dim("Name");
            ui.text_field("d-name", &mut d.name);
            ui.dim("Specification");
            ui.text_field("d-spec", &mut d.spec);
            ui.row(34.0, |ui| {
                for c in SPEC_CHIPS_EN {
                    let on = d
                        .spec
                        .text
                        .split(',')
                        .any(|p| p.trim().eq_ignore_ascii_case(c));
                    if ui.button_id(("d-chip", model_hash(c)), if on { "✓" } else { c }) {
                        // `DetailsSheet`: a chip adds itself, or takes itself out.
                        let mut parts: Vec<String> = d
                            .spec
                            .text
                            .split(',')
                            .map(|p| p.trim().to_owned())
                            .filter(|p| !p.is_empty())
                            .collect();
                        if on {
                            parts.retain(|p| !p.eq_ignore_ascii_case(c));
                        } else {
                            parts.push(c.to_owned());
                        }
                        d.spec.set(&parts.join(", "));
                    }
                }
            });
            ui.row(34.0, |ui| {
                let a = ui.area();
                let half = (a.w - t.px(8.0)) / 2;
                ui.text_field_at(Rect::new(a.x, a.y, half, a.h), "d-qty", &mut d.quantity);
                ui.text_field_at(
                    Rect::new(a.x + half + t.px(8.0), a.y, half, a.h),
                    "d-unit",
                    &mut d.unit,
                );
            });
            let section_name = sections
                .iter()
                .find(|s| Some(&s.0) == d.section.as_ref())
                .map_or("Other", |s| s.1.as_str());
            ui.row(34.0, |ui| {
                if ui.button_id("d-section", &format!("Section: {section_name}")) {
                    d.picking_section = !d.picking_section;
                    d.picking_list = false;
                }
                if !lists.is_empty() && ui.button_id("d-move", "Move item") {
                    d.picking_list = !d.picking_list;
                    d.picking_section = false;
                }
            });
            if d.picking_section {
                let r = ui.cut_top(t.px(130.0));
                let res = ui.list(
                    "d-sections",
                    &mut d.sections,
                    r,
                    sections.len(),
                    26.0,
                    |ui, i, _| {
                        let a = ui.area();
                        ui.text_in(
                            Rect::new(a.x + t.px(6.0), a.y, a.w, a.h),
                            Face::Sans,
                            t.pt(14.0),
                            t.item,
                            &sections[i].1,
                        );
                    },
                );
                if let Some(i) = res.activated {
                    d.section = Some(sections[i].0.clone());
                    d.picking_section = false;
                }
            }
            if d.picking_list {
                let r = ui.cut_top(t.px(130.0));
                let res = ui.list(
                    "d-lists",
                    &mut d.sections,
                    r,
                    lists.len(),
                    26.0,
                    |ui, i, _| {
                        let a = ui.area();
                        ui.text_in(
                            Rect::new(a.x + t.px(6.0), a.y, a.w, a.h),
                            Face::Sans,
                            t.pt(14.0),
                            t.item,
                            &lists[i].name,
                        );
                    },
                );
                if let Some(i) = res.activated {
                    move_to = Some(lists[i].id.clone());
                }
            }
            if !d.picking_section && !d.picking_list {
                let when = relative(&row.updated_at, now_ms());
                let by = source_name(&row.updated_by);
                ui.dim(&format!(
                    "Last change: {when}{}",
                    if by.is_empty() {
                        String::new()
                    } else {
                        format!(", via {by}")
                    }
                ));
            }
            let bottom = ui.cut_bottom(t.px(36.0));
            ui.within(bottom, |ui| {
                ui.row(34.0, |ui| {
                    done = ui.primary_button("Done");
                    if d.confirm {
                        del = ui.button("Delete item");
                        if ui.button("Cancel") {
                            d.confirm = false;
                        }
                    } else if ui.button_id("d-del", "Delete item") {
                        d.confirm = true;
                    }
                });
            });
        });
        if let Some(to) = move_to {
            self.store.move_item(&d.id, &to);
            return false;
        }
        if del {
            self.store.delete_item(&d.id);
            return false;
        }
        if done || ui.press().is_some_and(|p| p.code == keys::KEY_ESC) {
            let q = d
                .quantity
                .text
                .trim()
                .replace(',', ".")
                .parse::<f64>()
                .ok()
                .filter(|q| *q > 0.0);
            self.store.edit_item(
                &d.id,
                &d.name.text,
                &d.spec.text,
                q,
                &d.unit.text,
                d.section.clone(),
            );
            keep = false;
        }
        keep
    }

    fn draw_lists(&mut self, ui: &mut Ui, d: &mut ListsDialog) -> bool {
        let t = ui.theme();
        let lists = self.store.lists.clone();
        let current = self.store.current_list_id.clone();
        let (mut close, mut create, mut pick, mut settings) = (false, false, None, None);
        let (mut save, mut delete, mut back) = (false, false, false);
        ui.modal(560.0, 600.0, |ui| {
            if let Some(id) = &d.editing {
                ui.heading(&format!(
                    "Settings for {}",
                    lists
                        .iter()
                        .find(|l| l.id == *id)
                        .map_or("", |l| l.name.as_str())
                ));
                ui.dim("List name");
                ui.text_field("l-name", &mut d.edit_name);
                ui.dim("Theme");
                ui.row(34.0, |ui| {
                    for (i, (_, label)) in THEMES.iter().enumerate().take(4) {
                        if theme_button(ui, i, label, d.edit_theme == i) {
                            d.edit_theme = i;
                        }
                    }
                });
                ui.row(34.0, |ui| {
                    for (i, (_, label)) in THEMES.iter().enumerate().skip(4) {
                        if theme_button(ui, i, label, d.edit_theme == i) {
                            d.edit_theme = i;
                        }
                    }
                });
                ui.row(34.0, |ui| {
                    save = ui.primary_button("Save");
                    back = ui.button("Cancel");
                });
                if id == DEFAULT_LIST_ID {
                    ui.dim("The main list can't be deleted.");
                } else if d.confirm_delete {
                    ui.paragraph("Delete this list and all its items?");
                    ui.row(34.0, |ui| {
                        delete = ui.button_id("l-del-yes", "Delete list");
                        if ui.button_id("l-del-no", "Keep it") {
                            d.confirm_delete = false;
                        }
                    });
                } else if ui.button_id("l-del", "Delete list") {
                    d.confirm_delete = true;
                }
                return;
            }
            ui.heading("Lists");
            let bottom = ui.cut_bottom(t.px(150.0));
            let r = ui.area();
            let res = ui.list(
                "lists-list",
                &mut d.list,
                r,
                lists.len(),
                46.0,
                |ui, i, _| {
                    let a = ui.area();
                    let l = &lists[i];
                    let bw = t.px(84.0);
                    let b = Rect::new(
                        a.right() - bw - t.px(6.0),
                        a.y + t.px(6.0),
                        bw,
                        a.h - t.px(12.0),
                    );
                    if ui.button_at(b, ("l-set", i), "Settings") {
                        settings = Some(i);
                    }
                    let mark = if l.id == current { "● " } else { "" };
                    ui.text_in(
                        Rect::new(a.x + t.px(8.0), a.y, a.w - bw - t.px(90.0), a.h),
                        Face::Sans,
                        t.pt(15.0),
                        t.text,
                        &format!("{mark}{}", l.name),
                    );
                    let c = format!("{} to buy", l.needed);
                    let cw = ui.measure(Face::Sans, t.pt(12.0), &c);
                    ui.text_in(
                        Rect::new(b.x - cw - t.px(10.0), a.y, cw + 2, a.h),
                        Face::Sans,
                        t.pt(12.0),
                        t.dim,
                        &c,
                    );
                },
            );
            if let Some(i) = res.activated {
                pick = Some(lists[i].id.clone());
            }
            ui.within(bottom, |ui| {
                ui.text_field("new-list", &mut d.new_name);
                ui.row(34.0, |ui| {
                    for (i, (_, label)) in THEMES.iter().enumerate().take(4) {
                        if theme_button(ui, i, label, d.theme == i) {
                            d.theme = i;
                        }
                    }
                });
                ui.row(34.0, |ui| {
                    create = ui.button_id("create-list", "Create list");
                    close = ui.primary_button("Close");
                });
            });
        });
        if let Some(i) = settings {
            let l = &lists[i];
            d.editing = Some(l.id.clone());
            d.edit_name = TextEdit::with_text(&l.name);
            d.edit_theme = THEMES.iter().position(|(k, _)| *k == l.theme).unwrap_or(7);
            d.confirm_delete = false;
        }
        if back {
            d.editing = None;
        }
        if save && let Some(id) = d.editing.take() {
            self.store
                .update_list(&id, Some(&d.edit_name.text), Some(THEMES[d.edit_theme].0));
        }
        if delete && let Some(id) = d.editing.take() {
            self.store.remove_list(&id);
        }
        if create && !d.new_name.text.trim().is_empty() {
            self.store.create_list(&d.new_name.text, THEMES[d.theme].0);
            return false;
        }
        if let Some(id) = pick {
            self.store.select_list(&id);
            return false;
        }
        !close
    }

    fn draw_order(&mut self, ui: &mut Ui, list: &mut ListState) -> bool {
        let t = ui.theme();
        let secs = self.store.ordered_sections();
        let (mut close, mut reset) = (false, false);
        let mut mv = None;
        ui.modal(520.0, 620.0, |ui| {
            ui.heading("Store order");
            ui.paragraph("Put the sections in the order you walk through the store: the to-buy grid follows it.");
            let bottom = ui.cut_bottom(t.px(40.0));
            let r = ui.area();
            ui.list("order-list", list, r, secs.len(), 40.0, |ui, i, _| {
                let a = ui.area();
                let bw = t.px(34.0);
                let y = a.y + t.px(4.0);
                let h = a.h - t.px(8.0);
                if ui.button_at(Rect::new(a.right() - bw - t.px(6.0), y, bw, h), ("down", i), "↓") {
                    mv = Some((secs[i].id.clone(), 1));
                }
                if ui.button_at(Rect::new(a.right() - 2 * bw - t.px(12.0), y, bw, h), ("up", i), "↑") {
                    mv = Some((secs[i].id.clone(), -1));
                }
                ui.text_in(Rect::new(a.x + t.px(8.0), a.y, a.w - 2 * bw - t.px(30.0), a.h), Face::Sans, t.pt(15.0), t.text, &secs[i].name);
            });
            ui.within(bottom, |ui| {
                ui.row(34.0, |ui| {
                    close = ui.primary_button("Done");
                    reset = ui.button("Reset");
                });
            });
        });
        if let Some((id, d)) = mv {
            self.store.move_section(&id, d);
        }
        if reset {
            self.store.reset_section_order();
        }
        !close
    }

    fn draw_menu(&mut self, ui: &mut Ui) -> bool {
        let mut close = false;
        let mut grouped = self.store.grouped;
        let mut lang_pick = None;
        let mut refresh = false;
        let pref = self.store.item_lang_pref.clone();
        ui.modal(460.0, 300.0, |ui| {
            ui.heading("List settings");
            if ui.checkbox("Section headers", &mut grouped) {
                self.store.grouped = grouped;
                self.store.dirty = true;
                self.store.version += 1;
            }
            ui.dim("Item names");
            ui.row(34.0, |ui| {
                let auto = "Same as the list";
                if theme_button(ui, 90, auto, pref.is_none()) {
                    lang_pick = Some(None);
                }
                for (i, l) in LANGS.iter().enumerate() {
                    let label = match *l {
                        "it" => "Italiano",
                        "en" => "English",
                        _ => "Dansk",
                    };
                    if theme_button(ui, 91 + i, label, pref.as_deref() == Some(*l)) {
                        lang_pick = Some(Some((*l).to_owned()));
                    }
                }
            });
            ui.row(34.0, |ui| {
                close = ui.primary_button("Done");
                refresh = ui.button("Refresh");
            });
        });
        if let Some(l) = lang_pick {
            self.store.set_item_lang(l);
        }
        if refresh {
            self.store.refresh();
            self.store.retry_now();
        }
        !close
    }

    /// Keys nothing else wanted. The search field keeps the keyboard; the arrows walk the tiles under it.
    fn keys(&mut self, ui: &mut Ui, cols: usize) {
        if ui.key(keys::KEY_ESC) {
            self.clear_search();
            self.sel = None;
            ui.focus("search");
            return;
        }
        let Some(p) = ui.press() else {
            return;
        };
        const KEY_I: u16 = 23;
        if p.mods.ctrl && p.code == KEY_I {
            ui.take_press();
            if let Some(id) = self
                .sel
                .and_then(|s| self.tiles.get(s))
                .and_then(|t| t.details.clone())
            {
                self.open_details(&id);
            }
            return;
        }
        if p.mods.ctrl || p.mods.alt || p.mods.shift || self.tiles.is_empty() {
            return;
        }
        let cols = cols.max(1) as i64;
        let delta = match p.code {
            keys::KEY_DOWN => cols,
            keys::KEY_UP => -cols,
            keys::KEY_RIGHT => 1,
            keys::KEY_LEFT => -1,
            _ => return,
        };
        ui.take_press();
        self.walk(delta);
    }

    fn walk(&mut self, delta: i64) {
        if self.tiles.is_empty() {
            return;
        }
        let last = self.tiles.len() as i64 - 1;
        let next = match self.sel {
            None => 0,
            Some(s) => (s as i64 + delta).clamp(0, last),
        } as usize;
        self.sel = Some(next);
        if let Some(b) = self
            .lines
            .iter()
            .position(|l| matches!(l, Line::Tiles(f, t) if (*f..*t).contains(&next)))
        {
            self.list.select(b);
        }
    }
}

fn model_hash(s: &str) -> usize {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for b in s.bytes() {
        h = (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    h as usize
}

fn theme_button(ui: &mut Ui, i: usize, label: &str, on: bool) -> bool {
    if on {
        ui.primary_button(label)
    } else {
        ui.button_id(("opt", i), label)
    }
}

/// `relativeTime`.
fn relative(iso: &str, now: i64) -> String {
    let Some(at) = parse_iso(iso) else {
        return String::new();
    };
    let mins = (now - at) / 60_000;
    match mins {
        m if m < 1 => "just now".into(),
        m if m < 60 => format!("{m} min ago"),
        m if m < 1440 => format!("{} h ago", m / 60),
        m => format!("{} d ago", m / 1440),
    }
}

fn source_name(by: &str) -> String {
    match by {
        "telegram" => "Telegram".into(),
        "discord" => "Discord".into(),
        "web" => "the web".into(),
        "ios" => "the phone".into(),
        "alexa" => "Alexa".into(),
        "lucina" => "Lucina".into(),
        s => s.to_owned(),
    }
}

/// Up to two letters for a tile without a picture: DejaVu has no emoji, so the catalog's icons cannot be drawn.
fn monogram(name: &str) -> String {
    name.chars()
        .next()
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_default()
}

/// A tile's details corner: the box `tile` draws the " ⋯" in and hit-tests before the tile itself, so
/// that a click there opens the details instead of tapping the item.
pub(crate) fn tile_corner(t: &Theme, r: Rect) -> Rect {
    Rect::new(
        r.right() - t.px(26.0),
        r.y + t.px(3.0),
        t.px(23.0),
        t.px(20.0),
    )
}

/// A tile, and whether it (or its corner button) was clicked. The corner button is asked first, so that it gets the click.
fn tile(ui: &mut Ui, d: &TileData, art: Option<&Image>, r: Rect, selected: bool) -> (bool, bool) {
    let t = ui.theme();
    let h = model_hash(&d.key);
    let corner = tile_corner(&t, r);
    let more = d.details.is_some() && ui.hit(("more", h), corner).clicked;
    let hit = ui.hit(("tile", h), r);
    // The right button opens the details too, as a right-click does on the web.
    let (tapped, more) = (hit.clicked, more || (hit.secondary && d.details.is_some()));
    if !ui.painting() {
        return (tapped, more);
    }
    let rad = t.px(8.0);
    let fill = fill_of(d.kind, &t);
    let border = t.px(2.0).max(2);
    if d.kind == TileKind::Custom {
        ui.fill_round(r, rad, t.accent);
        ui.fill_round(r.inset(border), rad, t.bg);
    } else if selected {
        ui.fill_round(r, rad, t.text);
        ui.fill_round(r.inset(border), rad, fill);
    } else if hit.hovered {
        ui.fill_round(r, rad, t.selection);
        ui.fill_round(r.inset(border), rad, fill);
    } else {
        ui.fill_round(r.inset(border), rad, fill);
    }
    let ink = if d.kind == TileKind::Need {
        t.bg
    } else {
        t.text
    };
    let sub = if d.kind == TileKind::Need {
        Rgb::hex(0x2a2410)
    } else {
        t.dim
    };
    if let Some(on) = d.checked {
        let s = if on { "☑" } else { "☐" };
        ui.text_in(
            Rect::new(r.x + t.px(8.0), r.y + t.px(4.0), t.px(22.0), t.px(22.0)),
            Face::Sans,
            t.pt(15.0),
            ink,
            s,
        );
    }
    if d.details.is_some() {
        ui.text_in(corner, Face::Sans, t.pt(10.0), sub, " ⋯");
    }
    // The picture's place: a letter in a disc.
    let size = t.px(38.0);
    let disc = Rect::new(r.x + (r.w - size) / 2, r.y + t.px(8.0), size, size);
    if let Some(img) = art {
        ui.image_in(Rect::new(r.x, r.y + t.px(5.0), r.w, t.px(42.0)), img);
    } else {
        let tone = if d.kind == TileKind::Need {
            lucina_app::theme::GROUND.over(fill, 0x30)
        } else {
            Rgb::hex(0xffffff).over(fill, 0x18)
        };
        ui.fill_round(disc, size / 2, tone);
        let m = monogram(&d.name);
        let mw = ui.measure(Face::Bold, t.pt(18.0), &m);
        ui.text_in(
            Rect::new(disc.x + (disc.w - mw) / 2, disc.y, mw + 2, disc.h),
            Face::Bold,
            t.pt(18.0),
            ink,
            &m,
        );
    }
    let line = r.y + t.px(48.0);
    let lh = t.px(19.0);
    centered(
        ui,
        Rect::new(r.x + t.px(4.0), line, r.w - t.px(8.0), lh),
        Face::Bold,
        t.pt(12.5),
        ink,
        &d.name,
    );
    if !d.spec.is_empty() {
        centered(
            ui,
            Rect::new(r.x + t.px(4.0), line + lh, r.w - t.px(8.0), t.px(17.0)),
            Face::Sans,
            t.pt(11.0),
            sub,
            &d.spec,
        );
    }
    (tapped, more)
}

fn centered(ui: &mut Ui, r: Rect, f: Face, px: u16, c: Rgb, s: &str) {
    let w = ui.measure(f, px, s);
    if w >= r.w {
        ui.text_in(r, f, px, c, s);
    } else {
        ui.text_in(Rect::new(r.x + (r.w - w) / 2, r.y, w + 2, r.h), f, px, c, s);
    }
}

impl App for Shopping {
    type Msg = Msg;

    fn message(&mut self, m: Msg) {
        match m {
            Msg::Reply(r) => self.store.on_reply(r),
            Msg::Cache(c) => {
                self.store.start(*c);
                if self.needs_signin {
                    self.store.unauthorized = true;
                    self.ask_sign_in();
                }
            }
            Msg::Changed => {}
            Msg::Art(done) => self.images.extend(done),
            Msg::SignedIn => self.signed_in(),
        }
        self.pump();
    }

    /// The whole state fits in a few kilobytes, so it is written here, inside the half second.
    fn on_quit(&mut self) {
        if self.quit_saved {
            return;
        }
        self.quit_saved = true;
        if let Some(dir) = &self.dir {
            let bytes = self.store.cache_json();
            let _ = write_atomic(&dir.join("cache.json"), &bytes);
        }
    }

    fn view(&mut self, ui: &mut Ui) {
        if let Some(v) = &mut self.signin {
            ui.header("Shopping");
            v.view(ui, "Sign in to see your shopping lists here.");
            return;
        }
        self.tick(ui);
        ui.header("Shopping");
        ui.pad(12.0);
        let t = ui.theme();
        self.draw_head(ui);
        let rail_w = t.px(TILE_W + TILE_GAP) * 3 + t.px(26.0);
        let rail_r = ui.cut_right(rail_w);
        let gutter = t.px(12.0);
        ui.cut_right(gutter);
        self.draw_search(ui);
        let main = ui.area();
        let (tile_w, gap) = (t.px(TILE_W), t.px(TILE_GAP));
        let cols = ((main.w - t.px(8.0) + gap) / (tile_w + gap)).max(1) as usize;
        let rail_cols = 3;
        self.art_px = t.px(40.0).max(16) as u32;
        self.rebuild(cols, rail_cols);
        self.draw_tiles_list(ui, main, cols);
        self.draw_rail(ui, rail_r, rail_cols);
        self.draw_snack(ui, main);
        if self.dialog.is_some() {
            self.draw_dialog(ui);
        } else {
            self.keys(ui, cols);
        }
        self.pump();
    }
}

fn read_cache(dir: &std::path::Path) -> Box<CacheFile> {
    Box::new(
        std::fs::read(dir.join("cache.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default(),
    )
}

/// Draws one scene of the demo to a PPM, on the host, with the fake server behind it.
fn render(path: &str, scene: &str) {
    let (tx, rx) = std::sync::mpsc::channel();
    let tx = std::sync::Mutex::new(tx);
    let app = Shopping::new(
        Some(Box::new(fake::Fake::demo())),
        move |r| {
            let _ = tx.lock().map(|t| t.send(r));
        },
        None,
        None,
    );
    let mut h = Harness::new(app, 1920, 1080);
    h.core.message(Msg::Cache(Box::default()));
    let settle = |h: &mut Harness<Shopping>| {
        while !h.app().store.idle() {
            match rx.recv_timeout(Duration::from_secs(2)) {
                Ok(r) => h.core.message(Msg::Reply(r)),
                Err(_) => break,
            }
        }
        h.core.input(lucina_app::Input::Wheel(0));
        h.core.paint();
    };
    settle(&mut h);
    match scene {
        "search" => h.type_str("milk x2"),
        "open" => {
            h.app().store.expanded.insert("produce".into(), true);
            h.app().store.version += 1;
        }
        "details" => {
            let id = h.app().store.items[0].id.clone();
            h.app().open_details(&id);
        }
        "lists" => {
            h.click(100, 80);
        }
        _ => {}
    }
    settle(&mut h);
    h.core.input(lucina_app::Input::Wheel(0));
    h.core.paint();
    std::fs::write(path, h.ppm()).expect("write the picture");
}

fn main() {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some("--render") {
        let path = args.next().unwrap_or_else(|| "shopping.ppm".into());
        render(&path, &args.next().unwrap_or_default());
        return;
    }
    let demo = std::env::var_os("LUCINA_APP_DEMO").is_some();
    lucina_app::run("lucina-app-shopping", move |tx: Sender<Msg>| {
        let transport: Option<Box<dyn Transport>> = if demo {
            Some(Box::new(fake::Fake::demo()))
        } else {
            net::connect()
        };
        let dir = (!demo).then(|| data_dir("shopping"));
        let t2 = tx.clone();
        let app = Shopping::new(
            transport,
            move |r| t2.send(Msg::Reply(r)),
            dir.clone(),
            Some(tx.clone()),
        );
        tx.spawn(move || match dir {
            Some(d) => Msg::Cache(read_cache(&d)),
            None => Msg::Cache(Box::default()),
        });
        app
    });
}
