//! `web/src/views/Drive.tsx`: the roots, a folder, an album, search, type filter, sort, and a file's preview. Made for the keyboard: a list,
//! Enter opens, Backspace goes up, `/` searches, `s` and `o` sort, `d` downloads, Escape closes what is open.
//!
//! Keys, as Explorer and Finder have them. In the list: Up and Down, Page Up and Page Down, Home and End move the selection and stop
//! at the ends (`lucina-nav`: no wrap); Enter or Space opens; Backspace or Alt+Left goes up a folder; Left goes to the file types
//! and Right comes back. In an open file: Left and Right show the previous and next file of the list, Home and End the first and
//! last (not in a text or archive, which keep them for scrolling), Space plays a video, Escape, Backspace or Alt+Left close it and
//! leave that file selected in the list. A zoomed picture keeps its arrows for panning.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use lucina_account::ui::SignInView;
use lucina_app::program::{Ended, Program};
use lucina_app::time::Zone;
use lucina_app::{App, Face, Image, ListState, Rect, Rgb, Sender, TextEdit, Ui, keys};
use lucina_nav as nav;
use lucina_files::drive::{self, ArchiveListing, Crumb, DRIVE_SORTS, DRIVE_TYPES, DriveItem, DrivePreview, DriveRoots, DriveSort, DriveType, Filters, FolderEntry, Token};
use lucina_files::media::Connect;
use lucina_files::picker::{self, FilePicker, Mode, Picked, Setup};
use lucina_files::pictures::{Done, Fit, Pictures, Want};
use lucina_files::places;
use lucina_picture_view::PictureView;

use crate::server::{Job, Msg, Next, Page, TextAnswer};

pub type Link = Box<dyn FnMut(Job)>;

