//! lucina-app-notes: Keep and Notes on Juno, one program over Secretary's one note store. `docs/research/juno-apps-notes.md`
//! says why it is one app, what it copies from `web/src/views/Keep.tsx` and `Notes.tsx`, and what it leaves out.
//!
//! The screen always shows what is in memory; [`store::Store`] says what to send, and a worker thread sends it.

// Juno with no C library: Eyra is the libc and the program start, in Rust (docs/research/juno-pure-rust.md).
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
extern crate eyra;

mod editor;
mod fake;
mod intents;
mod model;
#[cfg(any(test, feature = "parity"))]
mod parity;
mod store;
#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lucina_account::ui::SignInView;
use lucina_app::keys::{self, Press};
use lucina_app::time::{self as tz, Zone};
use lucina_app::{App, Face, Harness, ListState, Rect, Rgb, Sender, TextEdit, Ui};
use lucina_app_support::dates::{edited_label, now_ms, reminder_label};
use lucina_app_support::files::{Saver, data_dir, write_atomic};
use lucina_app_support::net::{self, Reply, Transport, Worker};

use editor::{Editor, Outcome};
use model::{
    Block, Entry, NoteType, View, parse_blocks, search_notes, source_label, split_title_heading,
    view_notes,
};
use store::{CacheFile, Op, SYNC_EVERY, Store};

pub enum Msg {
    Reply(Reply),
    Cache(Box<CacheFile>, Vec<Op>),
    /// The sign-in screen has something new to show.
    Changed,
    /// The sign-in screen has kept a token.
    SignedIn,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Keep: coloured cards.
    Cards,
    /// Notes: a list and the note read as a document.
    Docs,
}

/// A row of the cards list: a heading, or one band of cards.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Band {
    Heading(&'static str),
    Cards(usize, usize),
}

const CARD_W: f32 = 232.0;
const CARD_H: f32 = 176.0;
const CARD_GAP: f32 = 12.0;
const HEADING_H: f32 = 30.0;
const CACHE_AFTER: Duration = Duration::from_secs(2);

/// Keep's dark palette, `web/src/lib/keepColors.ts`.
pub fn palette(color: &str) -> Option<Rgb> {
    Some(Rgb::hex(match color {
        "red" => 0x77172e,
        "orange" => 0x692b17,
        "yellow" => 0x7c4a03,
        "green" => 0x264d3b,
        "teal" => 0x0c625d,
        "blue" => 0x256377,
        "cerulean" => 0x284255,
        "purple" => 0x472e5b,
        "pink" => 0x6c394f,
        "brown" => 0x4b443a,
        "gray" => 0x232427,
        _ => return None,
    }))
}

