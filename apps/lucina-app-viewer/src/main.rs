//! lucina-app-viewer: the pictures on this laptop, a folder at a time, one full screen with zoom and pan. Gallery shows the owner's
//! library from the server; both draw through `lucina-picture-view`.
//!
//!   lucina-app-viewer [file or folder]
//!
//! Started with no argument (a tile on Home) it lists `/data/home`, or the memory home when `/data` is not there. Each picture shown
//! logs how long it took and the memory in use, as `picture <path> ...`: the numbers in `docs/research/juno-media.md`.

// Juno with no C library: Eyra is the libc and the program start, in Rust (docs/research/juno-pure-rust.md).
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
extern crate eyra;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Instant;

use lucina_app::{App, Face, Image, ListState, Rect, Rgb, Sender, Ui, keys};
use lucina_nav as nav;
use lucina_picture_view::{PictureView, flattened, is_picture_name, load};

const BG: u32 = 0xff_05_07_10;
const KEY_I: u16 = 23;
/// Fitted pictures kept: this one and its neighbours.
const KEPT: usize = 3;

enum Msg {
    /// A pass of a progressive picture, fitted.
    Pass { path: PathBuf, fitted: Image },
    Ready { path: PathBuf, fitted: Image, full: Option<Image>, stats: Stats },
    Failed { path: PathBuf, error: String },
    /// Asked for, and passed over: ask again if it comes back on screen.
    Dropped { path: PathBuf },
}

struct Stats {
    read_ms: u128,
    decode_ms: u128,
    fit_ms: u128,
    size: (u32, u32),
    bytes: u64,
}

/// What the decoding thread is asked for: a picture to show, and neighbours to have ready.
struct Job {
    path: PathBuf,
    screen: (u32, u32),
    /// Keep the unfitted picture and send it: it is on screen and may be zoomed.
    keep_full: bool,
}

enum Row {
    Up(PathBuf),
    Dir(String, PathBuf),
    Picture(String, PathBuf),
}

struct Viewer {
    jobs: Option<mpsc::Sender<Job>>,
    started: Instant,
    screen: (u32, u32),
    folder: PathBuf,
    rows: Vec<Row>,
    list: ListState,
    /// The folder's pictures, sorted, while one is open.
    pictures: Vec<PathBuf>,
    index: Option<usize>,
    fitted: HashMap<PathBuf, Image>,
    asked: Vec<PathBuf>,
    picture: PictureView,
    failed: Option<String>,
    /// Back to the list when closed, or out of the app if it was opened on a file.
    opened_on_file: bool,
    logged: Option<PathBuf>,
    overlay: bool,
}

fn home() -> PathBuf {
    if Path::new("/run/lucina/data-ready").exists() { "/data/home".into() } else { "/run/lucina/files".into() }
}

fn list_folder(dir: &Path) -> (Vec<Row>, Vec<PathBuf>) {
    let mut dirs = Vec::new();
    let mut pictures = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let path = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            if path.is_dir() {
                dirs.push((name, path));
            } else if is_picture_name(&name) {
                pictures.push((name, path));
            }
        }
    }
    dirs.sort();
    pictures.sort();
    let mut rows = Vec::new();
    if let Some(up) = dir.parent() {
        rows.push(Row::Up(up.to_owned()));
    }
    rows.extend(dirs.into_iter().map(|(n, p)| Row::Dir(n, p)));
    let paths = pictures.iter().map(|(_, p)| p.clone()).collect();
    rows.extend(pictures.into_iter().map(|(n, p)| Row::Picture(n, p)));
    (rows, paths)
}

impl Viewer {
    fn new(jobs: Option<mpsc::Sender<Job>>, screen: (u32, u32), target: Option<PathBuf>) -> Viewer {
        let mut v = Viewer {
            jobs,
            started: Instant::now(),
            screen,
            folder: PathBuf::new(),
            rows: Vec::new(),
            list: ListState::new(),
            pictures: Vec::new(),
            index: None,
            fitted: HashMap::new(),
            asked: Vec::new(),
            picture: PictureView::new(),
            failed: None,
            opened_on_file: false,
            logged: None,
            overlay: false,
        };
        match target {
            Some(p) if p.is_file() => {
                v.enter(p.parent().unwrap_or(Path::new("/")).to_owned());
                v.opened_on_file = true;
                let at = v.pictures.iter().position(|x| *x == p);
                if at.is_none() {
                    v.pictures.insert(0, p);
                }
                v.open(at.unwrap_or(0));
            }
            Some(p) => v.enter(p),
            None => v.enter(home()),
        }
        v
    }