/// `TEXT_RETRY_MS`, `TEXT_RETRIES`.
const TEXT_RETRY: Duration = Duration::from_secs(3);
const TEXT_RETRIES: u32 = 60;
const DEBOUNCE: Duration = Duration::from_millis(300);
/// Row pictures kept decoded. They are small; a screen holds about twenty.
const THUMBS_KEPT: usize = 96;
/// Next page when the selection is this close to the end.
const MORE_AHEAD: usize = 20;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Context {
    Roots,
    Folder { folder: String, path: String },
    Album { id: String, title: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    Root(usize),
    Collection(usize),
    Folder(usize),
    File(usize),
    More,
}

pub enum Body {
    Loading,
    /// 202: "Still reading this file…", asked again every 3 s.
    Pending(u32),
    Text { language: Option<String>, truncated: bool, bytes: Option<u64>, lines: Vec<Vec<Token>> },
    NoText,
    Failed,
    Archive(ArchiveListing),
    /// The server cannot list this kind of archive.
    Unsupported,
    /// The picture is on its way, or here. `None` with no url: nothing to show.
    Picture { url: Option<String>, image: Option<Image>, failed: bool },
    /// Video or audio, with its poster.
    Play { url: Option<String>, image: Option<Image> },
    Info { note: bool },
}

pub struct Preview {
    pub item: DriveItem,
    /// A PDF or Office file with a first-page picture: the picture or the text.
    pub as_text: bool,
    pub body: Body,
    pub lines: ListState,
    pub token: u64,
    pub focused: bool,
    /// A picture's zoom and pan.
    pub surface: PictureView,
}

/// A picture larger than the screen, for zooming.
const ZOOM_FIT: u32 = 8192;

pub struct Drive {
    tx: Option<Sender<Msg>>,
    link: Option<Link>,
    connect: Option<Connect>,
    sample: bool,
    zone: Zone,
    sign_in: Option<SignInView>,
    pub signed_out: bool,
    pub context: Context,
    pub filters: Filters,
    pub search: TextEdit,
    search_at: Option<Instant>,
    pub roots: Option<Result<DriveRoots, String>>,
    pub crumbs: Vec<Crumb>,
    pub folders: Vec<FolderEntry>,
    pub files: Vec<DriveItem>,
    pub total: Option<u64>,
    next: Next,
    pub loading: bool,
    pub error: String,
    pub(crate) generation: u64,
    pub list: ListState,
    types: ListState,
    pub preview: Option<Preview>,
    next_token: u64,
    pictures: Option<Pictures>,
    thumbs: HashMap<String, Image>,
    thumb_order: VecDeque<String>,
    wanted: Vec<String>,
    pub picker: Option<FilePicker>,
    download: Option<DriveItem>,
    pub toast: String,
    focus_list: bool,
    focus_search: bool,
    /// The row selected in each folder above this one.
    trail: Vec<usize>,
    /// The row to select when the listing arrives: the list has no rows until then, so it would forget.
    come_back_to: Option<usize>,
    make_sign_in: Option<Box<dyn FnMut() -> Option<SignInView>>>,
    make_worker: Option<Box<dyn FnMut() -> Option<Link>>>,
    /// Intents whose answer waits for the server, answered from `message`.
    pub(crate) pending: Vec<crate::intents::Pending>,
}

impl Drive {
    pub fn new(zone: Zone, tx: Option<Sender<Msg>>, link: Option<Link>, connect: Option<Connect>) -> Drive {
        let mut types = ListState::new();
        types.select(0);
        let mut d = Drive {
            tx,
            signed_out: false,
            link,
            connect,
            sample: false,
            zone,
            sign_in: None,
            context: Context::Roots,
            filters: Filters::NONE,
            search: TextEdit::new().placeholder("Search the Drive"),
            search_at: None,
            roots: None,
            crumbs: Vec::new(),
            folders: Vec::new(),
            files: Vec::new(),
            total: None,
            next: Next::None,
            loading: false,
            error: String::new(),
            generation: 0,
            list: ListState::new(),
            types,
            preview: None,
            next_token: 0,
            pictures: None,
            thumbs: HashMap::new(),
            thumb_order: VecDeque::new(),
            wanted: Vec::new(),
            picker: None,
            download: None,
            toast: String::new(),
            focus_list: true,
            focus_search: false,
            trail: Vec::new(),
            come_back_to: None,
            make_sign_in: None,
            make_worker: None,
            pending: Vec::new(),
        };
        d.start_pictures();
        d
    }

    pub fn sample(mut self) -> Drive {
        self.sample = true;
        self
    }

    pub fn with_sign_in(mut self, s: Box<dyn FnMut() -> Option<SignInView>>, w: Box<dyn FnMut() -> Option<Link>>) -> Drive {
        self.make_sign_in = Some(s);
        self.make_worker = Some(w);
        self
    }

    fn start_pictures(&mut self) {
        if let (Some(tx), Some(connect)) = (self.tx.clone(), self.connect.clone()) {
            let cache = places::cache_dir("drive").map(|d| Arc::new(lucina_files::cache::DiskCache::new(d.join("pictures"), 512 << 20)));
            self.pictures = Some(Pictures::start(connect, cache, move |d| tx.send(Msg::Picture(d))));
        }
    }

    fn send(&mut self, job: Job) {
        if let Some(l) = &mut self.link {
            l(job);
        }
    }

    pub fn start(&mut self) {
        if self.link.is_none() {
            self.begin_sign_in();
            return;
        }
        self.reload();
    }

    fn begin_sign_in(&mut self) {
        if self.sign_in.is_none()
            && let Some(make) = &mut self.make_sign_in
        {
            self.sign_in = make();
        }
    }

    pub fn searching(&self) -> bool {
        !self.filters.q.trim().is_empty()
    }

    fn root_search(&self) -> bool {
        self.context == Context::Roots && (self.searching() || self.filters.kind.is_some())
    }

    /// `showRoots`.
    pub fn show_roots(&self) -> bool {
        self.context == Context::Roots && !self.root_search()
    }

    /// `load(false)`: from the first page, what the context and the filters say.
    pub fn reload(&mut self) {
        self.generation += 1;
        self.folders.clear();
        self.files.clear();
        self.next = Next::None;
        self.total = None;
        self.error.clear();
        self.list = ListState::new();
        let generation = self.generation;
        if self.show_roots() {
            self.loading = false;
            self.roots = None;
            self.send(Job::Roots { generation });
            return;
        }
        self.loading = true;
        let filters = self.filters.clone();
        let context = self.context.clone();
        self.send(Job::List { generation, context, filters, after: Next::None, more: false });
    }

    /// `load(true)`.
    fn load_more(&mut self) {
        if self.loading || matches!(self.next, Next::None) {
            return;
        }
        self.loading = true;
        let (generation, filters, context, after) = (self.generation, self.filters.clone(), self.context.clone(), self.next.clone());
        self.send(Job::List { generation, context, filters, after, more: true });
    }

    pub fn rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        if self.show_roots() {
            if let Some(Ok(r)) = &self.roots {
                rows.extend((0..r.folders.len()).map(Row::Root));
                rows.extend((0..r.albums.len()).map(Row::Collection));
            }
            return rows;
        }
        rows.extend((0..self.folders.len()).map(Row::Folder));
        rows.extend((0..self.files.len()).map(Row::File));
        if !matches!(self.next, Next::None) {
            rows.push(Row::More);
        }
        rows
    }

    /// `openFolder`: the filters stay, the search does not.
    pub fn open_folder(&mut self, folder: String, path: String) {
        self.trail.push(self.list.selected.unwrap_or(0));
        self.context = Context::Folder { folder, path };
        self.clear_search();
        self.focus_list = true;
        self.reload();
    }

    fn open_album(&mut self, id: String, title: String) {
        self.trail.push(self.list.selected.unwrap_or(0));
        self.context = Context::Album { id, title };
        self.clear_search();
        self.focus_list = true;
        self.reload();
    }

    fn clear_search(&mut self) {
        self.filters.q.clear();
        self.search.clear();
        self.search_at = None;
    }

    pub fn up(&mut self) {
        let back = self.trail.pop();
        match self.context.clone() {
            Context::Roots => return,
            Context::Album { .. } => self.context = Context::Roots,
            Context::Folder { folder, path } => {
                self.context = if path.is_empty() {
                    Context::Roots
                } else {
                    let parent = path.rfind('/').map_or(String::new(), |i| path[..i].to_owned());
                    Context::Folder { folder, path: parent }
                };
            }
        }
        self.clear_search();
        self.focus_list = true;
        self.reload();
        self.come_back_to = back;
    }

    pub fn set_kind(&mut self, kind: Option<DriveType>) {
        if self.filters.kind != kind {
            self.filters.kind = kind;
            self.reload();
        }
    }

    /// The sort in force: `filters().sort ?? (searching() ? "modified" : "name")`.
    pub fn sort(&self) -> DriveSort {
        self.filters.sort.unwrap_or(if self.searching() { DriveSort::Modified } else { DriveSort::Name })
    }

    pub fn ascending(&self) -> bool {
        self.filters.ascending.unwrap_or_else(|| self.sort().default_ascending())
    }

    /// Only folders sort, as on the web.
    fn is_folder(&self) -> bool {
        matches!(self.context, Context::Folder { .. })
    }

    pub fn next_sort(&mut self) {
        if !self.is_folder() {
            return;
        }
        let at = DRIVE_SORTS.iter().position(|s| *s == self.sort()).unwrap_or(0);
        let sort = DRIVE_SORTS[(at + 1) % DRIVE_SORTS.len()];
        self.filters.sort = Some(sort);
        self.filters.ascending = Some(sort.default_ascending());
        self.reload();
    }

    pub fn flip_order(&mut self) {
        if !self.is_folder() {
            return;
        }
        self.filters.sort = Some(self.sort());
        self.filters.ascending = Some(!self.ascending());
        self.reload();
    }

    pub fn activate(&mut self, row: Row) {
        match row {
            Row::Root(i) => {
                let Some(Ok(r)) = &self.roots else { return };
                let folder = r.folders[i].folder.clone();
                self.open_folder(folder, String::new());
            }
            Row::Collection(i) => {
                let Some(Ok(r)) = &self.roots else { return };
                let (id, title) = (r.albums[i].id.clone(), r.albums[i].title.clone());
                self.open_album(id, title);
            }
            Row::Folder(i) => {
                let path = self.folders[i].path.clone();
                let folder = match &self.context {
                    Context::Folder { folder, .. } => folder.clone(),
                    _ => return,
                };
                self.open_folder(folder, path);
            }
            Row::File(i) => {
                let item = self.files[i].clone();
                self.open_preview(item);
            }
            Row::More => self.load_more(),
        }
    }

    pub fn open_preview(&mut self, item: DriveItem) {
        self.next_token += 1;
        let token = self.next_token;
        let mut p = Preview { item, as_text: false, body: Body::Loading, lines: ListState::new(), token, focused: false, surface: PictureView::new() };
        p.lines.plain = true;
        self.preview = Some(p);
        self.toast.clear();
        self.load_body();
    }

    /// `canToggle`, where the first view is the first-page picture rather than a PDF.
    fn has_page_picture(item: &DriveItem) -> bool {
        matches!(item.preview(), DrivePreview::Pdf | DrivePreview::Office) && item.urls.large.is_some() && item.thumb_state.as_deref() == Some("ready")
    }

    pub fn can_toggle(item: &DriveItem) -> bool {
        Self::has_page_picture(item) && item.urls.text.is_some()
    }

    /// `PreviewBody`'s `Switch`, on a machine with no PDF renderer: the first page as a picture, and the text.
    fn load_body(&mut self) {
        let Some(p) = &mut self.preview else { return };
        let item = p.item.clone();
        let token = p.token;
        let preview = item.preview();
        let text_first = p.as_text || !Self::has_page_picture(&item);
        let job = match preview {
            DrivePreview::Pdf | DrivePreview::Office | DrivePreview::Text | DrivePreview::Code if text_first => match &item.urls.text {
                Some(url) => {
                    p.body = Body::Loading;
                    Some(Job::Text { token, url: url.clone() })
                }
                None => {
                    p.body = Body::Info { note: true };
                    None
                }
            },
            DrivePreview::Pdf | DrivePreview::Office => {
                p.body = Body::Picture { url: item.urls.large.clone(), image: None, failed: false };
                None
            }
            DrivePreview::Archive => match &item.urls.archive {
                Some(url) => {
                    p.body = Body::Loading;
                    Some(Job::Archive { token, url: url.clone() })
                }
                None => {
                    p.body = Body::Unsupported;
                    None
                }
            },
            DrivePreview::Image => {
                let url = item.picture().map(str::to_owned);
                p.body = match url {
                    Some(u) => Body::Picture { url: Some(u), image: None, failed: false },
                    None => Body::Info { note: true },
                };
                None
            }
            DrivePreview::Video | DrivePreview::Audio => {
                p.body = Body::Play { url: item.urls.poster.clone().or_else(|| item.urls.large.clone()), image: None };
                None
            }
            _ => {
                p.body = Body::Info { note: false };
                None
            }
        };
        if let Some(j) = job {
            self.send(j);
        }
    }

    pub fn toggle_text(&mut self) {
        let Some(p) = &mut self.preview else { return };
        if Self::can_toggle(&p.item) {
            p.as_text = !p.as_text;
            p.lines = ListState::new();
            p.lines.plain = true;
            self.load_body();
        }
    }

    /// `d`: where to put it. The picker asks; the bytes then go straight to the file.
    pub fn start_download(&mut self, item: DriveItem) {
        let Some(wake) = self.wake() else { return };
        let setup = Setup {
            title: format!("Download {}", item.name()),
            mode: Mode::Save { name: picker::safe_name(item.name()) },
            places: {
                let mut p = places::standard();
                p.rotate_left(1);
                p
            },
            drive: None,
            fetch_to: None,
        };
        self.download = Some(item);
        self.picker = Some(FilePicker::start(setup, wake));
    }

    fn wake(&self) -> Option<Arc<dyn Fn() + Send + Sync>> {
        let tx = self.tx.clone();
        if self.tx.is_none() && !self.sample {
            return Some(Arc::new(|| {}));
        }
        tx.map(|tx| Arc::new(move || tx.send(Msg::Changed)) as Arc<dyn Fn() + Send + Sync>)
    }

    fn picked(&mut self, p: Picked) {
        self.picker = None;
        self.focus_list = true;
        let Some(item) = self.download.take() else { return };
        let Picked::File(dest) = p else { return };
        self.toast = format!("Downloading {}…", item.name());
        lucina_app::events::emit("download.started")
            .subject(&format!("file:{}", dest.display()))
            .field("name", item.name())
            .send();
        self.send(Job::Download { url: item.download(), dest });
    }

    fn keep_thumb(&mut self, url: String, img: Image) {
        if self.thumbs.insert(url.clone(), img).is_none() {
            self.thumb_order.push_back(url);
        }
        while self.thumb_order.len() > THUMBS_KEPT {
            if let Some(old) = self.thumb_order.pop_front() {
                if !self.wanted.contains(&old) {
                    self.thumbs.remove(&old);
                } else {
                    self.thumb_order.push_back(old);
                    break;
                }
            }
        }
    }

    /// What the screen shows now, nearest first: the preview's picture, then the rows in view.
    fn want_pictures(&mut self, rows: &[Row], first: usize, count: usize, side: u32, screen: (u32, u32)) {
        let mut wants = Vec::new();
        if let Some(p) = &self.preview {
            let url = match &p.body {
                Body::Picture { url: Some(u), image: None, failed: false } => Some(u.clone()),
                Body::Play { url: Some(u), image: None } => Some(u.clone()),
                _ => None,
            };
            if let Some(url) = url {
                wants.push(Want { url, fit: Fit::Within(screen.0, screen.1), rotation: 0 });
            }
            // Zooming: the same picture, unfitted.
            if let (true, None, Body::Picture { url: Some(u), .. }) = (p.surface.is_zoomed(), p.surface.source(), &p.body) {
                wants.push(Want { url: u.clone(), fit: Fit::Within(ZOOM_FIT, ZOOM_FIT), rotation: 0 });
            }
        }
        for r in rows.iter().skip(first).take(count) {
            if let Row::File(i) = r
                && let Some(u) = self.files.get(*i).and_then(|f| f.thumbnail())
                && !self.thumbs.contains_key(u)
            {
                wants.push(Want { url: u.to_owned(), fit: Fit::Square(side), rotation: 0 });
            }
        }
        let urls: Vec<String> = wants.iter().map(|w| w.url.clone()).collect();
        if urls != self.wanted {
            self.wanted = urls;
            if let Some(p) = &self.pictures {
                p.want(wants);
            }
        }
    }

    fn signed_out_now(&mut self) {
        self.signed_out = true;
        self.link = None;
        self.preview = None;
        self.picker = None;
        // What was fetched for this account goes with it.
        if let Some(p) = &self.pictures {
            p.clear_cache();
        }
        self.thumbs.clear();
        self.thumb_order.clear();
        crate::account::rejected();
        self.begin_sign_in();
    }
}