/// A widget id from a note or item id.
pub fn hid(s: &str) -> usize {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for b in s.bytes() {
        h = (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    h as usize
}

pub struct Notes {
    pub store: Store,
    worker: Option<Worker>,
    deliver: Arc<dyn Fn(Reply) + Send + Sync>,
    /// For the sign-in screen's thread; `None` in tests.
    tx: Option<Sender<Msg>>,
    pub signin: Option<SignInView>,
    /// The app has asked for sign-in (a test has no screen for it).
    pub needs_signin: bool,
    saver: Saver,
    dir: Option<PathBuf>,
    zone: Zone,
    pub search: TextEdit,
    pub note_type: Option<NoteType>,
    pub view: View,
    pub mode: Mode,
    /// What the screen shows, in screen order.
    pub shown: Vec<Arc<Entry>>,
    shown_for: (u64, String, Option<NoteType>, View, Mode),
    bands: Vec<Band>,
    bands_cols: usize,
    list: ListState,
    rail: ListState,
    /// The current note in the list, by its place in `shown`.
    pub sel: Option<usize>,
    sel_id: Option<String>,
    blocks: Vec<Block>,
    blocks_for: Option<(String, u64)>,
    reader: ListState,
    pub editor: Option<Editor>,
    pub confirm_empty: bool,
    labels_open: bool,
    label_new: TextEdit,
    label_rename: Option<(String, TextEdit)>,
    next_sync: Instant,
    cache_at: Option<Instant>,
    quit_saved: bool,
    want_search: u8,
    search_focused: bool,
    /// Intents whose answer waits for the server, answered from `message`.
    pending: Vec<intents::Pending>,
}

impl Notes {
    pub fn new(
        transport: Option<Box<dyn Transport>>,
        deliver: impl Fn(Reply) + Send + Sync + 'static,
        dir: Option<PathBuf>,
        tx: Option<Sender<Msg>>,
    ) -> Notes {
        let deliver: Arc<dyn Fn(Reply) + Send + Sync> = Arc::new(deliver);
        let d = deliver.clone();
        let needs_signin = transport.is_none();
        Notes {
            store: Store::new(),
            worker: Some(Worker::spawn(transport, move |r| d(r))),
            deliver,
            tx,
            signin: None,
            needs_signin,
            saver: Saver::spawn(),
            dir,
            zone: tz::load().1,
            search: TextEdit::new().placeholder("Search your notes"),
            note_type: None,
            view: View::Notes,
            mode: Mode::Cards,
            shown: Vec::new(),
            shown_for: (0, String::new(), None, View::Notes, Mode::Cards),
            bands: Vec::new(),
            bands_cols: 0,
            list: ListState::new(),
            rail: ListState::new(),
            sel: None,
            sel_id: None,
            blocks: Vec::new(),
            blocks_for: None,
            reader: ListState::new(),
            editor: None,
            confirm_empty: false,
            labels_open: false,
            label_new: TextEdit::new().placeholder("New label"),
            label_rename: None,
            next_sync: Instant::now() + SYNC_EVERY,
            cache_at: None,
            quit_saved: false,
            want_search: 3,
            search_focused: false,
            pending: Vec::new(),
        }
    }

    /// No token, or a dead one: `SignInView` takes the screen. What the owner typed stays queued.
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
            // The token is dead for every app; the cache beside it goes too, as the account guide asks.
            net::forget_token();
            if let Some(dir) = &self.dir {
                let _ = std::fs::remove_file(dir.join("cache.json"));
            }
            self.ask_sign_in();
        }
        while let Some(job) = self.store.next_job(now) {
            if let Some(w) = &self.worker {
                w.send(job);
            }
        }
        if self.store.outbox_dirty {
            let bytes = self.store.outbox_json();
            if let Some(dir) = &self.dir {
                let path = dir.join("outbox.json");
                self.saver.save(move || Some((path, bytes)));
            }
        }
        if self.store.cache_dirty && self.cache_at.is_none() {
            self.cache_at = Some(now + CACHE_AFTER);
        }
    }

    fn save_cache(&mut self) {
        self.cache_at = None;
        let (cursor, labels, notes) = self.store.cache_snapshot();
        if let Some(dir) = &self.dir {
            let path = dir.join("cache.json");
            self.saver.save(move || {
                let file = CacheFile {
                    cursor,
                    labels,
                    notes: notes.iter().map(|e| e.note.clone()).collect(),
                };
                serde_json::to_vec(&file).ok().map(|b| (path, b))
            });
        }
    }

    /// Time-based work, at the top of a pass; every part of it is a few comparisons when it is not due.
    fn tick(&mut self, ui: &mut Ui) {
        let now = Instant::now();
        if now >= self.next_sync {
            self.next_sync = now + SYNC_EVERY;
            self.store.want_sync();
        }
        if let Some(ed) = &mut self.editor {
            ed.autosave(&mut self.store, now);
        }
        if self.store.notice.as_ref().is_some_and(|n| n.until <= now) {
            self.store.notice = None;
        }
        self.pump();
        if self.cache_at.is_some_and(|t| t <= now) {
            self.save_cache();
        }
        let mut next = self.next_sync;
        let deadlines = [
            self.editor.as_ref().and_then(|e| e.save_at),
            self.store.notice.as_ref().map(|n| n.until),
            self.cache_at,
            self.store.retry_at(),
        ];
        for d in deadlines.into_iter().flatten() {
            next = next.min(d);
        }
        ui.wake_after(
            next.saturating_duration_since(now)
                .max(Duration::from_millis(5)),
        );
    }

    /// What the view shows, again only if something it depends on changed.
    fn refresh(&mut self) {
        let key = (
            self.store.version,
            self.search.text.clone(),
            self.note_type,
            self.view.clone(),
            self.mode,
        );
        if key == self.shown_for {
            return;
        }
        let searching = self.searching();
        let list = if searching {
            search_notes(self.store.all(), &self.search.text, self.note_type)
        } else {
            view_notes(self.store.all(), &self.view)
        };
        self.shown = list;
        self.bands_cols = 0;
        if !searching && matches!(self.view, View::Notes | View::Archive | View::Label(_)) {
            // `splitPinned`: the pinned notes first, keeping their order.
            self.shown.sort_by_key(|e| !e.note.pinned);
        }
        if key.1 != self.shown_for.1 || key.2 != self.shown_for.2 || key.3 != self.shown_for.3 {
            self.list.scroll = 0;
            self.sel =
                (!self.shown.is_empty() && (searching || self.mode == Mode::Docs)).then_some(0);
        } else if let Some(id) = &self.sel_id {
            // The same note stays current when the list changes under it.
            self.sel = self
                .shown
                .iter()
                .position(|e| e.note.id == *id)
                .or(self.sel.map(|s| s.min(self.shown.len().saturating_sub(1))))
                .filter(|_| !self.shown.is_empty());
        }
        self.sel_id = self
            .sel
            .and_then(|s| self.shown.get(s))
            .map(|e| e.note.id.clone());
        self.shown_for = key;
    }

    pub fn searching(&self) -> bool {
        !self.search.text.trim().is_empty() || self.note_type.is_some()
    }

    fn set_sel(&mut self, i: usize) {
        self.sel = Some(i);
        self.sel_id = self.shown.get(i).map(|e| e.note.id.clone());
    }

    /// The headings of `Keep.tsx`'s `sections`, over the screen order.
    fn build_bands(&mut self, cols: usize) {
        self.bands.clear();
        let cols = cols.max(1);
        let push =
            |bands: &mut Vec<Band>, from: usize, to: usize, heading: Option<&'static str>| {
                if from == to {
                    return;
                }
                if let Some(h) = heading {
                    bands.push(Band::Heading(h));
                }
                let mut i = from;
                while i < to {
                    let end = (i + cols).min(to);
                    bands.push(Band::Cards(i, end));
                    i = end;
                }
            };
        let n = self.shown.len();
        if self.searching() || self.view == View::Trash {
            push(&mut self.bands, 0, n, None);
        } else if self.view == View::Reminders {
            let now = now_ms();
            let upcoming = self
                .shown
                .iter()
                .take_while(|e| e.reminder_ms.unwrap_or(0) < now)
                .count();
            // Reminders are sorted oldest first; upcoming ones lead on the screen.
            self.shown.rotate_left(upcoming);
            let up = n - upcoming;
            push(&mut self.bands, 0, up, (upcoming > 0).then_some("Upcoming"));
            push(&mut self.bands, up, n, (up > 0).then_some("Earlier"));
        } else {
            let pinned = self.shown.iter().take_while(|e| e.note.pinned).count();
            push(&mut self.bands, 0, pinned, Some("Pinned"));
            push(&mut self.bands, pinned, n, (pinned > 0).then_some("Others"));
        }
        self.bands_cols = cols;
        self.list.invalidate();
    }

    pub fn open(&mut self, i: usize) {
        if let Some(e) = self.shown.get(i).cloned() {
            self.set_sel(i);
            self.editor = Some(Editor::open(&e.note, self.mode == Mode::Docs));
        }
    }

    pub fn new_note(&mut self, list: bool) {
        let label = match &self.view {
            View::Label(t) => Some(self.store.label_name(t)),
            _ => None,
        };
        self.editor = Some(Editor::blank(list, label));
    }

    /// `Keep.tsx` `go`: moving to another view drops the search.
    pub fn go(&mut self, v: View) {
        self.search.clear();
        self.note_type = None;
        self.view = v;
        self.sel = None;
        self.sel_id = None;
    }

    fn status(&self) -> Option<(String, bool)> {
        let s = &self.store;
        let waiting = s.waiting();
        if s.signed_out {
            let more = if waiting > 0 {
                format!(" {waiting} changes stay here until then.")
            } else {
                String::new()
            };
            return Some((
                format!("Not signed in: pair this laptop in Settings.{more}"),
                true,
            ));
        }
        if !s.online {
            return Some((format!("Offline: {waiting} changes waiting"), true));
        }
        if let Some(e) = &s.sync_error {
            return Some((format!("Sync failed: {e}"), true));
        }
        if s.syncing || waiting > 0 {
            return Some(("Syncing…".into(), false));
        }
        None
    }

    fn draw_rail(&mut self, ui: &mut Ui, r: Rect) {
        let t = ui.theme();
        ui.fill(r, t.bar);
        let labels = self.store.labels();
        let fixed = [
            ("Notes", View::Notes),
            ("Reminders", View::Reminders),
            ("Archive", View::Archive),
            ("Trash", View::Trash),
        ];
        let mut rows: Vec<(String, Option<View>, u32)> = fixed
            .iter()
            .map(|(n, v)| ((*n).to_owned(), Some(v.clone()), 0))
            .collect();
        rows.push(("Labels".to_owned(), None, 0));
        rows.extend(
            labels
                .iter()
                .map(|l| (l.name.clone(), Some(View::Label(l.tag.clone())), l.count)),
        );

        let edit_h = t.px(44.0);
        let list_r = Rect::new(r.x, r.y + t.px(6.0), r.w, r.h - edit_h - t.px(6.0));
        let current = (!self.searching()).then(|| self.view.clone());
        let res = ui.list(
            "rail",
            &mut self.rail,
            list_r,
            rows.len(),
            32.0,
            |ui, i, _| {
                let a = ui.area();
                let (name, v, count) = &rows[i];
                ui.fill(a, t.bar);
                let inner = a.inset(t.px(2.0));
                let on = v.is_some() && *v == current;
                if on {
                    ui.fill_round(inner, t.px(6.0), t.selection);
                }
                match v {
                    None => ui.text_in(
                        Rect::new(a.x + t.px(14.0), a.y, a.w - t.px(20.0), a.h),
                        Face::Bold,
                        t.pt(12.0),
                        t.dim,
                        name,
                    ),
                    Some(_) => {
                        let c = if on { t.text } else { t.item };
                        let mut w = a.w - t.px(24.0);
                        if *count > 0 {
                            let s = count.to_string();
                            let cw = ui.measure(Face::Sans, t.pt(12.0), &s);
                            w -= cw + t.px(6.0);
                            ui.text_in(
                                Rect::new(a.right() - cw - t.px(12.0), a.y, cw, a.h),
                                Face::Sans,
                                t.pt(12.0),
                                t.dim,
                                &s,
                            );
                        }
                        ui.text_in(
                            Rect::new(a.x + t.px(14.0), a.y, w, a.h),
                            Face::Sans,
                            t.pt(14.0),
                            c,
                            name,
                        );
                    }
                }
            },
        );
        if let Some(i) = res.activated
            && let Some(v) = rows[i].1.clone()
        {
            self.go(v);
        }
        let b = Rect::new(
            r.x + t.px(10.0),
            r.bottom() - edit_h + t.px(4.0),
            r.w - t.px(20.0),
            edit_h - t.px(12.0),
        );
        if ui.button_at(b, "edit-labels", "Edit labels") {
            self.labels_open = true;
            self.label_new.clear();
        }
    }

    fn draw_tools(&mut self, ui: &mut Ui) {
        let t = ui.theme();
        let h = t.px(34.0);
        let row = ui.cut_top(h);
        let gap = t.px(8.0);
        let mut right = row.right();
        let mut take = |ui: &mut Ui, label: &str, id: &str| -> bool {
            let w = ui.measure(Face::Sans, t.pt(15.0), label) + 2 * t.px(12.0);
            right -= w;
            let r = Rect::new(right, row.y, w, h);
            right -= gap;
            ui.button_at(r, id, label)
        };
        if take(
            ui,
            if self.mode == Mode::Cards {
                "Documents"
            } else {
                "Cards"
            },
            "mode",
        ) {
            self.mode = if self.mode == Mode::Cards {
                Mode::Docs
            } else {
                Mode::Cards
            };
            self.sel = (!self.shown.is_empty() && self.mode == Mode::Docs).then_some(0);
            self.sel_id = self
                .sel
                .and_then(|s| self.shown.get(s))
                .map(|e| e.note.id.clone());
        }
        if take(ui, "New list", "new-list") {
            self.new_note(true);
        }
        if take(ui, "New note", "new-note") {
            self.new_note(false);
        }
        let field = Rect::new(row.x, row.y, right - row.x, h);
        // With nothing typed, Left and Right have no use in the field: they walk the cards.
        let walk = self.search_focused
            && self.search.text.is_empty()
            && ui.press().is_some_and(|p| {
                matches!(p.code, keys::KEY_LEFT | keys::KEY_RIGHT) && !p.mods.ctrl && !p.mods.shift
            });
        let walked = if walk { ui.press() } else { None };
        if walk {
            ui.take_press();
        }
        // The keyboard belongs here: when the app starts, and after a dialog, for the passes it takes the screen behind it to
        // answer again. Asked before the field, so that the frame already shows it.
        if self.want_search > 0 {
            self.want_search -= 1;
            ui.focus("search");
        }
        let res = ui.text_field_at(field, "search", &mut self.search);
        self.search_focused = res.focused;
        if let Some(p) = walked
            && self.mode == Mode::Cards
        {
            self.walk(if p.code == keys::KEY_RIGHT { 1 } else { -1 });
        }
        if res.submitted
            && let Some(s) = self.sel.or((!self.shown.is_empty()).then_some(0))
        {
            self.open(s);
        }
        ui.space(6.0);
        if self.searching() {
            let chips: [(&str, NoteType); 5] = [
                ("Lists", NoteType::Lists),
                ("Images", NoteType::Images),
                ("Audio", NoteType::Audio),
                ("Links", NoteType::Urls),
                ("Reminders", NoteType::Reminders),
            ];
            ui.row(34.0, |ui| {
                for (label, ty) in chips {
                    let on = self.note_type == Some(ty);
                    let clicked = if on {
                        ui.primary_button(label)
                    } else {
                        ui.button_id(("chip", ty as usize), label)
                    };
                    if clicked {
                        self.note_type = if on { None } else { Some(ty) };
                    }
                }
            });
        }
        if self.view == View::Trash && !self.searching() && !self.shown.is_empty() {
            let mut empty = false;
            ui.row(34.0, |ui| {
                ui.label("Notes in the trash are deleted after 30 days.");
                empty = ui.button("Empty trash");
            });
            if empty {
                self.confirm_empty = true;
            }
        }
    }

    fn draw_empty(&mut self, ui: &mut Ui, r: Rect) {
        let t = ui.theme();
        let text =
            if !self.store.loaded || (self.store.syncing && self.store.all().next().is_none()) {
                "Loading your notes…".to_owned()
            } else if self.searching() {
                "No notes match.".to_owned()
            } else {
                match &self.view {
                    View::Notes => "Notes you add appear here.",
                    View::Reminders => "Notes with upcoming reminders appear here.",
                    View::Archive => "Your archived notes appear here.",
                    View::Trash => "No notes in the trash.",
                    View::Label(_) => "No notes with this label yet.",
                }
                .to_owned()
            };
        let w = ui.measure(Face::Sans, t.pt(16.0), &text);
        ui.text_in(
            Rect::new(r.x + (r.w - w) / 2, r.y + t.px(70.0), w + 2, t.px(30.0)),
            Face::Sans,
            t.pt(16.0),
            t.dim,
            &text,
        );
    }

    fn draw_cards(&mut self, ui: &mut Ui, r: Rect) {
        let t = ui.theme();
        let (card_w, gap) = (t.px(CARD_W), t.px(CARD_GAP));
        let cols = (((r.w - t.px(8.0)) + gap) / (card_w + gap)).max(1) as usize;
        if self.bands_cols != cols {
            self.build_bands(cols);
        }
        let w = (r.w - t.px(8.0) - gap * (cols as i32 - 1)) / cols as i32;
        let (band_h, head_h) = (t.px(CARD_H) + gap, t.px(HEADING_H));
        let bands = std::mem::take(&mut self.bands);
        let shown = &self.shown;
        let sel = self.sel;
        let zone = &self.zone;
        let now = now_ms();
        let mut open = None;
        ui.list_measured(
            "cards",
            &mut self.list,
            r,
            bands.len(),
            |_, i, _| {
                if matches!(bands[i], Band::Heading(_)) {
                    head_h
                } else {
                    band_h
                }
            },
            |ui, i, _| {
                let a = ui.area();
                ui.fill(a, t.bg); // The list's own hover and selection do not apply to a band.
                match bands[i] {
                    Band::Heading(h) => ui.text_in(
                        Rect::new(a.x + t.px(4.0), a.y, a.w, a.h),
                        Face::Bold,
                        t.pt(13.0),
                        t.dim,
                        h,
                    ),
                    Band::Cards(from, to) => {
                        for (k, e) in shown.iter().enumerate().skip(from).take(to - from) {
                            let x = a.x + (k - from) as i32 * (w + gap);
                            let rect = Rect::new(x, a.y, w, a.h - gap);
                            if card(ui, e, rect, sel == Some(k), zone, now) {
                                open = Some(k);
                            }
                        }
                    }
                }
            },
        );
        self.bands = bands;
        if let Some(k) = open {
            self.open(k);
        }
    }

    fn draw_docs(&mut self, ui: &mut Ui, r: Rect) {
        let t = ui.theme();
        let list_w = t.px(330.0).min(r.w / 2);
        let l = Rect::new(r.x, r.y, list_w, r.h);
        let pane = Rect::new(
            r.x + list_w + t.px(14.0),
            r.y,
            r.w - list_w - t.px(14.0),
            r.h,
        );
        if self.list.selected != self.sel {
            if let Some(s) = self.sel {
                self.list.select(s);
            } else {
                self.list.selected = None;
            }
        }
        let (shown, zone, now) = (&self.shown, &self.zone, now_ms());
        let res = ui.list("docs", &mut self.list, l, shown.len(), 60.0, |ui, i, _| {
            doc_row(ui, &shown[i], zone, now);
        });
        if res.selection_changed
            && let Some(s) = self.list.selected
        {
            self.set_sel(s);
        }
        if let Some(i) = res.activated {
            self.set_sel(i);
        }
        let current = self.sel.and_then(|s| self.shown.get(s)).cloned();
        if let Some(e) = current {
            self.draw_reader(ui, pane, &e);
        } else {
            ui.text_in(
                pane,
                Face::Sans,
                t.pt(15.0),
                t.dim,
                "Pick a note to read it.",
            );
        }
    }

    fn draw_reader(&mut self, ui: &mut Ui, pane: Rect, e: &Arc<Entry>) {
        let t = ui.theme();
        ui.fill_round(pane, t.px(8.0), t.bar);
        let inner = pane.inset(t.px(14.0));
        let n = &e.note;
        let key = (n.id.clone(), n.rev);
        let fresh = self.blocks_for.as_ref() != Some(&key);
        if fresh {
            let (_, body) = split_title_heading(&n.content, &n.title);
            self.blocks = parse_blocks(body);
            if self.blocks_for.as_ref().map(|k| &k.0) != Some(&key.0) {
                self.reader = ListState::new();
            }
            self.reader.invalidate();
            self.blocks_for = Some(key);
        }
        let (mut edit, mut pin, mut del) = (false, false, false);
        let trash = n.deleted_at.is_some();
        let mut restore = false;
        let rest = ui.within(inner, |ui| {
            ui.row(34.0, |ui| {
                if trash {
                    restore = ui.button("Restore");
                } else {
                    edit = ui.primary_button("Edit");
                    pin = ui.button_id("doc-pin", if n.pinned { "Unpin" } else { "Pin" });
                    del = ui.button_id("doc-del", "Delete");
                }
            });
            let title = if n.title.is_empty() {
                "Untitled"
            } else {
                &n.title
            };
            ui.wrapped(Face::Bold, t.pt(22.0), t.text, title);
            let mut meta = format!(
                "{} · {} · {}",
                source_label(&n.source),
                edited_label(&n.updated_at, now_ms(), &self.zone),
                n.slug
            );
            for tag in &n.tags {
                meta.push_str(&format!("  #{tag}"));
            }
            ui.dim(&meta);
            ui.separator();
            ui.area()
        });
        let id = n.id.clone();
        if edit {
            self.editor = Some(Editor::open(n, true));
        }
        if pin {
            self.store.set_pinned(std::slice::from_ref(&id), !n.pinned);
        }
        if del {
            self.store.trash(std::slice::from_ref(&id));
        }
        if restore {
            self.store.restore(std::slice::from_ref(&id));
        }
        let blocks = &self.blocks;
        let gap = t.px(6.0);
        ui.list_measured(
            "reader",
            &mut self.reader,
            rest,
            blocks.len(),
            |text, i, w| {
                let (face, px, s, indent) = block_style(&blocks[i], &t);
                Ui::wrapped_height(text, face, px, &s, w - indent) + gap
            },
            |ui, i, _| {
                let a = ui.area();
                ui.fill(a, t.bar);
                let (face, px, s, indent) = block_style(&blocks[i], &t);
                if !ui.painting() || s.is_empty() {
                    return;
                }
                let color = match &blocks[i] {
                    Block::Heading(..) => t.text,
                    Block::Code(_) => t.accent,
                    _ => t.item,
                };
                ui.within(Rect::new(a.x + indent, a.y, a.w - indent, a.h), |ui| {
                    ui.wrapped(face, px, color, &s);
                });
            },
        );
    }

    fn draw_notice(&mut self, ui: &mut Ui, r: Rect) {
        let Some(notice) = self.store.notice.clone() else {
            return;
        };
        let t = ui.theme();
        let w = t.px(460.0).min(r.w);
        let bar = Rect::new(r.x, r.bottom() - t.px(44.0), w, t.px(44.0));
        ui.fill_round(bar, t.px(8.0), t.selection);
        let mut right = bar.right() - t.px(6.0);
        let close = Rect::new(
            right - t.px(30.0),
            bar.y + t.px(6.0),
            t.px(30.0),
            t.px(32.0),
        );
        right = close.x - t.px(4.0);
        if ui.button_at(close, "notice-close", "×") {
            self.store.dismiss_notice();
            return;
        }
        if let Some(undo) = notice.undo {
            let b = Rect::new(
                right - t.px(70.0),
                bar.y + t.px(6.0),
                t.px(70.0),
                t.px(32.0),
            );
            right = b.x - t.px(4.0);
            if ui.button_at(b, "notice-undo", "Undo") {
                self.store.undo(undo);
                return;
            }
        }
        ui.text_in(
            Rect::new(bar.x + t.px(14.0), bar.y, right - bar.x - t.px(14.0), bar.h),
            Face::Sans,
            t.pt(14.0),
            t.text,
            &notice.text,
        );
    }

    fn draw_labels_dialog(&mut self, ui: &mut Ui) {
        let labels = self.store.labels();
        let (mut close, mut create, mut del) = (false, false, None);
        let mut rename_done = None;
        let mut start_rename = None;
        let mut rail = std::mem::take(&mut self.rail);
        ui.modal(480.0, 560.0, |ui| {
            ui.heading("Edit labels");
            ui.row(34.0, |ui| {
                let a = ui.area();
                let f = Rect::new(a.x, a.y, a.w - ui.px(90.0), a.h);
                let res = ui.text_field_at(f, "label-new", &mut self.label_new);
                create = res.submitted;
                let b = Rect::new(f.right() + ui.px(8.0), a.y, ui.px(82.0), a.h);
                create |= ui.button_at(b, "label-add", "Add");
            });
            let t = ui.theme();
            let bottom = ui.cut_bottom(ui.px(40.0));
            let r = ui.area();
            ui.list("labels", &mut rail, r, labels.len(), 40.0, |ui, i, _| {
                let a = ui.area();
                ui.fill(a, t.panel);
                let l = &labels[i];
                let (bw, h) = (t.px(70.0), t.px(32.0));
                let y = a.y + (a.h - h) / 2;
                let del_r = Rect::new(a.right() - bw, y, bw, h);
                if ui.button_at(del_r, ("label-del", i), "Delete") {
                    del = Some(l.tag.clone());
                }
                let ren = Rect::new(del_r.x - bw - t.px(6.0), y, bw, h);
                let name = Rect::new(a.x, y, ren.x - a.x - t.px(6.0), h);
                match &mut self.label_rename {
                    Some((tag, edit)) if *tag == l.tag => {
                        let res = ui.text_field_at(name, ("label-edit", i), edit);
                        let ok = ui.button_at(ren, ("label-ok", i), "OK");
                        if res.submitted || ok {
                            rename_done = Some((tag.clone(), edit.text.clone()));
                        }
                    }
                    _ => {
                        ui.text_in(
                            name,
                            Face::Sans,
                            t.pt(15.0),
                            t.text,
                            &format!("{} ({})", l.name, l.count),
                        );
                        if ui.button_at(ren, ("label-ren", i), "Rename") {
                            start_rename = Some((l.tag.clone(), l.name.clone(), i));
                        }
                    }
                }
            });
            ui.within(bottom, |ui| {
                ui.space(6.0);
                close = ui.primary_button("Done");
            });
        });
        self.rail = rail;
        if create && !self.label_new.text.trim().is_empty() {
            self.store.create_label(&self.label_new.text);
            self.label_new.clear();
        }
        if let Some(tag) = del {
            self.store.delete_label(&tag);
            if self.view == View::Label(tag) {
                self.go(View::Notes);
            }
        }
        if let Some((tag, name, i)) = start_rename {
            self.label_rename = Some((tag, TextEdit::with_text(&name)));
            ui.focus(("label-edit", i));
        }
        if let Some((tag, name)) = rename_done {
            self.store.rename_label(&tag, &name);
            self.label_rename = None;
        }
        if close || ui.key(keys::KEY_ESC) {
            self.labels_open = false;
            self.label_rename = None;
            self.want_search = 3;
        }
    }

    /// Keys nothing else wanted: moving through the list while the search field keeps the keyboard, and the shortcuts.
    fn keys(&mut self, ui: &mut Ui) {
        const KEY_F: u16 = 33;
        const KEY_L: u16 = 38;
        const KEY_N: u16 = 49;
        if ui.key(keys::KEY_ESC) {
            if self.searching() {
                self.search.clear();
                self.note_type = None;
            }
            ui.focus("search");
            return;
        }
        let Some(Press { code, mods }) = ui.press() else {
            return;
        };
        if mods.ctrl {
            match code {
                KEY_N => {
                    ui.take_press();
                    self.new_note(false);
                }
                KEY_L => {
                    ui.take_press();
                    self.new_note(true);
                }
                KEY_F => {
                    ui.take_press();
                    ui.focus("search");
                }
                _ => {}
            }
            return;
        }
        if mods.alt || mods.shift || self.shown.is_empty() {
            return;
        }
        let cols = if self.mode == Mode::Cards {
            self.bands_cols.max(1)
        } else {
            1
        } as i64;
        let page = cols * 3;
        let delta = match code {
            keys::KEY_DOWN => cols,
            keys::KEY_UP => -cols,
            keys::KEY_RIGHT if self.mode == Mode::Cards => 1,
            keys::KEY_LEFT if self.mode == Mode::Cards => -1,
            keys::KEY_PAGEDOWN => page,
            keys::KEY_PAGEUP => -page,
            _ => return,
        };
        ui.take_press();
        self.walk(delta);
    }

    fn walk(&mut self, delta: i64) {
        if self.shown.is_empty() {
            return;
        }
        let last = self.shown.len() as i64 - 1;
        let next = match self.sel {
            None => 0,
            Some(s) => (s as i64 + delta).clamp(0, last),
        } as usize;
        self.set_sel(next);
        if self.mode == Mode::Cards
            && let Some(b) = self
                .bands
                .iter()
                .position(|b| matches!(b, Band::Cards(f, t) if (*f..*t).contains(&next)))
        {
            self.list.select(b);
        }
    }
}

