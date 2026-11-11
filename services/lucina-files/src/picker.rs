//! The file picker: choose a file to open, or a folder and a name to save under, on this laptop or in the Drive. It works inside any
//! app on the toolkit: keep one in the app's state, call [`FilePicker::view`] from `view` while it is open, and act on what it answers.
//! It runs its own threads and tells the app to redraw through `wake`, as the sign-in screen does.
//!
//! A file from the Drive is fetched to the app's cache on `/data`, so whoever asked always gets a path on this machine. Saving is to
//! this laptop only: the server has no route for putting a new file in the Drive (`docs/media-library.md`, SecretaryIOS).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

use lucina_account::ui::SignInView;
use lucina_app::{Face, ListState, Rect, TextEdit, Ui, keys};
use lucina_app_support::net::enc;

use crate::drive::{self, Filters};
use crate::media::{Connect, MediaError};
use crate::places::{self, Place};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Choose an existing file. With extensions (lower case, no dot), only those are listed.
    Open { extensions: Vec<String> },
    /// Choose a folder and a name; this is the name it starts with.
    Save { name: String },
}

pub struct Setup {
    /// What it is for: "Export workouts".
    pub title: String,
    pub mode: Mode,
    /// The places on the left. [`places::standard`] is a good start.
    pub places: Vec<Place>,
    /// Offer the Drive, when opening.
    pub drive: Option<Connect>,
    /// The folder a file from the Drive is fetched into: an app's cache.
    pub fetch_to: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Picked {
    File(PathBuf),
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Spot {
    Local(PathBuf),
    DriveRoots,
    Drive { folder: String, path: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Row {
    Up,
    Dir { name: String, path: PathBuf },
    File { name: String, path: PathBuf, size: u64 },
    DriveDir { name: String, folder: String, path: String },
    DriveFile { name: String, url: String, id: String, size: Option<u64> },
    More { offset: u64 },
}

enum Event {
    /// A listing: rows to show, from `generation`; `more` means they go after what is there.
    Listed { generation: u64, rows: Vec<Row>, more: bool },
    Failed { generation: u64, message: String, signed_out: bool },
    Fetching { generation: u64, bytes: u64 },
    Fetched { generation: u64, path: PathBuf },
}

/// What a person can open, in this folder, as rows: folders first, then files, by name, with no hidden files.
fn list_local(dir: &Path, extensions: Option<&[String]>) -> Result<Vec<Row>, String> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        // A standard folder nobody has saved into yet.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("{}: {e}", dir.display())),
    };
    let (mut dirs, mut files) = (Vec::new(), Vec::new());
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let Ok(meta) = std::fs::metadata(e.path()) else { continue };
        if meta.is_dir() {
            dirs.push(Row::Dir { name, path: e.path() });
        } else if meta.is_file() && extensions.is_none_or(|x| x.contains(&drive::ext_of(&name))) {
            files.push(Row::File { name, path: e.path(), size: meta.len() });
        }
    }
    let key = |r: &Row| match r {
        Row::Dir { name, .. } | Row::File { name, .. } => name.to_lowercase(),
        _ => String::new(),
    };
    dirs.sort_by_key(key);
    files.sort_by_key(key);
    dirs.append(&mut files);
    Ok(dirs)
}

/// A name from the server, as a file name in one folder.
pub fn safe_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("").replace('\0', "");
    let trimmed = base.trim_start_matches('.').trim();
    if trimmed.is_empty() { "file".to_owned() } else { trimmed.to_owned() }
}

/// Why a typed name cannot be saved under, if it cannot.
pub fn bad_name(name: &str) -> Option<&'static str> {
    if name.trim().is_empty() {
        Some("Type a name for the file.")
    } else if name.contains('/') || name.contains('\0') {
        Some("A name can't have a / in it.")
    } else if name.starts_with('.') {
        Some("A name can't start with a dot.")
    } else {
        None
    }
}