impl App for Drive {
    type Msg = Msg;

    fn intents() -> &'static [lucina_app::intent::IntentSpec] {
        crate::intents::INTENTS
    }

    fn intent(&mut self, call: lucina_app::intent::Call, reply: lucina_app::intent::Responder) {
        crate::intents::intent(self, call, reply);
    }

    fn message(&mut self, m: Msg) {
        match m {
            Msg::Changed => {}
            Msg::SignedIn => {
                self.sign_in = None;
                self.signed_out = false;
                if let Some(make) = &mut self.make_worker {
                    self.link = make();
                }
                self.reload();
            }
            Msg::SignedOut => self.signed_out_now(),
            Msg::Roots { generation, result } => {
                if generation == self.generation {
                    self.roots = Some(result);
                    // Not out of the search field: someone may be typing there.
                    self.focus_list |= self.search.text.is_empty();
                    if let Some(i) = self.come_back_to.take() {
                        self.list.select(i);
                    }
                }
            }
            Msg::Page { generation, more, result } => {
                if generation != self.generation {
                    return;
                }
                self.loading = false;
                match result {
                    Ok(Page { crumbs, folders, files, total, next }) => {
                        if more {
                            self.files.extend(files);
                        } else {
                            self.folders = folders;
                            self.files = files;
                            self.crumbs = crumbs;
                            if let Some(i) = self.come_back_to.take() {
                                self.list.select(i);
                            } else if self.list.selected.is_none() && (self.folders.len() + self.files.len()) > 0 {
                                self.list.select(0);
                            }
                        }
                        self.total = total;
                        self.next = next;
                    }
                    Err(e) => self.error = e,
                }
            }
            Msg::Text { token, answer } => {
                let Some(p) = &mut self.preview else { return };
                if p.token != token {
                    return;
                }
                match answer {
                    TextAnswer::Pending => {
                        let attempt = match p.body {
                            Body::Pending(n) => n + 1,
                            _ => 1,
                        };
                        p.body = Body::Pending(attempt);
                        if attempt < TEXT_RETRIES {
                            let url = p.item.urls.text.clone().unwrap_or_default();
                            self.send(Job::TextAgain { token, url, after: TEXT_RETRY });
                        }
                    }
                    TextAnswer::Ready { language, truncated, bytes, lines } => {
                        p.body = if lines.is_empty() { Body::NoText } else { Body::Text { language, truncated, bytes, lines } };
                        p.focused = false;
                    }
                    TextAnswer::Empty => p.body = Body::NoText,
                    TextAnswer::Failed => p.body = Body::Failed,
                }
            }
            Msg::Archive { token, result } => {
                let Some(p) = &mut self.preview else { return };
                if p.token != token {
                    return;
                }
                p.body = match result {
                    Ok(l) => Body::Archive(l),
                    Err(Some(415)) => Body::Unsupported,
                    Err(_) => Body::Failed,
                };
                p.focused = false;
            }
            Msg::Picture(done) => match done {
                Done::Ready(want, img) => {
                    if let Some(p) = &mut self.preview {
                        if want.fit == Fit::Within(ZOOM_FIT, ZOOM_FIT) {
                            p.surface.set_source(img);
                            return;
                        }
                        match &mut p.body {
                            Body::Picture { url: Some(u), image, .. } | Body::Play { url: Some(u), image } if *u == want.url => {
                                *image = Some(img);
                                return;
                            }
                            _ => {}
                        }
                    }
                    if matches!(want.fit, Fit::Square(_)) {
                        self.keep_thumb(want.url, img);
                    }
                }
                Done::Failed(want, _) => {
                    if let Some(p) = &mut self.preview
                        && let Body::Picture { url: Some(u), failed, .. } = &mut p.body
                        && *u == want.url
                        && want.fit != Fit::Within(ZOOM_FIT, ZOOM_FIT)
                    {
                        *failed = true;
                    }
                }
                Done::SignedOut => self.signed_out_now(),
            },
            Msg::Download(r) => {
                // Lucina waits for this instead of watching the toast (stage D).
                match &r {
                    Ok(p) => lucina_app::events::emit("download.done").subject(&format!("file:{}", p.display())).send(),
                    Err(e) => lucina_app::events::emit("download.failed").field("error", e.to_string()).send(),
                }
                self.toast = match r {
                    Ok(p) => format!("Saved in {}", p.display()),
                    Err(e) => format!("Not saved: {e}"),
                };
            }
            Msg::DownloadProgress(bytes) => {
                self.toast = format!("Downloading… {}", drive::format_bytes(Some(bytes)));
            }
        }
        crate::intents::answer_pending(self);
    }

    fn view(&mut self, ui: &mut Ui) {
        ui.header(if self.sample { "Drive: sample data" } else { "Drive" });
        ui.pad(10.0);
        if let Some(view) = &mut self.sign_in {
            view.view(ui, "Sign in to see your Drive.");
            return;
        }
        if self.picker.is_some() {
            let got = self.picker.as_mut().and_then(|p| p.view(ui));
            if let Some(p) = got {
                self.picked(p);
            }
            return;
        }
        if self.preview.is_some() {
            self.preview_view(ui);
            return;
        }
        self.browse_view(ui);
    }
}

