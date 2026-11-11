//! The screens. Left: files, search and tags. Middle: the note, its source to edit, or the graph. Right: backlinks and outgoing links.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::time::Duration;

use lucina_app::keys::{self, Mods};
use lucina_app::{App, Face, Image, Input, ListState, Rect, TextEdit, Ui};
use lucina_vault::graph::Graph;
use lucina_vault::search::Hit;
use lucina_vault::vault::{Counts, TagCount};

use crate::doc::{self, Kind, Row};
use crate::graphview::{self, View};
use crate::worker::{Job, Listing, Msg, NoteData};

/// Keys the toolkit has no name for, by evdev code.
pub mod k {
    pub const E: u16 = 18;
    pub const N: u16 = 49;
    pub const F4: u16 = 62;
    pub const F5: u16 = 63;
    pub const F6: u16 = 64;
    pub const F7: u16 = 65;
    pub const F8: u16 = 66;
    pub const KPMINUS: u16 = 74;
    pub const KPPLUS: u16 = 78;
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Files,
    Search,
    Tags,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Middle {
    Read,
    Edit,
    Graph,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeRow {
    pub path: String,
    pub name: String,
    pub depth: usize,
    pub folder: bool,
    pub note: bool,
    pub open: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SideRow {
    Header(String),
    Link { label: String, dest: Option<String> },
}

pub enum Dialog {
    AllowEditing,
    New(TextEdit),
    Rename(TextEdit),
    Trash,
    Conflict { path: String, text: String },
}

enum Status {
    Opening,
    Missing(String),
    Ready,
}

pub struct GraphState {
    pub data: Graph,
    pub view: Option<View>,
    pub local: bool,
    pub picture: Vec<u32>,
    pub drawn: Option<(View, i32, i32, Option<usize>)>,
}

pub struct VaultApp {
    jobs: Sender<Job>,
    status: Status,
    pub root: String,
    pub writable: bool,
    pub counts: Option<Counts>,
    pub tab: Tab,
    notes: Vec<String>,
    children: BTreeMap<String, (Vec<String>, Vec<(String, bool)>)>,
    open_folders: BTreeSet<String>,
    pub tree: Vec<TreeRow>,
    tree_list: ListState,
    pub query: TextEdit,
    generation: u64,
    pub names: Vec<String>,
    pub hits: Vec<Hit>,
    hits_list: ListState,
    pub tags: Vec<TagCount>,
    tags_list: ListState,
    pub note: Option<NoteData>,
    pub rows: Vec<Row>,
    doc_list: ListState,
    side: Vec<SideRow>,
    side_list: ListState,
    pub images: HashMap<String, Option<Image>>,
    pub middle: Middle,
    pub edit: TextEdit,
    pub dialog: Option<Dialog>,
    pub graph: Option<GraphState>,
    pub said: String,
    history: Vec<String>,
    /// Width the pictures were asked for.
    image_w: u32,
    /// Who gets the keyboard next. A widget must exist first, and in the pass after a dialog closes nothing can take it, so it is
    /// asked for over two passes.
    want_focus: Option<&'static str>,
    focus_asked: Option<&'static str>,
}

impl VaultApp {
    pub fn new(root: PathBuf, jobs: Sender<Job>) -> VaultApp {
        let _ = jobs.send(Job::Open(root.clone()));
        VaultApp {
            jobs,
            status: Status::Opening,
            root: root.display().to_string(),
            writable: false,
            counts: None,
            tab: Tab::Files,
            notes: Vec::new(),
            children: BTreeMap::new(),
            open_folders: BTreeSet::new(),
            tree: Vec::new(),
            tree_list: ListState::new(),
            query: TextEdit::new().placeholder("Search: words, \"a phrase\", -not, tag:x, path:x"),
            generation: 0,
            names: Vec::new(),
            hits: Vec::new(),
            hits_list: ListState::new(),
            tags: Vec::new(),
            tags_list: ListState::new(),
            note: None,
            rows: Vec::new(),
            doc_list: ListState::new(),
            side: Vec::new(),
            side_list: ListState::new(),
            images: HashMap::new(),
            middle: Middle::Read,
            edit: TextEdit::multiline(),
            dialog: None,
            graph: None,
            said: String::new(),
            history: Vec::new(),
            image_w: 900,
            want_focus: Some("tree"),
            focus_asked: None,
        }
    }

    fn send(&self, j: Job) {
        let _ = self.jobs.send(j);
    }

    /// The vault at /data/vault follows the server; any other folder is left alone.
    fn sync_now(&mut self) {
        if self.root == crate::sync::SYNCED_ROOT {
            self.said = "Syncing with the server…".into();
            self.send(Job::Sync(PathBuf::from(crate::sync::SYNCED_ROOT)));
        } else {
            self.said = format!("Only {} syncs; this is {}.", crate::sync::SYNCED_ROOT, self.root);
        }
    }

    fn take_listing(&mut self, l: Listing) {
        self.root = l.root;
        self.writable = l.writable;
        self.counts = Some(l.counts);
        self.tags = l.tags;
        self.children.clear();
        let is_note: BTreeSet<&String> = l.notes.iter().collect();
        for p in l.notes.iter().chain(l.files.iter()) {
            let mut parent = String::new();
            let parts: Vec<&str> = p.split('/').collect();
            for (i, c) in parts.iter().enumerate() {
                if i + 1 == parts.len() {
                    self.children
                        .entry(parent.clone())
                        .or_default()
                        .1
                        .push((p.clone(), is_note.contains(p)));
                } else {
                    let here = if parent.is_empty() {
                        (*c).to_owned()
                    } else {
                        format!("{parent}/{c}")
                    };
                    let e = self.children.entry(parent.clone()).or_default();
                    if !e.0.contains(&here) {
                        e.0.push(here.clone());
                    }
                    parent = here;
                }
            }
        }
        for (dirs, files) in self.children.values_mut() {
            dirs.sort_by_cached_key(|d| d.to_lowercase());
            files.sort_by_cached_key(|f| f.0.to_lowercase());
        }
        self.notes = l.notes;
        self.rebuild_tree();
        self.tree_list.invalidate();
        if self.note.is_none() {
            let home = self
                .notes
                .iter()
                .find(|p| p.ends_with("Home.md") && !p.contains('/'))
                .or_else(|| self.notes.first())
                .cloned();
            if let Some(h) = home {
                self.send(Job::Note(h));
            }
        }
        if self.tree_list.selected.is_none() && !self.tree.is_empty() {
            self.tree_list.select(0);
        }
    }

    fn rebuild_tree(&mut self) {
        fn visit(app: &VaultApp, dir: &str, depth: usize, out: &mut Vec<TreeRow>) {
            let Some((dirs, files)) = app.children.get(dir) else {
                return;
            };
            for d in dirs {
                let open = app.open_folders.contains(d);
                out.push(TreeRow {
                    path: d.clone(),
                    name: d.rsplit('/').next().unwrap_or(d).to_owned(),
                    depth,
                    folder: true,
                    note: false,
                    open,
                });
                if open {
                    visit(app, d, depth + 1, out);
                }
            }
            for (f, note) in files {
                let name = f.rsplit('/').next().unwrap_or(f);
                let shown = lucina_vault::vault::note_name(name);
                out.push(TreeRow {
                    path: f.clone(),
                    name: shown.to_owned(),
                    depth,
                    folder: false,
                    note: *note,
                    open: false,
                });
            }
        }
        let mut out = Vec::new();
        visit(self, "", 0, &mut out);
        self.tree = out;
    }

    fn take_note(&mut self, n: NoteData) {
        if let Some(old) = &self.note {
            if old.path != n.path && self.history.last() != Some(&old.path) {
                self.history.push(old.path.clone());
            }
        }
        self.rows = doc::rows(&n.blocks);
        self.doc_list = ListState::new();
        self.doc_list.plain = true;
        self.side = side_rows(&n);
        self.side_list = ListState::new();
        self.images.clear();
        for r in &self.rows {
            if let Kind::Embed {
                target,
                external: false,
                ..
            } = &r.kind
            {
                if !self.images.contains_key(target) && looks_like_picture(target) {
                    self.images.insert(target.clone(), None);
                    let _ = self.jobs.send(Job::Image {
                        target: target.clone(),
                        from: n.path.clone(),
                        w: self.image_w,
                        h: self.image_w * 2 / 3,
                    });
                }
            }
        }
        self.note = Some(n);
        if self.middle == Middle::Edit {
            self.middle = Middle::Read;
        }
        if let Some(g) = &mut self.graph {
            g.drawn = None;
        }
    }

    fn open_tab(&mut self, tab: Tab) {
        self.tab = tab;
        match tab {
            Tab::Files => self.want_focus = Some("tree"),
            Tab::Search => {
                self.query.select_all();
                self.want_focus = Some("query");
            }
            Tab::Tags => self.want_focus = Some("tags"),
        }
    }

    fn search_now(&mut self) {
        self.generation += 1;
        if self.query.text.trim().is_empty() {
            self.hits.clear();
            self.names.clear();
            return;
        }
        self.send(Job::Search {
            q: self.query.text.clone(),
            generation: self.generation,
        });
    }

    fn start_edit(&mut self) {
        if !self.writable {
            self.dialog = Some(Dialog::AllowEditing);
            self.want_focus = Some("Allow editing");
            return;
        }
        if let Some(n) = &self.note {
            self.edit = TextEdit::multiline();
            self.edit.text = n.text.clone();
            self.middle = Middle::Edit;
            self.want_focus = Some("edit");
        }
    }

    fn save(&mut self) {
        if let Some(n) = &self.note {
            self.send(Job::Save {
                path: n.path.clone(),
                text: self.edit.text.clone(),
                revision: n.revision.clone(),
                quitting: false,
            });
        }
    }

    fn toggle_graph(&mut self, local: bool) {
        if self.middle == Middle::Graph && self.graph.as_ref().is_some_and(|g| g.local == local) {
            self.middle = Middle::Read;
            return;
        }
        self.middle = Middle::Graph;
        let around = if local {
            self.note.as_ref().map(|n| n.path.clone())
        } else {
            None
        };
        let local = around.is_some();
        self.graph = None;
        self.send(Job::Graph { around, depth: 2 });
        self.said = if local {
            "The note and what is near it.".into()
        } else {
            "The whole vault.".into()
        };
    }

    fn left(&mut self, ui: &mut Ui) {
        let t = ui.theme();
        let tabs = ui.cut_top(ui.px(30.0));
        ui.fill(tabs, t.bar);
        ui.within(tabs, |ui| {
            ui.row(26.0, |ui| {
                if ui.button_id("tab-files", "Files") {
                    self.open_tab(Tab::Files);
                }
                if ui.button_id("tab-search", "Search") {
                    self.open_tab(Tab::Search);
                }
                if ui.button_id("tab-tags", "Tags") {
                    self.open_tab(Tab::Tags);
                }
            });
        });
        ui.pad(4.0);
        match self.tab {
            Tab::Files => {
                let r = ui.area();
                let rows = &self.tree;
                let res = ui.list(
                    "tree",
                    &mut self.tree_list,
                    r,
                    rows.len(),
                    24.0,
                    |ui, i, _| {
                        let row = &rows[i];
                        let a = ui.area();
                        let x = a.x + ui.px(6.0 + 12.0 * row.depth as f32);
                        let name = if row.folder {
                            format!("{} {}", if row.open { "v" } else { ">" }, row.name)
                        } else {
                            row.name.clone()
                        };
                        let c = if row.folder {
                            t.text
                        } else if row.note {
                            t.item
                        } else {
                            t.dim
                        };
                        ui.text_in(
                            Rect::new(x, a.y, a.right() - x, a.h),
                            Face::Sans,
                            t.pt(13.0),
                            c,
                            &name,
                        );
                    },
                );
                if res.selection_changed {
                    if let Some(row) = self.tree_list.selected.and_then(|i| self.tree.get(i)) {
                        if row.note {
                            self.send(Job::Note(row.path.clone()));
                        }
                    }
                }
                if let Some(i) = res.activated {
                    if let Some(row) = self.tree.get(i).cloned() {
                        if row.folder {
                            if !self.open_folders.remove(&row.path) {
                                self.open_folders.insert(row.path.clone());
                            }
                            self.rebuild_tree();
                        } else if row.note {
                            self.send(Job::Note(row.path));
                            self.want_focus = Some("doc");
                        } else {
                            self.said = format!("{} is a file, not a note.", row.path);
                        }
                    }
                }
            }
            Tab::Search => {
                if ui.text_field("query", &mut self.query).changed {
                    self.search_now();
                }
                ui.pad(4.0);
                let r = ui.area();
                let hits = &self.hits;
                let names = &self.names;
                let total = names.len() + hits.len();
                let res = ui.list("hits", &mut self.hits_list, r, total, 44.0, |ui, i, _| {
                    let a = ui.area();
                    let pad = ui.px(6.0);
                    let (top, under, top_colour) = if i < names.len() {
                        (
                            lucina_vault::vault::note_name(&names[i]).to_owned(),
                            names[i].clone(),
                            t.text,
                        )
                    } else {
                        let h = &hits[i - names.len()];
                        let first = h.lines.first().map_or(String::new(), |l| l.1.clone());
                        (h.name.clone(), first, t.item)
                    };
                    let half = a.h / 2;
                    ui.text_in(
                        Rect::new(a.x + pad, a.y, a.w - pad, half),
                        Face::Bold,
                        t.pt(12.0),
                        top_colour,
                        &top,
                    );
                    ui.text_in(
                        Rect::new(a.x + pad, a.y + half, a.w - pad, half),
                        Face::Sans,
                        t.pt(11.0),
                        t.dim,
                        &under,
                    );
                });
                let pick = res.activated.or(if res.selection_changed {
                    self.hits_list.selected
                } else {
                    None
                });
                if let Some(i) = pick {
                    let path = if i < self.names.len() {
                        self.names.get(i).cloned()
                    } else {
                        self.hits.get(i - self.names.len()).map(|h| h.path.clone())
                    };
                    if let Some(p) = path {
                        self.send(Job::Note(p));
                    }
                }
            }
            Tab::Tags => {
                let r = ui.area();
                let tags = &self.tags;
                let res = ui.list(
                    "tags",
                    &mut self.tags_list,
                    r,
                    tags.len(),
                    24.0,
                    |ui, i, _| {
                        let a = ui.area();
                        let x = a.x + ui.px(6.0);
                        ui.text_in(
                            Rect::new(x, a.y, a.w - ui.px(50.0), a.h),
                            Face::Sans,
                            t.pt(13.0),
                            t.item,
                            &format!("#{}", tags[i].tag),
                        );
                        ui.text_in(
                            Rect::new(a.right() - ui.px(44.0), a.y, ui.px(40.0), a.h),
                            Face::Sans,
                            t.pt(11.0),
                            t.dim,
                            &tags[i].count.to_string(),
                        );
                    },
                );
                if let Some(i) = res.activated {
                    if let Some(tag) = self.tags.get(i) {
                        self.query.set(&format!("tag:{}", tag.tag));
                        self.tab = Tab::Search;
                        self.search_now();
                        self.want_focus = Some("query");
                    }
                }
            }
        }
    }

    fn side(&mut self, ui: &mut Ui) {
        let t = ui.theme();
        let r = ui.area();
        ui.fill(r, t.panel);
        let rows = &self.side;
        let res = ui.list(
            "side",
            &mut self.side_list,
            r,
            rows.len(),
            24.0,
            |ui, i, _| {
                let a = ui.area();
                let x = a.x + ui.px(6.0);
                let rr = Rect::new(x, a.y, a.w - ui.px(8.0), a.h);
                match &rows[i] {
                    SideRow::Header(h) => ui.text_in(rr, Face::Bold, t.pt(12.0), t.accent, h),
                    SideRow::Link { label, dest } => {
                        let c = if dest.is_some() { t.item } else { t.dim };
                        ui.text_in(rr, Face::Sans, t.pt(12.0), c, label);
                    }
                }
            },
        );
        if let Some(i) = res.activated {
            if let Some(SideRow::Link { dest: Some(d), .. }) = self.side.get(i) {
                if self.notes.binary_search(d).is_ok() {
                    self.send(Job::Note(d.clone()));
                } else {
                    self.said = format!("{d} is a file, not a note.");
                }
            }
        }
    }

    fn reading(&mut self, ui: &mut Ui) {
        let t = ui.theme();
        let Some(note) = &self.note else {
            ui.pad(14.0);
            ui.dim("Choose a note on the left.");
            return;
        };
        let bar = ui.cut_top(ui.px(40.0));
        let (name, path) = (note.name.clone(), note.path.clone());
        let tags = note.tags.clone();
        ui.within(bar, |ui| {
            ui.text_in(
                ui.area().inset(ui.px(4.0)),
                Face::Bold,
                t.pt(17.0),
                t.text,
                &name,
            );
        });
        if !tags.is_empty() {
            let line = ui.cut_top(ui.px(20.0));
            let text: Vec<String> = tags.iter().map(|x| format!("#{x}")).collect();
            ui.text_in(line, Face::Sans, t.pt(11.0), t.accent, &text.join("  "));
        }
        let r = ui.area();
        let wanted = (r.w - ui.px(40.0)).max(100) as u32;
        self.image_w = wanted;
        let rows = &self.rows;
        let images = &self.images;
        let k = ui.px(1000.0) as f32 / 1000.0;
        let mut clicked: Option<String> = None;
        let entered = ui.is_focused("doc")
            && matches!(ui.input(), Some(Input::Key(p)) if p.code == keys::KEY_ENTER || p.code == keys::KEY_KPENTER);
        let res = ui.list_measured(
            "doc",
            &mut self.doc_list,
            r,
            rows.len(),
            |text, i, w| {
                let row = &rows[i];
                let inner = w - row_left(row, k);
                match &row.kind {
                    Kind::Text { spans, level } => {
                        let px = text_size(&t, *level);
                        let lh = text.ascent(Face::Sans, px)
                            + text.descent(Face::Sans, px)
                            + (px as i32 / 6).max(1);
                        let n = doc::wrap(spans, *level, inner, |f, s| text.measure(f, px, s)).len()
                            as i32;
                        lh * n.max(1)
                            + (6.0 * k) as i32
                            + if *level > 0 { (10.0 * k) as i32 } else { 0 }
                    }
                    Kind::Code(_) => {
                        let px = t.pt(13.0);
                        text.ascent(Face::Mono, px) + text.descent(Face::Mono, px) + 2
                    }
                    Kind::Rule => (20.0 * k) as i32,
                    Kind::Embed { target, .. } => match images.get(target) {
                        Some(Some(img)) => img.h as i32 + (10.0 * k) as i32,
                        _ => (28.0 * k) as i32,
                    },
                    Kind::Table { cells, .. } => {
                        let px = t.pt(13.0);
                        let lh = text.ascent(Face::Sans, px)
                            + text.descent(Face::Sans, px)
                            + (px as i32 / 6).max(1);
                        let cw = inner / cells.len().max(1) as i32 - (8.0 * k) as i32;
                        let most = cells
                            .iter()
                            .map(|c| {
                                doc::wrap(c, 0, cw, |f, s| text.measure(f, px, s)).len() as i32
                            })
                            .max()
                            .unwrap_or(1);
                        lh * most.max(1) + (6.0 * k) as i32
                    }
                }
            },
            |ui, i, selected| {
                let row = &rows[i];
                let a = ui.area();
                if selected {
                    ui.fill(Rect::new(a.x, a.y, ui.px(3.0), a.h), t.accent);
                }
                if row.bar {
                    ui.fill(
                        Rect::new(a.x + ui.px(8.0), a.y, ui.px(3.0), a.h),
                        if row.callout { t.accent } else { t.panel_hi },
                    );
                }
                let left = a.x + row_left(row, k);
                if let Some(m) = &row.marker {
                    let px = t.pt(13.0);
                    let w = ui.measure(Face::Sans, px, m);
                    let asc = ui.line_height(Face::Sans, px);
                    ui.text_at(
                        left - w - ui.px(5.0),
                        a.y + asc - ui.px(3.0),
                        Face::Sans,
                        px,
                        t.dim,
                        m,
                    );
                }
                let inner = a.right() - left;
                match &row.kind {
                    Kind::Text { spans, level } => {
                        let px = text_size(&t, *level);
                        let y = a.y + if *level > 0 { ui.px(10.0) } else { 0 };
                        draw_spans(
                            ui,
                            spans,
                            *level,
                            left,
                            y,
                            inner,
                            px,
                            ("t", i),
                            &mut clicked,
                        );
                    }
                    Kind::Code(line) => {
                        let px = t.pt(13.0);
                        ui.fill(Rect::new(left - ui.px(4.0), a.y, inner, a.h), t.bar);
                        ui.text_in(
                            Rect::new(left, a.y, inner - ui.px(8.0), a.h),
                            Face::Mono,
                            px,
                            t.item,
                            line,
                        );
                    }
                    Kind::Rule => ui.fill(Rect::new(left, a.y + a.h / 2, inner, 1), t.panel_hi),
                    Kind::Embed {
                        target,
                        alt,
                        external,
                    } => match images.get(target) {
                        Some(Some(img)) => {
                            ui.image_in(Rect::new(left, a.y, img.w as i32, img.h as i32), img)
                        }
                        _ => {
                            let label = if *external {
                                // Never fetched: a note must not tell a server that it was opened.
                                format!("Remote picture not loaded: {target}")
                            } else if alt.is_empty() {
                                format!("Embedded: {target}")
                            } else {
                                format!("Embedded: {target} ({alt})")
                            };
                            let rr = Rect::new(left, a.y, inner, a.h);
                            let h = ui.hit(("embed", i), rr);
                            if h.clicked && !*external {
                                clicked = Some(target.clone());
                            }
                            ui.text_in(
                                rr,
                                Face::Sans,
                                t.pt(13.0),
                                if h.hovered { t.accent } else { t.dim },
                                &label,
                            );
                        }
                    },
                    Kind::Table { cells, head } => {
                        let px = t.pt(13.0);
                        let n = cells.len().max(1) as i32;
                        let cw = inner / n;
                        if *head {
                            ui.fill(Rect::new(left, a.y, inner, a.h), t.panel);
                        }
                        ui.fill(Rect::new(left, a.bottom() - 1, inner, 1), t.panel_hi);
                        for (c, spans) in cells.iter().enumerate() {
                            let x = left + c as i32 * cw + ui.px(4.0);
                            let level = u8::from(*head) * 9;
                            draw_spans(
                                ui,
                                spans,
                                level,
                                x,
                                a.y,
                                cw - ui.px(8.0),
                                px,
                                ("c", i * 64 + c),
                                &mut clicked,
                            );
                        }
                    }
                }
            },
        );
        if entered {
            if let Some(i) = res.activated {
                clicked = first_link(&self.rows[i]);
            }
        }
        if let Some(link) = clicked {
            if !lucina_vault_external(&link) {
                self.send(Job::Follow { from: path, link });
            } else {
                self.said = format!("Outside link, not opened here: {link}");
            }
        }
    }

    fn editing(&mut self, ui: &mut Ui) {
        let t = ui.theme();
        let bar = ui.cut_top(ui.px(34.0));
        ui.fill(bar, t.bar);
        let mut save = false;
        let mut cancel = false;
        ui.within(bar, |ui| {
            ui.row(28.0, |ui| {
                save = ui.primary_button("Save");
                cancel = ui.button("Cancel");
            });
        });
        let r = ui.area();
        let lh = ui.line_height(Face::Sans, t.pt(14.0)).max(1);
        ui.within(r, |ui| {
            ui.text_area("edit", &mut self.edit, (r.h / lh - 1).max(3));
        });
        if save {
            self.save();
        }
        if cancel {
            self.middle = Middle::Read;
        }
    }

    fn graph_view(&mut self, ui: &mut Ui) {
        let t = ui.theme();
        let r = ui.area();
        let hot_path = self.note.as_ref().map(|n| n.path.clone());
        let Some(g) = &mut self.graph else {
            ui.pad(14.0);
            ui.dim("Laying out the graph…");
            return;
        };
        let view = *g
            .view
            .get_or_insert_with(|| graphview::fit(&g.data, r.w, r.h));
        let hot = hot_path.as_ref().and_then(|p| {
            g.data
                .nodes
                .binary_search_by(|n| n.path.as_str().cmp(p))
                .ok()
        });
        let key = (view, r.w, r.h, hot);
        if g.drawn != Some(key) {
            g.picture = graphview::draw(&g.data, view, r.w, r.h, hot);
            g.drawn = Some(key);
        }
        ui.pixels(r, r.w.max(0) as usize, &g.picture);
        // Names only where they can be read: the open note and the busiest ones.
        let mut names: Vec<usize> = (0..g.data.nodes.len()).collect();
        names.sort_by_key(|i| std::cmp::Reverse(g.data.nodes[*i].degree));
        names.truncate(if view.zoom > 0.8 { 40 } else { 12 });
        names.extend(hot);
        for i in names {
            let n = &g.data.nodes[i];
            let p = view.project(n.x, n.y, r.w, r.h);
            if p.0 >= 0 && p.1 >= 0 && p.0 < r.w && p.1 < r.h {
                let c = if Some(i) == hot { t.accent } else { t.text };
                let mut name = n.name.clone();
                if name.chars().count() > 34 {
                    name = name.chars().take(32).collect::<String>() + "…";
                }
                ui.text_at(
                    r.x + p.0 + ui.px(6.0),
                    r.y + p.1,
                    Face::Sans,
                    t.pt(11.0),
                    c,
                    &name,
                );
            }
        }
        let hit = ui.focus_area("graph", r);
        if hit.clicked {
            let (x, y) = ui.pointer();
            if let Some(i) = graphview::nearest(&g.data, view, r.w, r.h, x - r.x, y - r.y) {
                let p = g.data.nodes[i].path.clone();
                let _ = self.jobs.send(Job::Note(p));
            }
        }
        if ui.is_focused("graph") {
            let step = 60.0 / view.zoom.max(0.05);
            let mut v = view;
            if ui.key(keys::KEY_LEFT) {
                v.x -= step;
            }
            if ui.key(keys::KEY_RIGHT) {
                v.x += step;
            }
            if ui.key(keys::KEY_UP) {
                v.y -= step;
            }
            if ui.key(keys::KEY_DOWN) {
                v.y += step;
            }
            if ui.key(keys::KEY_EQUAL) || ui.key(crate::app::k::KPPLUS) {
                v.zoom *= 1.4;
            }
            if ui.key(keys::KEY_MINUS) || ui.key(crate::app::k::KPMINUS) {
                v.zoom /= 1.4;
            }
            if v != view {
                g.view = Some(v);
            }
        }
    }

    fn dialogs(&mut self, ui: &mut Ui) {
        let Some(d) = self.dialog.as_mut() else {
            return;
        };
        let mut close = false;
        let mut job = None;
        let mut said = None;
        let mut reload = None;
        match d {
            Dialog::AllowEditing => ui.modal(460.0, 150.0, |ui| {
                ui.heading("This vault is read-only here");
                ui.paragraph("Saves never overwrite a change they have not seen, and a conflict keeps both as notes. Allow editing this vault on this device?");
                ui.space(8.0);
                ui.row(30.0, |ui| {
                    if ui.primary_button("Allow editing") {
                        job = Some(Job::AllowWrites);
                        close = true;
                    }
                    if ui.button("Stay read-only") {
                        close = true;
                    }
                });
            }),
            Dialog::New(e) => ui.modal(420.0, 130.0, |ui| {
                ui.heading("New note");
                let res = ui.text_field("new-name", e);
                ui.row(30.0, |ui| {
                    if (ui.primary_button("Create") || res.submitted) && !e.text.trim().is_empty() {
                        job = Some(Job::Create {
                            name: e.text.trim().to_owned(),
                            folder: String::new(),
                        });
                        close = true;
                    }
                    if ui.button("Cancel") {
                        close = true;
                    }
                });
            }),
            Dialog::Rename(e) => {
                let from = self.note.as_ref().map(|n| n.path.clone()).unwrap_or_default();
                ui.modal(520.0, 140.0, |ui| {
                    ui.heading("Move or rename, links follow");
                    let res = ui.text_field("rename", e);
                    ui.row(30.0, |ui| {
                        if (ui.primary_button("Move") || res.submitted) && !e.text.trim().is_empty() {
                            job = Some(Job::Rename {
                                from: from.clone(),
                                to: e.text.trim().to_owned(),
                            });
                            close = true;
                        }
                        if ui.button("Cancel") {
                            close = true;
                        }
                    });
                });
            }
            Dialog::Trash => {
                let path = self.note.as_ref().map(|n| n.path.clone()).unwrap_or_default();
                ui.modal(460.0, 130.0, |ui| {
                    ui.heading("Move to the vault's trash?");
                    ui.paragraph(&path);
                    ui.row(30.0, |ui| {
                        if ui.button("Move to trash") {
                            job = Some(Job::Trash(path.clone()));
                            close = true;
                        }
                        if ui.primary_button("Keep it") {
                            close = true;
                        }
                    });
                });
            }
            Dialog::Conflict { path, text } => ui.modal(520.0, 160.0, |ui| {
                ui.heading("This note changed on disk");
                ui.paragraph("Something else wrote it after you opened it. Nothing was overwritten.");
                ui.space(8.0);
                ui.row(30.0, |ui| {
                    if ui.primary_button("Keep mine as a new note") {
                        job = Some(Job::KeepCopy {
                            path: path.clone(),
                            text: text.clone(),
                        });
                        close = true;
                    }
                    if ui.button("Drop mine, read theirs") {
                        reload = Some(path.clone());
                        said = Some("Your text was dropped.".to_owned());
                        close = true;
                    }
                });
            }),
        }
        if close {
            self.dialog = None;
        }
        if let Some(j) = job {
            self.send(j);
        }
        if let Some(p) = reload {
            self.middle = Middle::Read;
            self.send(Job::Refresh);
            self.send(Job::Note(p));
        }
        if let Some(s) = said {
            self.said = s;
        }
    }

    fn keys(&mut self, ui: &mut Ui) {
        if self.dialog.is_some() {
            if ui.key(keys::KEY_ESC) {
                if !matches!(self.dialog, Some(Dialog::Conflict { .. })) {
                    self.dialog = None;
                }
            }
            return;
        }
        let Some(p) = ui.press() else {
            return;
        };
        let m: Mods = p.mods;
        let mut taken = true;
        match (p.code, m.ctrl, m.alt) {
            (keys::KEY_F2, false, false) => self.open_tab(Tab::Files),
            (keys::KEY_F3, false, false) => self.open_tab(Tab::Search),
            (crate::app::k::F4, false, false) => self.open_tab(Tab::Tags),
            (crate::app::k::F5, false, false) => self.toggle_graph(false),
            (crate::app::k::F6, false, false) => self.toggle_graph(true),
            (crate::app::k::F7, false, false) => self.send(Job::Refresh),
            (crate::app::k::F8, false, false) => self.sync_now(),
            (crate::app::k::E, true, false)
                if self.note.is_some() && self.middle != Middle::Edit =>
            {
                self.start_edit()
            }
            (keys::KEY_S, true, false) if self.middle == Middle::Edit => self.save(),
            (crate::app::k::N, true, false) if self.writable => {
                self.dialog = Some(Dialog::New(TextEdit::new().placeholder("Name")));
                self.want_focus = Some("new-name");
            }
            (keys::KEY_R, true, false) if self.writable && self.note.is_some() => {
                let path = self.note.as_ref().map_or(String::new(), |n| n.path.clone());
                self.dialog = Some(Dialog::Rename(TextEdit::with_text(&path)));
                self.want_focus = Some("rename");
            }
            (keys::KEY_DELETE, true, false) if self.writable && self.note.is_some() => {
                self.dialog = Some(Dialog::Trash)
            }
            (keys::KEY_LEFT, false, true) => {
                if let Some(prev) = self.history.pop() {
                    self.send(Job::Note(prev));
                    // Going back is not a new place to come back from.
                    self.note = None;
                }
            }
            (keys::KEY_ESC, false, false) if self.middle != Middle::Read => {
                self.middle = Middle::Read
            }
            _ => taken = false,
        }
        if taken {
            ui.take_press();
        }
    }
}

fn lucina_vault_external(link: &str) -> bool {
    link.split_once(':').is_some_and(|(s, _)| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    })
}

fn looks_like_picture(target: &str) -> bool {
    let t = target.split('#').next().unwrap_or(target).to_lowercase();
    [".png", ".jpg", ".jpeg", ".webp"]
        .iter()
        .any(|e| t.ends_with(e))
}

fn first_link(row: &Row) -> Option<String> {
    let spans: Vec<&lucina_vault::markdown::Span> = match &row.kind {
        Kind::Text { spans, .. } => spans.iter().collect(),
        Kind::Table { cells, .. } => cells.iter().flatten().collect(),
        Kind::Embed {
            target,
            external: false,
            ..
        } => return Some(target.clone()),
        _ => Vec::new(),
    };
    spans.iter().find_map(|s| s.link.clone())
}

fn side_rows(n: &NoteData) -> Vec<SideRow> {
    let mut out = vec![SideRow::Header(format!(
        "Backlinks ({})",
        n.backlinks.len()
    ))];
    for b in &n.backlinks {
        out.push(SideRow::Link {
            label: lucina_vault::vault::note_name(&b.from).to_owned(),
            dest: Some(b.from.clone()),
        });
    }
    out.push(SideRow::Header(format!("Links out ({})", n.outgoing.len())));
    for l in &n.outgoing {
        out.push(SideRow::Link {
            label: l.label.clone(),
            dest: l.dest.clone(),
        });
    }
    if !n.aliases.is_empty() {
        out.push(SideRow::Header("Also known as".to_owned()));
        for a in &n.aliases {
            out.push(SideRow::Link {
                label: a.clone(),
                dest: None,
            });
        }
    }
    out
}

fn row_left(row: &Row, k: f32) -> i32 {
    ((14.0 + 22.0 * row.indent as f32 + if row.bar { 10.0 } else { 0.0 }) * k) as i32
}

fn text_size(t: &lucina_app::Theme, level: u8) -> u16 {
    t.pt(match level {
        1 => 24.0,
        2 => 20.0,
        3 => 17.0,
        4 => 15.5,
        _ => 14.5,
    })
}

#[allow(clippy::too_many_arguments)]
fn draw_spans(
    ui: &mut Ui,
    spans: &[lucina_vault::markdown::Span],
    level: u8,
    left: i32,
    top: i32,
    width: i32,
    px: u16,
    id: (&str, usize),
    clicked: &mut Option<String>,
) {
    let t = ui.theme();
    let lines = doc::wrap(spans, level, width, |f, s| ui.measure(f, px, s));
    let lh = ui.line_height(Face::Sans, px);
    let asc = lh - lh / 4;
    for (ln, line) in lines.iter().enumerate() {
        let y = top + ln as i32 * lh;
        for (pi, p) in line.iter().enumerate() {
            let s = &spans[p.span];
            let f = doc::face_of(s, level);
            let c = if s.link.is_some() {
                t.accent
            } else if s.tag.is_some() {
                t.ok
            } else if s.code {
                t.item
            } else if s.italic {
                t.dim
            } else if level > 0 {
                t.text
            } else {
                t.item
            };
            let r = Rect::new(left + p.x, y, p.w, lh);
            if s.mark {
                ui.fill(r, t.selection);
            }
            if s.code {
                ui.fill(r, t.bar);
            }
            ui.text_at(r.x, y + asc, f, px, c, &p.text);
            if s.strike {
                ui.fill(Rect::new(r.x, y + lh / 2, r.w, 1), c);
            }
            if let Some(link) = &s.link {
                ui.fill(Rect::new(r.x, y + lh - 2, r.w, 1), c);
                if ui.hit((id.0, id.1 * 4096 + ln * 64 + pi), r).clicked {
                    *clicked = Some(link.clone());
                }
            }
        }
    }
}

impl App for VaultApp {
    type Msg = Msg;

    fn message(&mut self, m: Msg) {
        match m {
            Msg::Opened(l) => {
                self.status = Status::Ready;
                self.take_listing(*l);
            }
            Msg::Missing(where_) => self.status = Status::Missing(where_),
            Msg::Note(n) => self.take_note(*n),
            Msg::Hits {
                generation,
                names,
                hits,
            } => {
                if generation == self.generation {
                    self.names = names;
                    self.hits = hits;
                    self.hits_list = ListState::new();
                    if !self.names.is_empty() || !self.hits.is_empty() {
                        self.hits_list.selected = Some(0);
                    }
                }
            }
            Msg::Graph(g) => {
                let local = self.said.starts_with("The note");
                self.want_focus = Some("graph");
                self.graph = Some(GraphState {
                    data: g,
                    view: None,
                    local,
                    picture: Vec::new(),
                    drawn: None,
                });
            }
            Msg::Image { target, image } => {
                self.images.insert(target, image);
                self.doc_list.invalidate();
            }
            Msg::Saved { path, revision } => {
                self.said = format!("Saved {path}");
                self.middle = Middle::Read;
                self.want_focus = Some("doc");
                self.send(Job::Note(path));
                let _ = revision;
                // The edit goes to the server now rather than at the next tick.
                if self.root == crate::sync::SYNCED_ROOT {
                    self.send(Job::Sync(PathBuf::from(crate::sync::SYNCED_ROOT)));
                }
            }
            Msg::Conflict { path } => {
                self.dialog = Some(Dialog::Conflict {
                    path,
                    text: self.edit.text.clone(),
                });
                self.want_focus = Some("Keep mine as a new note");
            }
            Msg::Said(s) => self.said = s,
            Msg::Failed(s) => self.said = format!("Could not: {s}"),
        }
    }

    fn view(&mut self, ui: &mut Ui) {
        let t = ui.theme();
        let name = self.root.rsplit('/').next().unwrap_or("").to_owned();
        ui.header(&if name.is_empty() {
            "Vault".to_owned()
        } else {
            format!("Vault: {name}")
        });
        match &self.status {
            Status::Opening => {
                ui.pad(14.0);
                ui.label("Reading the vault…");
                return;
            }
            Status::Missing(where_) => {
                let s = where_.clone();
                ui.pad(14.0);
                ui.heading("No vault here");
                ui.paragraph(&format!(
                    "Looked in {s}. A vault is a folder of Markdown files, the same folder real Obsidian opens: put one at /data/vault."
                ));
                if ui.key(crate::app::k::F7) {
                    self.send(Job::Open(PathBuf::from(s)));
                }
                return;
            }
            Status::Ready => {}
        }
        let bar = ui.cut_bottom(ui.px(22.0));
        ui.fill(bar, t.bar);
        let mode = if self.writable {
            "editing on"
        } else {
            "read-only"
        };
        let counts = self.counts.as_ref().map_or(String::new(), |c| {
            format!(
                "{} notes, {} links, {} unresolved, {}",
                c.notes, c.links, c.unresolved, mode
            )
        });
        let hints =
            "F2 files  F3 search  F4 tags  F5 graph  F6 local graph  F8 sync  Ctrl+E edit  Alt+Left back";
        let line = if self.said.is_empty() {
            format!("{counts}   {hints}")
        } else {
            format!("{}   {counts}", self.said)
        };
        ui.text_in(bar.inset(ui.px(4.0)), Face::Sans, t.pt(11.0), t.dim, &line);

        let left = ui.cut_left(ui.px(280.0));
        let wide = ui.area().w > ui.px(620.0);
        let right = wide.then(|| ui.cut_right(ui.px(240.0)));
        let mid = ui.area();
        ui.within(left, |ui| self.left(ui));
        if let Some(r) = right {
            ui.within(r, |ui| self.side(ui));
        }
        ui.within(mid, |ui| match self.middle {
            Middle::Read => self.reading(ui),
            Middle::Edit => self.editing(ui),
            Middle::Graph => self.graph_view(ui),
        });
        self.keys(ui);
        self.dialogs(ui);
        if let Some(id) = self.want_focus {
            ui.focus(id);
            if self.focus_asked == Some(id) {
                self.want_focus = None;
                self.focus_asked = None;
            } else {
                self.focus_asked = Some(id);
            }
        }
        ui.wake_after(Duration::from_secs(3600));
    }

    fn on_quit(&mut self) {
        if self.middle == Middle::Edit {
            if let Some(n) = &self.note {
                if self.edit.text != n.text {
                    self.send(Job::Save {
                        path: n.path.clone(),
                        text: self.edit.text.clone(),
                        revision: n.revision.clone(),
                        quitting: true,
                    });
                    std::thread::sleep(Duration::from_millis(300));
                }
            }
        }
    }
}