pub struct FilePicker {
    setup: Setup,
    spot: Spot,
    place: Option<usize>,
    places: ListState,
    rows: Vec<Row>,
    list: ListState,
    name: TextEdit,
    status: String,
    error: Option<String>,
    busy: bool,
    fetching: Option<String>,
    generation: u64,
    stop: Arc<AtomicBool>,
    events: Receiver<Event>,
    tx: Sender<Event>,
    wake: Arc<dyn Fn() + Send + Sync>,
    /// Asked to replace this.
    replace: Option<PathBuf>,
    focused_once: bool,
    signed_out: bool,
    /// The machine's sign-in, for the Drive, shown here.
    sign_in: Option<SignInView>,
    signed_in: Arc<AtomicBool>,
    /// Only a test's way of telling that a thread is out.
    running: Arc<Mutex<u32>>,
}

const ROW: f32 = 30.0;

impl FilePicker {
    pub fn start(setup: Setup, wake: Arc<dyn Fn() + Send + Sync>) -> FilePicker {
        let (tx, events) = mpsc::channel();
        let name = match &setup.mode {
            Mode::Save { name } => TextEdit::with_text(name),
            Mode::Open { .. } => TextEdit::new(),
        };
        let spot = setup.places.first().map_or(Spot::DriveRoots, |p| Spot::Local(p.path.clone()));
        let mut p = FilePicker {
            spot: spot.clone(),
            place: Some(0),
            places: ListState::new(),
            rows: Vec::new(),
            list: ListState::new(),
            name,
            status: String::new(),
            error: None,
            busy: false,
            fetching: None,
            generation: 0,
            stop: Arc::new(AtomicBool::new(false)),
            events,
            tx,
            wake,
            replace: None,
            focused_once: false,
            signed_out: false,
            sign_in: None,
            signed_in: Arc::new(AtomicBool::new(false)),
            running: Arc::new(Mutex::new(0)),
            setup,
        };
        p.places.select(0);
        p.go(spot);
        p
    }

    fn extensions(&self) -> Option<Vec<String>> {
        match &self.setup.mode {
            Mode::Open { extensions } if !extensions.is_empty() => Some(extensions.clone()),
            _ => None,
        }
    }

    fn drive_offered(&self) -> bool {
        self.setup.drive.is_some() && matches!(self.setup.mode, Mode::Open { .. })
    }

    fn spawn(&self, f: impl FnOnce() + Send + 'static) {
        let running = self.running.clone();
        *running.lock().unwrap_or_else(|e| e.into_inner()) += 1;
        std::thread::spawn(move || {
            f();
            *running.lock().unwrap_or_else(|e| e.into_inner()) -= 1;
        });
    }

    /// List the place on screen again: something has put files in it.
    pub fn refresh(&mut self) {
        let keep = self.list.selected;
        let spot = self.spot.clone();
        self.go(spot);
        self.list.selected = keep;
    }

    /// A listing has started or a file is on its way.
    pub fn is_busy(&self) -> bool {
        self.busy
    }

    /// Show another place and forget the one before.
    fn go(&mut self, spot: Spot) {
        self.generation += 1;
        self.spot = spot.clone();
        self.rows.clear();
        self.list = ListState::new();
        self.error = None;
        self.busy = true;
        self.load(spot, 0);
    }