fn row_meta(ui: &mut Ui, a: Rect, top: i32, text: &str, c: Rgb) {
    let t = ui.theme();
    ui.text_in(Rect::new(a.x, top, a.w, t.px(16.0)), Face::Sans, t.pt(12.0), c, text);
}

/// `FileGlyph`: the picture when there is one, else a page with the type's badge.
fn glyph(ui: &mut Ui, r: Rect, kind: DriveType, ext: &str, thumb: Option<&Image>) {
    if let Some(img) = thumb {
        ui.image_in(r, img);
        return;
    }
    let t = ui.theme();
    ui.fill_round(r.inset(t.px(3.0)), t.px(4.0), Rgb(drive::type_tone(kind)));
    let badge = drive::type_badge(kind, ext);
    if !badge.is_empty() {
        let w = ui.measure(Face::Bold, t.pt(10.0), &badge);
        ui.text_in(Rect::new(r.x + (r.w - w) / 2, r.y, w + 2, r.h), Face::Bold, t.pt(10.0), Rgb(0xff_ff_ff), &badge);
    }
}

fn folder_glyph(ui: &mut Ui, r: Rect) {
    let t = ui.theme();
    let tab = Rect::new(r.x + t.px(4.0), r.y + t.px(8.0), r.w / 2 - t.px(4.0), t.px(6.0));
    ui.fill_round(tab, t.px(2.0), t.accent);
    ui.fill_round(Rect::new(r.x + t.px(4.0), r.y + t.px(12.0), r.w - t.px(8.0), r.h - t.px(22.0)), t.px(3.0), t.accent);
}

/// A token's colour (`tokenClass` and the web's `tok-*` styles).
fn token_colour(ui: &Ui, cls: &str) -> Rgb {
    let t = ui.theme();
    match cls {
        "kw" | "tag" => Rgb(0xc7_92_ea),
        "str" | "attr" => Rgb(0xa5_d6_7f),
        "com" => t.dim,
        "num" | "const" => Rgb(0xf7_8c_6c),
        "fn" | "head" => Rgb(0x82_aa_ff),
        "type" | "var" => Rgb(0xff_cb_6b),
        "punct" => t.item,
        "em" => t.accent,
        _ => t.text,
    }
}

