//! `web/src/views/Gallery.tsx` and its grid (`GridFeed`, `GridView`): one grid of tiles by day, a page at a time. A tile is a small decoded
//! picture; those far from the screen are let go, so memory follows the screen and not the library.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use lucina_account::ui::SignInView;
use lucina_app::time::Zone;
use lucina_app::{App, Face, Image, Input, ListState, Rect, Rgb, Sender, TextEdit, Ui, keys};
use lucina_app_support::dates;
use lucina_files::cache::DiskCache;
use lucina_files::drive::format_bytes;
use lucina_files::media::Connect;
use lucina_files::picker::{self, FilePicker, Mode, Picked, Setup};
use lucina_files::pictures::{Done, Fit, Pictures, Want};
use lucina_files::places;
use serde_json::{Value, json};

use crate::model::{self, Album, Item, Query, Source};
use crate::server::{Job, Msg};
use crate::viewer::Viewer;

pub type Link = Box<dyn FnMut(Job)>;

/// `FIRST_PAGE`, `SEEK_PAGE`.
pub const FIRST_PAGE: u32 = 200;
pub const SEEK_PAGE: u32 = 500;
const DEBOUNCE: Duration = Duration::from_millis(300);
/// All fetched pictures, tiles and previews, on `/data`.
const CACHE_LIMIT: u64 = 2 << 30;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Shelf {
    Everything,
    Album { id: String, title: String },
    Trash,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GridRow {
    Header { key_at: usize },
    Tiles { start: usize, end: usize },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Dialog {
    Sources,
    Months,
    /// `AlbumPicker`, for these ids.
    AddToAlbum(Vec<String>),
    /// `DeleteDialog`, for these ids.
    Delete { ids: Vec<String>, purge: bool, also_device: bool, understood: bool, error: Option<String>, busy: bool },
}

pub struct Tile {
    pub image: Image,
    /// A ThumbHash, for the moment before the picture.
    pub placeholder: bool,
}

pub struct Gallery {
    pub(crate) tx: Option<Sender<Msg>>,
    pub(crate) link: Option<Link>,
    pub(crate) sample: bool,
    pub(crate) zone: Zone,
    sign_in: Option<SignInView>,
    pub signed_out: bool,
    make_sign_in: Option<Box<dyn FnMut() -> Option<SignInView>>>,
    make_worker: Option<Box<dyn FnMut() -> Option<Link>>>,
    pub shelf: Shelf,
    pub origin: String,
    pub kind: Option<&'static str>,
    pub favorites: bool,
    pub search: TextEdit,
    search_at: Option<Instant>,
    pub words: String,
    smart_ready: bool,
    smart_refused: bool,
    // The feed.
    pub items: Vec<Item>,
    known: HashSet<String>,
    cursor: Option<String>,
    pub(crate) started: bool,
    pub loading: bool,
    pub failed: Option<String>,
    generation: u64,
    // The grid.
    pub rows: Vec<GridRow>,
    labels: HashMap<usize, String>,
    pub focus: usize,
    pub scroll: i32,
    /// Smooth-scroll pixels not yet a whole notch (40 logical px), kept between events.
    scroll_rem: i32,
    cols: usize,
    side: i32,
    view_h: i32,
    pub selected: HashSet<String>,
    anchor: Option<usize>,
    // Pictures.
    pub(crate) pictures: Option<Pictures>,
    pub tiles: HashMap<String, Tile>,
    wanted: Vec<String>,
    asked_placeholder: HashSet<String>,
    // Everything else.
    pub viewer: Option<Viewer>,
    pub dialog: Option<Dialog>,
    pub sources: Option<Result<(Vec<Source>, u64), String>>,
    pub months: Option<Result<Vec<(String, u64)>, String>>,
    pub albums: Option<Result<Vec<Album>, String>>,
    pub albums_page: bool,
    albums_list: ListState,
    dialog_list: ListState,
    new_album: TextEdit,
    seek_month: Option<String>,
    pub picker: Option<FilePicker>,
    download: Option<Item>,
    pub toast: String,
    toast_until: Option<Instant>,
    focus_grid: bool,
    focus_search: bool,
    pub(crate) now_ms: i64,
    /// Tiles decoded at most: three screens. A number the first draw works out from the screen.
    pub keep: usize,
    /// Intents whose answer waits for the server, answered from `message`.
    pub(crate) pending: Vec<crate::intents::Pending>,
}

impl Gallery {
    pub fn new(zone: Zone, tx: Option<Sender<Msg>>, link: Option<Link>, connect: Option<Connect>) -> Gallery {
        let pictures = match (&tx, connect) {
            (Some(tx), Some(c)) => {
                let tx = tx.clone();
                let cache = places::cache_dir("gallery").map(|d| Arc::new(DiskCache::new(d.join("pictures"), CACHE_LIMIT)));
                Some(Pictures::start(c, cache, move |d| tx.send(Msg::Picture(d))))
            }
            _ => None,
        };
        Gallery {
            tx,
            link,
            sample: false,
            zone,
            sign_in: None,
            signed_out: false,
            make_sign_in: None,
            make_worker: None,
            shelf: Shelf::Everything,
            origin: "all".into(),
            kind: None,
            favorites: false,
            search: TextEdit::new().placeholder("Search"),
            search_at: None,
            words: String::new(),
            smart_ready: false,
            smart_refused: false,
            items: Vec::new(),
            known: HashSet::new(),
            cursor: None,
            started: false,
            loading: false,
            failed: None,
            generation: 0,
            rows: Vec::new(),
            labels: HashMap::new(),
            focus: 0,
            scroll: 0,
            scroll_rem: 0,
            cols: 1,
            side: 1,
            view_h: 1,
            selected: HashSet::new(),
            anchor: None,
            pictures,
            tiles: HashMap::new(),
            wanted: Vec::new(),
            asked_placeholder: HashSet::new(),
            viewer: None,
            dialog: None,
            sources: None,
            months: None,
            albums: None,
            albums_page: false,
            albums_list: ListState::new(),
            dialog_list: ListState::new(),
            new_album: TextEdit::new().placeholder("Album name"),
            seek_month: None,
            picker: None,
            download: None,
            toast: String::new(),
            toast_until: None,
            focus_grid: true,
            focus_search: false,
            now_ms: dates::now_ms(),
            keep: 180,
            pending: Vec::new(),
        }
    }

    pub fn sample(mut self) -> Gallery {
        self.sample = true;
        self
    }

    pub fn with_sign_in(mut self, s: Box<dyn FnMut() -> Option<SignInView>>, w: Box<dyn FnMut() -> Option<Link>>) -> Gallery {
        self.make_sign_in = Some(s);
        self.make_worker = Some(w);
        self
    }

    pub(crate) fn send(&mut self, job: Job) {
        if let Some(l) = &mut self.link {
            l(job);
        }
    }

    pub fn start(&mut self) {
        if self.link.is_none() {
            if let Some(make) = &mut self.make_sign_in {
                self.sign_in = make();
            }
            return;
        }
        self.send(Job::Status);
        self.send(Job::Sources);
        self.send(Job::Albums);
        self.reload();
    }

    /// `usesSmart`: by meaning when the server can, else by words.
    fn query(&self) -> Query {
        let trash = self.shelf == Shelf::Trash;
        Query {
            // The shelves list everything of theirs, whatever source was last chosen.
            origin: (self.shelf == Shelf::Everything).then(|| self.origin.clone()),
            kind: self.kind,
            q: self.words.clone(),
            favorite: self.favorites && !trash,
            trashed: trash,
            album: if let Shelf::Album { id, .. } = &self.shelf { Some(id.clone()) } else { None },
            smart: self.smart_ready && !self.smart_refused && !trash && !self.words.trim().is_empty(),
        }
    }

    /// `reload`: from the first page, same query or a new one.
    pub fn reload(&mut self) {
        self.generation += 1;
        self.items.clear();
        self.known.clear();
        self.cursor = None;
        self.started = false;
        self.failed = None;
        self.rows.clear();
        self.labels.clear();
        self.focus = 0;
        self.scroll = 0;
        self.selected.clear();
        self.anchor = None;
        self.loading = false;
        self.seek_month = None;
        self.load_more(FIRST_PAGE);
    }

    /// `loadMore`.
    pub fn load_more(&mut self, limit: u32) {
        if self.loading || (self.started && self.cursor.is_none()) {
            return;
        }
        self.loading = true;
        let job = Job::Page { generation: self.generation, query: self.query(), cursor: self.cursor.clone(), limit };
        self.send(job);
    }

    pub fn has_more(&self) -> bool {
        !self.started || self.cursor.is_some()
    }

    fn rebuild_rows(&mut self) {
        self.rows.clear();
        self.labels.clear();
        let cols = self.cols.max(1);
        let mut i = 0;
        let mut last_key: Option<String> = None;
        let mut in_row = 0usize;
        let n = self.items.len();
        while i < n {
            let key = model::section_key(self.items[i].at_ms, &self.zone);
            if last_key.as_deref() != Some(&key) {
                self.labels.insert(self.rows.len(), model::section_label(&key));
                self.rows.push(GridRow::Header { key_at: i });
                last_key = Some(key);
                in_row = 0;
            }
            if in_row == 0 {
                self.rows.push(GridRow::Tiles { start: i, end: i + 1 });
            } else if let Some(GridRow::Tiles { end, .. }) = self.rows.last_mut() {
                *end = i + 1;
            }
            in_row = (in_row + 1) % cols;
            i += 1;
        }
    }

    pub(crate) fn header_h(&self) -> i32 {
        (self.side / 5).clamp(24, 40)
    }

    pub(crate) fn gap(&self) -> i32 {
        (self.side / 40).max(2)
    }

    fn row_h(&self, r: GridRow) -> i32 {
        match r {
            GridRow::Header { .. } => self.header_h(),
            GridRow::Tiles { .. } => self.side + self.gap(),
        }
    }

    fn total_h(&self) -> i32 {
        self.rows.iter().map(|r| self.row_h(*r)).sum()
    }

    /// `offsetOfIndex`, and the row it is in.
    fn locate(&self, index: usize) -> Option<(usize, i32)> {
        let mut y = 0;
        for (i, r) in self.rows.iter().enumerate() {
            if let GridRow::Tiles { start, end } = r
                && (*start..*end).contains(&index)
            {
                return Some((i, y));
            }
            y += self.row_h(*r);
        }
        None
    }

    /// `scrollToReveal`.
    fn reveal(&mut self) {
        let Some((row, y)) = self.locate(self.focus) else { return };
        // The day's header too, when it is the first row of its section.
        let top = if row > 0 && matches!(self.rows[row - 1], GridRow::Header { .. }) { y - self.header_h() } else { y };
        if top < self.scroll {
            self.scroll = top;
        } else if y + self.side > self.scroll + self.view_h {
            self.scroll = y + self.side - self.view_h;
        }
        self.clamp_scroll();
    }

    fn clamp_scroll(&mut self) {
        self.scroll = self.scroll.clamp(0, (self.total_h() - self.view_h).max(0));
    }

    /// `neighbour`: left and right follow the reading order; up and down stay in the column.
    pub fn neighbour(&self, index: usize, up: bool) -> usize {
        let Some((row, _)) = self.locate(index) else { return index };
        let GridRow::Tiles { start, .. } = self.rows[row] else { return index };
        let col = index - start;
        let mut r = row as i64 + if up { -1 } else { 1 };
        while r >= 0 && (r as usize) < self.rows.len() {
            if let GridRow::Tiles { start, end } = self.rows[r as usize] {
                return (start + col).min(end - 1);
            }
            r += if up { -1 } else { 1 };
        }
        index
    }

    fn move_focus(&mut self, to: usize, extend: bool) {
        if self.items.is_empty() {
            return;
        }
        let to = to.min(self.items.len() - 1);
        if extend {
            // `rangeIds`.
            let anchor = *self.anchor.get_or_insert(self.focus);
            let (a, b) = (anchor.min(to), anchor.max(to));
            self.selected = self.items[a..=b].iter().map(|i| i.id.clone()).collect();
        }
        self.focus = to;
        self.reveal();
    }

    fn toggle_selected(&mut self, index: usize) {
        let Some(id) = self.items.get(index).map(|i| i.id.clone()) else { return };
        if !self.selected.remove(&id) {
            self.selected.insert(id);
        }
        self.anchor = Some(index);
    }

    pub fn selected_ids(&self) -> Vec<String> {
        self.items.iter().filter(|i| self.selected.contains(&i.id)).map(|i| i.id.clone()).collect()
    }

    fn items_of(&self, ids: &[String]) -> Vec<&Item> {
        self.items.iter().filter(|i| ids.contains(&i.id)).collect()
    }

    pub fn set_shelf(&mut self, shelf: Shelf) {
        self.shelf = shelf;
        self.albums_page = false;
        self.clear_search();
        self.reload();
        self.focus_grid = true;
    }

    fn clear_search(&mut self) {
        self.search.clear();
        self.words.clear();
        self.search_at = None;
    }

    /// `remove`: moved to the Trash, deleted, restored: no longer in this list.
    pub fn remove_items(&mut self, ids: &[String]) {
        if ids.is_empty() {
            return;
        }
        let drop: HashSet<&String> = ids.iter().collect();
        self.items.retain(|i| !drop.contains(&i.id));
        for id in ids {
            self.known.remove(id);
            self.selected.remove(id);
            self.tiles.remove(id);
        }
        self.rebuild_rows();
        if self.focus >= self.items.len() {
            self.focus = self.items.len().saturating_sub(1);
        }
        self.clamp_scroll();
        if let Some(v) = &mut self.viewer {
            let n = self.items.len();
            if n == 0 {
                self.viewer = None;
            } else {
                v.index = v.index.min(n - 1);
                v.reset_for_item();
            }
        }
    }

    /// `replace`: its updated copy.
    pub fn replace_item(&mut self, item: Item) {
        if let Some(i) = self.items.iter().position(|x| x.id == item.id) {
            if self.items[i].rotation != item.rotation {
                self.tiles.remove(&item.id);
                self.wanted.clear();
            }
            self.items[i] = item;
        }
    }

    pub(crate) fn say(&mut self, s: String) {
        self.toast = s;
        self.toast_until = Some(Instant::now() + Duration::from_secs(6));
    }

    pub(crate) fn run_batch(&mut self, op: &'static str, ids: Vec<String>, album: Option<String>, also_from_device: bool) {
        if !ids.is_empty() {
            self.send(Job::Batch { op, ids, album, also_from_device });
        }
    }

    pub(crate) fn open_delete(&mut self, ids: Vec<String>) {
        if ids.is_empty() {
            return;
        }
        let purge = self.shelf == Shelf::Trash;
        self.dialog = Some(Dialog::Delete { ids, purge, also_device: false, understood: false, error: None, busy: false });
    }

    pub(crate) fn open_album_picker(&mut self, ids: Vec<String>) {
        if ids.is_empty() {
            return;
        }
        self.send(Job::Albums);
        self.dialog_list = ListState::new();
        self.new_album.clear();
        self.dialog = Some(Dialog::AddToAlbum(ids));
    }

    pub(crate) fn start_download(&mut self, item: Item) {
        let tx = self.tx.clone();
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            if let Some(tx) = &tx {
                tx.send(Msg::Changed);
            }
        });
        let name = if item.title.is_empty() { "photo".to_owned() } else { picker::safe_name(&item.title) };
        let mut list = places::standard();
        list.rotate_left(2); // Pictures first.
        let setup = Setup { title: format!("Download {name}"), mode: Mode::Save { name }, places: list, drive: None, fetch_to: None };
        self.download = Some(item);
        self.picker = Some(FilePicker::start(setup, wake));
    }

    fn picked(&mut self, p: Picked) {
        self.picker = None;
        let Some(item) = self.download.take() else { return };
        let Picked::File(dest): Picked = p else { return };
        self.say("Downloading…".to_owned());
        let url = item.download_url();
        self.send(Job::Download { url, dest });
    }

    fn signed_out_now(&mut self) {
        self.signed_out = true;
        self.link = None;
        self.viewer = None;
        self.dialog = None;
        self.picker = None;
        // What was fetched for this account goes with it.
        if let Some(p) = &self.pictures {
            p.clear_cache();
        }
        self.tiles.clear();
        crate::account::rejected();
        if let Some(make) = &mut self.make_sign_in {
            self.sign_in = make();
        }
    }

    /// `monthReached` and the seek: a page after a page until the month is on the grid.
    pub(crate) fn seek(&mut self, month: String) {
        self.seek_month = Some(month);
        self.continue_seek();
    }

    fn continue_seek(&mut self) {
        let Some(month) = self.seek_month.clone() else { return };
        let day = month.len();
        let found = self.items.iter().position(|i| model::section_key(i.at_ms, &self.zone)[..day.min(7)] <= *month);
        if let Some(i) = found {
            self.seek_month = None;
            self.move_focus(i, false);
            return;
        }
        if self.has_more() {
            self.load_more(SEEK_PAGE);
        } else {
            self.seek_month = None;
        }
    }

    /// What the screen wants: the viewer's pictures, or the tiles in view, then a screen on each side. The rest may go.
    fn want_pictures(&mut self, side: u32) {
        let mut wants: Vec<Want> = Vec::new();
        let mut hashes: Vec<(String, String)> = Vec::new();
        if let Some(v) = &self.viewer {
            wants = v.wants(&self.items);
        } else {
            let (first, last) = self.visible_range();
            let span = last.saturating_sub(first).max(1);
            let order = (first..last).chain(last..(last + span).min(self.items.len())).chain(first.saturating_sub(span)..first);
            for i in order {
                let item = &self.items[i];
                if self.tiles.get(&item.id).is_some_and(|t| !t.placeholder) {
                    continue;
                }
                let Some(url) = item.tile_url() else { continue };
                wants.push(Want { url: url.to_owned(), fit: Fit::Square(side), rotation: item.rotation });
                if !self.tiles.contains_key(&item.id)
                    && !self.asked_placeholder.contains(&item.id)
                    && let Some(h) = &item.thumbhash
                    && (first..last).contains(&i)
                {
                    hashes.push((item.id.clone(), h.clone()));
                }
            }
        }
        let urls: Vec<String> = wants.iter().map(|w| w.url.clone()).collect();
        if urls != self.wanted {
            self.wanted = urls;
            if let Some(p) = &self.pictures {
                p.want(wants);
            }
        }
        if !hashes.is_empty() {
            for (id, _) in &hashes {
                self.asked_placeholder.insert(id.clone());
            }
            self.send(Job::Placeholders { side, wanted: hashes });
        }
        self.let_go();
    }

    /// The first and past-the-last item the screen shows.
    fn visible_range(&self) -> (usize, usize) {
        let (mut y, mut first, mut last) = (0, None, 0);
        for r in &self.rows {
            let h = self.row_h(*r);
            if y + h > self.scroll && y < self.scroll + self.view_h {
                if let GridRow::Tiles { start, end } = r {
                    first.get_or_insert(*start);
                    last = *end;
                }
            }
            y += h;
            if y >= self.scroll + self.view_h {
                break;
            }
        }
        (first.unwrap_or(0), last)
    }

    /// Tiles past what `keep` allows, farthest from the screen first.
    fn let_go(&mut self) {
        if self.tiles.len() <= self.keep {
            return;
        }
        let (first, last) = self.visible_range();
        let centre = (first + last) / 2;
        let mut far: Vec<(usize, String)> = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, it)| self.tiles.contains_key(&it.id))
            .map(|(i, it)| (i.abs_diff(centre), it.id.clone()))
            .collect();
        far.sort_unstable_by_key(|e| std::cmp::Reverse(e.0));
        let extra = self.tiles.len() - self.keep;
        for (_, id) in far.into_iter().take(extra) {
            self.tiles.remove(&id);
            self.asked_placeholder.remove(&id);
        }
    }

    pub(crate) fn close_viewer(&mut self) {
        self.viewer = None;
        self.toast.clear();
        self.wanted.clear();
        self.focus_grid = true;
    }

    pub fn open_viewer(&mut self, index: usize) {
        if index < self.items.len() {
            self.viewer = Some(Viewer::new(index));
            self.wanted.clear();
        }
    }
}

