//! The screen. Everything slow is on the engine's thread or the worker's; `view` only reads the state.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use lucina_app::intent::{ArgSpec, Call, Effect, Entity, IntentSpec, Reply, Responder};
use lucina_app::{App, Face, ListState, Rect, Ui, keys};
use secretary_player::progress::{self, Book, Entry};
use secretary_player::queue::{Item, Queue};
use serde::{Deserialize, Serialize};
use serde_json::json;

use lucina_audio_decode::engine::{Cmd, Report};
use crate::library;

/// Between the kernel's -30 dB start and its -12 dB ceiling, and below.
pub const MIN_VOLUME: i32 = -60;
pub const MAX_VOLUME: i32 = -12;
pub const DEFAULT_VOLUME: i32 = -24;
const VOLUME_STEP: i32 = 3;
const SEEK_STEP_MS: u64 = 10_000;
const STATE_FILE: &str = "player.json";

/// The intents Player offers Lucina, listed by `<program> --intents` and named in `manifest.app`.
const INTENTS: &[IntentSpec] = &[
    IntentSpec {
        name: "player.play",
        description: "Play an audio file on this laptop, from the start or from a position in seconds, and return the track.",
        args: &[
            ArgSpec::path("path", "The audio file to play.", true),
            ArgSpec::int("at", "Start this many seconds in.", false, 0, 86_400),
        ],
        effect: Effect::Write,
        shows: true,
        inverse: None,
    },
    IntentSpec {
        name: "player.pause",
        description: "Pause what is playing.",
        args: &[],
        effect: Effect::Write,
        shows: false,
        inverse: None,
    },
    IntentSpec {
        name: "player.resume",
        description: "Resume what is paused.",
        args: &[],
        effect: Effect::Write,
        shows: false,
        inverse: None,
    },
    IntentSpec {
        name: "player.now",
        description: "Say what is playing now: whether it plays, the track, and the position.",
        args: &[],
        effect: Effect::Read,
        shows: false,
        inverse: None,
    },
];

pub enum Msg {
    Engine(Report),
    Local(Vec<Item>),
    Remote(Result<Vec<Item>, String>),
    /// The file of a library item is on the disk.
    Fetched { id: String, result: Result<PathBuf, String> },
    Progress(Vec<Entry>),
    Pushed(Vec<(String, u64)>),
}

pub enum Job {
    ListLocal,
    ListRemote,
    Fetch(String),
    Push(Vec<Entry>),
}

pub type Link = Box<dyn Fn(Job)>;
pub type Engine = Box<dyn Fn(Cmd)>;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Here,
    Library,
}

#[derive(Default, Serialize, Deserialize)]
struct Saved {
    #[serde(default)]
    queue: Queue,
    #[serde(default)]
    book: Book,
    #[serde(default)]
    volume: Option<i32>,
}

pub struct Player {
    pub tab: Tab,
    pub here: Vec<Item>,
    pub remote: Vec<Item>,
    pub remote_state: String,
    pub list: ListState,
    pub queue: Queue,
    pub book: Book,
    pub volume: i32,
    pub position_ms: u64,
    pub duration_ms: Option<u64>,
    pub playing: bool,
    pub has_track: bool,
    pub message: String,
    pub sample: bool,
    data_dir: PathBuf,
    engine: Engine,
    link: Link,
    signed_in: bool,
    last_save_ms: u64,
    /// The list gets the keyboard when it first has rows: on a button, Space would press that button.
    focus_list: bool,
    /// A file given on the command line (`lucina-app-player <file>`): it plays as soon as the list has it.
    pub open_on_start: Option<String>,
}

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