impl Drive {
    fn crumbs_bar(&mut self, ui: &mut Ui, r: Rect) {
        let t = ui.theme();
        let px = t.pt(14.0);
        let mut x = r.x;
        let mut go: Option<usize> = None;
        let names: Vec<String> = match &self.context {
            Context::Roots => Vec::new(),
            Context::Album { title, .. } => vec![title.clone()],
            Context::Folder { folder, path } => {
                if self.crumbs.is_empty() { drive::breadcrumbs(folder, path).into_iter().map(|c| c.name).collect() } else { self.crumbs.iter().map(|c| c.name.clone()).collect() }
            }
        };
        let n = names.len();
        let root_w = ui.measure(Face::Bold, px, "Drive");
        ui.text_in(Rect::new(x, r.y, root_w, r.h), Face::Bold, px, if n == 0 { t.text } else { t.accent }, "Drive");
        if ui.hit("crumb-root", Rect::new(x, r.y, root_w, r.h)).clicked {
            go = Some(usize::MAX);
        }
        x += root_w;
        for (i, name) in names.iter().enumerate() {
            let sep = " › ";
            let sw = ui.measure(Face::Sans, px, sep);
            ui.text_in(Rect::new(x, r.y, sw, r.h), Face::Sans, px, t.dim, sep);
            x += sw;
            let w = ui.measure(Face::Sans, px, name);
            let last = i + 1 == n;
            ui.text_in(Rect::new(x, r.y, (r.right() - x).max(0), r.h), Face::Sans, px, if last { t.text } else { t.item }, name);
            if ui.hit(("crumb", i), Rect::new(x, r.y, w, r.h)).clicked && !last {
                go = Some(i);
            }
            x += w;
            if x > r.right() {
                break;
            }
        }
        match go {
            Some(usize::MAX) => {
                self.trail.clear();
                self.context = Context::Roots;
                self.clear_search();
                self.reload();
            }
            Some(i) => {
                if let (Context::Folder { folder, .. }, Some(c)) = (self.context.clone(), self.crumbs.get(i).cloned()) {
                    self.open_folder(folder, c.path);
                }
            }
            None => {}
        }
    }

