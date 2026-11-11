//! The screen: one button, the time, a level meter, and the recordings with how far each one has got to the server. Choosing a
//! recording plays it; choosing it again stops it.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use lucina_app::{App, Face, ListState, Rect, Ui};
use lucina_audio_decode::engine::{Cmd, Report};

use crate::capture::{Finished, Live};
use crate::upload::{self, Meta};
use crate::wav;

pub enum Msg {
    Finished(Finished),
    Listed(Vec<Row>),
    Progress { path: PathBuf, uploaded: u64 },
    Uploaded { path: PathBuf, result: Result<(), String> },
    /// From the thread that plays a recording back.
    Played(Report),
}

pub enum Job {
    /// Fill in what a crash left unfinished, and list what is there.
    List,
    /// Send what has not gone yet, one at a time.
    Send,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub path: PathBuf,
    pub title: String,
    pub seconds: f64,
    pub size: u64,
    pub uploaded: u64,
    pub done: bool,
    pub error: Option<String>,
}

pub type Link = Box<dyn Fn(Job)>;
pub type Start = Box<dyn Fn(PathBuf, Arc<Live>)>;
pub type Play = Box<dyn Fn(Cmd)>;

/// Shorter than this is a slip of the finger.
const MIN_SECONDS: f64 = 0.5;
const RETRY: Duration = Duration::from_secs(60);

pub struct Recorder {
    pub rows: Vec<Row>,
    pub list: ListState,
    pub live: Option<Arc<Live>>,
    pub message: String,
    pub signed_in: bool,
    dir: PathBuf,
    start: Start,
    link: Link,
    timezone: String,
    retry_at: Option<std::time::Instant>,
    play: Play,
    /// The recording playing back, if any.
    pub playing: Option<PathBuf>,
}

pub fn clock(seconds: f64) -> String {
    let s = seconds as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

pub fn new_meta(data_bytes: u64, timezone: &str, recorded_at: String, client_id: String) -> Meta {
    Meta {
        client_id,
        recorded_at,
        timezone: timezone.to_owned(),
        duration_sec: wav::seconds(data_bytes),
        size_bytes: data_bytes + 44,
        server_id: None,
        uploaded: 0,
        done: false,
        error: None,
    }
}

impl Recorder {
    pub fn new(dir: PathBuf, signed_in: bool, timezone: String, start: Start, link: Link, play: Play) -> Recorder {
        link(Job::List);
        if signed_in {
            link(Job::Send);
        }
        Recorder {
            rows: Vec::new(),
            list: ListState::new(),
            live: None,
            message: String::new(),
            signed_in,
            dir,
            start,
            link,
            timezone,
            retry_at: None,
            play,
            playing: None,
        }
    }

    /// Plays recording `i`, or stops it if it is the one playing. Not while recording: the device takes one stream at a time.
    pub fn choose(&mut self, i: usize) {
        if self.live.is_some() {
            return;
        }
        let Some(path) = self.rows.get(i).map(|r| r.path.clone()) else {
            return;
        };
        if self.playing.as_ref() == Some(&path) {
            (self.play)(Cmd::Stop);
            self.playing = None;
        } else {
            (self.play)(Cmd::Play { path: path.clone(), start_ms: 0 });
            self.playing = Some(path);
            self.message.clear();
        }
    }

    pub fn file_name(now: chrono::DateTime<chrono::Local>) -> String {
        now.format("%Y-%m-%d %H.%M.%S.wav").to_string()
    }

    pub fn begin(&mut self, name: &str) {
        if self.live.is_some() {
            return;
        }
        if self.playing.take().is_some() {
            (self.play)(Cmd::Stop);
        }
        let live = Arc::new(Live::default());
        self.live = Some(live.clone());
        self.message.clear();
        (self.start)(self.dir.join(name), live);
    }

    pub fn stop(&mut self) {
        if let Some(live) = &self.live {
            live.stop.store(true, Ordering::Relaxed);
        }
    }

    fn finished(&mut self, finished: Finished) {
        self.live = None;
        if let Some(why) = &finished.error {
            self.message = why.clone();
        }
        if wav::seconds(finished.data_bytes) < MIN_SECONDS {
            let _ = std::fs::remove_file(&finished.path);
            if finished.error.is_none() {
                self.message = String::from("Too short to keep");
            }
            return;
        }
        let meta = new_meta(
            finished.data_bytes,
            &self.timezone,
            chrono::Utc::now().to_rfc3339(),
            lucina_account::engine::new_uuid(),
        );
        upload::save_meta(&finished.path, &meta);
        (self.link)(Job::List);
        if self.signed_in {
            (self.link)(Job::Send);
        }
    }
}

/// What is in the directory. A recording with no file beside it was cut off: its header is filled in here.
pub fn list(dir: &Path, timezone: &str) -> Vec<Row> {
    let mut rows = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return rows;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("wav") {
            continue;
        }
        let meta = upload::load_meta(&path).or_else(|| {
            let data = wav::repair(&path).ok()?;
            let modified: chrono::DateTime<chrono::Utc> = entry.metadata().ok()?.modified().ok()?.into();
            let meta = new_meta(data, timezone, modified.to_rfc3339(), lucina_account::engine::new_uuid());
            upload::save_meta(&path, &meta);
            Some(meta)
        });
        let Some(meta) = meta else {
            continue;
        };
        rows.push(Row {
            title: path.file_stem().map_or_else(String::new, |s| s.to_string_lossy().into_owned()),
            seconds: meta.duration_sec,
            size: meta.size_bytes,
            uploaded: meta.uploaded,
            done: meta.done,
            error: meta.error.clone(),
            path,
        });
    }
    rows.sort_by(|a, b| b.title.cmp(&a.title));
    rows
}