    fn load(&mut self, spot: Spot, offset: u64) {
        let (tx, wake, generation) = (self.tx.clone(), self.wake.clone(), self.generation);
        let more = offset > 0;
        let extensions = self.extensions();
        let connect = self.setup.drive.clone();
        self.spawn(move || {
            let result: Result<Vec<Row>, MediaError> = match &spot {
                Spot::Local(dir) => list_local(dir, extensions.as_deref())
                    .map_err(|message| MediaError::Refused { status: 0, code: "io".into(), message }),
                Spot::DriveRoots | Spot::Drive { .. } => match connect.and_then(|c| c()) {
                    None => Err(MediaError::SignedOut),
                    Some(mut m) => match &spot {
                        Spot::Drive { folder, path } => {
                            m.get(&drive::browse_url(folder, path, &Filters::NONE, offset)).map(|v| {
                                let page = drive::parse_browse(&v, folder, path);
                                let mut rows = Vec::new();
                                for f in page.folders {
                                    rows.push(Row::DriveDir { name: f.name, folder: folder.clone(), path: f.path });
                                }
                                for f in page.files {
                                    let name = f.name().to_owned();
                                    if extensions.as_ref().is_none_or(|x| x.contains(&drive::ext_of(&name))) {
                                        rows.push(Row::DriveFile { url: f.download(), id: f.id, size: f.size, name });
                                    }
                                }
                                if let Some(offset) = page.next_offset {
                                    rows.push(Row::More { offset });
                                }
                                rows
                            })
                        }
                        _ => m.get("/v1/media/drive").map(|v| {
                            drive::parse_roots(&v)
                                .folders
                                .into_iter()
                                .map(|r| Row::DriveDir { name: r.folder.clone(), folder: r.folder, path: String::new() })
                                .collect()
                        }),
                    },
                },
            };
            let _ = tx.send(match result {
                Ok(rows) => Event::Listed { generation, rows, more },
                Err(e) => Event::Failed { generation, message: e.text(), signed_out: e == MediaError::SignedOut },
            });
            wake();
        });
    }

    fn fetch(&mut self, name: String, url: String, id: String) {
        let Some(dir) = self.setup.fetch_to.clone() else {
            self.error = Some("There is nowhere to keep a file from the Drive.".into());
            return;
        };
        let Some(connect) = self.setup.drive.clone() else { return };
        self.generation += 1;
        self.busy = true;
        self.fetching = Some(name.clone());
        self.status = format!("Fetching {name}…");
        self.stop = Arc::new(AtomicBool::new(false));
        let (tx, wake, generation, stop) = (self.tx.clone(), self.wake.clone(), self.generation, self.stop.clone());
        self.spawn(move || {
            let event = fetch_file(&connect, &dir, &name, &url, &id, &stop, &mut |bytes| {
                let _ = tx.send(Event::Fetching { generation, bytes });
                wake();
            });
            let _ = tx.send(match event {
                Ok(path) => Event::Fetched { generation, path },
                Err(e) => Event::Failed { generation, message: e.text(), signed_out: e == MediaError::SignedOut },
            });
            wake();
        });
    }

    fn up(&mut self) {
        match self.spot.clone() {
            Spot::Local(dir) => {
                // Not above the place it started from.
                let root = self.place.and_then(|i| self.setup.places.get(i)).map(|p| p.path.clone());
                if Some(&dir) != root.as_ref()
                    && let Some(parent) = dir.parent()
                {
                    self.go(Spot::Local(parent.to_owned()));
                }
            }
            Spot::DriveRoots => {}
            Spot::Drive { folder, path } => {
                if path.is_empty() {
                    self.go(Spot::DriveRoots);
                } else {
                    let parent = path.rfind('/').map_or(String::new(), |i| path[..i].to_owned());
                    self.go(Spot::Drive { folder, path: parent });
                }
            }
        }
    }

    fn at_root(&self) -> bool {
        match &self.spot {
            Spot::Local(dir) => self.place.and_then(|i| self.setup.places.get(i)).is_none_or(|p| p.path == *dir),
            Spot::DriveRoots => true,
            Spot::Drive { .. } => false,
        }
    }

    fn drain(&mut self) -> Option<Picked> {
        while let Ok(e) = self.events.try_recv() {
            match e {
                Event::Listed { generation, rows, more } if generation == self.generation => {
                    self.busy = false;
                    if more {
                        self.rows.retain(|r| !matches!(r, Row::More { .. }));
                    } else {
                        self.rows = if self.at_root() { Vec::new() } else { vec![Row::Up] };
                    }
                    let had = self.rows.len();
                    self.rows.extend(rows);
                    self.status.clear();
                    if self.list.selected.is_none() && self.rows.len() > had {
                        // The first thing in the folder, not "up".
                        self.list.select(had);
                    } else if self.list.selected.is_none() && !self.rows.is_empty() {
                        self.list.select(0);
                    }
                }
                Event::Failed { generation, message, signed_out } if generation == self.generation => {
                    self.busy = false;
                    self.fetching = None;
                    self.signed_out = signed_out;
                    self.status.clear();
                    self.error = Some(if signed_out { "Sign in to Secretary to see the Drive.".into() } else { message });
                }
                Event::Fetching { generation, bytes } if generation == self.generation => {
                    if let Some(n) = &self.fetching {
                        self.status = format!("Fetching {n}… {}", drive::format_bytes(Some(bytes)));
                    }
                }
                Event::Fetched { generation, path } if generation == self.generation => {
                    self.busy = false;
                    self.fetching = None;
                    return Some(Picked::File(path));
                }
                _ => {}
            }
        }
        None
    }