    fn browse_view(&mut self, ui: &mut Ui) {
        let t = ui.theme();
        let area = ui.area();
        let side_w = t.px(150.0);
        let gap = t.px(12.0);
        let side = Rect::new(area.x + t.px(10.0), area.y, side_w, area.h - t.px(8.0));
        let main = Rect::new(side.right() + gap, area.y, area.w - side_w - gap - t.px(20.0), area.h - t.px(8.0));

        // The file types.
        let filter = |i: usize| if i == 0 { None } else { Some(DRIVE_TYPES[i - 1]) };
        let res = ui.list("types", &mut self.types, side, DRIVE_TYPES.len() + 1, 28.0, |ui, i, selected| {
            let a = ui.area();
            let label = filter(i).map_or("All", DriveType::filter_label);
            let c = if selected { t.text } else { t.item };
            ui.text_in(a.inset(t.px(4.0)), Face::Sans, t.pt(14.0), c, label);
        });
        if res.selection_changed || res.activated.is_some() {
            let i = res.activated.or(self.types.selected).unwrap_or(0);
            self.set_kind(filter(i));
            if res.activated.is_some() {
                self.focus_list = true;
            }
        }

        // The bar.
        let mut column = main;
        let bar = Rect::new(column.x, column.y, column.w, t.px(34.0));
        column = Rect::new(column.x, bar.bottom() + t.px(4.0), column.w, column.h - bar.h - t.px(4.0));
        let sort_w = if self.is_folder() { t.px(150.0) } else { 0 };
        let field = Rect::new(bar.x, bar.y, bar.w - sort_w - if sort_w > 0 { t.px(8.0) } else { 0 }, bar.h);
        let placeholder = match &self.context {
            Context::Roots => "Search the Drive".to_owned(),
            Context::Album { title, .. } => format!("Search in {title}"),
            Context::Folder { folder, path } => format!("Search in {}", path.rsplit('/').next().filter(|s| !s.is_empty()).unwrap_or(folder)),
        };
        self.search.placeholder = placeholder;
        let res = ui.text_field_at(field, "search", &mut self.search);
        if res.changed {
            self.search_at = Some(Instant::now() + DEBOUNCE);
        }
        if res.submitted {
            self.search_at = Some(Instant::now());
            self.focus_list = true;
        }
        if ui.is_focused("search") && ui.key(keys::KEY_ESC) {
            self.clear_search_field();
            self.focus_list = true;
        }
        if let Some(at) = self.search_at {
            let now = Instant::now();
            if now >= at {
                self.search_at = None;
                let q = self.search.text.trim().to_owned();
                if q != self.filters.q.trim() {
                    self.filters.q = q;
                    self.reload();
                }
            } else {
                ui.wake_after(at - now);
            }
        }
        if sort_w > 0 {
            let arrow = if self.ascending() { "↑" } else { "↓" };
            let label = format!("{} {arrow}", self.sort().label());
            if ui.button_at(Rect::new(bar.right() - sort_w, bar.y, sort_w, bar.h), "sort", &label) {
                self.next_sort();
            }
        }

        // Where we are, and how many.
        let crumb = Rect::new(column.x, column.y, column.w, t.px(26.0));
        column = Rect::new(column.x, crumb.bottom(), column.w, column.h - crumb.h);
        self.crumbs_bar(ui, crumb);

        // A line for what is going on.
        let mut note = String::new();
        if !self.error.is_empty() {
            note = self.error.clone();
        } else if let Some(Err(e)) = &self.roots
            && self.show_roots()
        {
            note = e.clone();
        } else if self.loading || (self.show_roots() && self.roots.is_none()) {
            note = "Loading…".to_owned();
        } else if (self.searching() || self.root_search()) && self.total.is_some() {
            let n = self.total.unwrap_or(0);
            note = if n == 1 { "1 result".to_owned() } else { format!("{n} results") };
        }
        if !self.toast.is_empty() {
            note = self.toast.clone();
        }
        if !note.is_empty() {
            let line = Rect::new(column.x, column.y, column.w, t.px(20.0));
            column = Rect::new(column.x, line.bottom(), column.w, column.h - line.h);
            let c = if !self.error.is_empty() { t.danger } else { t.dim };
            ui.text_in(line, Face::Sans, t.pt(13.0), c, &note);
        }

        // The rows.
        let rows = self.rows();
        let showing_location = self.searching() || self.root_search();
        let row_h = if showing_location { 58.0 } else { 44.0 };
        let thumb_side = t.px(36.0) as u32;
        {
            let zone = self.zone.clone();
            let (roots, folders, files, thumbs) = (&self.roots, &self.folders, &self.files, &self.thumbs);
            let res = ui.list("rows", &mut self.list, column, rows.len(), row_h, |ui, i, selected| {
                let a = ui.area();
                let glyph_rect = Rect::new(a.x + t.px(6.0), a.y + (a.h - t.px(36.0)) / 2, t.px(36.0), t.px(36.0));
                let text = Rect::new(glyph_rect.right() + t.px(10.0), a.y, a.w - t.px(58.0), a.h);
                let name_c = if selected { t.text } else { t.item };
                let (name, meta): (String, String) = match rows[i] {
                    Row::Root(n) => {
                        folder_glyph(ui, glyph_rect);
                        let Some(Ok(r)) = roots else { return };
                        let f = &r.folders[n];
                        (f.folder.clone(), drive::folder_detail(f.files, f.bytes, f.modified.as_deref(), &zone))
                    }
                    Row::Collection(n) => {
                        folder_glyph(ui, glyph_rect);
                        let Some(Ok(r)) = roots else { return };
                        let al = &r.albums[n];
                        let count = al.count.map_or(String::new(), |c| if c == 1 { "1 item".to_owned() } else { format!("{c} items") });
                        (al.title.clone(), count)
                    }
                    Row::Folder(n) => {
                        folder_glyph(ui, glyph_rect);
                        let f = &folders[n];
                        (f.name.clone(), drive::folder_detail(f.files, f.bytes, f.modified.as_deref(), &zone))
                    }
                    Row::File(n) => {
                        let f = &files[n];
                        let thumb = f.thumbnail().and_then(|u| thumbs.get(u));
                        let ext = f.drive.as_ref().map_or(String::new(), |d| d.ext.clone());
                        glyph(ui, glyph_rect, f.file_type(), &ext, thumb);
                        (f.name().to_owned(), f.meta(&zone))
                    }
                    Row::More => {
                        ui.text_in(a.inset(t.px(10.0)), Face::Bold, t.pt(14.0), t.accent, "Load more");
                        return;
                    }
                };
                ui.text_in(Rect::new(text.x, a.y + t.px(5.0), text.w, t.px(19.0)), Face::Sans, t.pt(15.0), name_c, &name);
                row_meta(ui, text, a.y + t.px(24.0), &meta, t.dim);
                if showing_location && let Row::File(n) = rows[i] {
                    let f = &files[n];
                    let line = f.snippet.clone().unwrap_or_else(|| f.location());
                    row_meta(ui, text, a.y + t.px(40.0), &line, t.item);
                }
            });
            if let Some(i) = res.activated
                && let Some(r) = rows.get(i)
            {
                let r = *r;
                self.activate(r);
            }
            if res.selection_changed && let Some(s) = self.list.selected {
                if rows.len().saturating_sub(s) < MORE_AHEAD {
                    self.load_more();
                }
            }
        }
        if self.list.scroll > 0 {
            // A wheel can reach the end with the selection left behind.
            let bottom = (self.list.scroll + column.h) / t.px(row_h).max(1);
            if rows.len().saturating_sub(bottom as usize) < MORE_AHEAD / 2 {
                self.load_more();
            }
        }
        // `EmptyState`.
        if rows.is_empty() && !self.loading && self.error.is_empty() {
            let text = if self.show_roots() {
                self.roots.as_ref().and_then(|r| r.as_ref().ok()).map(|_| "Nothing in the Drive yet")
            } else if self.searching() || self.filters.kind.is_some() {
                Some("Nothing found")
            } else {
                Some("This folder is empty")
            };
            if let Some(text) = text {
                ui.text_in(Rect::new(column.x + t.px(10.0), column.y, column.w, t.px(30.0)), Face::Sans, t.pt(15.0), t.dim, text);
            }
        }
        // A pass after a dialog closes still counts as inside it, and forgets a focus given then: until it has taken.
        if self.focus_list {
            if self.list.selected.is_none() && !rows.is_empty() {
                self.list.select(0);
            }
            if ui.is_focused("rows") {
                self.focus_list = false;
            } else {
                ui.focus("rows");
            }
        }
        if self.focus_search {
            self.focus_search = false;
            ui.focus("search");
        }
        // Pictures for the rows in view.
        let first = (self.list.scroll / t.px(row_h).max(1)) as usize;
        let count = (column.h / t.px(row_h).max(1)) as usize + 2;
        let (w, h) = (area.w as u32, area.h as u32);
        self.want_pictures(&rows, first, count, thumb_side, (w, h));
        // Keys nobody took.
        if let Some(p) = ui.press() {
            let plain = !p.mods.ctrl && !p.mods.alt && !p.mods.meta;
            let alt_only = p.mods.alt && !p.mods.ctrl && !p.mods.meta && !p.mods.shift;
            let on_rows = ui.is_focused("rows");
            let on_types = ui.is_focused("types");
            match p.code {
                keys::KEY_BACKSPACE if plain => {
                    ui.take_press();
                    self.up();
                }
                // Alt+Left is "back" in Explorer, Finder and every browser: up a folder.
                keys::KEY_LEFT if alt_only => {
                    ui.take_press();
                    self.up();
                }
                // Left and Right do not walk the focus round the buttons: Left from the rows goes to the file types beside
                // them and Right comes back, as in Explorer's tree and list.
                keys::KEY_LEFT if plain && !p.mods.shift && on_rows => {
                    ui.take_press();
                    ui.focus("types");
                }
                keys::KEY_RIGHT if plain && !p.mods.shift && on_types => {
                    ui.take_press();
                    ui.focus("rows");
                }
                keys::KEY_LEFT | keys::KEY_RIGHT if plain && !p.mods.shift && (on_rows || on_types) => {
                    ui.take_press();
                }
                // Space on a file opens it, as Space shows Quick Look in Finder.
                keys::KEY_SPACE if plain && !p.mods.shift && on_rows => {
                    ui.take_press();
                    if let Some(r @ Row::File(_)) = self.list.selected.and_then(|i| rows.get(i).copied()) {
                        self.activate(r);
                    }
                }
                keys::KEY_ESC if plain => {
                    ui.take_press();
                    if self.searching() {
                        self.clear_search_field();
                    } else {
                        self.up();
                    }
                }
                keys::KEY_SLASH if plain => {
                    ui.take_press();
                    self.focus_search = true;
                }
                keys::KEY_F if p.mods.ctrl => {
                    ui.take_press();
                    self.focus_search = true;
                }
                keys::KEY_S if plain => {
                    ui.take_press();
                    self.next_sort();
                }
                keys::KEY_O if plain => {
                    ui.take_press();
                    self.flip_order();
                }
                keys::KEY_R if plain => {
                    ui.take_press();
                    self.reload();
                }
                keys::KEY_D if plain => {
                    ui.take_press();
                    if let Some(Row::File(n)) = self.list.selected.and_then(|i| rows.get(i).copied()) {
                        let item = self.files[n].clone();
                        self.start_download(item);
                    }
                }
                _ => {}
            }
        }
    }

