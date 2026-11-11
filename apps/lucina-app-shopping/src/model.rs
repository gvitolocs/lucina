//! Shopping v2's rows, ops, catalog and quick specs: `web/src/lib/shoppingOps.ts`, `shoppingCatalog.ts`, `shoppingQuery.ts`,
//! with `docs/shopping-v2.md` for the contract. `apply_ops` is the client's copy of the server's semantics, used to show an
//! edit before it is confirmed and to rebase the outbox. The tests at the end repeat those of `web/src/lib/*.test.ts`.

use std::collections::{HashMap, HashSet};

use lucina_app_support::text::fold_name as fold;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const DEFAULT_LIST_ID: &str = "default";
pub const OTHER_SECTION: &str = "other";
pub const LANGS: [&str; 3] = ["it", "en", "da"];
pub const THEMES: [(&str, &str); 8] = [
    ("home", "Home"),
    ("grocery", "Groceries"),
    ("work", "Work"),
    ("holiday", "Holiday"),
    ("garden", "Garden"),
    ("hardware", "Hardware"),
    ("baby", "Baby"),
    ("other", "Other"),
];
pub const SPEC_CHIPS_EN: [&str; 6] = ["x2", "x6", "large", "small", "organic", "lactose-free"];
const MAX_TEXT: usize = 200;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    #[default]
    Needed,
    Recent,
    Removed,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Row {
    pub id: String,
    pub catalog_id: Option<String>,
    pub name: String,
    pub spec: String,
    pub quantity: Option<f64>,
    pub unit: Option<String>,
    pub section: Option<String>,
    pub icon: Option<String>,
    pub status: Status,
    pub position: f64,
    pub updated_at: String,
    pub updated_by: String,
}