impl App for Gallery {
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
                self.start();
            }
            Msg::SignedOut => self.signed_out_now(),
            Msg::SmartOff => self.smart_refused = true,
            Msg::SmartReady(on) => self.smart_ready = on,
            Msg::Page { generation, result } => {
                if generation != self.generation {
                    return;
                }
                self.loading = false;
                match result {
                    Ok((page, next)) => {
                        let fresh: Vec<Item> = page.into_iter().filter(|i| self.known.insert(i.id.clone())).collect();
                        self.items.extend(fresh);
                        self.cursor = next;
                        self.started = true;
                        self.rebuild_rows();
                        self.continue_seek();
                    }
                    Err(e) => self.failed = Some(e),
                }
            }
            Msg::Sources(r) => self.sources = Some(r),
            Msg::Months(r) => self.months = Some(r),
            Msg::Albums(r) => self.albums = Some(r),
            Msg::Details { id, result } => {
                if let Some(v) = &mut self.viewer {
                    v.details_arrived(&id, result);
                }
            }
            Msg::Patched(result) => match result {
                Ok(item) => self.replace_item(*item),
                Err((item, e)) => {
                    self.replace_item(*item);
                    self.say(e);
                }
            },
            Msg::Batched { op, album, done, failed, error } => self.batched(op, album, done, failed, error),
            Msg::AlbumMade(r) => match r {
                Ok(a) => {
                    if let Some(Ok(list)) = &mut self.albums {
                        list.push(a);
                    }
                    self.send(Job::Albums);
                }
                Err(e) => self.say(e),
            },
            Msg::Picture(done) => match done {
                Done::Ready(want, img) => self.picture_arrived(want, img),
                Done::Failed(want, e) => {
                    if let Some(v) = &mut self.viewer {
                        v.picture_failed(&want, &e);
                    }
                }
                Done::SignedOut => self.signed_out_now(),
            },
            Msg::Placeholder(id, image) => {
                self.tiles.entry(id).or_insert(Tile { image, placeholder: true });
            }
            Msg::Download(r) => {
                let s = match r {
                    Ok(p) => format!("Saved in {}", p.display()),
                    Err(e) => format!("Not saved: {e}"),
                };
                self.say(s);
            }
            Msg::DownloadProgress(bytes) => self.say(format!("Downloading… {}", format_bytes(Some(bytes)))),
        }
        crate::intents::answer_pending(self);
    }

    fn view(&mut self, ui: &mut Ui) {
        if let Some(view) = &mut self.sign_in {
            ui.header("Gallery");
            ui.pad(14.0);
            view.view(ui, "Sign in to see your photos.");
            return;
        }
        if self.picker.is_some() {
            ui.header("Gallery");
            ui.pad(14.0);
            let got = self.picker.as_mut().and_then(|p| p.view(ui));
            if let Some(p) = got {
                self.picked(p);
            }
            return;
        }
        if self.viewer.is_some() {
            crate::viewer::view(self, ui);
            let side = self.side.max(1) as u32;
            self.want_pictures(side);
            if self.dialog.is_some() {
                self.dialog_view(ui);
            }
            return;
        }
        ui.header(if self.sample { "Gallery: sample data" } else { "Gallery" });
        ui.pad(8.0);
        if self.albums_page {
            self.albums_view(ui);
        } else {
            self.grid_view(ui);
        }
        if self.dialog.is_some() {
            self.dialog_view(ui);
        }
    }

    fn on_quit(&mut self) {}
}