fn block_style(b: &Block, t: &lucina_app::Theme) -> (Face, u16, String, i32) {
    match b {
        Block::Heading(1, s) => (Face::Bold, t.pt(20.0), s.clone(), 0),
        Block::Heading(2, s) => (Face::Bold, t.pt(17.0), s.clone(), 0),
        Block::Heading(_, s) => (Face::Bold, t.pt(15.0), s.clone(), 0),
        Block::Task(done, s) => (
            Face::Sans,
            t.pt(15.0),
            format!("{} {s}", if *done { "☑" } else { "☐" }),
            t.px(6.0),
        ),
        Block::Bullet(m, indent, s) => (
            Face::Sans,
            t.pt(15.0),
            format!("{m} {s}"),
            t.px(6.0 + 16.0 * f32::from(*indent)),
        ),
        Block::Code(s) => (Face::Mono, t.pt(13.0), s.clone(), t.px(10.0)),
        Block::Para(s) => (Face::Sans, t.pt(15.0), s.clone(), 0),
        Block::Blank => (Face::Sans, t.pt(8.0), " ".into(), 0),
    }
}

fn doc_row(ui: &mut Ui, e: &Entry, zone: &Zone, now: i64) {
    let t = ui.theme();
    let a = ui.area();
    let n = &e.note;
    if let Some(c) = palette(&n.color) {
        ui.fill(
            Rect::new(a.x, a.y + t.px(3.0), t.px(4.0), a.h - t.px(6.0)),
            c,
        );
    }
    let x = a.x + t.px(12.0);
    let w = a.w - t.px(18.0);
    let mid = a.y + a.h / 2;
    let title = if n.title.is_empty() {
        e.preview.lines().next().unwrap_or("Untitled")
    } else {
        &n.title
    };
    let shown = if n.pinned {
        format!("★ {title}")
    } else {
        title.to_owned()
    };
    ui.text_in(
        Rect::new(x, a.y + t.px(4.0), w, mid - a.y - t.px(4.0)),
        Face::Bold,
        t.pt(14.0),
        t.text,
        &shown,
    );
    let flat = e
        .preview
        .split_whitespace()
        .take(24)
        .collect::<Vec<_>>()
        .join(" ");
    let sub = format!(
        "{} · {} · {flat}",
        source_label(&n.source),
        edited_label(&n.updated_at, now, zone)
    );
    ui.text_in(
        Rect::new(x, mid, w, a.bottom() - mid - t.px(4.0)),
        Face::Sans,
        t.pt(12.0),
        t.dim,
        &sub,
    );
}