    fn activate(&mut self, row: Row) -> Option<Picked> {
        match row {
            Row::Up => self.up(),
            Row::Dir { path, .. } => self.go(Spot::Local(path)),
            Row::DriveDir { folder, path, .. } => self.go(Spot::Drive { folder, path }),
            Row::More { offset } => {
                self.busy = true;
                self.load(self.spot.clone(), offset);
            }
            Row::File { name, path, .. } => match self.setup.mode {
                Mode::Open { .. } => return Some(Picked::File(path)),
                Mode::Save { .. } => self.name.set(&name),
            },
            Row::DriveFile { name, url, id, .. } => self.fetch(name, url, id),
        }
        None
    }

    /// The folder and name the Save button means, once they make sense.
    fn save(&mut self) -> Option<Picked> {
        let name = self.name.text.trim().to_owned();
        if let Some(why) = bad_name(&name) {
            self.error = Some(why.to_owned());
            return None;
        }
        let Spot::Local(dir) = &self.spot else { return None };
        if let Err(e) = std::fs::create_dir_all(dir) {
            self.error = Some(format!("{}: {e}", dir.display()));
            return None;
        }
        let path = dir.join(name);
        if path.is_dir() {
            self.error = Some("There is a folder with that name.".to_owned());
            return None;
        }
        if path.exists() {
            self.replace = Some(path);
            return None;
        }
        Some(Picked::File(path))
    }