impl Gallery {
    fn picture_arrived(&mut self, want: Want, img: Image) {
        let img = match &mut self.viewer {
            Some(v) => match v.picture_arrived(&want, img) {
                Ok(()) => return,
                Err(img) => img,
            },
            None => img,
        };
        if let Fit::Square(_) = want.fit {
            // The url names the item: `/v1/media/items/<id>/…` or a token's `/s/<token>/<id>/…`.
            if let Some(item) = self.items.iter().find(|i| i.tile_url() == Some(want.url.as_str()) && i.rotation == want.rotation) {
                let id = item.id.clone();
                self.tiles.insert(id, Tile { image: img, placeholder: false });
            }
        }
    }

    fn batched(&mut self, op: &'static str, album: Option<String>, done: Vec<String>, failed: usize, error: Option<String>) {
        if let Some(Dialog::Delete { error: slot, busy, .. }) = &mut self.dialog {
            *busy = false;
            if done.is_empty() && (failed > 0 || error.is_some()) {
                *slot = Some(if failed > 0 {
                    if failed == 1 { "1 item couldn't be removed.".to_owned() } else { format!("{failed} items couldn't be removed.") }
                } else {
                    "Couldn't finish. Check the connection and try again.".to_owned()
                });
                return;
            }
        }
        if matches!(self.dialog, Some(Dialog::Delete { .. })) {
            self.dialog = None;
            self.focus_grid = true;
        }
        let n = done.len();
        let plural = |one: &str, many: String| if n == 1 { one.to_owned() } else { many };
        let mut note = match op {
            "trash" => plural("1 item moved to the Trash.", format!("{n} items moved to the Trash.")),
            "purge" => plural("1 item deleted.", format!("{n} items deleted.")),
            "restore" => plural("1 item restored.", format!("{n} items restored.")),
            "album_add" => plural("1 item added to the album.", format!("{n} items added to the album.")),
            "album_remove" => plural("1 item removed from the album.", format!("{n} items removed from the album.")),
            "favorite" => plural("1 item added to favorites.", format!("{n} items added to favorites.")),
            "unfavorite" => plural("1 item removed from favorites.", format!("{n} items removed from favorites.")),
            _ => String::new(),
        };
        if failed > 0 {
            note.push(' ');
            note.push_str(&if failed == 1 { "1 item couldn't be changed.".to_owned() } else { format!("{failed} items couldn't be changed.") });
        }
        if error.is_some() && done.is_empty() {
            note = "That didn't work. Try again.".to_owned();
        }
        match op {
            "trash" | "purge" | "restore" => self.remove_items(&done),
            "album_remove" => {
                if matches!(&self.shelf, Shelf::Album { id, .. } if Some(id) == album.as_ref()) {
                    self.remove_items(&done);
                }
            }
            "album_add" => {
                if let Some(a) = &album {
                    for item in self.items.iter_mut().filter(|i| done.contains(&i.id)) {
                        if !item.albums.contains(a) {
                            item.albums.push(a.clone());
                        }
                    }
                }
                self.selected.clear();
                self.dialog = None;
                self.send(Job::Albums);
            }
            "favorite" | "unfavorite" => {
                let on = op == "favorite";
                for item in self.items.iter_mut().filter(|i| done.contains(&i.id)) {
                    item.favorite = on;
                }
                if self.favorites && !on {
                    self.remove_items(&done);
                }
                self.selected.clear();
            }
            _ => {}
        }
        self.say(note);
    }