    fn clear_search_field(&mut self) {
        let had = self.searching();
        self.clear_search();
        if had {
            self.reload();
        }
    }

    /// Back to the list with the file that was open selected (Explorer, Photos and Quick Look all return to it).
    fn close_preview(&mut self) {
        if let Some(p) = self.preview.take()
            && let Some(at) = self.files.iter().position(|f| f.id == p.item.id)
            && let Some(row) = self.rows().iter().position(|r| *r == Row::File(at))
        {
            self.list.select(row);
        }
        self.preview = None;
        self.focus_list = true;
        self.toast.clear();
    }

    /// The open file's neighbour in the list's own order. This viewer shows every kind of file, so it steps over every file;
    /// a viewer of one kind (pictures, videos) would pass `false` for the rest to [`nav::walk`]. The ends stop: Windows Photos
    /// does not wrap, nor does a list selection (`lucina-nav`).
    fn step_preview(&mut self, step: nav::Step) {
        let Some(id) = self.preview.as_ref().map(|p| p.item.id.clone()) else { return };
        let Some(at) = self.files.iter().position(|f| f.id == id) else { return };
        let ok = vec![true; self.files.len()];
        let to = nav::walk(&ok, at, step);
        if self.files.len().saturating_sub(to) < MORE_AHEAD {
            self.load_more();
        }
        if to == at {
            return;
        }
        let item = self.files[to].clone();
        if let Some(row) = self.rows().iter().position(|r| *r == Row::File(to)) {
            self.list.select(row);
        }
        self.open_preview(item);
    }

    /// Keys of an open file that no button took: Left and Right go to the previous and next file, Home and End to the first and
    /// last, Alt+Left closes it. A zoomed picture keeps its arrows for panning, and a text or archive list keeps Home and End.
    fn preview_keys(&mut self, ui: &mut Ui) {
        let Some(p) = ui.press() else { return };
        let Some(open) = self.preview.as_ref() else { return };
        let zoomed = open.surface.is_zoomed();
        let in_list = matches!(open.body, Body::Text { .. } | Body::Archive(_));
        if p.mods.alt && !p.mods.ctrl && !p.mods.meta && !p.mods.shift && p.code == keys::KEY_LEFT {
            ui.take_press();
            self.close_preview();
            return;
        }
        if p.mods.ctrl || p.mods.alt || p.mods.meta || p.mods.shift {
            return;
        }
        let step = match p.code {
            keys::KEY_LEFT if !zoomed => nav::Step::Prev,
            keys::KEY_RIGHT if !zoomed => nav::Step::Next,
            keys::KEY_HOME if !in_list => nav::Step::First,
            keys::KEY_END if !in_list => nav::Step::Last,
            _ => return,
        };
        ui.take_press();
        self.step_preview(step);
    }