fn row_status(row: &Row, signed_in: bool) -> String {
    if row.done {
        String::from("Sent: being transcribed on the server")
    } else if let Some(why) = &row.error {
        format!("Not sent yet: {why}")
    } else if !signed_in {
        String::from("Sign in from an account app to send it")
    } else if row.uploaded > 0 {
        format!("Sending, {} %", row.uploaded * 100 / row.size.max(1))
    } else {
        String::from("Waiting to send")
    }
}

impl App for Recorder {
    type Msg = Msg;

    fn message(&mut self, msg: Msg) {
        match msg {
            Msg::Finished(finished) => self.finished(finished),
            Msg::Listed(rows) => self.rows = rows,
            // As the Player does when nothing follows: the device is let go, so the stream ends and the amplifier goes off.
            Msg::Played(Report::Ended) => {
                self.playing = None;
                (self.play)(Cmd::Stop);
            }
            Msg::Played(Report::Failed(why)) => {
                self.playing = None;
                self.message = format!("Cannot play it: {why}");
            }
            Msg::Played(_) => {}
            Msg::Progress { path, uploaded } => {
                if let Some(row) = self.rows.iter_mut().find(|r| r.path == path) {
                    row.uploaded = uploaded;
                    row.error = None;
                }
            }
            Msg::Uploaded { path, result } => {
                if let Some(row) = self.rows.iter_mut().find(|r| r.path == path) {
                    match result {
                        Ok(()) => {
                            row.done = true;
                            row.uploaded = row.size;
                            row.error = None;
                        }
                        Err(why) => {
                            row.error = Some(why);
                            self.retry_at = Some(std::time::Instant::now() + RETRY);
                        }
                    }
                }
            }
        }
    }

    fn on_quit(&mut self) {
        self.stop();
    }

    fn view(&mut self, ui: &mut Ui) {
        ui.header("Recorder");
        ui.pad(14.0);
        let t = ui.theme();
        let recording = self.live.clone();
        ui.row(44.0, |ui| {
            let label = if recording.is_some() { "Stop" } else { "Record" };
            if ui.primary_button(label) {
                if recording.is_some() {
                    self.stop();
                } else {
                    let name = Recorder::file_name(chrono::Local::now());
                    self.begin(&name);
                }
            }
            if let Some(live) = &recording {
                let bytes = live.bytes.load(Ordering::Relaxed);
                ui.label(&clock(wav::seconds(bytes)));
            }
        });
        ui.space(6.0);
        let meter = ui.cut_top(t.px(10.0));
        ui.fill(meter, t.panel);
        if let Some(live) = &recording {
            let peak = i64::from(live.peak.load(Ordering::Relaxed).min(32767));
            let w = (i64::from(meter.w) * peak / 32767) as i32;
            ui.fill(Rect::new(meter.x, meter.y, w, meter.h), t.accent);
        }
        ui.space(6.0);
        if !self.message.is_empty() {
            let message = self.message.clone();
            ui.dim(&message);
        }
        let r = ui.area();
        let rows = &self.rows;
        let signed_in = self.signed_in;
        let playing = self.playing.clone();
        let res = ui.list("recordings", &mut self.list, r, rows.len(), 34.0, |ui, i, selected| {
            let a = ui.area();
            let th = ui.theme();
            let colour = if selected { th.text } else { th.item };
            let right = th.px(620.0).min(a.w / 2);
            let name = format!("{}   {}", rows[i].title, clock(rows[i].seconds));
            let text = Rect::new(a.x + th.px(8.0), a.y, a.w - th.px(8.0) - right, a.h);
            ui.text_in(text, Face::Sans, th.pt(15.0), colour, &name);
            let status = Rect::new(a.x + a.w - right, a.y, right, a.h);
            let state = if playing.as_ref() == Some(&rows[i].path) { String::from("Playing") } else { row_status(&rows[i], signed_in) };
            ui.text_in(status, Face::Sans, th.pt(13.0), th.dim, &state);
        });
        if let Some(i) = res.activated {
            self.choose(i);
        }
        if recording.is_some() {
            ui.wake_after(Duration::from_millis(100));
        }
        if let Some(at) = self.retry_at {
            if std::time::Instant::now() >= at {
                self.retry_at = None;
                (self.link)(Job::Send);
            } else {
                ui.wake_after(RETRY);
            }
        }
    }
}