/// One note in the grid (`components/keep/NoteCard.tsx`). True when it was clicked.
fn card(ui: &mut Ui, e: &Entry, r: Rect, selected: bool, zone: &Zone, now: i64) -> bool {
    let n = &e.note;
    let clicked = ui.hit(("card", hid(&n.id)), r);
    if !ui.painting() {
        return clicked.clicked;
    }
    let t = ui.theme();
    let rad = t.px(8.0);
    let inner = r.inset(t.px(2.0).max(2));
    if selected {
        ui.fill_round(r, rad, t.accent);
    } else if clicked.hovered {
        ui.fill_round(r, rad, t.selection);
    }
    ui.fill_round(inner, rad, palette(&n.color).unwrap_or(t.panel));
    let body = inner.inset(t.px(10.0));
    ui.within(body, |ui| {
        let chips = ui.cut_bottom(t.px(24.0));
        let line = ui.line_height(Face::Sans, t.pt(13.0));
        if !n.title.is_empty() || n.pinned {
            let title = ui.cut_top(t.px(26.0));
            let w = if n.pinned {
                title.w - t.px(20.0)
            } else {
                title.w
            };
            ui.text_in(
                Rect::new(title.x, title.y, w, title.h),
                Face::Bold,
                t.pt(15.0),
                t.text,
                &n.title,
            );
            if n.pinned {
                ui.text_in(
                    Rect::new(title.right() - t.px(16.0), title.y, t.px(16.0), title.h),
                    Face::Sans,
                    t.pt(14.0),
                    t.accent,
                    "★",
                );
            }
        }
        if let Some(items) = &n.items {
            let area = ui.area();
            let fit = (area.h / line).max(1) as usize;
            let open: Vec<&model::Item> = items.iter().filter(|i| !i.checked).collect();
            let done = items.len() - open.len();
            let room = fit.saturating_sub(usize::from(done > 0));
            let (show, more) = if open.len() > room {
                (room.saturating_sub(1), true)
            } else {
                (open.len(), false)
            };
            let mut y = area.y;
            let mut row = |ui: &mut Ui, s: &str, c: Rgb, x: i32| {
                ui.text_in(
                    Rect::new(area.x + x, y, area.w - x, line),
                    Face::Sans,
                    t.pt(13.0),
                    c,
                    s,
                );
                y += line;
            };
            for i in open.iter().take(show) {
                row(
                    ui,
                    &format!("☐ {}", i.text),
                    t.item,
                    t.px(12.0) * i32::from(i.indent),
                );
            }
            if more {
                row(ui, &format!("+{} more", open.len() - show), t.dim, 0);
            }
            if done > 0 {
                row(ui, &format!("☑ {done} checked"), t.dim, 0);
            }
        } else if !e.preview.is_empty() {
            // Clipped to what is left: a long note must not run under the chips.
            let left = ui.area();
            ui.within(left, |ui| {
                ui.wrapped(Face::Sans, t.pt(13.0), t.item, &e.preview)
            });
        } else if !n.attachments.is_empty() {
            ui.dim(&format!("{} attachments", n.attachments.len()));
        }
        // Reminder and labels at the bottom, as many as fit.
        let mut x = chips.x;
        let mut pills: Vec<String> = Vec::new();
        if let Some(r) = &n.reminder {
            pills.push(reminder_label(&r.at, now, zone));
        }
        pills.extend(n.tags.iter().cloned());
        if !n.attachments.is_empty() && !e.preview.is_empty() {
            pills.push(format!("{} files", n.attachments.len()));
        }
        for p in pills {
            let w = ui.measure(Face::Sans, t.pt(11.0), &p) + t.px(14.0);
            if x + w > chips.right() {
                break;
            }
            let pill = Rect::new(x, chips.y + t.px(2.0), w, chips.h - t.px(4.0));
            ui.fill_round(
                pill,
                pill.h / 2,
                Rgb::hex(0xffffff).over(palette(&n.color).unwrap_or(t.panel), 0x28),
            );
            ui.text_in(
                Rect::new(x + t.px(7.0), pill.y, w - t.px(10.0), pill.h),
                Face::Sans,
                t.pt(11.0),
                t.text,
                &p,
            );
            x += w + t.px(4.0);
        }
    });
    clicked.clicked
}