    fn preview_view(&mut self, ui: &mut Ui) {
        let t = ui.theme();
        // How the player went, if one ran.
        if let Some(Ended::Failed(why) | Ended::NotStarted(why)) = ui.program_ended() {
            self.toast = why;
        }
        let close;
        let mut toggle = false;
        let mut download = None;
        let mut play = None;
        let zone = self.zone.clone();
        let toast = self.toast.clone();
        let area = ui.area();
        let (sw, sh) = (area.w.max(1) as u32, (area.h - t.px(70.0)).max(1) as u32);
        {
            let Some(p) = &mut self.preview else { return };
            let item = p.item.clone();
            let bar = ui.cut_top(t.px(44.0));
            let glyph_rect = Rect::new(bar.x + t.px(10.0), bar.y + t.px(4.0), t.px(36.0), t.px(36.0));
            let ext = item.drive.as_ref().map_or(String::new(), |d| d.ext.clone());
            glyph(ui, glyph_rect, item.file_type(), &ext, None);
            let is_video = item.preview() == DrivePreview::Video;
            let buttons_w = t.px(if Self::can_toggle(&item) || is_video { 300.0 } else { 200.0 });
            let title = Rect::new(glyph_rect.right() + t.px(10.0), bar.y, bar.w - buttons_w - t.px(60.0), bar.h);
            ui.text_in(Rect::new(title.x, bar.y + t.px(3.0), title.w, t.px(20.0)), Face::Bold, t.pt(15.0), t.text, item.name());
            let meta = drive::join_dots(&[item.meta(&zone), item.location()]);
            row_meta(ui, title, bar.y + t.px(23.0), &meta, t.dim);
            let mut x = bar.right() - t.px(10.0);
            let mut button = |ui: &mut Ui, id: &str, label: &str, w: f32| {
                let w = t.px(w);
                x -= w;
                let r = Rect::new(x, bar.y + t.px(5.0), w, t.px(34.0));
                x -= t.px(6.0);
                ui.button_at(r, id, label)
            };
            // First in the focus order, so that Enter plays it.
            if is_video && button(ui, "play", "▶ Play", 80.0) {
                play = Some(item.id.clone());
            }
            close = button(ui, "close", "Close", 70.0);
            if button(ui, "download", "Download", 100.0) {
                download = Some(item.clone());
            }
            if Self::can_toggle(&item) && button(ui, "toggle", if p.as_text { "Page" } else { "Text" }, 70.0) {
                toggle = true;
            }
            ui.pad(4.0);
            if !toast.is_empty() {
                ui.dim(&toast);
            }
            let body = ui.area();
            match &mut p.body {
                Body::Loading => {
                    ui.within(body, |ui| ui.dim("Loading…"));
                }
                Body::Pending(_) => {
                    ui.within(body, |ui| ui.dim("Still reading this file…"));
                }
                Body::NoText => {
                    ui.within(body, |ui| ui.dim("This file has no text to show."));
                }
                Body::Failed => {
                    ui.within(body, |ui| ui.label("The preview could not be loaded."));
                }
                Body::Unsupported => {
                    ui.within(body, |ui| ui.label("This kind of archive can't be listed here. Download it to look inside."));
                }
                Body::Info { note } => {
                    let note = *note;
                    ui.within(body, |ui| {
                        if note {
                            ui.dim("No preview for this file.");
                            ui.space(6.0);
                        }
                        info_table(ui, &item, &zone);
                    });
                }
                Body::Picture { image, failed, .. } => {
                    if let Some(img) = image {
                        let base = (img.w, img.h);
                        let used = p.surface.input(ui, "preview-picture", body, base);
                        if used.clicked {
                            let (px, py) = ui.pointer();
                            p.surface.toggle((px - body.x) as f32, (py - body.y) as f32, base, body);
                        }
                        p.surface.show(ui, body, Some(img), 0x05_07_10);
                    } else if *failed {
                        ui.within(body, |ui| ui.label("The preview could not be loaded."));
                    } else {
                        ui.within(body, |ui| ui.dim("Loading…"));
                    }
                }
                Body::Play { image, .. } => {
                    let foot = ui.cut_bottom(t.px(28.0));
                    if let Some(img) = image {
                        let inner = ui.area();
                        ui.image_in(inner, img);
                    }
                    let note = if item.preview() == DrivePreview::Video { "Enter plays it." } else { "No player for sound here yet. Download it to keep it here." };
                    ui.text_in(foot, Face::Sans, t.pt(14.0), t.item, note);
                }
                Body::Text { language, truncated, bytes, lines } => {
                    let count = if lines.len() == 1 { "1 line".to_owned() } else { format!("{} lines", lines.len()) };
                    ui.dim(&drive::join_dots(&[language.clone().unwrap_or_default(), count, drive::format_bytes(*bytes)]));
                    if *truncated {
                        ui.dim("Only the beginning of this file is shown. Download it for the rest.");
                    }
                    let r = ui.area();
                    let digits = lines.len().to_string().len() as i32;
                    let gutter = ui.measure(Face::Mono, t.pt(13.0), &"0".repeat(digits as usize + 1));
                    let lines_ref = &*lines;
                    ui.list("text", &mut p.lines, r, lines_ref.len(), 20.0, |ui, i, selected| {
                        let a = ui.area();
                        if selected {
                            ui.fill(a, t.panel);
                        }
                        let px = t.pt(13.0);
                        ui.text_in(Rect::new(a.x + t.px(4.0), a.y, gutter, a.h), Face::Mono, px, t.dim, &(i + 1).to_string());
                        let mut x = a.x + t.px(10.0) + gutter;
                        for (cls, text) in &lines_ref[i] {
                            if x >= a.right() {
                                break;
                            }
                            let c = token_colour(ui, cls);
                            let clean = text.replace('\t', "    ");
                            ui.text_in(Rect::new(x, a.y, a.right() - x, a.h), Face::Mono, px, c, &clean);
                            x += ui.measure(Face::Mono, px, &clean);
                        }
                    });
                    if !p.focused {
                        p.focused = true;
                        p.lines.select(0);
                        ui.focus("text");
                    }
                }
                Body::Archive(l) => {
                    let n = l.total.unwrap_or(l.entries.len() as u64);
                    let count = if n == 1 { "1 entry".to_owned() } else { format!("{n} entries") };
                    ui.dim(&drive::join_dots(&[l.format.clone().unwrap_or_default(), count]));
                    if l.truncated {
                        ui.dim(&format!("Only the first {} entries are listed.", l.entries.len()));
                    }
                    let r = ui.area();
                    let entries = &l.entries;
                    ui.list("archive", &mut p.lines, r, entries.len(), 24.0, |ui, i, selected| {
                        let a = ui.area();
                        let e = &entries[i];
                        if selected {
                            ui.fill(a, t.panel);
                        }
                        let rw = t.px(90.0);
                        ui.text_in(Rect::new(a.x + t.px(6.0), a.y, a.w - rw - t.px(10.0), a.h), Face::Sans, t.pt(14.0), if e.dir { t.accent } else { t.item }, &e.path);
                        if !e.dir {
                            ui.text_in(Rect::new(a.right() - rw, a.y, rw, a.h), Face::Sans, t.pt(13.0), t.dim, &drive::format_bytes(e.size));
                        }
                    });
                    if !p.focused {
                        p.focused = true;
                        p.lines.select(0);
                        ui.focus("archive");
                    }
                }
            }
        }
        self.want_pictures(&[], 0, 0, 0, (sw, sh));
        // Space plays a video, as Enter does (a button that has the focus takes the key first).
        if play.is_none()
            && let Some(p) = self.preview.as_ref().filter(|p| p.item.preview() == DrivePreview::Video)
            && ui.key(keys::KEY_SPACE)
        {
            play = Some(p.item.id.clone());
        }
        if let Some(id) = play {
            match Program::app("video", &["--item", &id]) {
                Some(p) => ui.run_program(p),
                None => self.toast = "The video player is not on this machine.".to_owned(),
            }
        }
        if close || ui.key(keys::KEY_BACKSPACE) {
            self.close_preview();
            return;
        }
        if ui.key(keys::KEY_ESC) {
            // A zoomed picture comes back first.
            if let Some(p) = self.preview.as_mut().filter(|p| p.surface.is_zoomed()) {
                p.surface.fit();
            } else {
                self.close_preview();
                return;
            }
        }
        self.preview_keys(ui);
        if self.preview.is_none() {
            return;
        }
        if let Some(item) = download {
            self.start_download(item);
            return;
        }
        if toggle || ui.key(keys::KEY_T) {
            self.toggle_text();
        }
        if ui.key(keys::KEY_D) {
            if let Some(item) = self.preview.as_ref().map(|p| p.item.clone()) {
                self.start_download(item);
            }
        }
    }
}

/// `InfoTable`.
fn info_table(ui: &mut Ui, item: &DriveItem, zone: &Zone) {
    let t = ui.theme();
    let rows = [
        ("Name", item.name().to_owned()),
        ("Type", item.file_type().label().to_owned()),
        ("Size", drive::format_bytes(item.size)),
        ("Modified", drive::format_modified(item.modified(), zone)),
        ("Location", item.location()),
    ];
    for (k, v) in rows {
        if v.is_empty() {
            continue;
        }
        let r = ui.cut_top(t.px(24.0));
        let kw = t.px(90.0);
        ui.text_in(Rect::new(r.x, r.y, kw, r.h), Face::Sans, t.pt(13.0), t.dim, k);
        ui.text_in(Rect::new(r.x + kw, r.y, r.w - kw, r.h), Face::Sans, t.pt(14.0), t.text, &v);
    }
}