    fn enter(&mut self, dir: PathBuf) {
        let (rows, pictures) = list_folder(&dir);
        self.folder = dir;
        self.rows = rows;
        self.pictures = pictures;
        self.list = ListState::new();
        if !self.rows.is_empty() {
            self.list.selected = Some(0);
        }
    }

    fn open(&mut self, i: usize) {
        self.index = Some(i);
        // One line per picture shown, so a test (or the owner) can follow stepping with the arrows in the log.
        if let Some(p) = self.pictures.get(i) {
            eprintln!("lucina-app-viewer: showing {} ({} of {})", p.display(), i + 1, self.pictures.len());
        }
        self.picture.clear();
        self.failed = None;
        self.overlay = false;
        self.started = Instant::now();
        self.logged = None;
        self.ask();
    }

    /// Ask for what the screen needs, this picture first. Its neighbours come fitted, so that walking is instant. The unfitted picture is
    /// kept when it is the one on screen as it decodes, and otherwise decoded again only when someone zooms.
    fn ask(&mut self) {
        let Some(i) = self.index else { return };
        let Some(jobs) = &self.jobs else { return };
        let Some(path) = self.pictures.get(i).cloned() else { return };
        let zoom_needs_it = self.picture.is_zoomed() && self.picture.source().is_none();
        if (!self.fitted.contains_key(&path) || zoom_needs_it) && !self.asked.contains(&path) && self.failed.is_none() {
            self.asked.push(path.clone());
            let _ = jobs.send(Job { path, screen: self.screen, keep_full: true });
        }
        for n in [i + 1, i.wrapping_sub(1)] {
            if let Some(p) = self.pictures.get(n)
                && !self.fitted.contains_key(p)
                && !self.asked.contains(p)
            {
                self.asked.push(p.clone());
                let _ = jobs.send(Job { path: p.clone(), screen: self.screen, keep_full: false });
            }
        }
        // Forget what is not near: the pictures take more room than anything else here.
        let near: Vec<PathBuf> = (i.saturating_sub(1)..=i + 1).filter_map(|n| self.pictures.get(n).cloned()).collect();
        if self.fitted.len() > KEPT {
            self.fitted.retain(|p, _| near.contains(p));
        }
    }

    /// Left and Right: the folder's previous and next picture, Home and End the first and last. Every file in `pictures` is
    /// one this viewer shows, in the order the list shows them. The ends stop: Windows Photos does not wrap (`lucina-nav`).
    fn step(&mut self, step: nav::Step) {
        let Some(i) = self.index else { return };
        if i >= self.pictures.len() {
            return;
        }
        let ok = vec![true; self.pictures.len()];
        let to = nav::walk(&ok, i, step);
        if to != i {
            self.open(to);
        }
    }

    /// Up a folder, with the folder we left selected in the list above it.
    fn up(&mut self) {
        let Some(parent) = self.folder.parent().map(Path::to_owned) else { return };
        let came = std::mem::replace(&mut self.folder, PathBuf::new());
        self.enter(parent);
        if let Some(row) = self.rows.iter().position(|r| matches!(r, Row::Dir(_, p) if *p == came)) {
            self.list.select(row);
        }
    }

    fn close(&mut self, ui: &mut Ui) {
        if self.opened_on_file {
            ui.quit();
            return;
        }
        // Back to the list with the picture that was open selected.
        if let Some(path) = self.current().cloned()
            && let Some(row) = self.rows.iter().position(|r| matches!(r, Row::Picture(_, p) if *p == path))
        {
            self.list.select(row);
        }
        self.index = None;
        self.picture.clear();
        self.fitted.clear();
        self.asked.clear();
    }

    fn current(&self) -> Option<&PathBuf> {
        self.index.and_then(|i| self.pictures.get(i))
    }

    fn log_shown(&mut self, stats: &Stats, path: &Path) {
        eprintln!(
            "lucina-app-viewer: picture {} size={}x{} bytes={} read_ms={} decode_ms={} fit_ms={} to_picture_ms={} rss_kib={}",
            path.display(),
            stats.size.0,
            stats.size.1,
            stats.bytes,
            stats.read_ms,
            stats.decode_ms,
            stats.fit_ms,
            self.started.elapsed().as_millis(),
            rss_kib(),
        );
    }