pub fn clock(ms: u64) -> String {
    let s = ms / 1000;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

impl Player {
    pub fn new(data_dir: PathBuf, signed_in: bool, engine: Engine, link: Link) -> Player {
        let saved: Saved = std::fs::read(data_dir.join(STATE_FILE))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let volume = saved.volume.unwrap_or(DEFAULT_VOLUME).clamp(MIN_VOLUME, MAX_VOLUME);
        engine(Cmd::Volume(volume));
        link(Job::ListLocal);
        if signed_in {
            link(Job::ListRemote);
        }
        Player {
            tab: Tab::Here,
            here: Vec::new(),
            remote: Vec::new(),
            remote_state: String::from(if signed_in { "Loading the library" } else { "Sign in from an account app to see the library" }),
            list: ListState::new(),
            queue: saved.queue,
            book: saved.book,
            volume,
            position_ms: 0,
            duration_ms: None,
            playing: false,
            has_track: false,
            message: String::new(),
            sample: false,
            data_dir,
            engine,
            link,
            signed_in,
            last_save_ms: 0,
            focus_list: true,
            open_on_start: None,
        }
    }

    fn rows(&self) -> &[Item] {
        match self.tab {
            Tab::Here => &self.here,
            Tab::Library => &self.remote,
        }
    }

    pub fn save(&mut self) {
        let saved = serde_json::json!({ "queue": self.queue, "book": self.book, "volume": self.volume });
        let path = self.data_dir.join(STATE_FILE);
        if let Ok(bytes) = serde_json::to_vec(&saved) {
            let _ = lucina_app_support::files::write_atomic(&path, &bytes);
        }
        self.last_save_ms = now_ms();
    }

    /// Plays what the queue says is current, from where it was left.
    pub fn start_current(&mut self) {
        let Some(item) = self.queue.current().cloned() else {
            self.has_track = false;
            self.playing = false;
            return;
        };
        self.has_track = true;
        self.position_ms = 0;
        self.duration_ms = item.duration_ms;
        self.message.clear();
        match library::local_path(&item.id) {
            Some(path) => self.play_file(&item.id, path),
            None => {
                self.message = format!("Fetching {}", item.title);
                (self.link)(Job::Fetch(item.id));
            }
        }
    }

    fn play_file(&mut self, id: &str, path: PathBuf) {
        let start_ms = progress::resume_point(self.book.get(id), self.duration_ms);
        self.position_ms = start_ms;
        self.playing = true;
        (self.engine)(Cmd::Play { path, start_ms });
    }

    pub fn activate(&mut self, index: usize) {
        let items = self.rows().to_vec();
        self.queue.set(items, index, now_ms());
        self.start_current();
    }

    pub fn toggle(&mut self) {
        if self.has_track {
            if self.playing {
                (self.engine)(Cmd::Pause);
            } else {
                self.playing = true;
                (self.engine)(Cmd::Resume);
            }
        } else if self.queue.current().is_some() {
            self.start_current();
        }
    }

    fn record(&mut self, playing: bool) {
        let Some(id) = self.queue.current().map(|i| i.id.clone()) else {
            return;
        };
        let tick = self.book.tick(&id, self.position_ms, self.duration_ms.unwrap_or(0), playing, now_ms(), Some(String::from("juno")));
        if tick.sync && !library::is_local(&id) && self.signed_in {
            let pending: Vec<Entry> = self.book.pending().into_iter().filter(|e| !library::is_local(&e.id)).collect();
            if !pending.is_empty() {
                (self.link)(Job::Push(pending));
            }
        }
    }

    pub fn seek_by(&mut self, delta_ms: i64) {
        if !self.has_track {
            return;
        }
        let mut target = self.position_ms.saturating_add_signed(delta_ms);
        if let Some(duration) = self.duration_ms {
            target = target.min(duration.saturating_sub(1000));
        }
        self.position_ms = target;
        (self.engine)(Cmd::Seek(target));
    }

    pub fn change_volume(&mut self, delta: i32) {
        self.volume = (self.volume + delta).clamp(MIN_VOLUME, MAX_VOLUME);
        (self.engine)(Cmd::Volume(self.volume));
    }

    pub fn next(&mut self, auto: bool) {
        if self.has_track {
            self.record(false);
        }
        if self.queue.next(auto) {
            self.start_current();
        } else {
            self.has_track = false;
            self.playing = false;
            (self.engine)(Cmd::Stop);
            self.save();
        }
    }

    pub fn previous(&mut self) {
        if !self.has_track {
            return;
        }
        match self.queue.prev(self.position_ms) {
            secretary_player::queue::Prev::Restart => {
                self.seek_by(-(self.position_ms as i64));
            }
            secretary_player::queue::Prev::Moved => {
                self.record(false);
                self.start_current();
            }
            secretary_player::queue::Prev::None => {}
        }
    }

    fn draw_now_playing(&mut self, ui: &mut Ui) {
        let t = ui.theme();
        let title = self.queue.current().map_or_else(|| String::from("Nothing playing"), |i| i.title.clone());
        ui.row(26.0, |ui| {
            let r = ui.area();
            ui.text_in(r, Face::Sans, t.pt(17.0), t.text, &title);
        });
        let times = match self.duration_ms {
            Some(duration) => format!("{} / {}", clock(self.position_ms), clock(duration)),
            None => clock(self.position_ms),
        };
        ui.dim(&times);
        let bar = ui.cut_top(t.px(6.0));
        ui.fill(bar, t.panel);
        if let Some(duration) = self.duration_ms.filter(|d| *d > 0) {
            let w = (i64::from(bar.w) * self.position_ms.min(duration) as i64 / duration as i64) as i32;
            ui.fill(Rect::new(bar.x, bar.y, w, bar.h), t.accent);
        }
        ui.space(8.0);
        ui.row(36.0, |ui| {
            if ui.button("Previous") {
                self.previous();
            }
            if ui.button("Back 10 s") {
                self.seek_by(-(SEEK_STEP_MS as i64));
            }
            if ui.primary_button(if self.playing { "Pause" } else { "Play" }) {
                self.toggle();
            }
            if ui.button("Forward 10 s") {
                self.seek_by(SEEK_STEP_MS as i64);
            }
            if ui.button("Next") {
                self.next(false);
            }
            if ui.button("Quieter") {
                self.change_volume(-VOLUME_STEP);
            }
            if ui.button("Louder") {
                self.change_volume(VOLUME_STEP);
            }
            let volume = format!("{} dB", self.volume);
            ui.dim(&volume);
        });
    }
}

impl Player {
    /// `player.play`: the file among the local items, or as a new local item, made current the way a row's activation does.
    fn intent_play(&mut self, call: &Call, reply: Responder) {
        let path = PathBuf::from(call.args["path"].as_str().unwrap_or(""));
        if !path.is_file() {
            return reply.send(Reply::failed(format!("{} is not a file", path.display())));
        }
        if !library::is_audio_file(&path) {
            return reply.send(Reply::failed(format!(
                "{} is not an audio file Player can play",
                path.display()
            )));
        }
        let mut items = self.here.clone();
        let index = match items
            .iter()
            .position(|i| library::local_path(&i.id).as_deref() == Some(path.as_path()))
        {
            Some(i) => i,
            None => {
                items.push(library::item_of(path.clone()));
                items.len() - 1
            }
        };
        self.queue.set(items, index, now_ms());
        self.start_current();
        if let Some(at) = call.args["at"].as_i64() {
            let at_ms = (at * 1000) as u64;
            self.seek_by(at_ms as i64 - self.position_ms as i64);
        }
        let Some(item) = self.queue.current() else {
            return reply.send(Reply::failed(format!("{} did not start", path.display())));
        };
        let (id, title) = (item.id.clone(), item.title.clone());
        reply.send(
            Reply::done(format!("Playing {title}"))
                .with_entity(Entity::new("track", &id, &title))
                .with_value(json!({"title": title, "path": path.display().to_string(), "at_ms": self.position_ms})),
        );
    }
}

impl App for Player {
    type Msg = Msg;

    fn intents() -> &'static [IntentSpec] {
        INTENTS
    }

    fn intent(&mut self, call: Call, reply: Responder) {
        match call.name.as_str() {
            "player.play" => self.intent_play(&call, reply),
            "player.pause" => {
                if self.playing {
                    let title = self
                        .queue
                        .current()
                        .map_or_else(String::new, |i| i.title.clone());
                    self.toggle();
                    reply.send(Reply::done(format!("Paused {title}")));
                } else {
                    reply.send(Reply::failed("Nothing is playing"));
                }
            }
            "player.resume" => {
                if self.has_track && !self.playing {
                    let title = self
                        .queue
                        .current()
                        .map_or_else(String::new, |i| i.title.clone());
                    self.toggle();
                    reply.send(Reply::done(format!("Resumed {title}")));
                } else {
                    reply.send(Reply::failed("Nothing to resume"));
                }
            }
            "player.now" => {
                let mut value = json!({"playing": self.playing, "position_ms": self.position_ms});
                match self.queue.current() {
                    Some(item) => {
                        value["title"] = json!(item.title);
                        value["id"] = json!(item.id);
                        if let Some(duration) = self.duration_ms {
                            value["duration_ms"] = json!(duration);
                        }
                        let (id, title) = (item.id.clone(), item.title.clone());
                        let state = if self.playing { "Playing" } else { "Paused on" };
                        reply.send(
                            Reply::done(format!("{state} {title}"))
                                .with_value(value)
                                .with_entity(Entity::new("track", &id, &title)),
                        );
                    }
                    None => reply.send(Reply::done("Nothing playing").with_value(value)),
                }
            }
            _ => reply.send(Reply::failed(format!("no intent {} here", call.name))),
        }
    }

    fn message(&mut self, msg: Msg) {
        match msg {
            Msg::Engine(Report::Position { ms, duration_ms }) => {
                self.position_ms = ms;
                if duration_ms.is_some() {
                    self.duration_ms = duration_ms;
                }
                self.record(true);
                if now_ms() - self.last_save_ms > 10_000 {
                    self.save();
                }
            }
            Msg::Engine(Report::Paused { ms }) => {
                self.position_ms = ms;
                self.playing = false;
                self.record(false);
                self.save();
            }
            Msg::Engine(Report::Ended) => {
                if let Some(duration) = self.duration_ms {
                    self.position_ms = duration;
                }
                self.next(true);
            }
            Msg::Engine(Report::Failed(why)) => {
                self.message = why;
                self.playing = false;
                self.has_track = false;
            }
            Msg::Local(items) => {
                self.here = items;
                if let Some(id) = self.open_on_start.take() {
                    match self.here.iter().position(|i| i.id == id) {
                        Some(index) => {
                            self.tab = Tab::Here;
                            self.activate(index);
                        }
                        None => self.message = "That file is not one Player can play".into(),
                    }
                }
            }
            Msg::Remote(Ok(items)) => {
                self.remote_state = format!("{} items", items.len());
                self.remote = items;
            }
            Msg::Remote(Err(why)) => self.remote_state = why,
            Msg::Fetched { id, result } => {
                // Only if it is still what should play.
                if self.queue.current().is_some_and(|i| i.id == id) {
                    match result {
                        Ok(path) => {
                            self.message.clear();
                            self.play_file(&id, path);
                        }
                        Err(why) => {
                            self.message = why;
                            self.has_track = false;
                        }
                    }
                }
            }
            Msg::Progress(entries) => {
                self.book.merge(entries);
            }
            Msg::Pushed(acks) => self.book.acknowledge(&acks),
        }
    }

    fn on_quit(&mut self) {
        if self.has_track {
            self.record(false);
        }
        (self.engine)(Cmd::Stop);
        self.save();
    }

    fn view(&mut self, ui: &mut Ui) {
        ui.header("Player");
        ui.pad(14.0);
        let t = ui.theme();
        if self.sample {
            ui.dim("Sample: made-up tracks, no account");
        }
        ui.row(36.0, |ui| {
            if ui.button("On this laptop") && self.tab != Tab::Here {
                self.tab = Tab::Here;
                self.list.invalidate();
                self.focus_list = true;
            }
            if ui.button("Media library") && self.tab != Tab::Library {
                self.tab = Tab::Library;
                self.list.invalidate();
                self.focus_list = true;
            }
            let note = match self.tab {
                Tab::Here => format!("{} files in /data/music and /data/recordings", self.here.len()),
                Tab::Library => self.remote_state.clone(),
            };
            ui.dim(&note);
        });
        ui.space(6.0);
        let bottom = ui.cut_bottom(t.px(112.0));
        let mut activated = None;
        {
            let r = ui.area();
            let rows: Vec<(String, String)> = self
                .rows()
                .iter()
                .map(|i| (i.title.clone(), i.duration_ms.map_or_else(String::new, clock)))
                .collect();
            let current = self.queue.current().map(|i| i.id.clone());
            let ids: Vec<String> = self.rows().iter().map(|i| i.id.clone()).collect();
            let res = ui.list("tracks", &mut self.list, r, rows.len(), 30.0, |ui, i, selected| {
                let a = ui.area();
                let th = ui.theme();
                let playing_this = current.as_deref() == Some(ids[i].as_str());
                let colour = if playing_this {
                    th.accent
                } else if selected {
                    th.text
                } else {
                    th.item
                };
                let time_w = th.px(90.0);
                let text = Rect::new(a.x + th.px(8.0), a.y, a.w - th.px(8.0) - time_w, a.h);
                ui.text_in(text, Face::Sans, th.pt(15.0), colour, &rows[i].0);
                let time = Rect::new(a.x + a.w - time_w, a.y, time_w, a.h);
                ui.text_in(time, Face::Sans, th.pt(13.0), th.dim, &rows[i].1);
            });
            activated = res.activated.or(activated);
        }
        if self.focus_list && !self.rows().is_empty() {
            self.focus_list = false;
            if self.list.selected.is_none() {
                self.list.select(0);
            }
            ui.focus("tracks");
        }
        if let Some(i) = activated {
            self.activate(i);
        }
        ui.within(bottom, |ui| {
            if !self.message.is_empty() {
                let message = self.message.clone();
                ui.dim(&message);
            }
            self.draw_now_playing(ui);
        });
        if ui.key(keys::KEY_SPACE) {
            self.toggle();
        }
        if self.playing {
            ui.wake_after(Duration::from_millis(500));
        }
    }
}