    fn toolbar(&mut self, ui: &mut Ui) {
        let t = ui.theme();
        let bar = ui.cut_top(t.px(36.0));
        let mut x = bar.right() - t.px(8.0);
        let mut place = |w: f32| {
            let w = t.px(w);
            x -= w;
            let r = Rect::new(x, bar.y, w, t.px(32.0));
            x -= t.px(4.0);
            r
        };
        let mut go = None;
        let in_trash = self.shelf == Shelf::Trash;
        if ui.button_at(place(58.0), "tb-trash", if in_trash { "Back" } else { "Trash" }) {
            go = Some(if in_trash { Shelf::Everything } else { Shelf::Trash });
        }
        if ui.button_at(place(70.0), "tb-albums", "Albums") {
            self.albums_page = true;
            self.send(Job::Albums);
        }
        if ui.button_at(place(70.0), "tb-months", "Months") {
            self.open_months();
        }
        if !in_trash {
            let fav_label = if self.favorites { "♥ On" } else { "♥" };
            if ui.button_at(place(44.0), "tb-fav", fav_label) {
                self.favorites = !self.favorites;
                self.reload();
            }
            let kind_label = match self.kind {
                None => "All",
                Some("photo") => "Photos",
                _ => "Videos",
            };
            if ui.button_at(place(70.0), "tb-kind", kind_label) {
                self.cycle_kind();
            }
            if self.shelf == Shelf::Everything {
                let label = self.source_label();
                if ui.button_at(place(130.0), "tb-source", &label) {
                    self.open_sources();
                }
            }
        }
        let field = Rect::new(bar.x + t.px(10.0), bar.y, (x - bar.x - t.px(10.0)).max(t.px(60.0)), t.px(32.0));
        let res = ui.text_field_at(field, "search", &mut self.search);
        if res.changed {
            self.search_at = Some(Instant::now() + DEBOUNCE);
        }
        if res.submitted {
            self.search_at = Some(Instant::now());
            self.focus_grid = true;
        }
        if ui.is_focused("search") && ui.key(keys::KEY_ESC) {
            let had = !self.words.is_empty();
            self.clear_search();
            if had {
                self.reload();
            }
            self.focus_grid = true;
        }
        if let Some(at) = self.search_at {
            let now = Instant::now();
            if now >= at {
                self.search_at = None;
                let q = self.search.text.trim().to_owned();
                if q != self.words {
                    self.words = q;
                    self.reload();
                }
            } else {
                ui.wake_after(at - now);
            }
        }
        if let Some(s) = go {
            self.set_shelf(s);
        }
    }