    fn list_view(&mut self, ui: &mut Ui) {
        ui.header("Pictures");
        ui.pad(14.0);
        let folder = self.folder.display().to_string();
        ui.dim(&folder);
        if self.pictures.is_empty() && self.rows.iter().all(|r| matches!(r, Row::Up(_))) {
            ui.label("No pictures or folders here. JPEG, PNG, WebP and JPEG XL files show in this list.");
        }
        let r = ui.area();
        let rows = &self.rows;
        let res = ui.list("rows", &mut self.list, r, rows.len(), 30.0, |ui, i, _| match &rows[i] {
            Row::Up(_) => ui.dim(".."),
            Row::Dir(n, _) => ui.label(&format!("{n}/")),
            Row::Picture(n, _) => ui.label(n),
        });
        if let Some(i) = res.activated {
            match &self.rows[i] {
                Row::Up(p) | Row::Dir(_, p) => {
                    let p = p.clone();
                    self.enter(p);
                }
                Row::Picture(_, p) => {
                    let p = p.clone();
                    if let Some(at) = self.pictures.iter().position(|x| *x == p) {
                        self.open(at);
                    }
                }
            }
        }
        // Backspace and Alt+Left go up a folder. Left and Right stay on the list rather than walking the focus.
        if let Some(p) = ui.press() {
            let no_mods = !p.mods.ctrl && !p.mods.alt && !p.mods.meta && !p.mods.shift;
            if (p.mods.alt && !p.mods.ctrl && !p.mods.meta && !p.mods.shift && p.code == keys::KEY_LEFT) || (no_mods && p.code == keys::KEY_BACKSPACE) {
                ui.take_press();
                self.up();
            } else if no_mods && matches!(p.code, keys::KEY_LEFT | keys::KEY_RIGHT) {
                ui.take_press();
            }
        }
        if ui.key(keys::KEY_ESC) {
            ui.quit();
        }
    }

    fn picture_view(&mut self, ui: &mut Ui) {
        let t = ui.theme();
        let r = ui.area();
        ui.fill(r, Rgb::hex(BG));
        let path = self.current().cloned().unwrap_or_default();
        let fitted = self.fitted.get(&path);
        let base = self.picture.source().map(|s| (s.w, s.h)).or(fitted.map(|f| (f.w, f.h))).unwrap_or((1, 1));
        let resp = self.picture.input(ui, "picture", r, base);
        if resp.clicked {
            let (px, py) = ui.pointer();
            self.picture.toggle((px - r.x) as f32, (py - r.y) as f32, base, r);
        }
        let fitted = self.fitted.get(&path);
        self.picture.show(ui, r, fitted, BG);
        if self.failed.is_some() || fitted.is_none() {
            let msg = self.failed.clone().unwrap_or_else(|| "Opening…".to_owned());
            ui.text_in(Rect::new(r.x + ui.px(20.0), r.y + r.h / 2, r.w - ui.px(40.0), ui.px(30.0)), Face::Sans, t.pt(15.0), t.text, &msg);
        }
        if self.overlay {
            let bar = Rect::new(r.x, r.y, r.w, ui.px(34.0));
            ui.fill(bar, t.bar);
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let of = self.index.map_or(0, |i| i + 1);
            let (w, h) = self.picture.source().map_or((0, 0), |s| (s.w, s.h));
            let zoom = self.picture.view().map_or(1.0, |v| v.scale);
            let text = format!("{name}   {of} of {}   {w} x {h}   zoom {zoom:.2}", self.pictures.len());
            ui.text_in(bar, Face::Sans, t.pt(12.0), t.text, &text);
        }
        let zoomed = self.picture.is_zoomed();
        if zoomed && self.picture.source().is_none() {
            self.ask();
        }
        let Some(p) = ui.press() else { return };
        // Alt+Left is "back", like Esc.
        if p.mods.alt && !p.mods.ctrl && !p.mods.meta && !p.mods.shift && p.code == keys::KEY_LEFT && !zoomed {
            self.close(ui);
            ui.take_press();
            return;
        }
        if p.mods.ctrl || p.mods.alt || p.mods.meta {
            return;
        }
        match p.code {
            keys::KEY_ESC => {
                if zoomed {
                    self.picture.fit();
                } else {
                    self.close(ui);
                }
            }
            keys::KEY_LEFT if !zoomed && !p.mods.shift => self.step(nav::Step::Prev),
            keys::KEY_RIGHT if !zoomed && !p.mods.shift => self.step(nav::Step::Next),
            keys::KEY_HOME if !zoomed && !p.mods.shift => self.step(nav::Step::First),
            keys::KEY_END if !zoomed && !p.mods.shift => self.step(nav::Step::Last),
            KEY_I => self.overlay = !self.overlay,
            keys::KEY_ENTER | keys::KEY_KPENTER => self.picture.toggle(r.w as f32 / 2.0, r.h as f32 / 2.0, base, r),
            _ => return,
        }
        ui.take_press();
    }
}