    /// The whole area. Answers once: what was chosen, or that nothing was.
    pub fn view(&mut self, ui: &mut Ui) -> Option<Picked> {
        if let Some(p) = self.drain() {
            return Some(p);
        }
        if self.signed_in.swap(false, Ordering::SeqCst) {
            self.sign_in = None;
            self.signed_out = false;
            let spot = self.spot.clone();
            self.go(spot);
        }
        if let Some(v) = &mut self.sign_in {
            v.view(ui, "Sign in to open files from your Drive.");
            if ui.key(keys::KEY_ESC) {
                self.sign_in = None;
            }
            return None;
        }
        let t = ui.theme();
        let saving = matches!(self.setup.mode, Mode::Save { .. });
        let mut out = None;
        let area = ui.area();
        let title = self.setup.title.clone();
        ui.within(area, |ui| {
            ui.heading(&title);
            let where_now = self.where_now();
            ui.dim(&where_now);
            ui.pad(4.0);
            let bottom = ui.cut_bottom(t.px(if saving { 88.0 } else { 44.0 }));
            let body = ui.area();
            let left_w = t.px(170.0).min(body.w / 3);
            let left = Rect::new(body.x, body.y, left_w, body.h);
            let right = Rect::new(body.x + left_w + t.px(10.0), body.y, body.w - left_w - t.px(10.0), body.h);
            // The places.
            let drive = self.drive_offered();
            let n_places = self.setup.places.len() + usize::from(drive);
            let names: Vec<String> = self.setup.places.iter().map(|p| p.name.clone()).collect();
            let res = ui.list("picker-places", &mut self.places, left, n_places, ROW, |ui, i, selected| {
                let a = ui.area();
                let c = if selected { t.text } else { t.item };
                let name = names.get(i).map_or("Drive", String::as_str);
                ui.text_in(a.inset(t.px(6.0)), Face::Sans, t.pt(15.0), c, name);
            });
            if res.selection_changed || res.activated.is_some() {
                let i = res.activated.or(self.places.selected).unwrap_or(0);
                if self.place != Some(i) {
                    self.place = Some(i);
                    let to = self.setup.places.get(i).map_or(Spot::DriveRoots, |p| Spot::Local(p.path.clone()));
                    self.go(to);
                }
                if res.activated.is_some() {
                    ui.focus("picker-list");
                }
            }
            // The folder.
            let rows = &self.rows;
            let res = ui.list("picker-list", &mut self.list, right, rows.len(), ROW, |ui, i, selected| paint_row(ui, &rows[i], selected));
            if !self.focused_once {
                self.focused_once = true;
                ui.focus("picker-list");
            }
            if rows.is_empty() && !self.busy && self.error.is_none() {
                ui.within(right, |ui| {
                    ui.dim("Nothing here yet.");
                });
            }
            if let Some(i) = res.activated {
                if !self.busy || matches!(self.rows.get(i), Some(Row::Up)) {
                    let row = self.rows[i].clone();
                    out = self.activate(row);
                }
            }
            // The foot.
            ui.within(bottom, |ui| {
                if saving {
                    if ui.text_field("picker-name", &mut self.name).submitted {
                        out = out.take().or_else(|| self.save());
                    }
                    ui.pad(4.0);
                }
                let line = self.error.clone().unwrap_or_else(|| self.status.clone());
                let danger = self.error.is_some();
                ui.row(36.0, |ui| {
                    if ui.button_id("picker-cancel", "Cancel") {
                        out = Some(Picked::Cancelled);
                    }
                    if saving {
                        if ui.primary_button("Save") {
                            out = out.take().or_else(|| self.save());
                        }
                    } else {
                        let chosen = self.list.selected.and_then(|i| self.rows.get(i)).cloned();
                        let can = matches!(chosen, Some(Row::File { .. } | Row::DriveFile { .. }));
                        if ui.primary_button("Open") && can && !self.busy {
                            if let Some(row) = chosen {
                                out = self.activate(row);
                            }
                        }
                    }
                    if self.signed_out && ui.button_id("picker-sign-in", "Sign in") {
                        let (changed, flag, wake) = (self.wake.clone(), self.signed_in.clone(), self.wake.clone());
                        let done: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
                            flag.store(true, Ordering::SeqCst);
                            wake();
                        });
                        self.sign_in = Some(SignInView::start(changed, done));
                    }
                    let a = ui.area();
                    let c = if danger { t.danger } else { t.dim };
                    ui.text_in(Rect::new(a.x + t.px(10.0), a.y, a.w - t.px(10.0), a.h), Face::Sans, t.pt(13.0), c, &line);
                });
            });
        });
        if let Some(path) = self.replace.clone() {
            let name = path.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned());
            ui.modal(420.0, 150.0, |ui| {
                ui.heading("Replace it?");
                ui.paragraph(&format!("{name} is already in this folder."));
                ui.row(36.0, |ui| {
                    if ui.primary_button("Replace") {
                        self.replace = None;
                        out = Some(Picked::File(path.clone()));
                    }
                    if ui.button_id("picker-keep", "Keep it") {
                        self.replace = None;
                    }
                });
            });
            if ui.key(keys::KEY_ESC) {
                self.replace = None;
            }
            return out;
        }
        // Alt+Left is "back": up a folder. Left from the folder goes to the places beside it and Right comes back; in either list
        // the two keys stay there rather than walking the focus round the buttons. (A name field keeps them for its cursor.)
        if let Some(p) = ui.press() {
            let on_list = ui.is_focused("picker-list");
            let on_places = ui.is_focused("picker-places");
            let bare = !p.mods.ctrl && !p.mods.alt && !p.mods.meta && !p.mods.shift;
            if p.mods.alt && !p.mods.ctrl && !p.mods.meta && !p.mods.shift && p.code == keys::KEY_LEFT {
                ui.take_press();
                self.up();
            } else if bare && p.code == keys::KEY_LEFT && on_list {
                ui.take_press();
                ui.focus("picker-places");
            } else if bare && p.code == keys::KEY_RIGHT && on_places {
                ui.take_press();
                ui.focus("picker-list");
            } else if bare && matches!(p.code, keys::KEY_LEFT | keys::KEY_RIGHT) && (on_list || on_places) {
                ui.take_press();
            }
        }
        if ui.key(keys::KEY_BACKSPACE) {
            self.up();
        } else if ui.key(keys::KEY_ESC) {
            if self.fetching.is_some() {
                self.stop.store(true, Ordering::Relaxed);
                self.generation += 1;
                self.fetching = None;
                self.busy = false;
                self.status.clear();
            } else {
                out = Some(Picked::Cancelled);
            }
        }
        out
    }

    fn where_now(&self) -> String {
        match &self.spot {
            Spot::Local(dir) => dir.display().to_string(),
            Spot::DriveRoots => "Drive".to_owned(),
            Spot::Drive { folder, path } if path.is_empty() => format!("Drive › {folder}"),
            Spot::Drive { folder, path } => format!("Drive › {folder} › {}", path.replace('/', " › ")),
        }
    }
}