impl Row {
    pub fn active(&self) -> bool {
        self.status != Status::Removed
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct List {
    pub id: String,
    pub name: String,
    pub theme: String,
    pub needed: u32,
    pub recent: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ListState {
    pub list: List,
    pub revision: u64,
    pub items: Vec<Row>,
}

/// An op without its id, as the screen produces it. The field names are the wire's.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum Kind {
    Add {
        /// Client-chosen id for a new row.
        #[serde(skip_serializing_if = "Option::is_none")]
        item_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        catalog_id: Option<String>,
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        spec: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        quantity: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        unit: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        section: Option<String>,
    },
    Need {
        item_id: String,
    },
    Purchase {
        item_id: String,
    },
    Remove {
        item_id: String,
    },
    SetSpec {
        item_id: String,
        spec: String,
    },
    SetQuantity {
        item_id: String,
        quantity: Option<f64>,
        unit: Option<String>,
    },
    SetSection {
        item_id: String,
        section: Option<String>,
    },
    Rename {
        item_id: String,
        name: String,
    },
    Move {
        item_id: String,
        to_list_id: String,
    },
    ClearRecent,
    Reorder {
        item_id: String,
        position: f64,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Op {
    #[serde(rename = "opId")]
    pub op_id: String,
    #[serde(flatten)]
    pub kind: Kind,
}

impl Kind {
    /// `opTarget`.
    pub fn target(&self) -> Option<&str> {
        match self {
            Kind::ClearRecent => None,
            Kind::Add { item_id, .. } => item_id.as_deref(),
            Kind::Need { item_id }
            | Kind::Purchase { item_id }
            | Kind::Remove { item_id }
            | Kind::SetSpec { item_id, .. }
            | Kind::SetQuantity { item_id, .. }
            | Kind::SetSection { item_id, .. }
            | Kind::Rename { item_id, .. }
            | Kind::Move { item_id, .. }
            | Kind::Reorder { item_id, .. } => Some(item_id),
        }
    }

    pub fn retarget(&mut self, to: &str) {
        match self {
            Kind::ClearRecent => {}
            Kind::Add { item_id, .. } => *item_id = Some(to.to_owned()),
            Kind::Need { item_id }
            | Kind::Purchase { item_id }
            | Kind::Remove { item_id }
            | Kind::SetSpec { item_id, .. }
            | Kind::SetQuantity { item_id, .. }
            | Kind::SetSection { item_id, .. }
            | Kind::Rename { item_id, .. }
            | Kind::Move { item_id, .. }
            | Kind::Reorder { item_id, .. } => *item_id = to.to_owned(),
        }
    }
}

fn clip(text: &str) -> String {
    text.trim().chars().take(MAX_TEXT).collect()
}

fn valid_quantity(q: Option<f64>) -> Option<f64> {
    q.filter(|v| v.is_finite() && *v > 0.0)
}

/// `sameRow`: one active row per (list, catalogId-or-folded-name). A row matches when the catalog ids agree, or when the
/// folded names do.
pub fn same_row(row: &Row, catalog_id: Option<&str>, name: &str) -> bool {
    if let Some(c) = catalog_id {
        if row.catalog_id.as_deref() == Some(c) {
            return true;
        }
        if row.catalog_id.is_some() {
            return false;
        }
    }
    !name.is_empty() && fold(&row.name) == fold(name)
}

pub struct Hint {
    pub name: String,
    pub section: Option<String>,
    pub icon: String,
}

pub struct Ctx<'a> {
    pub list_id: &'a str,
    pub now: &'a str,
    pub by: &'a str,
    pub new_id: &'a mut dyn FnMut() -> String,
    pub catalog: &'a dyn Fn(&str) -> Option<Hint>,
}

fn next_position(items: &[Row]) -> f64 {
    items
        .iter()
        .filter(|i| i.status == Status::Needed)
        .fold(0.0, |m: f64, i| m.max(i.position))
        + 1.0
}

/// `applyOne`.
fn apply_one(items: &mut Vec<Row>, op: &Kind, ctx: &mut Ctx) {
    let (now, by) = (ctx.now.to_owned(), ctx.by.to_owned());
    let touch = |r: &mut Row| {
        r.updated_at = now.clone();
        r.updated_by = by.clone();
    };
    let active = |items: &[Row], id: &str| items.iter().position(|i| i.id == id && i.active());
    match op {
        Kind::Add {
            item_id,
            catalog_id,
            name,
            spec,
            quantity,
            unit,
            section,
        } => {
            let hint = catalog_id.as_deref().and_then(|c| (ctx.catalog)(c));
            let source = if name.is_empty() {
                hint.as_ref().map_or("", |h| h.name.as_str())
            } else {
                name
            };
            let clean = clip(source);
            if clean.is_empty() && catalog_id.is_none() {
                return;
            }
            let existing = item_id
                .as_deref()
                .and_then(|i| active(items, i))
                .or_else(|| {
                    items
                        .iter()
                        .position(|i| i.active() && same_row(i, catalog_id.as_deref(), &clean))
                });
            if let Some(at) = existing {
                if items[at].status == Status::Recent {
                    let p = next_position(items);
                    items[at].position = p;
                    items[at].status = Status::Needed;
                }
                let r = &mut items[at];
                if let Some(s) = spec {
                    r.spec = clip(s);
                }
                if quantity.is_some() {
                    r.quantity = valid_quantity(*quantity);
                }
                if let Some(u) = unit {
                    r.unit = (!u.is_empty()).then(|| clip(u));
                }
                if section.is_some() {
                    r.section = section.clone();
                }
                touch(r);
                return;
            }
            // A removed row keeps its id forever, so a reused id gets a fresh one.
            let id = match item_id {
                Some(i) if !items.iter().any(|x| x.id == *i) => i.clone(),
                _ => (ctx.new_id)(),
            };
            let position = next_position(items);
            items.push(Row {
                id,
                catalog_id: catalog_id.clone(),
                name: if clean.is_empty() {
                    catalog_id.clone().unwrap_or_default()
                } else {
                    clean
                },
                spec: spec.as_deref().map(clip).unwrap_or_default(),
                quantity: valid_quantity(*quantity),
                unit: unit.as_deref().filter(|u| !u.is_empty()).map(clip),
                section: section
                    .clone()
                    .or_else(|| hint.as_ref().and_then(|h| h.section.clone())),
                icon: hint.map(|h| h.icon),
                status: Status::Needed,
                position,
                updated_at: now.clone(),
                updated_by: by.clone(),
            });
        }
        Kind::Need { item_id } => {
            if let Some(at) = active(items, item_id).filter(|&a| items[a].status != Status::Needed)
            {
                let p = next_position(items);
                let r = &mut items[at];
                r.position = p;
                r.status = Status::Needed;
                touch(r);
            }
        }
        Kind::Purchase { item_id } => {
            if let Some(at) = active(items, item_id).filter(|&a| items[a].status != Status::Recent)
            {
                items[at].status = Status::Recent;
                touch(&mut items[at]);
            }
        }
        Kind::Remove { item_id } => {
            if let Some(at) = active(items, item_id) {
                items[at].status = Status::Removed;
                touch(&mut items[at]);
            }
        }
        Kind::Move {
            item_id,
            to_list_id,
        } => {
            if to_list_id != ctx.list_id
                && let Some(at) = active(items, item_id)
            {
                items[at].status = Status::Removed;
                touch(&mut items[at]);
            }
        }
        Kind::SetSpec { item_id, spec } => {
            if let Some(at) = active(items, item_id) {
                items[at].spec = clip(spec);
                touch(&mut items[at]);
            }
        }
        Kind::SetQuantity {
            item_id,
            quantity,
            unit,
        } => {
            if let Some(at) = active(items, item_id) {
                items[at].quantity = valid_quantity(*quantity);
                items[at].unit = unit.as_deref().filter(|u| !u.is_empty()).map(clip);
                touch(&mut items[at]);
            }
        }
        Kind::SetSection { item_id, section } => {
            if let Some(at) = active(items, item_id) {
                items[at].section = section.clone();
                touch(&mut items[at]);
            }
        }
        Kind::Rename { item_id, name } => {
            let clean = clip(name);
            if let Some(at) = active(items, item_id).filter(|_| !clean.is_empty()) {
                items[at].name = clean;
                touch(&mut items[at]);
            }
        }
        Kind::Reorder { item_id, position } => {
            if let Some(at) = active(items, item_id).filter(|_| position.is_finite()) {
                items[at].position = *position;
                touch(&mut items[at]);
            }
        }
        Kind::ClearRecent => {
            for r in items.iter_mut().filter(|r| r.status == Status::Recent) {
                r.status = Status::Removed;
                touch(r);
            }
        }
    }
}

/// `applyOps`: in order, on a copy. Ops whose id was seen are reported as duplicates and change nothing.
pub fn apply_ops(
    items: &[Row],
    ops: &[Op],
    seen: &mut HashSet<String>,
    ctx: &mut Ctx,
) -> (Vec<Row>, Vec<String>, Vec<String>) {
    let mut out = items.to_vec();
    let (mut applied, mut duplicates) = (Vec::new(), Vec::new());
    for op in ops {
        if !seen.insert(op.op_id.clone()) {
            duplicates.push(op.op_id.clone());
            continue;
        }
        apply_one(&mut out, &op.kind, ctx);
        applied.push(op.op_id.clone());
    }
    (out, applied, duplicates)
}

fn signature(items: &[Row]) -> Vec<Row> {
    items
        .iter()
        .map(|r| Row {
            updated_at: String::new(),
            updated_by: String::new(),
            ..r.clone()
        })
        .collect()
}

/// `rebaseOps`, after a 409: every op gets a new id; ops on rows that no longer exist, and ops that would change nothing any
/// more, are dropped.
pub fn rebase_ops(fresh: &[Row], pending: &[Op], ctx: &mut Ctx) -> Vec<Op> {
    let mut items = fresh.to_vec();
    let mut out = Vec::new();
    for op in pending {
        let target = op.kind.target();
        if !matches!(op.kind, Kind::Add { .. })
            && let Some(t) = target
            && !items.iter().any(|i| i.id == t && i.active())
        {
            continue;
        }
        let next = Op {
            op_id: (ctx.new_id)(),
            kind: op.kind.clone(),
        };
        let before = signature(&items);
        let (res, _, _) = apply_ops(
            &items,
            std::slice::from_ref(&next),
            &mut HashSet::new(),
            ctx,
        );
        if signature(&res) == before {
            continue;
        }
        items = res;
        out.push(next);
    }
    out
}

/// `parseItem`: rows without an id or a name are dropped; an unknown status counts as `needed`, as on the web.
pub fn parse_row(v: &Value, index: usize) -> Option<Row> {
    let id = match &v["id"] {
        Value::String(s) if !s.trim().is_empty() => s.trim().to_owned(),
        Value::Number(n) => n.to_string(),
        _ => return None,
    };
    let text = |k: &str| {
        v[k].as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    Some(Row {
        id,
        catalog_id: text("catalogId"),
        name: text("name")?,
        spec: v["spec"].as_str().unwrap_or("").to_owned(),
        quantity: valid_quantity(v["quantity"].as_f64()),
        unit: text("unit"),
        section: text("section"),
        icon: text("icon"),
        status: match v["status"].as_str() {
            Some("recent") => Status::Recent,
            Some("removed") => Status::Removed,
            _ => Status::Needed,
        },
        position: v["position"].as_f64().unwrap_or(index as f64),
        updated_at: text("updatedAt").unwrap_or_default(),
        updated_by: text("updatedBy").unwrap_or_default(),
    })
}

pub fn parse_rows(v: &Value) -> Vec<Row> {
    v.as_array()
        .map(|a| {
            a.iter()
                .enumerate()
                .filter_map(|(i, r)| parse_row(r, i))
                .collect()
        })
        .unwrap_or_default()
}

/// `parseList`.
pub fn parse_list(v: &Value) -> Option<List> {
    let id = v["id"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())?
        .to_owned();
    let theme = v["theme"]
        .as_str()
        .filter(|t| THEMES.iter().any(|(k, _)| k == t))
        .unwrap_or("other");
    Some(List {
        name: v["name"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(&id)
            .to_owned(),
        id,
        theme: theme.to_owned(),
        needed: v["needed"].as_u64().unwrap_or(0) as u32,
        recent: v["recent"].as_u64().unwrap_or(0) as u32,
    })
}

/// `parseLists`: an array, or Lucina's `{lists}`.
pub fn parse_lists(v: &Value) -> Vec<List> {
    let arr = v.as_array().or_else(|| v["lists"].as_array());
    arr.map(|a| a.iter().filter_map(parse_list).collect())
        .unwrap_or_default()
}

/// `parseListState`.
pub fn parse_list_state(v: &Value, fallback: &str) -> ListState {
    let list = parse_list(&v["list"]).unwrap_or(List {
        id: fallback.to_owned(),
        name: fallback.to_owned(),
        theme: "other".into(),
        ..List::default()
    });
    ListState {
        list,
        revision: v["revision"].as_u64().unwrap_or(0),
        items: parse_rows(&v["items"]),
    }
}

// ---------------------------------------------------------------- the catalog

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Section {
    pub id: String,
    pub name: String,
    pub icon: String,
    pub order: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CatalogItem {
    pub id: String,
    pub name: String,
    pub names: HashMap<String, String>,
    pub aliases: Vec<String>,
    pub section: Option<String>,
    pub icon: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Catalog {
    pub version: u32,
    pub lang: String,
    pub sections: Vec<Section>,
    pub items: Vec<CatalogItem>,
}

/// `parseCatalog`: broken entries are dropped; sections sort by their order.
pub fn parse_catalog(v: &Value) -> Catalog {
    let mut sections: Vec<Section> = v["sections"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(i, s)| {
            let id = s["id"]
                .as_str()
                .filter(|x| !x.trim().is_empty())?
                .to_owned();
            Some(Section {
                name: s["name"].as_str().unwrap_or(&id).to_owned(),
                icon: s["icon"].as_str().unwrap_or("").to_owned(),
                order: s["order"].as_f64().unwrap_or(i as f64 + 1.0),
                id,
            })
        })
        .collect();
    sections.sort_by(|a, b| a.order.total_cmp(&b.order));
    let items = v["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|i| {
            let id = i["id"]
                .as_str()
                .filter(|x| !x.trim().is_empty())?
                .to_owned();
            let name = i["name"]
                .as_str()
                .filter(|x| !x.trim().is_empty())?
                .to_owned();
            Some(CatalogItem {
                id,
                name,
                names: i["names"]
                    .as_object()
                    .map(|m| {
                        m.iter()
                            .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
                            .collect()
                    })
                    .unwrap_or_default(),
                aliases: i["aliases"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default(),
                section: i["section"].as_str().map(str::to_owned),
                icon: i["icon"].as_str().unwrap_or("").to_owned(),
            })
        })
        .collect();
    Catalog {
        version: v["version"].as_u64().unwrap_or(1) as u32,
        lang: v["lang"].as_str().unwrap_or("it").to_owned(),
        sections,
        items,
    }
}

/// `itemKey`: a row's identity: its catalog id, else its folded name.
pub fn item_key(catalog_id: Option<&str>, name: &str) -> String {
    match catalog_id {
        Some(c) => format!("c:{c}"),
        None => format!("n:{}", fold(name)),
    }
}

pub fn catalog_name<'a>(item: &'a CatalogItem, lang: &str) -> &'a str {
    item.names.get(lang).unwrap_or(&item.name)
}

struct Indexed {
    at: usize,
    names: Vec<String>,
    aliases: Vec<String>,
}

/// `buildIndex`.
pub struct Index {
    pub lang: String,
    entries: Vec<Indexed>,
    by_id: HashMap<String, usize>,
    by_name: HashMap<String, usize>,
    by_alias: HashMap<String, usize>,
}

impl Index {
    pub fn build(catalog: &Catalog, lang: &str) -> Index {
        let mut entries = Vec::with_capacity(catalog.items.len());
        let (mut by_name, mut by_alias, mut by_id) =
            (HashMap::new(), HashMap::new(), HashMap::new());
        for (at, item) in catalog.items.iter().enumerate() {
            by_id.insert(item.id.clone(), at);
            let primary = fold(catalog_name(item, lang));
            let mut aliases: Vec<String> = Vec::new();
            for a in item
                .aliases
                .iter()
                .map(|s| fold(s))
                .chain(item.names.values().map(|s| fold(s)))
                .chain([fold(&item.name)])
            {
                if !a.is_empty() && a != primary && !aliases.contains(&a) {
                    aliases.push(a);
                }
            }
            entries.push(Indexed {
                at,
                names: vec![primary],
                aliases,
            });
            for n in std::iter::once(&item.name).chain(item.names.values()) {
                let k = fold(n);
                if !k.is_empty() {
                    by_name.entry(k).or_insert(at);
                }
            }
        }
        for (at, item) in catalog.items.iter().enumerate() {
            for a in &item.aliases {
                let k = fold(a);
                if !k.is_empty() {
                    by_alias.entry(k).or_insert(at);
                }
            }
        }
        Index {
            lang: lang.to_owned(),
            entries,
            by_id,
            by_name,
            by_alias,
        }
    }

    pub fn by_id(&self, id: &str) -> Option<usize> {
        self.by_id.get(id).copied()
    }

    /// `resolveByName`: a row's free text: a name in any language, else an alias. Exact matches only.
    pub fn resolve(&self, text: &str) -> Option<usize> {
        let q = fold(text);
        self.by_name
            .get(&q)
            .or_else(|| self.by_alias.get(&q))
            .copied()
    }

    /// `exactMatch`.
    pub fn exact(&self, text: &str) -> Option<usize> {
        let q = fold(text);
        if q.is_empty() {
            return None;
        }
        self.entries
            .iter()
            .find(|e| e.names.contains(&q))
            .or_else(|| self.entries.iter().find(|e| e.aliases.contains(&q)))
            .map(|e| e.at)
    }

    /// `searchCatalog`: exact, prefix and word-start on the name first, then the same on aliases, then plain substrings.
    /// Ties go to the boost, then the shorter name, then the alphabet.
    pub fn search(&self, query: &str, limit: usize, boost: &dyn Fn(usize) -> i32) -> Vec<usize> {
        let q = fold(query);
        if q.is_empty() {
            return Vec::new();
        }
        let qw: Vec<&str> = q.split(' ').collect();
        let mut hits: Vec<(u8, i32, usize, &str, usize)> = Vec::new();
        for e in &self.entries {
            let mut best = None::<u8>;
            for n in &e.names {
                if let Some(t) = tier(n, &q, &qw) {
                    best = Some(best.map_or(t, |b| b.min(t)));
                }
            }
            for a in &e.aliases {
                if let Some(t) = tier(a, &q, &qw) {
                    let shifted = if t == 6 { 7 } else { t + 3 };
                    best = Some(best.map_or(shifted, |b| b.min(shifted)));
                }
            }
            if let Some(b) = best {
                let name = e.names[0].as_str();
                hits.push((b, -boost(e.at), name.len(), name, e.at));
            }
        }
        hits.sort();
        hits.into_iter().take(limit).map(|h| h.4).collect()
    }
}

fn tier(text: &str, q: &str, qw: &[&str]) -> Option<u8> {
    if text == q {
        return Some(0);
    }
    if text.starts_with(q) {
        return Some(1);
    }
    if text.split(' ').any(|w| w.starts_with(q)) {
        return Some(2);
    }
    if qw.len() > 1
        && qw
            .iter()
            .all(|qw| text.split(' ').any(|w| w.starts_with(qw)))
    {
        return Some(3);
    }
    text.contains(q).then_some(6)
}

// ---------------------------------------------------------------- quick specs

#[derive(Debug, PartialEq, Eq)]
pub struct Parsed {
    pub text: String,
    pub spec: String,
}

const UNITS: [&str; 15] = [
    "kg", "g", "gr", "hg", "l", "lt", "ml", "cl", "dl", "pz", "pcs", "stk", "pk", "pack", "",
];

fn spec_of_token(token: &str) -> Option<String> {
    // "x2", "×3", "2x", "2 x".
    let t = token.to_lowercase();
    let digits = |s: &str| !s.is_empty() && s.len() <= 3 && s.bytes().all(|b| b.is_ascii_digit());
    for x in ["x", "×"] {
        if let Some(r) = t.strip_prefix(x)
            && digits(r.trim_start())
        {
            return Some(format!("x{}", r.trim_start()));
        }
        if let Some(r) = t.strip_suffix(x)
            && digits(r.trim_end())
        {
            return Some(format!("x{}", r.trim_end()));
        }
    }
    // "1 kg", "200gr", "0.5 lt", with an optional full stop.
    let t = t.strip_suffix('.').unwrap_or(&t);
    let end = t
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == ','))
        .unwrap_or(t.len());
    let (num, rest) = t.split_at(end);
    let rest = rest.strip_prefix(' ').unwrap_or(rest);
    let number_ok = {
        let mut parts = num.split(['.', ',']);
        let a = parts.next().unwrap_or("");
        let b = parts.next();
        !a.is_empty() && parts.next().is_none() && b != Some("")
    };
    if number_ok && !rest.is_empty() && UNITS.contains(&rest) && !rest.contains(' ') {
        let unit = match rest {
            "gr" => "g",
            "lt" => "l",
            u => u,
        };
        return Some(format!("{} {unit}", num.replace('.', ",")));
    }
    None
}

/// `parseQuery`: "pane x2", "2x latte", "farina 1 kg", "uova 6", "pane, integrale". Never an empty text for a query.
pub fn parse_query(query: &str) -> Parsed {
    let raw = query.split_whitespace().collect::<Vec<_>>().join(" ");
    if raw.is_empty() {
        return Parsed {
            text: String::new(),
            spec: String::new(),
        };
    }
    if let Some(comma) = raw.find(',').filter(|&c| c > 0) {
        let text = raw[..comma].trim();
        let rest = raw[comma + 1..].trim();
        if !text.is_empty() {
            let inner = parse_query(text);
            let spec = [inner.spec.as_str(), rest]
                .iter()
                .filter(|s| !s.is_empty())
                .copied()
                .collect::<Vec<_>>()
                .join(" ");
            return Parsed {
                text: inner.text,
                spec,
            };
        }
    }
    let words: Vec<&str> = raw.split(' ').collect();
    let tail = |n: usize| -> Option<Parsed> {
        if words.len() <= n {
            return None;
        }
        let token = words[words.len() - n..].join(" ");
        let count = (n == 1 && token.len() <= 2 && token.bytes().all(|b| b.is_ascii_digit()))
            .then(|| {
                token
                    .parse::<u32>()
                    .ok()
                    .filter(|v| *v > 0)
                    .map(|v| format!("x{v}"))
            })
            .flatten();
        let spec = spec_of_token(&token).or(count)?;
        Some(Parsed {
            text: words[..words.len() - n].join(" "),
            spec,
        })
    };
    let head = |n: usize| -> Option<Parsed> {
        if words.len() <= n {
            return None;
        }
        let spec = spec_of_token(&words[..n].join(" "))?;
        Some(Parsed {
            text: words[n..].join(" "),
            spec,
        })
    };
    tail(2)
        .or_else(|| tail(1))
        .or_else(|| head(2))
        .or_else(|| head(1))
        .unwrap_or(Parsed {
            text: raw,
            spec: String::new(),
        })
}

/// `joinSpec`: the typed spec with the chips that are on, none twice.
pub fn join_spec(typed: &str, chips: &[String]) -> String {
    let mut out: Vec<&str> = Vec::new();
    for p in std::iter::once(typed).chain(chips.iter().map(String::as_str)) {
        let p = p.trim();
        if !p.is_empty() && !out.contains(&p) {
            out.push(p);
        }
    }
    out.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn p(text: &str, spec: &str) -> Parsed {
        Parsed {
            text: text.into(),
            spec: spec.into(),
        }
    }

    // web/src/lib/shoppingQuery.test.ts, all of it.
    #[test]
    fn parse_query_matches_the_web() {
        assert_eq!(parse_query("pane x2"), p("pane", "x2"));
        assert_eq!(parse_query("2x latte"), p("latte", "x2"));
        assert_eq!(parse_query("Uova ×6"), p("Uova", "x6"));
        assert_eq!(parse_query("uova 6"), p("uova", "x6"));
        assert_eq!(parse_query("farina 1 kg"), p("farina", "1 kg"));
        assert_eq!(parse_query("latte 2l"), p("latte", "2 l"));
        assert_eq!(
            parse_query("prosciutto crudo 200gr"),
            p("prosciutto crudo", "200 g")
        );
        assert_eq!(parse_query("olio 0.5 lt"), p("olio", "0,5 l"));
        assert_eq!(parse_query("pane, integrale"), p("pane", "integrale"));
        assert_eq!(parse_query("pane x2, integrale"), p("pane", "x2 integrale"));
        assert_eq!(parse_query("Pane integrale"), p("Pane integrale", ""));
        assert_eq!(parse_query("7up"), p("7up", ""));
        assert_eq!(parse_query("x2"), p("x2", ""));
        assert_eq!(parse_query("6"), p("6", ""));
        assert_eq!(parse_query("uova 0"), p("uova 0", ""));
        assert_eq!(parse_query("   "), p("", ""));
    }

    #[test]
    fn join_spec_matches_the_web() {
        assert_eq!(join_spec("x2", &["bio".into()]), "x2 bio");
        assert_eq!(join_spec("", &["x2".into(), "x2".into()]), "x2");
        assert_eq!(join_spec("", &[]), "");
    }

    // web/src/lib/shoppingCatalog.test.ts.
    fn catalog() -> Catalog {
        parse_catalog(&json!({
            "version": 1, "lang": "it",
            "sections": [
                {"id": "dairy", "name": "Latticini", "icon": "x", "order": 2},
                {"id": "produce", "name": "Frutta", "icon": "x", "order": 1},
            ],
            "items": [
                {"id": "milk", "name": "Latte", "names": {"en": "Milk", "da": "Mælk"}, "aliases": [], "section": "dairy", "icon": "x"},
                {"id": "oat-milk", "name": "Latte d'avena", "names": {"en": "Oat milk"}, "aliases": ["avena"], "section": "dairy", "icon": "x"},
                {"id": "chocolate-milk", "name": "Cioccolato al latte", "aliases": [], "section": "pantry", "icon": "x"},
                {"id": "coffee", "name": "Caffè", "aliases": ["espresso"], "section": "pantry", "icon": "x"},
                {"id": "lettuce", "name": "Insalata", "aliases": ["lattuga"], "section": "produce", "icon": "x"},
                {"id": "parmesan", "name": "Parmigiano", "aliases": ["grana padano"], "section": "dairy", "icon": "x"},
                {"id": "apple", "name": "Mele", "names": {"da": "Æbler"}, "aliases": [], "section": "produce", "icon": "x"},
                {"id": "broken"},
            ],
        }))
    }

    fn names(c: &Catalog, i: &Index, q: &str) -> Vec<String> {
        i.search(q, 24, &|_| 0)
            .iter()
            .map(|&a| c.items[a].id.clone())
            .collect()
    }

    #[test]
    fn the_catalog_parses_and_searches_like_the_web() {
        let c = catalog();
        assert_eq!(c.items.len(), 7);
        assert_eq!(c.sections[0].id, "produce");
        let it = Index::build(&c, "it");
        assert_eq!(
            names(&c, &it, "latte"),
            ["milk", "oat-milk", "chocolate-milk"]
        );
        assert_eq!(names(&c, &it, "caffe"), ["coffee"]);
        assert_eq!(names(&c, &it, "CAFFÈ"), ["coffee"]);
        assert_eq!(names(&c, &it, "espresso"), ["coffee"]);
        assert_eq!(names(&c, &it, "grana"), ["parmesan"]);
        assert_eq!(names(&c, &it, "milk"), ["milk", "oat-milk"]);
        assert_eq!(
            names(&c, &it, "latt"),
            ["milk", "oat-milk", "chocolate-milk", "lettuce"]
        );
        assert_eq!(names(&c, &it, "lat ave"), ["oat-milk"]);
        let da = Index::build(&c, "da");
        assert_eq!(names(&c, &da, "aebler"), ["apple"]);
        assert_eq!(names(&c, &da, "æb"), ["apple"]);
    }

    #[test]
    fn exact_and_resolve_match_names_and_aliases() {
        let c = catalog();
        let i = Index::build(&c, "it");
        assert_eq!(
            i.exact("latte").map(|a| c.items[a].id.as_str()),
            Some("milk")
        );
        assert_eq!(
            i.exact("Espresso").map(|a| c.items[a].id.as_str()),
            Some("coffee")
        );
        assert_eq!(i.exact("lat"), None);
        assert_eq!(
            i.resolve("Mælk").map(|a| c.items[a].id.as_str()),
            Some("milk")
        );
        assert_eq!(item_key(None, "Pané"), item_key(None, "pane"));
    }

    // The real catalog this app ships.
    #[test]
    fn the_shipped_catalog_parses() {
        let c = parse_catalog(&serde_json::from_str(crate::store::CATALOG_EN).unwrap());
        assert!(c.items.len() > 300);
        assert!(c.sections.len() > 10);
        let i = Index::build(&c, "en");
        assert_eq!(c.items[i.search("banana", 5, &|_| 0)[0]].id, "banana");
        assert_eq!(c.items[i.search("latte", 5, &|_| 0)[0]].id, "milk"); // Another language's name.
    }

    // web/src/lib/shoppingOps.test.ts.
    fn row(id: &str, name: &str, status: Status) -> Row {
        Row {
            id: id.into(),
            name: name.into(),
            status,
            ..Row::default()
        }
    }

    fn with_ctx<R>(f: impl FnOnce(&mut Ctx) -> R) -> R {
        let mut n = 0;
        let mut new_id = || {
            n += 1;
            format!("new{n}")
        };
        let cat = |c: &str| {
            (c == "banana").then(|| Hint {
                name: "Banane".into(),
                section: Some("produce".into()),
                icon: "b".into(),
            })
        };
        let mut ctx = Ctx {
            list_id: "default",
            now: "T",
            by: "web",
            new_id: &mut new_id,
            catalog: &cat,
        };
        f(&mut ctx)
    }

    fn op(id: &str, kind: Kind) -> Op {
        Op {
            op_id: id.into(),
            kind,
        }
    }

    fn add(name: &str) -> Kind {
        Kind::Add {
            item_id: None,
            catalog_id: None,
            name: name.into(),
            spec: None,
            quantity: None,
            unit: None,
            section: None,
        }
    }

    #[test]
    fn status_transitions_stamp_the_source() {
        let (res, ..) = with_ctx(|c| {
            apply_ops(
                &[row("a", "Pane", Status::Needed)],
                &[op(
                    "1",
                    Kind::Purchase {
                        item_id: "a".into(),
                    },
                )],
                &mut HashSet::new(),
                c,
            )
        });
        assert_eq!(res[0].status, Status::Recent);
        assert_eq!(
            (res[0].updated_at.as_str(), res[0].updated_by.as_str()),
            ("T", "web")
        );
    }

    #[test]
    fn op_ids_dedupe_across_calls_and_within_a_batch() {
        let mut seen = HashSet::new();
        let ops = [op("1", add("Pane")), op("1", add("Pane"))];
        let (res, applied, dups) = with_ctx(|c| apply_ops(&[], &ops, &mut seen, c));
        assert_eq!((res.len(), applied.len(), dups.len()), (1, 1, 1));
        let (res2, _, dups2) = with_ctx(|c| apply_ops(&res, &ops[..1], &mut seen, c));
        assert_eq!((res2.len(), dups2.len()), (1, 1));
    }

    #[test]
    fn add_revives_a_recent_row() {
        let mut banana = row("r", "Banane", Status::Recent);
        banana.catalog_id = Some("banana".into());
        let by_cat = Kind::Add {
            item_id: None,
            catalog_id: Some("banana".into()),
            name: "Bananer".into(),
            spec: None,
            quantity: None,
            unit: None,
            section: None,
        };
        let (res, ..) =
            with_ctx(|c| apply_ops(&[banana], &[op("1", by_cat)], &mut HashSet::new(), c));
        assert_eq!(res.len(), 1);
        assert_eq!(res[0].status, Status::Needed);
        let (res, ..) = with_ctx(|c| {
            apply_ops(
                &[row("p", "Pàne", Status::Recent)],
                &[op("1", add("pane"))],
                &mut HashSet::new(),
                c,
            )
        });
        assert_eq!(res.len(), 1);
        assert_eq!(res[0].status, Status::Needed);
    }

    #[test]
    fn add_uses_the_client_id_and_catalog_hints() {
        let k = Kind::Add {
            item_id: Some("mine".into()),
            catalog_id: Some("banana".into()),
            name: String::new(),
            spec: None,
            quantity: None,
            unit: None,
            section: None,
        };
        let (res, ..) = with_ctx(|c| apply_ops(&[], &[op("1", k)], &mut HashSet::new(), c));
        assert_eq!(res[0].id, "mine");
        assert_eq!(res[0].name, "Banane");
        assert_eq!(res[0].section.as_deref(), Some("produce"));
        assert_eq!(res[0].icon.as_deref(), Some("b"));
    }

    #[test]
    fn deleted_stays_deleted_and_adding_again_makes_a_new_row() {
        let start = [row("a", "Pane", Status::Removed)];
        let (res, ..) = with_ctx(|c| {
            apply_ops(
                &start,
                &[op(
                    "1",
                    Kind::Need {
                        item_id: "a".into(),
                    },
                )],
                &mut HashSet::new(),
                c,
            )
        });
        assert_eq!(res[0].status, Status::Removed);
        let k = Kind::Add {
            item_id: Some("a".into()),
            catalog_id: None,
            name: "Pane".into(),
            spec: None,
            quantity: None,
            unit: None,
            section: None,
        };
        let (res, ..) = with_ctx(|c| apply_ops(&start, &[op("2", k)], &mut HashSet::new(), c));
        assert_eq!(res.len(), 2);
        assert_eq!(res[1].id, "new1");
    }

    #[test]
    fn clear_recent_and_move_remove_what_they_should() {
        let start = [row("a", "A", Status::Recent), row("b", "B", Status::Needed)];
        let (res, ..) = with_ctx(|c| {
            apply_ops(
                &start,
                &[op("1", Kind::ClearRecent)],
                &mut HashSet::new(),
                c,
            )
        });
        assert_eq!(
            (res[0].status, res[1].status),
            (Status::Removed, Status::Needed)
        );
        let (res, ..) = with_ctx(|c| {
            apply_ops(
                &start,
                &[op(
                    "1",
                    Kind::Move {
                        item_id: "b".into(),
                        to_list_id: "other".into(),
                    },
                )],
                &mut HashSet::new(),
                c,
            )
        });
        assert_eq!(res[1].status, Status::Removed);
        let (res, ..) = with_ctx(|c| {
            apply_ops(
                &start,
                &[op(
                    "1",
                    Kind::Move {
                        item_id: "b".into(),
                        to_list_id: "default".into(),
                    },
                )],
                &mut HashSet::new(),
                c,
            )
        });
        assert_eq!(res[1].status, Status::Needed);
    }

    #[test]
    fn set_quantity_validates_numbers() {
        let k = |q| Kind::SetQuantity {
            item_id: "a".into(),
            quantity: q,
            unit: Some("kg".into()),
        };
        let (res, ..) = with_ctx(|c| {
            apply_ops(
                &[row("a", "A", Status::Needed)],
                &[op("1", k(Some(2.5)))],
                &mut HashSet::new(),
                c,
            )
        });
        assert_eq!(res[0].quantity, Some(2.5));
        let (res, ..) =
            with_ctx(|c| apply_ops(&res, &[op("2", k(Some(-1.0)))], &mut HashSet::new(), c));
        assert_eq!(res[0].quantity, None);
    }

    #[test]
    fn rebase_gives_new_ids_and_drops_what_is_gone_or_changes_nothing() {
        let fresh = [row("a", "A", Status::Needed), row("b", "B", Status::Recent)];
        let pending = [
            op(
                "1",
                Kind::Purchase {
                    item_id: "a".into(),
                },
            ),
            op(
                "2",
                Kind::Purchase {
                    item_id: "gone".into(),
                },
            ),
            op(
                "3",
                Kind::Purchase {
                    item_id: "b".into(),
                },
            ), // Already recent.
        ];
        let out = with_ctx(|c| rebase_ops(&fresh, &pending, c));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, pending[0].kind);
        assert_ne!(out[0].op_id, "1");
    }

    #[test]
    fn rows_without_id_or_name_are_dropped_and_status_defaults() {
        let r = parse_rows(
            &json!([{"id": "a", "name": "A"}, {"id": "b"}, {"name": "C"}, {"id": 7, "name": "D", "status": "recent"}]),
        );
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].status, Status::Needed);
        assert_eq!(r[1].id, "7");
        assert_eq!(r[1].status, Status::Recent);
    }