impl App for Notes {
    type Msg = Msg;

    fn message(&mut self, m: Msg) {
        match m {
            Msg::Reply(r) => self.store.on_reply(r),
            Msg::Cache(c, outbox) => {
                self.store.load_cache(*c, outbox);
                if self.needs_signin {
                    self.store.unauthorized = true;
                    self.ask_sign_in();
                }
            }
            Msg::Changed => {}
            Msg::SignedIn => self.signed_in(),
        }
        self.pump();
        intents::answer_pending(self);
    }

    fn intents() -> &'static [lucina_app::intent::IntentSpec] {
        intents::INTENTS
    }

    fn intent(&mut self, call: lucina_app::intent::Call, reply: lucina_app::intent::Responder) {
        intents::intent(self, call, reply);
    }

    /// The outbox goes to its file now. The cache only if it is small enough to make the half second; otherwise the old
    /// file and its older cursor still load, and the first sync brings the rest.
    fn on_quit(&mut self) {
        if let Some(ed) = &mut self.editor {
            ed.flush(&mut self.store);
        }
        if self.quit_saved {
            return;
        }
        self.quit_saved = true;
        let Some(dir) = self.dir.clone() else {
            return;
        };
        let _ = write_atomic(&dir.join("outbox.json"), &self.store.outbox_json());
        if self.store.cache_dirty && self.store.all().count() <= 500 {
            let (cursor, labels, notes) = self.store.cache_snapshot();
            let file = CacheFile {
                cursor,
                labels,
                notes: notes.iter().map(|e| e.note.clone()).collect(),
            };
            if let Ok(b) = serde_json::to_vec(&file) {
                let _ = write_atomic(&dir.join("cache.json"), &b);
            }
        }
    }

    fn view(&mut self, ui: &mut Ui) {
        if let Some(v) = &mut self.signin {
            ui.header("Notes");
            v.view(ui, "Sign in to see your notes and lists here.");
            return;
        }
        self.tick(ui);
        self.refresh();
        ui.header("Notes");
        let t = ui.theme();
        let rail = ui.cut_left(t.px(190.0));
        self.draw_rail(ui, rail);
        ui.pad(12.0);
        if let Some((text, bad)) = self.status() {
            let r = ui.cut_bottom(t.px(24.0));
            let c = if bad { t.danger } else { t.dim };
            ui.text_in(r, Face::Sans, t.pt(13.0), c, &text);
        }
        self.draw_tools(ui);
        ui.space(6.0);
        let content = ui.area();
        if self.shown.is_empty() {
            self.draw_empty(ui, content);
        } else if self.mode == Mode::Cards {
            self.draw_cards(ui, content);
        } else {
            self.draw_docs(ui, content);
        }
        self.draw_notice(ui, content);

        if self.labels_open {
            self.draw_labels_dialog(ui);
        } else if self.confirm_empty {
            let (mut yes, mut no) = (false, false);
            ui.modal(420.0, 170.0, |ui| {
                ui.heading("Empty the trash?");
                ui.paragraph("All notes in the trash are deleted forever.");
                ui.row(34.0, |ui| {
                    yes = ui.primary_button("Empty trash");
                    no = ui.button("Cancel");
                });
            });
            if yes {
                self.store.empty_trash();
            }
            if yes || no || ui.key(keys::KEY_ESC) {
                self.confirm_empty = false;
                self.want_search = 3;
            }
        } else if let Some(mut ed) = self.editor.take() {
            let labels = self.store.labels();
            let out = ed.view(ui, &mut self.store, &labels, &self.zone);
            match out {
                Outcome::Stay => self.editor = Some(ed),
                Outcome::Close => {
                    ed.flush(&mut self.store);
                    self.want_search = 3;
                }
                Outcome::Deleted => self.want_search = 3,
            }
        } else {
            self.keys(ui);
        }
        self.pump();
    }
}