fn paint_row(ui: &mut Ui, row: &Row, selected: bool) {
    let a = ui.area();
    let t = ui.theme();
    let c = if selected { t.text } else { t.item };
    let (mark, name, right) = match row {
        Row::Up => ("←", "Up", String::new()),
        Row::Dir { name, .. } | Row::DriveDir { name, .. } => ("▸", name.as_str(), String::new()),
        Row::File { name, size, .. } => ("", name.as_str(), drive::format_bytes(Some(*size))),
        Row::DriveFile { name, size, .. } => ("", name.as_str(), drive::format_bytes(*size)),
        Row::More { .. } => ("", "Load more", String::new()),
    };
    let lw = t.px(22.0);
    if !mark.is_empty() {
        ui.text_in(Rect::new(a.x + t.px(6.0), a.y, lw, a.h), Face::Bold, t.pt(15.0), t.accent, mark);
    }
    let rw = if right.is_empty() { 0 } else { t.px(90.0) };
    ui.text_in(Rect::new(a.x + t.px(6.0) + lw, a.y, a.w - lw - rw - t.px(12.0), a.h), Face::Sans, t.pt(15.0), c, name);
    if !right.is_empty() {
        ui.text_in(Rect::new(a.x + a.w - rw, a.y, rw, a.h), Face::Sans, t.pt(13.0), t.dim, &right);
    }
}

/// A file from the Drive, into `dir`. Whole or not at all: it arrives under a temporary name and is renamed.
fn fetch_file(
    connect: &Connect,
    dir: &Path,
    name: &str,
    url: &str,
    id: &str,
    stop: &AtomicBool,
    progress: &mut dyn FnMut(u64),
) -> Result<PathBuf, MediaError> {
    let io = |e: std::io::Error| MediaError::Refused { status: 0, code: "io".into(), message: format!("{}: {e}", dir.display()) };
    let mut m = connect().ok_or(MediaError::SignedOut)?;
    std::fs::create_dir_all(dir).map_err(io)?;
    let dest = dir.join(format!("{}-{}", enc(id), safe_name(name)));
    let tmp = dest.with_extension("part");
    let mut f = std::fs::File::create(&tmp).map_err(io)?;
    let mut last = 0u64;
    let result = m.download(url, &mut f, &mut |bytes| {
        if bytes - last >= 1 << 20 {
            last = bytes;
            progress(bytes);
        }
        !stop.load(Ordering::Relaxed)
    });
    drop(f);
    match result {
        Ok(_) => std::fs::rename(&tmp, &dest).map(|()| dest).map_err(io),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

#[cfg(test)]
impl FilePicker {
    pub fn set_name_for_test(&mut self, s: &str) {
        self.name.set(s);
    }

    pub fn save_for_test(&mut self) -> Option<Picked> {
        self.save()
    }

    pub fn asks_to_replace(&self) -> bool {
        self.replace.is_some()
    }

    pub fn error_for_test(&self) -> Option<String> {
        self.error.clone()
    }
}

/// The app's cache folder for what the picker fetches.
pub fn fetch_dir(app: &str) -> Option<PathBuf> {
    places::cache_dir(app).map(|d| d.join("fetched"))
}