    fn source_label(&self) -> String {
        if self.origin == "all" {
            return "From: All".to_owned();
        }
        if let Some(Ok((list, _))) = &self.sources
            && let Some(s) = list.iter().find(|s| s.id == self.origin)
        {
            return s.label.clone();
        }
        self.origin.clone()
    }

    fn open_sources(&mut self) {
        self.send(Job::Sources);
        self.dialog_list = ListState::new();
        self.dialog_list.select(0);
        self.dialog = Some(Dialog::Sources);
    }

    fn open_months(&mut self) {
        let q = self.query();
        self.months = None;
        self.send(Job::Months { query: q });
        self.dialog_list = ListState::new();
        self.dialog_list.select(0);
        self.dialog = Some(Dialog::Months);
    }

    fn cycle_kind(&mut self) {
        self.kind = match self.kind {
            None => Some("photo"),
            Some("photo") => Some("video"),
            _ => None,
        };
        self.reload();
    }

    fn grid_view(&mut self, ui: &mut Ui) {
        let t = ui.theme();
        self.toolbar(ui);
        ui.pad(4.0);
        // The shelf's own line, the selection, what happened.
        if let Some(until) = self.toast_until {
            let now = Instant::now();
            if now >= until {
                self.toast.clear();
                self.toast_until = None;
            } else {
                ui.wake_after(until - now);
            }
        }
        let title = match &self.shelf {
            Shelf::Everything => String::new(),
            Shelf::Album { title, .. } => title.clone(),
            Shelf::Trash => "Trash. Items are deleted for good after 30 days.".to_owned(),
        };
        if !self.selected.is_empty() {
            self.selection_bar(ui);
        } else if !title.is_empty() || !self.toast.is_empty() {
            let r = ui.cut_top(t.px(22.0));
            let line = if self.toast.is_empty() { title } else { self.toast.clone() };
            ui.text_in(Rect::new(r.x + t.px(12.0), r.y, r.w - t.px(24.0), r.h), Face::Sans, t.pt(13.0), t.item, &line);
        }
        let area = ui.area();
        let r = Rect::new(area.x + t.px(10.0), area.y, area.w - t.px(20.0), area.h - t.px(4.0));
        // The grid's shape. It changes only with the screen, so a tile fetched once stays right.
        let target = t.px(128.0).max(48);
        let gap = (target / 40).max(2);
        let bar_w = t.px(6.0).max(4);
        let cols = ((r.w - bar_w + gap) / (target + gap)).max(1) as usize;
        let side = (r.w - bar_w - gap * (cols as i32 - 1)) / cols as i32;
        let screen_rows = (r.h / side.max(1) + 1) as usize;
        self.keep = (screen_rows * cols * 3).max(32);
        if cols != self.cols || side != self.side {
            self.cols = cols;
            self.side = side;
            self.rebuild_rows();
        }
        self.view_h = r.h;
        self.grid_keys(ui);
        // The wheel, over the grid.
        if let Some(Input::Wheel(d)) = ui.input() {
            let (px, py) = ui.pointer();
            if d != 0 && r.contains(px, py) {
                self.scroll -= d * t.px(90.0);
                self.clamp_scroll();
                ui.take_input();
            }
        }
        // Smooth scroll: one notch per 40 logical pixels of it, the rest kept for the next event.
        if let Some(Input::Scroll { dy, .. }) = ui.input() {
            let (px, py) = ui.pointer();
            if dy != 0 && r.contains(px, py) {
                self.scroll_rem += dy;
                let notch = t.px(40.0);
                let d = self.scroll_rem / notch;
                if d != 0 {
                    self.scroll_rem -= d * notch;
                    self.scroll -= d * t.px(90.0);
                    self.clamp_scroll();
                }
                ui.take_input();
            }
        }
        ui.focus_area("grid", r);
        self.paint_grid(ui, r);
        // `EmptyState` and friends.
        let empty_note = if let Some(e) = &self.failed {
            Some(if self.items.is_empty() { format!("Couldn't load the gallery. {e}") } else { format!("Couldn't load more. {e}") })
        } else if self.items.is_empty() && self.loading {
            Some("Loading…".to_owned())
        } else if self.items.is_empty() && self.started {
            Some(match &self.shelf {
                Shelf::Trash => "The Trash is empty.".to_owned(),
                Shelf::Album { .. } => "No photos in this album yet. Select photos in the Gallery and choose Add to album.".to_owned(),
                Shelf::Everything if !self.words.is_empty() || self.favorites || self.kind.is_some() || self.origin != "all" => "Nothing matches these filters.".to_owned(),
                Shelf::Everything => "No photos here yet.".to_owned(),
            })
        } else {
            None
        };
        if let Some(n) = empty_note {
            ui.text_in(Rect::new(r.x + t.px(4.0), r.y + t.px(4.0), r.w, t.px(28.0)), Face::Sans, t.pt(15.0), t.dim, &n);
        }
        // Next page, ahead of the scroll.
        if self.has_more() && !self.loading && self.failed.is_none() && self.total_h() - self.scroll - self.view_h < self.view_h * 2 {
            self.load_more(FIRST_PAGE);
        }
        // A pass after a dialog closes still counts as inside it, and forgets a focus given then: until it has taken.
        if self.focus_grid && self.dialog.is_none() {
            if ui.is_focused("grid") {
                self.focus_grid = false;
            } else {
                ui.focus("grid");
            }
        }
        if self.focus_search {
            self.focus_search = false;
            ui.focus("search");
        }
        self.want_pictures(side as u32);
    }