fn read_cache(dir: &std::path::Path) -> (Box<CacheFile>, Vec<Op>) {
    let cache = std::fs::read(dir.join("cache.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let outbox = std::fs::read(dir.join("outbox.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    (Box::new(cache), outbox)
}

/// Draws one scene of the demo to a PPM, on the host, with the fake server behind it.
fn render(path: &str, scene: &str) {
    let (tx, rx) = std::sync::mpsc::channel();
    let tx = std::sync::Mutex::new(tx);
    let app = Notes::new(
        Some(Box::new(fake::Fake::demo())),
        move |r| {
            let _ = tx.lock().map(|t| t.send(r));
        },
        None,
        None,
    );
    let mut h = Harness::new(app, 1920, 1080);
    h.core.message(Msg::Cache(Box::default(), Vec::new()));
    let settle = |h: &mut Harness<Notes>| {
        while !h.app().store.idle() {
            match rx.recv_timeout(Duration::from_secs(2)) {
                Ok(r) => h.core.message(Msg::Reply(r)),
                Err(_) => break,
            }
        }
        h.core.paint();
    };
    settle(&mut h);
    match scene {
        "search" => h.type_str("ideas"),
        "docs" => {
            h.app().mode = Mode::Docs;
            h.core.input(lucina_app::Input::Wheel(0));
            h.core.paint();
            let i = h
                .app()
                .shown
                .iter()
                .position(|e| e.note.title == "Meeting briefing")
                .unwrap_or(0);
            h.app().set_sel(i);
            h.core.input(lucina_app::Input::Wheel(0));
            h.core.paint();
        }
        "edit" => {
            let i = h
                .app()
                .shown
                .iter()
                .position(|e| e.note.title == "Rust ideas")
                .unwrap_or(0);
            h.app().open(i);
            h.core.input(lucina_app::Input::Wheel(0));
            h.core.paint();
        }
        "list" => {
            let i = h
                .app()
                .shown
                .iter()
                .position(|e| e.note.title == "Pasta night")
                .unwrap_or(0);
            h.app().open(i);
            h.core.input(lucina_app::Input::Wheel(0));
            h.core.paint();
        }
        _ => {}
    }
    settle(&mut h);
    std::fs::write(path, h.ppm()).expect("write the picture");
}

fn main() {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some("--render") {
        let path = args.next().unwrap_or_else(|| "notes.ppm".into());
        render(&path, &args.next().unwrap_or_default());
        return;
    }
    // scripts/notes-parity.sh: the scenario against a real server on this machine.
    #[cfg(feature = "parity")]
    if std::env::args().nth(1).as_deref() == Some("--parity") {
        let url = std::env::args().nth(2).expect("a url");
        let token = std::env::var("LUCINA_NOTES_PARITY_TOKEN").expect("LUCINA_NOTES_PARITY_TOKEN");
        parity::scenario(&|| {
            Box::new(net::Loopback::new(&url, Some(token.clone())).expect("loopback"))
        });
        println!("the scenario passed against {url}");
        return;
    }
    let demo = std::env::var_os("LUCINA_APP_DEMO").is_some();
    lucina_app::run("lucina-app-notes", move |tx: Sender<Msg>| {
        let transport: Option<Box<dyn Transport>> = if demo {
            let f = fake::Fake::demo();
            // LUCINA_APP_DEMO=5000 adds that many invented notes, to see the app at the store's stated size.
            if let Some(n) = std::env::var("LUCINA_APP_DEMO")
                .ok()
                .and_then(|v| v.parse::<u32>().ok())
                .filter(|n| *n > 1)
            {
                f.fill(n);
            }
            Some(Box::new(f))
        } else {
            net::connect()
        };
        let dir = (!demo).then(|| data_dir("notes"));
        let t2 = tx.clone();
        let app = Notes::new(
            transport,
            move |r| t2.send(Msg::Reply(r)),
            dir.clone(),
            Some(tx.clone()),
        );
        // Files on a thread, then the first request: the first frame is on the screen before any of it.
        tx.spawn(move || match dir {
            Some(d) => {
                let (c, o) = read_cache(&d);
                Msg::Cache(c, o)
            }
            None => Msg::Cache(Box::default(), Vec::new()),
        });
        app
    });
}