impl App for Viewer {
    type Msg = Msg;

    fn message(&mut self, m: Msg) {
        match m {
            Msg::Pass { path, fitted } => {
                if self.current() == Some(&path) && self.logged.is_none() {
                    eprintln!("lucina-app-viewer: first pass {} to_picture_ms={}", path.display(), self.started.elapsed().as_millis());
                }
                self.fitted.insert(path, fitted);
            }
            Msg::Ready { path, fitted, full, stats } => {
                self.asked.retain(|p| *p != path);
                self.fitted.insert(path.clone(), fitted);
                if self.current() == Some(&path) {
                    if let Some(full) = full {
                        self.picture.set_source(full);
                    }
                    self.log_shown(&stats, &path);
                    self.logged = Some(path);
                }
            }
            Msg::Dropped { path } => self.asked.retain(|p| *p != path),
            Msg::Failed { path, error } => {
                self.asked.retain(|p| *p != path);
                if self.current() == Some(&path) {
                    self.failed = Some(error);
                }
            }
        }
    }

    fn view(&mut self, ui: &mut Ui) {
        if self.index.is_some() { self.picture_view(ui) } else { self.list_view(ui) }
    }
}

fn rss_kib() -> i64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| s.lines().find(|l| l.starts_with("VmRSS:"))?.split_whitespace().nth(1)?.parse().ok())
        .unwrap_or(-1)
}

/// One thread decodes, which is all the one CPU can do at a time. A newer job for the screen goes ahead of any neighbours waiting.
fn decode_one(job: &Job, tx: &Sender<Msg>) {
    let t0 = Instant::now();
    let bytes = match std::fs::read(&job.path) {
        Ok(b) => b,
        Err(e) => {
            tx.send(Msg::Failed { path: job.path.clone(), error: format!("Cannot read this file: {e}") });
            return;
        }
    };
    let read_ms = t0.elapsed().as_millis();
    let t1 = Instant::now();
    let (sw, sh) = job.screen;
    let bg = BG;
    let on_pass = |img: Image| {
        let fitted = flattened(img, bg).fit_within(sw, sh);
        tx.send(Msg::Pass { path: job.path.clone(), fitted });
    };
    let img = match load(&bytes, on_pass) {
        Ok(i) => flattened(i, bg),
        Err(e) => {
            tx.send(Msg::Failed { path: job.path.clone(), error: format!("Cannot show this picture: {e}") });
            return;
        }
    };
    let decode_ms = t1.elapsed().as_millis();
    let t2 = Instant::now();
    let fitted = img.fit_within(sw, sh);
    let fit_ms = t2.elapsed().as_millis();
    let stats = Stats { read_ms, decode_ms, fit_ms, size: (img.w, img.h), bytes: bytes.len() as u64 };
    drop(bytes);
    tx.send(Msg::Ready { path: job.path.clone(), fitted, full: job.keep_full.then_some(img), stats });
}

fn start_worker(tx: Sender<Msg>) -> mpsc::Sender<Job> {
    let (jobs, rx) = mpsc::channel::<Job>();
    std::thread::spawn(move || {
        let mut waiting: Vec<Job> = Vec::new();
        loop {
            if waiting.is_empty() {
                match rx.recv() {
                    Ok(j) => waiting.push(j),
                    Err(_) => return,
                }
            }
            while let Ok(j) = rx.try_recv() {
                waiting.push(j);
            }
            // The newest picture for the screen first; an older one for the screen has been walked past.
            let at = waiting.iter().rposition(|j| j.keep_full).unwrap_or(0);
            let job = waiting.remove(at);
            let mut dropped = Vec::new();
            waiting.retain(|j| {
                if j.keep_full {
                    dropped.push(j.path.clone());
                }
                !j.keep_full
            });
            for path in dropped {
                tx.send(Msg::Dropped { path });
            }
            decode_one(&job, &tx);
        }
    });
    jobs
}

fn main() {
    let target = std::env::args_os().nth(1).map(PathBuf::from);
    let (w, h) = lucina_app::fb::Fb::open(&std::env::var("LUCINA_FB").unwrap_or_else(|_| "/dev/fb0".into())).map_or((1920, 1080), |f| (f.width, f.height));
    lucina_app::run("lucina-app-viewer", move |tx| {
        let jobs = start_worker(tx);
        Viewer::new(Some(jobs), (w, h), target)
    });
}

#[cfg(test)]
mod tests;