    fn grid_keys(&mut self, ui: &mut Ui) {
        if !ui.is_focused("grid") || self.dialog.is_some() {
            return;
        }
        let Some(p) = ui.press() else { return };
        let (shift, plain) = (p.mods.shift, !p.mods.ctrl && !p.mods.alt && !p.mods.meta);
        let n = self.items.len();
        let per_page = ((self.view_h / (self.side + self.gap()).max(1)).max(1) as usize) * self.cols;
        let mut taken = true;
        match p.code {
            keys::KEY_LEFT if plain => self.move_focus(self.focus.saturating_sub(1), shift),
            keys::KEY_RIGHT if plain => self.move_focus((self.focus + 1).min(n.saturating_sub(1)), shift),
            keys::KEY_UP if plain => self.move_focus(self.neighbour(self.focus, true), shift),
            keys::KEY_DOWN if plain => self.move_focus(self.neighbour(self.focus, false), shift),
            keys::KEY_PAGEDOWN if plain => self.move_focus(self.focus + per_page, shift),
            keys::KEY_PAGEUP if plain => self.move_focus(self.focus.saturating_sub(per_page), shift),
            keys::KEY_HOME if plain => self.move_focus(0, shift),
            keys::KEY_END if plain => self.move_focus(n.saturating_sub(1), shift),
            keys::KEY_ENTER | keys::KEY_KPENTER if plain => self.open_viewer(self.focus),
            keys::KEY_SPACE if plain => self.toggle_selected(self.focus),
            keys::KEY_A if p.mods.ctrl => {
                self.selected = self.items.iter().map(|i| i.id.clone()).collect();
            }
            keys::KEY_ESC if plain => {
                if !self.selected.is_empty() {
                    self.selected.clear();
                } else if !self.words.is_empty() {
                    self.clear_search();
                    self.reload();
                } else if self.shelf != Shelf::Everything {
                    self.set_shelf(Shelf::Everything);
                } else {
                    taken = false;
                }
            }
            keys::KEY_DELETE | keys::KEY_BACKSPACE if plain => {
                let ids = if self.selected.is_empty() { self.items.get(self.focus).map(|i| vec![i.id.clone()]).unwrap_or_default() } else { self.selected_ids() };
                self.open_delete(ids);
            }
            keys::KEY_SLASH if plain && !shift => self.focus_search = true,
            keys::KEY_F if p.mods.ctrl => self.focus_search = true,
            keys::KEY_S if plain => self.open_sources(),
            keys::KEY_G if plain => self.open_months(),
            keys::KEY_T if plain => {
                let s = if self.shelf == Shelf::Trash { Shelf::Everything } else { Shelf::Trash };
                self.set_shelf(s);
            }
            keys::KEY_A if plain && !shift => {
                self.albums_page = true;
                self.send(Job::Albums);
            }
            keys::KEY_A if plain && shift => {
                let ids = if self.selected.is_empty() { self.items.get(self.focus).map(|i| vec![i.id.clone()]).unwrap_or_default() } else { self.selected_ids() };
                self.open_album_picker(ids);
            }
            keys::KEY_F if plain && !shift => self.favorite_key(),
            keys::KEY_F if plain && shift => {
                self.favorites = !self.favorites;
                self.reload();
            }
            keys::KEY_O if plain => self.cycle_kind(),
            keys::KEY_D if plain => {
                if let Some(item) = self.items.get(self.focus).cloned() {
                    self.start_download(item);
                }
            }
            keys::KEY_R if plain => {
                self.send(Job::Sources);
                self.reload();
            }
            _ => taken = false,
        }
        if taken {
            ui.take_press();
        }
    }

    /// `f` on the grid: the selection, else the tile under the cursor; on when any of them is off.
    fn favorite_key(&mut self) {
        let ids = if self.selected.is_empty() { self.items.get(self.focus).map(|i| vec![i.id.clone()]).unwrap_or_default() } else { self.selected_ids() };
        if self.shelf == Shelf::Trash || ids.is_empty() {
            return;
        }
        let any_off = self.items_of(&ids).iter().any(|i| !i.favorite);
        self.run_batch(if any_off { "favorite" } else { "unfavorite" }, ids, None, false);
    }

    fn selection_bar(&mut self, ui: &mut Ui) {
        let t = ui.theme();
        let r = ui.cut_top(t.px(36.0));
        ui.fill(r, t.bar);
        let n = self.selected.len();
        ui.text_in(Rect::new(r.x + t.px(12.0), r.y, t.px(110.0), r.h), Face::Bold, t.pt(14.0), t.text, &format!("{n} selected"));
        let mut x = r.x + t.px(124.0);
        let mut button = |ui: &mut Ui, id: &str, label: &str, w: f32| {
            let w = t.px(w);
            let b = Rect::new(x, r.y + t.px(2.0), w, t.px(32.0));
            x += w + t.px(4.0);
            ui.button_at(b, id, label)
        };
        let ids = self.selected_ids();
        if self.shelf == Shelf::Trash {
            if button(ui, "sel-restore", "Restore", 80.0) {
                self.run_batch("restore", ids.clone(), None, false);
            }
            if button(ui, "sel-delete", "Delete forever", 130.0) {
                self.open_delete(ids.clone());
            }
        } else {
            if button(ui, "sel-fav", "Favorite", 86.0) {
                self.favorite_key();
            }
            if button(ui, "sel-album", "Add to album", 120.0) {
                self.open_album_picker(ids.clone());
            }
            if let Shelf::Album { id, .. } = self.shelf.clone()
                && button(ui, "sel-remove", "Remove from album", 160.0)
            {
                self.run_batch("album_remove", ids.clone(), Some(id), false);
            }
            if button(ui, "sel-delete", "Delete", 70.0) {
                self.open_delete(ids.clone());
            }
        }
        if button(ui, "sel-clear", "Clear", 60.0) {
            self.selected.clear();
        }
    }