    #[test]
    fn ops_go_on_the_wire_as_the_contract_says() {
        let v = serde_json::to_value(op(
            "id-1",
            Kind::Add {
                item_id: Some("r".into()),
                catalog_id: Some("banana".into()),
                name: "Banane".into(),
                spec: Some("6".into()),
                quantity: None,
                unit: None,
                section: None,
            },
        ))
        .unwrap();
        assert_eq!(
            v,
            json!({"opId": "id-1", "type": "add", "itemId": "r", "catalogId": "banana", "name": "Banane", "spec": "6"})
        );
        let v = serde_json::to_value(op(
            "2",
            Kind::SetQuantity {
                item_id: "r".into(),
                quantity: None,
                unit: None,
            },
        ))
        .unwrap();
        assert_eq!(
            v,
            json!({"opId": "2", "type": "set_quantity", "itemId": "r", "quantity": null, "unit": null})
        );
        let v = serde_json::to_value(op("3", Kind::ClearRecent)).unwrap();
        assert_eq!(v, json!({"opId": "3", "type": "clear_recent"}));
        let back: Op = serde_json::from_value(
            json!({"opId": "4", "type": "move", "itemId": "r", "toListId": "x"}),
        )
        .unwrap();
        assert_eq!(
            back.kind,
            Kind::Move {
                item_id: "r".into(),
                to_list_id: "x".into()
            }
        );
    }
}