    fn paint_grid(&mut self, ui: &mut Ui, r: Rect) {
        let t = ui.theme();
        let (side, gap) = (self.side, self.gap());
        let mut y = r.y - self.scroll;
        let mut click = None;
        let mut mark = None;
        ui.within(r, |ui| {
            for (row_i, row) in self.rows.iter().enumerate() {
                let h = match row {
                    GridRow::Header { .. } => self.header_h(),
                    GridRow::Tiles { .. } => side + gap,
                };
                if y + h <= r.y {
                    y += h;
                    continue;
                }
                if y >= r.bottom() {
                    break;
                }
                match row {
                    GridRow::Header { .. } => {
                        if let Some(label) = self.labels.get(&row_i) {
                            ui.text_in(Rect::new(r.x + t.px(2.0), y, r.w, h), Face::Bold, t.pt(13.0), t.item, label);
                        }
                    }
                    GridRow::Tiles { start, end } => {
                        for i in *start..*end {
                            let x = r.x + (i - start) as i32 * (side + gap);
                            let cell = Rect::new(x, y, side, side);
                            let item = &self.items[i];
                            match self.tiles.get(&item.id) {
                                Some(tile) => ui.image_in(cell, &tile.image),
                                None => ui.fill(cell, t.panel),
                            }
                            let selected = self.selected.contains(&item.id);
                            if item.video {
                                let badge = model::format_duration(item.duration_ms.unwrap_or(0));
                                let b = Rect::new(x + t.px(3.0), y + side - t.px(18.0), t.px(44.0), t.px(16.0));
                                ui.fill_round(b, t.px(3.0), Rgb(0x10_14_28));
                                ui.text_in(b.inset(t.px(2.0)), Face::Sans, t.pt(11.0), t.text, &format!("▶ {badge}"));
                            }
                            if item.favorite {
                                ui.text_in(Rect::new(x + side - t.px(18.0), y + t.px(2.0), t.px(16.0), t.px(16.0)), Face::Bold, t.pt(13.0), Rgb(0xff_6b_81), "♥");
                            }
                            if let Some(days) = item.trash_expires_at.as_deref().and_then(|e| model::days_left(e, self.now_ms)) {
                                let b = Rect::new(x + t.px(3.0), y + side - t.px(18.0), t.px(52.0), t.px(16.0));
                                ui.fill_round(b, t.px(3.0), Rgb(0x10_14_28));
                                let label = if days == 0 { "today".to_owned() } else if days == 1 { "1 day".to_owned() } else { format!("{days} days") };
                                ui.text_in(b.inset(t.px(2.0)), Face::Sans, t.pt(11.0), t.text, &label);
                            }
                            if selected {
                                let tick = Rect::new(x + t.px(3.0), y + t.px(3.0), t.px(18.0), t.px(18.0));
                                ui.fill_round(tick, t.px(9.0), t.accent);
                                ui.text_in(tick.inset(t.px(2.0)), Face::Bold, t.pt(12.0), t.bg, "✓");
                            }
                            if i == self.focus {
                                outline(ui, cell, t.px(3.0).max(2), if ui.is_focused("grid") { t.accent } else { t.dim });
                            } else if selected {
                                outline(ui, cell, t.px(2.0).max(2), t.accent);
                            }
                            let hit = ui.hit(("tile", i), cell);
                            if hit.clicked {
                                click = Some(i);
                            }
                            if hit.secondary {
                                mark = Some(i);
                            }
                        }
                    }
                }
                y += h;
            }
        });
        if let Some(i) = click {
            self.focus = i;
            if self.selected.is_empty() {
                self.open_viewer(i);
            } else {
                self.toggle_selected(i);
            }
        }
        if let Some(i) = mark {
            self.focus = i;
            self.toggle_selected(i);
        }
        // The scroll bar.
        let total = self.total_h();
        if total > self.view_h {
            let bar = t.px(6.0).max(4);
            let h = ((self.view_h as i64 * self.view_h as i64 / total as i64) as i32).max(t.px(24.0));
            let top = r.y + (self.scroll as i64 * (self.view_h - h) as i64 / (total - self.view_h).max(1) as i64) as i32;
            ui.fill_round(Rect::new(r.right() - bar + 1, top, bar - 2, h), bar / 2, if ui.is_focused("grid") { t.accent } else { t.panel_hi });
        }
    }

    /// `GalleryAlbums`.
    fn albums_view(&mut self, ui: &mut Ui) {
        let t = ui.theme();
        let r = ui.cut_top(t.px(30.0));
        ui.text_in(Rect::new(r.x + t.px(12.0), r.y, r.w, r.h), Face::Bold, t.pt(16.0), t.text, "Albums");
        let area = ui.area();
        let list = Rect::new(area.x + t.px(10.0), area.y, area.w - t.px(20.0), area.h - t.px(6.0));
        let albums: Vec<Album> = match &self.albums {
            Some(Ok(a)) => a.clone(),
            Some(Err(e)) => {
                ui.within(list, |ui| ui.label(&format!("Couldn't load the albums. {e}")));
                Vec::new()
            }
            None => {
                ui.within(list, |ui| ui.dim("Loading…"));
                Vec::new()
            }
        };
        let res = ui.list("albums", &mut self.albums_list, list, albums.len(), 44.0, |ui, i, selected| {
            let a = ui.area();
            let al = &albums[i];
            let c = if selected { t.text } else { t.item };
            ui.text_in(Rect::new(a.x + t.px(10.0), a.y + t.px(4.0), a.w - t.px(20.0), t.px(20.0)), Face::Sans, t.pt(15.0), c, &al.title);
            let count = if al.count == 1 { "1 photo".to_owned() } else { format!("{} photos", al.count) };
            let sub = if al.system { format!("{count} · Made by Lucina") } else { count };
            ui.text_in(Rect::new(a.x + t.px(10.0), a.y + t.px(24.0), a.w - t.px(20.0), t.px(16.0)), Face::Sans, t.pt(12.0), t.dim, &sub);
        });
        if albums.is_empty() && matches!(self.albums, Some(Ok(_))) {
            ui.within(list, |ui| {
                ui.dim("No albums yet.");
                ui.dim("Select photos in the Gallery and choose Add to album.");
            });
        }
        if self.albums_list.selected.is_none() && !albums.is_empty() {
            self.albums_list.select(0);
            ui.focus("albums");
        }
        if let Some(i) = res.activated {
            let a = &albums[i];
            self.set_shelf(Shelf::Album { id: a.id.clone(), title: a.title.clone() });
            return;
        }
        if ui.key(keys::KEY_ESC) || ui.key(keys::KEY_BACKSPACE) {
            self.albums_page = false;
            self.focus_grid = true;
        }
    }

    fn dialog_view(&mut self, ui: &mut Ui) {
        let Some(d) = self.dialog.clone() else { return };
        let t = ui.theme();
        match d {
            Dialog::Sources => {
                let (list, all): (Vec<Source>, u64) = match &self.sources {
                    Some(Ok((l, a))) => (l.clone(), *a),
                    _ => (Vec::new(), 0),
                };
                let n = list.len() + 1;
                let mut chosen = None;
                let mut cancelled = false;
                ui.modal(420.0, 90.0 + 34.0 * n.min(10) as f32, |ui| {
                    ui.heading("Where the photos come from");
                    let r = ui.area();
                    let res = ui.list("sources", &mut self.dialog_list, r, n, 32.0, |ui, i, selected| {
                        let a = ui.area();
                        let c = if selected { t.text } else { t.item };
                        let (label, count) = if i == 0 { ("All".to_owned(), all) } else { (list[i - 1].label.clone(), list[i - 1].photos + list[i - 1].videos) };
                        ui.text_in(Rect::new(a.x + t.px(8.0), a.y, a.w - t.px(80.0), a.h), Face::Sans, t.pt(14.0), c, &label);
                        ui.text_in(Rect::new(a.right() - t.px(70.0), a.y, t.px(66.0), a.h), Face::Sans, t.pt(12.0), t.dim, &count.to_string());
                    });
                    if let Some(i) = res.activated {
                        chosen = Some(if i == 0 { "all".to_owned() } else { list[i - 1].id.clone() });
                    }
                    ui.focus("sources");
                    if ui.key(keys::KEY_ESC) {
                        cancelled = true;
                    }
                });
                if let Some(o) = chosen {
                    self.dialog = None;
                    if o != self.origin {
                        self.origin = o;
                        self.reload();
                    }
                    self.focus_grid = true;
                } else if cancelled {
                    self.dialog = None;
                    self.focus_grid = true;
                }
            }
            Dialog::Months => {
                let list: Vec<(String, u64)> = match &self.months {
                    Some(Ok(l)) => l.clone(),
                    _ => Vec::new(),
                };
                let mut chosen = None;
                let mut cancelled = false;
                let months_failed = matches!(self.months, Some(Err(_)));
                ui.modal(360.0, 420.0, |ui| {
                    ui.heading("Jump to a month");
                    if months_failed {
                        ui.dim("Couldn't load the months.");
                    }
                    let r = ui.area();
                    let res = ui.list("months", &mut self.dialog_list, r, list.len(), 30.0, |ui, i, selected| {
                        let a = ui.area();
                        let c = if selected { t.text } else { t.item };
                        ui.text_in(Rect::new(a.x + t.px(8.0), a.y, a.w - t.px(80.0), a.h), Face::Sans, t.pt(14.0), c, &model::month_label(&list[i].0));
                        ui.text_in(Rect::new(a.right() - t.px(70.0), a.y, t.px(66.0), a.h), Face::Sans, t.pt(12.0), t.dim, &list[i].1.to_string());
                    });
                    if let Some(i) = res.activated {
                        chosen = Some(list[i].0.clone());
                    }
                    ui.focus("months");
                    if ui.key(keys::KEY_ESC) {
                        cancelled = true;
                    }
                });
                if let Some(m) = chosen {
                    self.dialog = None;
                    self.focus_grid = true;
                    self.seek(m);
                } else if cancelled {
                    self.dialog = None;
                    self.focus_grid = true;
                }
            }
            Dialog::AddToAlbum(ids) => {
                let albums: Vec<Album> = match &self.albums {
                    Some(Ok(a)) => a.iter().filter(|a| !a.system).cloned().collect(),
                    _ => Vec::new(),
                };
                let mut chosen = None;
                let mut create = false;
                let mut cancelled = false;
                let what = if ids.len() == 1 { "1 item".to_owned() } else { format!("{} items", ids.len()) };
                ui.modal(440.0, 420.0, |ui| {
                    ui.heading("Add to album");
                    ui.dim(&what);
                    let foot = ui.cut_bottom(t.px(84.0));
                    let r = ui.area();
                    let res = ui.list("album-pick", &mut self.dialog_list, r, albums.len(), 34.0, |ui, i, selected| {
                        let a = ui.area();
                        let c = if selected { t.text } else { t.item };
                        ui.text_in(Rect::new(a.x + t.px(8.0), a.y, a.w - t.px(90.0), a.h), Face::Sans, t.pt(14.0), c, &albums[i].title);
                        let count = if albums[i].count == 1 { "1 item".to_owned() } else { format!("{} items", albums[i].count) };
                        ui.text_in(Rect::new(a.right() - t.px(80.0), a.y, t.px(76.0), a.h), Face::Sans, t.pt(12.0), t.dim, &count);
                    });
                    if let Some(i) = res.activated {
                        chosen = Some(albums[i].id.clone());
                    }
                    if albums.is_empty() {
                        ui.within(r, |ui| ui.dim("No albums yet. Create the first one below."));
                    }
                    ui.within(foot, |ui| {
                        if ui.text_field("new-album", &mut self.new_album).submitted {
                            create = true;
                        }
                        ui.row(36.0, |ui| {
                            if ui.button_id("album-create", "Create and add") {
                                create = true;
                            }
                            if ui.button_id("album-cancel", "Cancel") {
                                cancelled = true;
                            }
                        });
                    });
                    if ui.key(keys::KEY_ESC) {
                        cancelled = true;
                    }
                });
                if let Some(a) = chosen {
                    self.run_batch("album_add", ids, Some(a), false);
                    self.dialog = None;
                    self.focus_grid = true;
                } else if create && !self.new_album.text.trim().is_empty() {
                    let title = self.new_album.text.trim().to_owned();
                    self.send(Job::CreateAlbum { title, add: ids });
                    self.dialog = None;
                    self.focus_grid = true;
                } else if cancelled {
                    self.dialog = None;
                    self.focus_grid = true;
                }
            }
            Dialog::Delete { ids, purge, mut also_device, mut understood, error, busy } => {
                let count = ids.len();
                let devices = {
                    let items = self.items_of(&ids);
                    model::device_names(&items)
                };
                let joined = model::join_names(&devices);
                let mut confirm = false;
                let mut cancelled = false;
                ui.modal(500.0, 300.0, |ui| {
                    if purge {
                        ui.heading(&if count == 1 { "Delete forever?".to_owned() } else { format!("Delete {count} items forever?") });
                        ui.paragraph(&if count == 1 {
                            "This removes the item from Lucina for good. It can't be undone.".to_owned()
                        } else {
                            format!("This removes {count} items from Lucina for good. It can't be undone.")
                        });
                        ui.dim("Originals on your devices are not touched.");
                        ui.checkbox("I understand this can't be undone", &mut understood);
                    } else {
                        ui.heading(&if count == 1 { "Move to Trash?".to_owned() } else { format!("Move {count} items to Trash?") });
                        ui.paragraph(if count == 1 { "It moves to Lucina's Trash." } else { "They move to Lucina's Trash." });
                        if !devices.is_empty() {
                            ui.paragraph(&format!("The originals stay on {joined}."));
                        }
                        ui.dim("The Trash is emptied after 30 days; until then you can restore them.");
                        if !devices.is_empty() {
                            ui.checkbox(&format!("Also delete from {joined}"), &mut also_device);
                            if also_device {
                                ui.dim("Each device asks you to confirm before anything is removed from it.");
                            }
                        }
                    }
                    if let Some(e) = &error {
                        ui.label(e);
                    }
                    ui.space(6.0);
                    ui.row(36.0, |ui| {
                        if ui.button_id("del-cancel", "Cancel") {
                            cancelled = true;
                        }
                        let label = if busy {
                            "Working…"
                        } else if purge {
                            "Delete forever"
                        } else {
                            "Move to Trash"
                        };
                        if ui.primary_button(label) {
                            confirm = true;
                        }
                    });
                    if ui.key(keys::KEY_ESC) {
                        cancelled = true;
                    }
                });
                if cancelled {
                    self.dialog = None;
                    self.focus_grid = true;
                    return;
                }
                let also_from_device = also_device && !devices.is_empty();
                let may = !busy && (!purge || understood);
                self.dialog = Some(Dialog::Delete { ids: ids.clone(), purge, also_device, understood, error, busy: busy || (confirm && may) });
                if confirm && may {
                    self.run_batch(if purge { "purge" } else { "trash" }, ids, None, also_from_device);
                }
            }
        }
    }
}

pub(crate) fn outline(ui: &mut Ui, r: Rect, w: i32, c: Rgb) {
    ui.fill(Rect::new(r.x, r.y, r.w, w), c);
    ui.fill(Rect::new(r.x, r.bottom() - w, r.w, w), c);
    ui.fill(Rect::new(r.x, r.y, w, r.h), c);
    ui.fill(Rect::new(r.right() - w, r.y, w, r.h), c);
}

/// What the viewer asks the worker for, from where it can reach.
pub(crate) fn patch_job(item: &Item, body: Value) -> Job {
    Job::Patch { id: item.id.clone(), body, undo: Box::new(item.clone()) }
}

pub(crate) fn favorite_body(on: bool) -> Value {
    json!({"favorite": on})
}

pub(crate) fn rotation_body(r: u16) -> Value {
    json!({"rotation": r})
}

/// `nextRotation`.
pub(crate) fn next_rotation(r: u16) -> u16 {
    (r + 90) % 360
}
