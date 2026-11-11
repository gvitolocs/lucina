//! lucina-app-video: plays a video file full screen, following its sound.
//!
//!   lucina-app-video <file or folder> [more]
//!   lucina-app-video --item <library id>    or `newest`, for a test: the shortest of the newest
//!   lucina-app-video                        the tile: a list of the videos on this laptop
//!
//! This is not an `App`: a frame goes straight to the screen, and the toolkit's frame and its comparison would cost a third of what a frame
//! may take. It shares the toolkit's framebuffer, input and text. Space pauses, Left and Right go 10 s, Up and Down 60 s, N and P move in
//! the queue, S changes the scaling, I shows the bar, Escape ends. F1 is the shell's: it asks with SIGTERM, and the position is saved.
//!
//! Started from Gallery or Drive it is a child program, drawn over them: `lucina_app::program`.

// Juno with no C library: Eyra is the libc and the program start, in Rust (docs/research/juno-pure-rust.md).
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
extern crate eyra;

mod list;
mod present;
mod remote;

use lucina_video_play::{decode, pace, sound, worker};

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use decode::{H264_REORDER, Picture};
use lucina_app::fb::{Fb, GraphicsMode};
use lucina_app::gfx::{Frame, Text};
use lucina_app::hold::DisplayHold;
use lucina_app::input::{self, EV_KEY, EV_SYN, Inputs};
use lucina_app::keys::{self, Keyboard, Press};
use lucina_app::intent::{Call, Entity, IntentSpec, Reply, Responder};
use lucina_app::{App, Face, Rect, Rgb, Ui, input_watch};
use lucina_audio_decode::Decoder;
use lucina_nav as nav;
use lucina_video::{Clock, Codec, Demuxer, Quality, Scaler, Track};
use pace::{SKIP_AFTER_US, SKIP_UNTIL_US, State, Step};
use secretary_player::subtitles::{self, Cue};
use serde_json::{Value, json};
use sound::Sound;
use worker::{Command, Worker};

static TERMINATE: AtomicBool = AtomicBool::new(false);

extern "C" fn on_sigterm(_: libc::c_int) {
    TERMINATE.store(true, Ordering::SeqCst);
}

// Evdev codes the toolkit's `keys` does not name yet.
const KEY_I: u16 = 23;
const KEY_P: u16 = 25;
const KEY_S: u16 = 31;
const KEY_N: u16 = 49;
const KEY_SPACE: u16 = 57;
const KEY_V: u16 = 47;

/// When the frame rate is not known: a guess that only affects when a late frame is thrown away.
const DEFAULT_FRAME_US: i64 = 41_666;
const BAR_H: i32 = 48;

/// Where the position, queue and subtitle choice are kept. `/data` when there is one.
fn state_path() -> PathBuf {
    let base = if Path::new("/run/lucina/data-ready").exists() { "/data/home/.local/share/lucina-video" } else { "/run/lucina/files/.local/share/lucina-video" };
    PathBuf::from(base).join("player.json")
}

fn rss_kib() -> i64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| s.lines().find(|l| l.starts_with("VmRSS:"))?.split_whitespace().nth(1)?.parse().ok())
        .unwrap_or(-1)
}

fn cpu_seconds() -> f64 {
    // SAFETY: getrusage fills the struct it is given.
    let mut u: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut u) };
    let s = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 / 1e6;
    s(u.ru_utime) + s(u.ru_stime)
}

/// Counts what happened, for the log line a measurement reads.
#[derive(Default)]
struct Stats {
    shown: u64,
    dropped: u64,
    jumps: u64,
    convert_ms: f64,
    present_ms: f64,
    late_ms_sum: f64,
    worst_late_ms: f64,
    /// Frames the GPU converted and scaled.
    gpu_frames: u64,
    /// Frames shown against the sound's position; their lateness is how far the picture is behind the sound.
    sound_frames: u64,
}

struct Session {
    path: PathBuf,
    id: String,
    /// The account's connection, when this came from the library.
    client: Option<lucina_account::Client>,
    /// What the server's `/play` said to start from, over what this machine remembers.
    resume_ms: Option<u64>,
    title: String,
    track: Track,
    worker: Worker,
    codec: Codec,
    reorder: usize,
    cues: Vec<Cue>,
    started: Instant,
    cpu_at_start: f64,
    stats: Stats,
    position_us: i64,
    first_shown: bool,
    /// How to read the file's sound, where there is a way.
    open_sound: Option<sound::Open>,
    /// It played to its end, rather than being left.
    finished: bool,
    /// When the present report was last sent: the first frame, then once a second, not 30 times.
    reported_at: Option<Instant>,
}

/// The sidecar subtitle file with the video's name, if any: `.srt` or `.vtt`.
fn sidecar_cues(path: &Path) -> Vec<Cue> {
    for ext in ["srt", "vtt"] {
        if let Ok(text) = std::fs::read_to_string(path.with_extension(ext)) {
            let cues = subtitles::parse(&text);
            if !cues.is_empty() {
                return cues;
            }
        }
    }
    Vec::new()
}

fn start_session(path: PathBuf, id: String, title: String, demux: Box<dyn Demuxer>, cues: Vec<Cue>, client: Option<lucina_account::Client>, resume_ms: Option<u64>, wake_fd: libc::c_int) -> Result<Session, String> {
    let track = demux.track().clone();
    let codec = track.codec;
    let reorder = if codec == Codec::H264 { H264_REORDER } else { 0 };
    // Stage D: Lucina waits for `media.ended` instead of watching the screen. The subject is the library item or the file.
    lucina_app::events::emit("media.started").subject(&media_subject(&id)).field("title", title.as_str()).send();
    Ok(Session {
        path,
        id,
        client,
        resume_ms,
        title,
        track,
        worker: Worker::start(demux, codec, reorder, wake_fd)?,
        codec,
        reorder,
        cues,
        started: Instant::now(),
        cpu_at_start: cpu_seconds(),
        stats: Stats::default(),
        position_us: 0,
        first_shown: false,
        open_sound: None,
        finished: false,
        reported_at: None,
    })
}

/// `item:<id>` for the owner's library, `file:<path>` for a file here.
fn media_subject(id: &str) -> String {
    match id.strip_prefix("local:") {
        Some(path) => format!("file:{path}"),
        None => format!("item:{id}"),
    }
}

fn open_session(path: &Path, wake_fd: libc::c_int) -> Result<Session, String> {
    let file = File::open(path).map_err(|e| format!("Cannot open this file: {e}"))?;
    let demux = lucina_video::open(BufReader::with_capacity(256 * 1024, file)).map_err(|e| e.to_string())?;
    let title = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut session = start_session(path.to_owned(), format!("local:{}", path.display()), title, demux, sidecar_cues(path), None, None, wake_fd)?;
    let sound_path = path.to_owned();
    session.open_sound = Some(Arc::new(move || Decoder::open(&sound_path)));
    Ok(session)
}

/// A session on a video of the owner's library.
fn open_item(id: &str, wake_fd: libc::c_int) -> Result<Session, String> {
    let client = lucina_account::system::client().ok_or("This laptop is not signed in, so the library is out of reach.")?;
    let chosen;
    let id = if id == "newest" {
        chosen = remote::newest_video(&client)?;
        &chosen
    } else {
        id
    };
    let plan = remote::plan(&client, id)?;
    eprintln!("lucina-app-video: library item {id} mode={} hls={}", plan.mode, plan.hls_url.is_some());
    let demux = remote::demuxer(client.clone(), &plan)?;
    let cues = remote::cues(&client, &plan);
    let mut session = start_session(PathBuf::from(id), id.to_owned(), plan.title.clone(), demux, cues, Some(client.clone()), Some(plan.resume_ms), wake_fd)?;
    session.open_sound = remote::sound(client, &plan);
    Ok(session)
}

impl Session {
    /// The present report for `wait` (stage D): this program draws past the toolkit's `Screen`, so it reports itself, the whole
    /// screen as the rectangle.
    fn report_present(&mut self, w: u32, h: u32) {
        if self.reported_at.is_some_and(|t| t.elapsed() < Duration::from_secs(1)) {
            return;
        }
        self.reported_at = Some(Instant::now());
        lucina_app::events::present(self.stats.shown, &[[0, 0, w as i32, h as i32]], 0);
    }

    /// The sound of this file, if it says it has any. The last file's goes first: it has to let the device go.
    fn sound(&self) -> Sound {
        // For measuring with nobody at the laptop: the picture follows the wall clock and the speakers stay quiet.
        if std::env::var_os("LUCINA_VIDEO_MUTE").is_some_and(|v| v == "1") {
            eprintln!("lucina-app-video: muted by LUCINA_VIDEO_MUTE {}", self.title);
            return Sound::none();
        }
        match (&self.track.audio, &self.open_sound) {
            (Some(codec), Some(open)) => {
                eprintln!("lucina-app-video: sound {} codec={codec}", self.title);
                Sound::from(open.clone())
            }
            (Some(codec), None) => {
                eprintln!("lucina-app-video: no sound from this source yet: {} codec={codec}", self.title);
                Sound::none()
            }
            (None, _) => Sound::none(),
        }
    }

    fn duration_ms(&self) -> u64 {
        self.track.duration_us.map_or(0, |d| (d / 1000) as u64)
    }

    /// Tells the decoding thread to go there, and waits until it has: a local file takes no time, and a remote one is what a seek costs.
    fn seek(&mut self, pts_us: i64, forward: bool) -> Result<i64, String> {
        self.worker.command(Command::Seek { pts: pts_us, forward, from: self.position_us });
        loop {
            let mut s = self.worker.lock();
            if let Some(at) = s.landed.take() {
                self.position_us = at;
                return Ok(at);
            }
            if let Some(e) = s.error.take() {
                return Err(e);
            }
            drop(s);
            if TERMINATE.load(Ordering::SeqCst) {
                return Ok(self.position_us);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn log_summary(&self, why: &str) {
        if !self.finished {
            lucina_app::events::emit("media.stopped")
                .subject(&media_subject(&self.id))
                .field("reason", if why == "error" { "error" } else { "left" })
                .field("position_s", self.position_us.max(0) / 1_000_000)
                .send();
        }
        let wall = self.started.elapsed().as_secs_f64();
        let cpu = cpu_seconds() - self.cpu_at_start;
        let s = &self.stats;
        let per = |ms: f64, n: u64| ms / n.max(1) as f64;
        let w = self.worker.lock();
        let c = &w.counts;
        eprintln!(
            "lucina-app-video: played {} codec={} why={why} wall_s={wall:.1} shown={} fps={:.2} dropped={} skipped={} jumps={} decode_ms={:.1} decode_ms_worst={:.0} convert_ms={:.1} present_ms={:.1} late_ms_mean={:.1} late_ms_worst={:.0} cpu_pct={:.0} rss_kib={} present={} decoder={} sound_frames={}",
            self.path.display(),
            self.codec.name(),
            s.shown,
            s.shown as f64 / wall.max(0.001),
            s.dropped,
            c.skipped,
            s.jumps,
            per(c.decode_ms, c.decoded),
            c.worst_decode_ms,
            per(s.convert_ms, s.shown),
            per(s.present_ms, s.shown),
            per(s.late_ms_sum, s.shown),
            s.worst_late_ms,
            cpu / wall.max(0.001) * 100.0,
            rss_kib(),
            if s.gpu_frames == 0 {
                "cpu"
            } else if s.gpu_frames == s.shown {
                "gpu"
            } else {
                "mixed"
            },
            // "mixed": the video block stopped in the middle and the CPU went on.
            if c.on_venus == 0 {
                "software"
            } else if c.on_venus == c.decoded {
                "venus"
            } else {
                "mixed"
            },
            s.sound_frames,
        );
    }
}

fn format_time(us: i64) -> String {
    let s = us.max(0) / 1_000_000;
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

/// The engine's answer about where to start, from the file's id.
fn resume_ms(engine: &mut secretary_player::Engine, id: &str, duration_ms: u64) -> u64 {
    let cmd = json!({"cmd": "resume", "id": id, "duration_ms": duration_ms}).to_string();
    engine.handle(&cmd)["position_ms"].as_u64().unwrap_or(0)
}

fn report_position(engine: &mut secretary_player::Engine, id: &str, position_us: i64, duration_ms: u64, playing: bool) -> Value {
    let cmd = json!({"cmd": "position", "id": id, "position_ms": position_us.max(0) / 1000, "duration_ms": duration_ms, "playing": playing}).to_string();
    engine.handle(&cmd)
}

/// Rows of the screen something was drawn on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Band {
    y: i32,
    h: i32,
}

/// What was on the screen last frame and is not this one. A picture repaints only its own rectangle, so anything over the black around it
/// stays until it is cleared.
fn gone(last: &[Band], now: &[Band]) -> Vec<Band> {
    last.iter().filter(|b| !now.contains(b)).copied().collect()
}

/// The bar and the subtitles, drawn into frames of their own and copied over the video.
struct Overlay {
    text: Text,
    bar: Frame,
    k: f32,
    w: i32,
    h: i32,
}

impl Overlay {
    fn new(w: u32, h: u32) -> Overlay {
        let k = (w as f32 / 1024.0).min(h as f32 / 768.0).max(1.0);
        let bar_h = (BAR_H as f32 * k) as u32;
        Overlay { text: Text::new(), bar: Frame::new(w, bar_h), k, w: w as i32, h: h as i32 }
    }

    fn bar_height(&self) -> i32 {
        self.bar.h as i32
    }

    fn draw_bar(&mut self, fb: &mut Fb, s: &Session, paused: bool, quality: Quality) {
        let (w, h) = (self.w, self.bar_height());
        let px = (15.0 * self.k) as u16;
        self.bar.fill_rect(Rect::new(0, 0, w, h), Rgb::hex(0x10141c));
        let pos = s.position_us;
        let dur = s.track.duration_us;
        let left = format!(
            "{}  {}  {} / {}  {} {}x{}{}{}",
            if paused { "Paused" } else { "Playing" },
            s.title,
            format_time(pos),
            dur.map_or("?".to_owned(), format_time),
            s.codec.name(),
            s.track.width,
            s.track.height,
            if quality == Quality::Smooth { "  smooth" } else { "  fast" },
            if s.stats.dropped > 0 { format!("  {} dropped", s.stats.dropped) } else { String::new() },
        );
        let y = (h - self.text.ascent(Face::Sans, px)) / 2 + self.text.ascent(Face::Sans, px) - 4;
        let clip = self.bar.bounds();
        let line = self.text.fit(Face::Sans, px, &left, w - 24);
        self.text.draw_clipped(&mut self.bar, clip, 12, y, Face::Sans, px, Rgb::hex(0xe6e8ee), &line);
        if let Some(d) = dur.filter(|d| *d > 0) {
            let filled = ((pos.clamp(0, d) as f64 / d as f64) * w as f64) as i32;
            self.bar.fill_rect(Rect::new(0, 0, filled, 3), Rgb::hex(0x5b8cff));
        }
        let at = self.h - h;
        for (i, (_, row)) in self.bar.rows(Rect::new(0, 0, w, h)).enumerate() {
            fb.present_image(0, at + i as i32, w as usize, row);
        }
    }

    /// The rows a cue of this many lines covers.
    fn cue_band(&self, lines: usize) -> Band {
        let px = (21.0 * self.k) as u16;
        let h = (px as f32 * 1.3) as i32 * lines as i32 + 12;
        Band { y: self.h - h - (40.0 * self.k) as i32, h }
    }

    /// The cue's lines in white on a dark strip above the bottom of the screen. Only when a cue is on: nothing is paid otherwise.
    fn draw_cue(&mut self, fb: &mut Fb, lines: &[&str]) {
        let px = (21.0 * self.k) as u16;
        let line_h = (px as f32 * 1.3) as i32;
        let Band { y: at, h } = self.cue_band(lines.len());
        let mut f = Frame::new(self.w as u32, h as u32);
        f.fill_rect(Rect::new(0, 0, self.w, h), Rgb::hex(0x000000));
        let clip = f.bounds();
        for (i, l) in lines.iter().enumerate() {
            let tw = self.text.measure(Face::Sans, px, l);
            let y = 6 + line_h * i as i32 + self.text.ascent(Face::Sans, px);
            self.text.draw_clipped(&mut f, clip, (self.w - tw).max(0) / 2, y, Face::Sans, px, Rgb::hex(0xffffff), l);
        }
        for (i, (_, row)) in f.rows(Rect::new(0, 0, self.w, h)).enumerate() {
            fb.present_image(0, at + i as i32, self.w as usize, row);
        }
    }
}

fn clear_screen(fb: &mut Fb) {
    let row = vec![0xff00_0000u32; fb.width as usize];
    for y in 0..fb.height as i32 {
        fb.present_image(0, y, row.len(), &row);
    }
}

/// A message in the middle of a black screen, for what cannot be played.
fn show_message(fb: &mut Fb, overlay: &mut Overlay, title: &str, message: &str) {
    clear_screen(fb);
    let (w, h) = (fb.width, (fb.height / 4).max(1));
    let mut f = Frame::new(w, h);
    let px = (16.0 * overlay.k) as u16;
    let clip = f.bounds();
    let y0 = overlay.text.ascent(Face::Sans, px) + 8;
    let t = overlay.text.fit(Face::Sans, px, title, w as i32 - 40);
    overlay.text.draw_clipped(&mut f, clip, 20, y0, Face::Sans, px, Rgb::hex(0xffffff), &t);
    let m = overlay.text.fit(Face::Sans, px, message, w as i32 - 40);
    overlay.text.draw_clipped(&mut f, clip, 20, y0 + (px as i32) * 2, Face::Sans, px, Rgb::hex(0xd0d4dc), &m);
    for (i, (_, row)) in f.rows(Rect::new(0, 0, w as i32, h as i32)).enumerate() {
        fb.present_image(0, fb.height as i32 * 3 / 8 + i as i32, w as usize, row);
    }
}

enum Next {
    Quit,
    Move(i32),
}

/// The screen is held on only while a session is open, not paused and not finished.
fn holds_screen(open: bool, paused: bool, finished: bool) -> bool {
    open && !paused && !finished
}

struct Player {
    /// Written by the decoding thread when pictures arrive for a player that was waiting for them.
    wake_r: libc::c_int,
    fb: Fb,
    inputs: Inputs,
    /// The kernel's input.added and input.removed through the hub; `None` without a pipe.
    watch: Option<input_watch::InputWatch>,
    kb: Keyboard,
    overlay: Overlay,
    clock: Clock<Sound>,
    quality: Quality,
    /// Conversion and scaling on the Adreno; `None` once it fails, and the CPU does them.
    gpu: Option<present::GpuPresent>,
    bar_until: Option<Instant>,
    /// The bar's and the cue's rows, as they were last drawn.
    bands: Vec<Band>,
    engine: secretary_player::Engine,
    last_report: Instant,
    /// The shell's screen-off hold: on while a session plays, off while paused or done.
    hold: DisplayHold,
    /// The file playing in its playlist (the videos of the list's folder, in the list's order) and how many there are. A
    /// playlist of one has nothing to step to, so the arrows seek in it.
    pos: usize,
    count: usize,
}

/// What a key does to the playlist.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Act {
    /// To the file this many places on (negative: back).
    Go(i32),
    /// Seek this many seconds.
    Seek(i64),
    /// A key for the playlist, but there is nowhere to go: the first file's Left, the last file's Right. No wrap.
    Stay,
    /// Not a playlist key.
    Pass,
}

/// Left and Right: the previous and next file, Home and End the first and last (Windows Photos, Quick Look). A playlist of
/// one file has none, so the arrows seek 10 s as in any player; Shift+arrow seeks in a playlist too. The ends stop and do not
/// wrap: the pure arithmetic is `lucina-nav`, proved by Verus.
fn playlist_key(p: Press, pos: usize, count: usize) -> Act {
    if p.mods.ctrl || p.mods.alt || p.mods.meta {
        return Act::Pass;
    }
    let step = match p.code {
        keys::KEY_LEFT if p.mods.shift => return Act::Seek(-10),
        keys::KEY_RIGHT if p.mods.shift => return Act::Seek(10),
        keys::KEY_LEFT if count <= 1 => return Act::Seek(-10),
        keys::KEY_RIGHT if count <= 1 => return Act::Seek(10),
        keys::KEY_LEFT => nav::Step::Prev,
        keys::KEY_RIGHT => nav::Step::Next,
        keys::KEY_HOME if count > 1 && !p.mods.shift => nav::Step::First,
        keys::KEY_END if count > 1 && !p.mods.shift => nav::Step::Last,
        _ => return Act::Pass,
    };
    if pos >= count {
        return Act::Stay;
    }
    let ok = vec![true; count];
    let to = nav::walk(&ok, pos, step);
    if to == pos { Act::Stay } else { Act::Go(to as i32 - pos as i32) }
}

impl Player {
    /// Keeps the shell's screen-off hold equal to "playing": a session is open, not paused, not finished.
    fn update_hold(&mut self, session: Option<&Session>) {
        let playing = session.is_some_and(|s| holds_screen(true, self.clock.is_paused(), s.finished));
        self.hold.set(playing, "playing video");
    }

    /// Keys since the last look. `Some` to leave the file.
    fn poll_keys(&mut self, s: &mut Session, wait: Duration) -> Result<Option<Next>, String> {
        let now = Instant::now();
        input_watch::rescan(self.watch.as_mut(), &mut self.inputs, now);
        let mut fds: Vec<libc::pollfd> = self.inputs.fds().into_iter().chain(self.watch.as_ref().map(|w| w.fd())).chain([self.wake_r]).map(|fd| libc::pollfd { fd, events: libc::POLLIN, revents: 0 }).collect();
        if !wait.is_zero() {
            let ms = (wait.as_micros().div_ceil(1000)).clamp(1, 100) as i32;
            // SAFETY: `fds` is a valid array for the call. An interrupted poll (SIGTERM) goes round the loop.
            unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, ms) };
            if fds.last().is_some_and(|f| f.revents & libc::POLLIN != 0) {
                let mut sink = [0u8; 64];
                // SAFETY: reads into a live local buffer; the pipe is non-blocking.
                while unsafe { libc::read(self.wake_r, sink.as_mut_ptr().cast(), sink.len()) } > 0 {}
            }
        }
        let mut evs = Vec::new();
        if self.inputs.read(&mut evs) {
            self.kb.release_all();
        }
        let now = Instant::now();
        let mut presses: Vec<Press> = Vec::new();
        for (_, ev) in evs.drain(..) {
            match ev.type_ {
                EV_SYN if ev.code == input::SYN_DROPPED => self.kb.release_all(),
                EV_KEY => {
                    if let Some(p) = self.kb.feed(ev.code, ev.value, now) {
                        presses.push(p);
                    }
                }
                _ => {}
            }
        }
        while let Some(p) = self.kb.repeat(now) {
            presses.push(p);
        }
        for p in presses {
            if let Some(n) = self.key(s, p)? {
                return Ok(Some(n));
            }
        }
        Ok(None)
    }

    fn key(&mut self, s: &mut Session, p: Press) -> Result<Option<Next>, String> {
        let now = Instant::now();
        let seek = |me: &mut Player, s: &mut Session, by_s: i64| -> Result<(), String> {
            let target = s.position_us + by_s * 1_000_000;
            let at = s.seek(target, by_s > 0)?;
            me.clock.stop(at);
            me.show_bar_a_while();
            Ok(())
        };
        match playlist_key(p, self.pos, self.count) {
            Act::Go(by) => return Ok(Some(Next::Move(by))),
            Act::Seek(by) => {
                seek(self, s, by)?;
                return Ok(None);
            }
            Act::Stay => return Ok(None),
            Act::Pass => {}
        }
        match p.code {
            keys::KEY_ESC => return Ok(Some(Next::Quit)),
            keys::KEY_F1 if std::env::var_os("LUCINA_SHELL").is_none() => return Ok(Some(Next::Quit)),
            KEY_SPACE => {
                let paused = !self.clock.is_paused();
                self.clock.set_paused(paused, now);
                self.update_hold(Some(s));
                report_position(&mut self.engine, &s.id, s.position_us, s.duration_ms(), !paused);
                self.show_bar_a_while();
                self.overlay.draw_bar(&mut self.fb, s, paused, self.quality);
            }
            keys::KEY_DOWN => seek(self, s, -60)?,
            keys::KEY_UP => seek(self, s, 60)?,
            KEY_N => return Ok(Some(Next::Move(1))),
            KEY_P => return Ok(Some(Next::Move(-1))),
            KEY_S | KEY_V => {
                self.quality = if self.quality == Quality::Fast { Quality::Smooth } else { Quality::Fast };
                self.show_bar_a_while();
            }
            KEY_I => self.show_bar_a_while(),
            _ => {}
        }
        Ok(None)
    }

    fn show_bar_a_while(&mut self) {
        self.bar_until = Some(Instant::now() + Duration::from_secs(3));
    }

    /// Play one file to its end, or until a key says otherwise.
    fn play(&mut self, s: &mut Session) -> Result<Next, String> {
        clear_screen(&mut self.fb);
        let duration_ms = s.duration_ms();
        let resume = s.resume_ms.unwrap_or_else(|| resume_ms(&mut self.engine, &s.id, duration_ms));
        if resume > 0 {
            let at = s.seek(resume as i64 * 1000, false)?;
            eprintln!("lucina-app-video: resuming {} at {}", s.title, format_time(at));
            self.show_bar_a_while();
        }
        let mut scaler: Option<Scaler> = None;
        let mut frame_us = DEFAULT_FRAME_US;
        let mut last_pts = None;
        let mut last_shown = Instant::now();
        loop {
            if TERMINATE.load(Ordering::SeqCst) {
                return Ok(Next::Quit);
            }
            let now = Instant::now();
            let clock_now = self.clock.is_started().then(|| self.clock.now_us(now)).flatten();
            let (queued, first_pts, eof) = {
                let mut w = s.worker.lock();
                if let Some(e) = w.error.take() {
                    return Err(e);
                }
                let behind = pace::behind_us(w.queue.first_pts(), clock_now, last_pts, frame_us);
                let skip = if self.clock.is_paused() { false } else if behind > SKIP_AFTER_US { true } else if behind < SKIP_UNTIL_US { false } else { w.skip_nonref };
                w.skip_nonref = skip;
                let state = (w.queue.len(), w.queue.first_pts(), w.eof);
                w.starving = state.0 < s.reorder + 1;
                state
            };
            let state = State {
                queued,
                first_pts,
                now: clock_now,
                eof,
                reorder: s.reorder,
                frame_us,
                paused: self.clock.is_paused(),
                since_shown_us: last_shown.elapsed().as_micros() as i64,
            };
            match pace::next(&state) {
                Step::Starving => {
                    if let Some(n) = self.poll_keys(s, Duration::from_millis(20))? {
                        return Ok(n);
                    }
                }
                Step::SkipToKey => {
                    // Everything before the next key frame is out of date.
                    s.stats.jumps += 1;
                    self.clock.stop(clock_now.unwrap_or(0));
                    s.worker.command(Command::ToKey);
                }
                Step::Drop => {
                    s.worker.lock().queue.pop();
                    s.worker.wake();
                    s.stats.dropped += 1;
                }
                Step::Show => {
                    let Some(pic) = s.worker.lock().queue.pop() else { continue };
                    s.worker.wake();
                    let pts = pic.pts_us();
                    if clock_now.is_some() && self.clock.follows_sound() {
                        s.stats.sound_frames += 1;
                    }
                    self.show(s, &pic, &mut scaler, clock_now.map_or(0, |n| n - pts))?;
                    let t = Instant::now();
                    last_shown = t;
                    s.position_us = pts;
                    // A frame's time from the file: only used to tell when one is too late to show.
                    if let Some(d) = last_pts.map(|l: i64| pts - l).filter(|d| (1_000..1_000_000).contains(d)) {
                        frame_us = d;
                    }
                    last_pts = Some(pts);
                    if !self.clock.is_started() {
                        self.clock.start_at(pts, t);
                    }
                    if !s.first_shown {
                        s.first_shown = true;
                        eprintln!("lucina-app-video: first frame {} to_frame_ms={} rss_kib={}", s.title, s.started.elapsed().as_millis(), rss_kib());
                    }
                    if self.last_report.elapsed() >= Duration::from_secs(1) {
                        self.last_report = t;
                        let tick = report_position(&mut self.engine, &s.id, pts, duration_ms, true);
                        if tick["sync"] == true
                            && let Some(c) = &s.client
                        {
                            remote::push_progress(c.clone(), s.id.clone(), tick["entry"].clone());
                        }
                    }
                    if let Some(n) = self.poll_keys(s, Duration::ZERO)? {
                        return Ok(n);
                    }
                }
                Step::Wait(d) => {
                    if let Some(n) = self.poll_keys(s, d)? {
                        return Ok(n);
                    }
                }
                Step::Finished => {
                    s.finished = true;
                    self.update_hold(Some(s));
                    lucina_app::events::emit("media.ended")
                        .subject(&media_subject(&s.id))
                        .field("title", s.title.as_str())
                        .field("duration_s", duration_ms / 1000)
                        .send();
                    report_position(&mut self.engine, &s.id, s.position_us, duration_ms, false);
                    return Ok(Next::Move(1));
                }
            }
        }
    }

    fn show(&mut self, s: &mut Session, pic: &Picture, scaler: &mut Option<Scaler>, late_us: i64) -> Result<(), String> {
        if let Picture::Nv12(n) = pic {
            if self.gpu.is_some() {
                match self.show_by_gpu(s, GpuSource::Nv12(n), n.pts_us, Instant::now()) {
                    Ok(()) => {
                        s.stats.gpu_frames += 1;
                        let late = late_us.max(0) as f64 / 1000.0;
                        s.stats.late_ms_sum += late;
                        s.stats.worst_late_ms = s.stats.worst_late_ms.max(late);
                        return Ok(());
                    }
                    Err(e) => {
                        eprintln!("lucina-app-video: the GPU stopped showing pictures, the CPU takes over: {e}");
                        self.gpu = None;
                        self.quality = Quality::Fast;
                        clear_screen(&mut self.fb);
                        self.bands.clear();
                    }
                }
            }
            // The CPU path reads planes: the chroma is split here, only when the CPU is the one showing it.
            return self.show(s, &n.to_i420(), scaler, late_us);
        }
        let Some(p) = pic.planes() else {
            return Err("video above 8 bits is not playable on this laptop without the server converting it".to_owned());
        };
        let t0 = Instant::now();
        if self.gpu.is_some() {
            match self.show_by_gpu(s, GpuSource::Planes(&p), pic.pts_us(), t0) {
                Ok(()) => {
                    s.stats.gpu_frames += 1;
                    let late = late_us.max(0) as f64 / 1000.0;
                    s.stats.late_ms_sum += late;
                    s.stats.worst_late_ms = s.stats.worst_late_ms.max(late);
                    return Ok(());
                }
                Err(e) => {
                    eprintln!("lucina-app-video: the GPU stopped showing pictures, the CPU takes over: {e}");
                    self.gpu = None;
                    // Bilinear was the GPU's default; on the CPU it costs most of a frame.
                    self.quality = Quality::Fast;
                    clear_screen(&mut self.fb);
                    self.bands.clear();
                }
            }
        }
        if scaler.as_ref().is_none_or(|sc| sc.quality != self.quality || sc.source() != (p.w, p.h)) {
            *scaler = Some(Scaler::new((p.w, p.h), (self.fb.width as usize, self.fb.height as usize), self.quality));
        }
        let sc = scaler.as_mut().expect("just made");
        sc.render(&p);
        let t1 = Instant::now();
        let (dw, (x0, y0)) = (sc.dst.0, sc.at);
        // The bar's rows are the bar's while it shows.
        let bar_on = self.bar_until.is_some_and(|t| t > t1);
        let pts_ms = (pic.pts_us() / 1000).max(0) as u64;
        let lines: Vec<String> = subtitles::active(&s.cues, pts_ms).iter().flat_map(|c| c.text.lines().map(str::to_owned).collect::<Vec<_>>()).collect();
        let mut bands = Vec::with_capacity(2);
        if bar_on {
            let h = self.overlay.bar_height();
            bands.push(Band { y: self.fb.height as i32 - h, h });
        }
        if !lines.is_empty() {
            bands.push(self.overlay.cue_band(lines.len()));
        }
        // Before the picture: it then repaints what it covers, and only the black around it stays cleared.
        let black = vec![0xff00_0000u32; self.fb.width as usize];
        for b in gone(&self.bands, &bands) {
            for y in b.y..b.y + b.h {
                self.fb.present_image(0, y, black.len(), &black);
            }
        }
        self.bands = bands;
        let limit = if bar_on { (self.fb.height as i32 - self.overlay.bar_height()) as usize } else { usize::MAX };
        for (i, row) in sc.out.chunks_exact(dw).enumerate() {
            let y = y0 + i;
            if y >= limit {
                break;
            }
            self.fb.present_image(x0 as i32, y as i32, dw, row);
        }
        if !lines.is_empty() {
            let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
            self.overlay.draw_cue(&mut self.fb, &refs);
        }
        if bar_on {
            s.position_us = pic.pts_us();
            let paused = self.clock.is_paused();
            self.overlay.draw_bar(&mut self.fb, s, paused, self.quality);
        } else {
            self.bar_until = None;
        }
        let t2 = Instant::now();
        s.stats.convert_ms += (t1 - t0).as_secs_f64() * 1000.0;
        s.stats.present_ms += (t2 - t1).as_secs_f64() * 1000.0;
        s.stats.shown += 1;
        s.report_present(self.fb.width, self.fb.height);
        let late = late_us.max(0) as f64 / 1000.0;
        s.stats.late_ms_sum += late;
        s.stats.worst_late_ms = s.stats.worst_late_ms.max(late);
        Ok(())
    }
}

/// What [`Player::show_by_gpu`] uploads: a decoder's planes, or the video block's NV12 as it is.
enum GpuSource<'a> {
    Planes(&'a lucina_video::Planes<'a>),
    Nv12(&'a decode::Nv12Picture),
}

impl Player {
    /// One picture through [`present::GpuPresent`]: the same placing, bar and cues as the CPU path, with the picture drawn by the GPU
    /// before the CPU draws over it.
    fn show_by_gpu(&mut self, s: &mut Session, src: GpuSource, pts_us: i64, t0: Instant) -> std::io::Result<()> {
        let gpu = self.gpu.as_mut().expect("checked by the caller");
        let (layout, (w, h), crop) = match src {
            GpuSource::Planes(p) => (gpu.upload(p)?, (p.w, p.h), (0, 0)),
            GpuSource::Nv12(n) => (gpu.upload_nv12(n)?, (n.w, n.h), n.crop),
        };
        let t1 = Instant::now();
        let screen = (self.fb.width as usize, self.fb.height as usize);
        let ((dw, dh), (x0, y0)) = lucina_video::convert::fit((w, h), screen);
        let to = Rect::new(x0 as i32, y0 as i32, dw as i32, dh as i32);
        let bar_on = self.bar_until.is_some_and(|t| t > t1);
        let pts_ms = (pts_us / 1000).max(0) as u64;
        let lines: Vec<String> = subtitles::active(&s.cues, pts_ms).iter().flat_map(|c| c.text.lines().map(str::to_owned).collect::<Vec<_>>()).collect();
        let mut bands = Vec::with_capacity(2);
        if bar_on {
            let h = self.overlay.bar_height();
            bands.push(Band { y: self.fb.height as i32 - h, h });
        }
        if !lines.is_empty() {
            bands.push(self.overlay.cue_band(lines.len()));
        }
        let black = vec![0xff00_0000u32; self.fb.width as usize];
        for b in gone(&self.bands, &bands) {
            for y in b.y..b.y + b.h {
                self.fb.present_image(0, y, black.len(), &black);
            }
        }
        self.bands = bands;
        let limit = if bar_on { self.fb.height as i32 - self.overlay.bar_height() } else { i32::MAX };
        if let Some((from, to)) = present::above(w as u32, h as u32, to, limit) {
            // A crop that does not start at the corner moves the source rectangle, as juno-video-gpu's yuv command takes it.
            let from = Rect { x: from.x + crop.0 as i32, y: from.y + crop.1 as i32, ..from };
            let _ = layout;
            gpu.draw(from, to, self.quality == Quality::Smooth)?;
        }
        if !lines.is_empty() {
            let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
            self.overlay.draw_cue(&mut self.fb, &refs);
        }
        if bar_on {
            s.position_us = pts_us;
            let paused = self.clock.is_paused();
            self.overlay.draw_bar(&mut self.fb, s, paused, self.quality);
        } else {
            self.bar_until = None;
        }
        let t2 = Instant::now();
        s.stats.convert_ms += (t1 - t0).as_secs_f64() * 1000.0;
        s.stats.present_ms += (t2 - t1).as_secs_f64() * 1000.0;
        s.stats.shown += 1;
        s.report_present(self.fb.width, self.fb.height);
        Ok(())
    }
}

/// `file`, or the video files of a folder.
fn expand(args: &[String]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for a in args {
        let p = PathBuf::from(a);
        if p.is_dir() {
            let mut files: Vec<PathBuf> = std::fs::read_dir(&p)
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.path())
                .filter(|f| f.file_name().is_some_and(|n| list::is_video_name(&n.to_string_lossy())))
                .collect();
            files.sort();
            out.extend(files);
        } else {
            out.push(p);
        }
    }
    out
}

/// `--decode-only <file> [seconds]`: decodes as fast as the decoder goes, nothing on the screen, and prints the pictures, the time each
/// took from its packet in, a checksum of the first picture's planes and the process's CPU time, for comparing the decoders on one
/// clip (`LUCINA_VIDEO_VENUS=0` for the CPU's).
fn decode_only(args: &[String]) -> i32 {
    let Some(path) = args.first() else {
        eprintln!("usage: lucina-app-video --decode-only <file> [seconds]");
        return 2;
    };
    let limit = std::time::Duration::from_secs(args.get(1).and_then(|s| s.parse().ok()).unwrap_or(30));
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("decode-only: {path}: {e}");
            return 1;
        }
    };
    let mut demux = match lucina_video::demux::open(std::io::BufReader::new(file)) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("decode-only: {path}: {e}");
            return 1;
        }
    };
    let t = demux.track().clone();
    let mut dec = match decode::Decoder::for_track(t.codec, t.width, t.height) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("decode-only: {e}");
            return 1;
        }
    };
    let way = if matches!(dec, decode::Decoder::Venus(_)) { "venus" } else { "cpu" };
    let cpu0 = cpu_seconds();
    let start = std::time::Instant::now();
    let (mut packets, mut pictures, mut worst_ms) = (0u64, 0u64, 0f64);
    let mut first = None;
    let mut out = Vec::new();
    // LUCINA_VIDEO_SEEK_AFTER=<packets>: back to the start after that many, in the same session for the video block; the first picture
    // after it must be the first picture again.
    let seek_after: Option<u64> = std::env::var("LUCINA_VIDEO_SEEK_AFTER").ok().and_then(|v| v.parse().ok());
    let mut after_seek: Option<Option<u64>> = None;
    let mut sizes: Vec<(usize, usize)> = Vec::new();
    while start.elapsed() < limit {
        if seek_after == Some(packets) && after_seek.is_none() {
            out.clear();
            if let Err(e) = demux.seek_to(0) {
                eprintln!("decode-only: seek: {e}");
                return 1;
            }
            let kept = match &mut dec {
                decode::Decoder::Venus(v) => v.seek().map_err(|e| e.to_string()),
                _ => Err(String::from("not the video block")),
            };
            if let Err(e) = kept {
                eprintln!("decode-only: seek in the session: {e}; a new decoder");
                dec = match decode::Decoder::for_track(t.codec, t.width, t.height) {
                    Ok(d) => d,
                    Err(e) => {
                        eprintln!("decode-only: {e}");
                        return 1;
                    }
                };
            }
            after_seek = Some(None);
        }
        let p = match demux.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => break,
            Err(e) => {
                eprintln!("decode-only: {e}");
                return 1;
            }
        };
        let t0 = std::time::Instant::now();
        if let Err(e) = dec.decode(&p, &mut out) {
            eprintln!("decode-only: packet {packets}: {e}");
            return 1;
        }
        worst_ms = worst_ms.max(t0.elapsed().as_secs_f64() * 1000.0);
        packets += 1;
        for pic in out.drain(..) {
            if let decode::Picture::Nv12(n) = &pic {
                if sizes.last() != Some(&(n.w, n.h)) {
                    sizes.push((n.w, n.h));
                }
                if after_seek == Some(None) {
                    after_seek = Some(Some(n.luma_rows().fold(0u64, |a, row| row.iter().fold(a, |a, b| a.wrapping_mul(31).wrapping_add(u64::from(*b))))));
                }
            }
            if first.is_none()
                && let decode::Picture::Nv12(n) = &pic
            {
                let sum = n.luma_rows().fold(0u64, |a, row| row.iter().fold(a, |a, b| a.wrapping_mul(31).wrapping_add(u64::from(*b))));
                first = Some((n.w, n.h, sum));
            }
            if first.is_none()
                && let Some(pl) = pic.planes()
            {
                let mut sum = 0u64;
                for row in 0..pl.h {
                    sum = pl.y[row * pl.y_stride..row * pl.y_stride + pl.w].iter().fold(sum, |a, b| a.wrapping_mul(31).wrapping_add(u64::from(*b)));
                }
                first = Some((pl.w, pl.h, sum));
            }
            pictures += 1;
        }
    }
    pictures += dec.flush().len() as u64;
    let wall = start.elapsed().as_secs_f64();
    let cpu = cpu_seconds() - cpu0;
    println!(
        "decode-only: {way} {} {}x{} packets={packets} pictures={pictures} wall_s={wall:.2} fps={:.1} worst_packet_ms={worst_ms:.1} cpu_s={cpu:.2} cpu_pct_of_one={:.0} first={:?} after_seek={:?} sizes={:?}",
        t.codec.name(),
        t.width,
        t.height,
        pictures as f64 / wall,
        cpu / wall * 100.0,
        first,
        after_seek.flatten(),
        sizes
    );
    0
}

/// The App of the file on the screen, for the intent this process was started with: it knows which file is playing. The player
/// owns the screen outright, so this answers the call once, when the file is opened.
struct Playing {
    path: PathBuf,
    position_ms: u64,
}

impl App for Playing {
    type Msg = ();

    fn view(&mut self, ui: &mut Ui) {
        ui.header("Video");
    }

    fn intents() -> &'static [IntentSpec] {
        list::INTENTS
    }

    fn intent(&mut self, call: Call, reply: Responder) {
        match call.name.as_str() {
            "video.play" => {
                let path = call.args["path"].as_str().unwrap_or("");
                if Path::new(path) == self.path.as_path() {
                    let name = self
                        .path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    reply.send(
                        Reply::done(format!("Playing {name}")).with_entity(Entity::new(
                            "file",
                            &self.path.display().to_string(),
                            &name,
                        )),
                    );
                } else {
                    reply.send(Reply::failed(format!("{path} is not the file playing")));
                }
            }
            "video.now" => {
                reply.send(
                    Reply::done("Playing")
                        .with_value(json!({"playing": true, "path": self.path.display().to_string(), "position_ms": self.position_ms})),
                );
            }
            _ => reply.send(Reply::failed(format!("no intent {} here", call.name))),
        }
    }
}

/// Answer the `video.play` call this process was started with, now that `session`'s file is the one playing: to the reply file
/// `LUCINA_INTENT_REPLY` names, the way the toolkit writes it.
fn answer_start_intent(call: &Call, session: &Session) {
    let (responder, slot) = Responder::slot(&call.name);
    let mut playing = Playing {
        path: session.path.clone(),
        position_ms: (session.position_us.max(0) / 1000) as u64,
    };
    lucina_app::intent::dispatch(&mut playing, call.clone(), responder);
    let Ok(mut slot) = slot.lock() else { return };
    let Some(reply) = slot.take() else { return };
    let Some(path) = std::env::var_os("LUCINA_INTENT_REPLY") else {
        return;
    };
    let Ok(bytes) = serde_json::to_vec(&reply) else {
        return;
    };
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt;
    let path = PathBuf::from(path);
    let tmp = path.with_extension("part");
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)
        .and_then(|mut f| f.write_all(&bytes));
    if written.is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

fn main() {
    // The shell's screen-off hold, taken before any thread is started.
    let hold = DisplayHold::from_env();
    // lucina-devd's listing (`<program> --intents`): answered before the arguments below, which would take it for a file to play.
    if std::env::args().nth(1).as_deref() == Some("--intents") {
        println!("{}", lucina_app::intent::listing::<list::List>());
        return;
    }
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    // Started for `video.play`: the call's path is the file argument, exactly as if given on the command line.
    let start_intent: Option<Call> = std::env::var("LUCINA_INTENT")
        .ok()
        .and_then(|text| serde_json::from_str::<Call>(&text).ok())
        .filter(|c| c.name == "video.play");
    if let Some(call) = &start_intent
        && let Some(path) = call.args["path"].as_str()
    {
        args.push(path.to_owned());
    }
    if args.is_empty() {
        let home = if Path::new("/run/lucina/data-ready").exists() { "/data/home" } else { "/run/lucina/files" };
        let videos = Path::new(home).join("Videos");
        let start = if videos.is_dir() { videos } else { PathBuf::from(home) };
        lucina_app::run("lucina-app-video", move |_| list::List::new(start));
    }
    if args.first().is_some_and(|a| a == "--decode-only") {
        std::process::exit(decode_only(&args[1..]));
    }
    let item = args.first().is_some_and(|a| a == "--item").then(|| args.get(1).cloned()).flatten();
    // `--browse <file>`: started from the list. The playlist is the videos of that file's folder, in the list's order, starting at
    // the file, so that the arrows can step to its neighbours.
    let browse = args.first().is_some_and(|a| a == "--browse").then(|| args.get(1).map(PathBuf::from)).flatten();
    let (files, first) = match (&item, &browse) {
        (Some(_), _) => (Vec::new(), 0),
        (None, Some(file)) => list::playlist(file),
        (None, None) => (expand(&args), 0),
    };
    let under_shell = std::env::var_os("LUCINA_SHELL").is_some();
    let fb_path = std::env::var("LUCINA_FB").unwrap_or_else(|_| "/dev/fb0".into());
    let fb = match Fb::open(&fb_path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("lucina-app-video: no screen: {fb_path}: {e}");
            std::process::exit(1);
        }
    };
    let _graphics = if under_shell { None } else { GraphicsMode::enter().ok() };
    // SAFETY: the handler only sets an atomic.
    unsafe { libc::signal(libc::SIGTERM, on_sigterm as extern "C" fn(libc::c_int) as libc::sighandler_t) };
    let (w, h) = (fb.width, fb.height);
    let engine = secretary_player::Engine::open(state_path()).unwrap_or_else(|e| {
        eprintln!("lucina-app-video: positions will not be kept: {e}");
        secretary_player::Engine::in_memory()
    });
    let mut pipe = [-1; 2];
    // SAFETY: pipe2 fills the two-element array.
    if unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_NONBLOCK | libc::O_CLOEXEC) } < 0 {
        eprintln!("lucina-app-video: no pipe: {}", std::io::Error::last_os_error());
        std::process::exit(1);
    }
    let [wake_r, wake_w] = pipe;
    let gpu = present::GpuPresent::open();
    eprintln!(
        "lucina-app-video: pictures converted and scaled by the {}",
        match &gpu {
            Some(g) if g.behind_scan() => "GPU, drawn behind the scan",
            Some(_) => "GPU",
            None => "CPU",
        }
    );
    let mut player = Player {
        wake_r,
        fb,
        inputs: Inputs::default(),
        watch: input_watch::InputWatch::start(),
        kb: Keyboard::default(),
        overlay: Overlay::new(w, h),
        clock: Clock::new(Sound::none()),
        // Bilinear costs the GPU nothing worth counting; on the CPU it takes most of a frame.
        quality: if gpu.is_some() { Quality::Smooth } else { Quality::Fast },
        gpu,
        bar_until: None,
        bands: Vec::new(),
        engine,
        last_report: Instant::now(),
        hold,
        pos: 0,
        count: 1,
    };
    if let Some(id) = item {
        show_message(&mut player.fb, &mut player.overlay, "Opening", "Asking the server for this video.");
        match open_item(&id, wake_w) {
            Ok(mut session) => {
                player.clock = Clock::new(session.sound());
                player.update_hold(Some(&session));
                let result = player.play(&mut session);
                player.update_hold(None);
                session.log_summary(if result.is_err() { "error" } else { "done" });
                let tick = report_position(&mut player.engine, &session.id, session.position_us, session.duration_ms(), false);
                if let Some(c) = &session.client
                    && let Some(e) = tick.get("entry")
                    && let Err(err) = c.send_json("PUT", &format!("/v1/media/progress/{id}"), e)
                {
                    eprintln!("lucina-app-video: the server did not take the position: {err}");
                }
                if let Err(e) = result {
                    eprintln!("lucina-app-video: {e}");
                    wait_and_say(&mut player, &session.title.clone(), &e);
                    std::process::exit(1);
                }
            }
            Err(e) => {
                eprintln!("lucina-app-video: {id}: {e}");
                wait_and_say(&mut player, "This video did not open", &e);
                std::process::exit(1);
            }
        }
        return;
    }
    let mut at = first as i64;
    if files.is_empty() {
        wait_and_say(&mut player, "No videos here", "Nothing to play in what was asked for. Press Escape.");
        return;
    }
    while (0..files.len() as i64).contains(&at) {
        let path = files[at as usize].clone();
        let mut session = match open_session(&path, wake_w) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("lucina-app-video: {}: {e}", path.display());
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                wait_and_say(&mut player, &name, &e);
                if files.len() == 1 {
                    return;
                }
                at += 1;
                continue;
            }
        };
        // The start position of a `video.play` call: the player starts there through the same resume seek.
        if let Some(call) = &start_intent
            && call.args["path"].as_str() == Some(path.to_str().unwrap_or(""))
            && let Some(at) = call.args["at"].as_i64()
        {
            session.resume_ms = Some((at.max(0) * 1000) as u64);
        }
        if let Some(call) = &start_intent
            && call.args["path"].as_str() == Some(path.to_str().unwrap_or(""))
        {
            answer_start_intent(call, &session);
        }
        player.clock = Clock::new(session.sound());
        player.update_hold(Some(&session));
        (player.pos, player.count) = (at as usize, files.len());
        let result = player.play(&mut session);
        player.update_hold(None);
        let why = match &result {
            Ok(Next::Quit) => "quit",
            Ok(Next::Move(_)) => "next",
            Err(_) => "error",
        };
        session.log_summary(why);
        report_position(&mut player.engine, &session.id, session.position_us, session.duration_ms(), false);
        match result {
            Ok(Next::Quit) => return,
            Ok(Next::Move(by)) => at += by as i64,
            Err(e) => {
                eprintln!("lucina-app-video: {}: {e}", path.display());
                wait_and_say(&mut player, &session.title.clone(), &e);
                at += 1;
            }
        }
    }
}

fn wait_and_say(p: &mut Player, title: &str, message: &str) {
    show_message(&mut p.fb, &mut p.overlay, title, message);
    let started = Instant::now();
    loop {
        if TERMINATE.load(Ordering::SeqCst) || started.elapsed() > Duration::from_secs(120) {
            return;
        }
        let now = Instant::now();
        input_watch::rescan(p.watch.as_mut(), &mut p.inputs, now);
        let mut fds: Vec<libc::pollfd> = p.inputs.fds().into_iter().chain(p.watch.as_ref().map(|w| w.fd())).map(|fd| libc::pollfd { fd, events: libc::POLLIN, revents: 0 }).collect();
        // The earliest of the 120 s limit and the next timed rescan; a device event or the limit always ends the wait.
        let limit = started + Duration::from_secs(120);
        let timeout = p
            .inputs
            .next_rescan(now)
            .map_or(limit, |t| t.min(limit))
            .saturating_duration_since(now)
            .as_millis()
            .min(i32::MAX as u128) as i32;
        // SAFETY: `fds` is a valid array for the call.
        unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) };
        let mut evs = Vec::new();
        p.inputs.read(&mut evs);
        for (_, ev) in evs.drain(..) {
            if ev.type_ == EV_KEY && ev.value == 1 && matches!(ev.code, keys::KEY_ESC | keys::KEY_ENTER | KEY_SPACE) {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The screen is held only while a session is open, not paused and not finished.
    #[test]
    fn the_screen_is_held_only_while_a_session_plays() {
        assert!(holds_screen(true, false, false));
        assert!(!holds_screen(true, true, false));
        assert!(!holds_screen(true, false, true));
        assert!(!holds_screen(true, true, true));
        assert!(!holds_screen(false, false, false));
        assert!(!holds_screen(false, true, false));
        assert!(!holds_screen(false, false, true));
        assert!(!holds_screen(false, true, true));
    }

    /// The bug QEMU's capture found: a subtitle over the black below a film stayed there for good.
    #[test]
    fn what_stopped_being_drawn_gets_cleared() {
        let (bar, cue, two) = (Band { y: 1000, h: 80 }, Band { y: 900, h: 50 }, Band { y: 870, h: 80 });
        assert_eq!(gone(&[cue], &[]), [cue]);
        assert_eq!(gone(&[bar, cue], &[bar]), [cue]);
        assert_eq!(gone(&[cue], &[cue]), []);
        // A cue of another size covers other rows.
        assert_eq!(gone(&[two], &[cue]), [two]);
        assert_eq!(gone(&[], &[bar]), []);
    }

    /// The App of the playing file answers the intent the process was started with.
    #[test]
    fn the_playing_app_answers_play_and_now_for_its_file() {
        use lucina_app::Harness;
        let path = PathBuf::from("/data/home/Videos/film.mp4");
        let mut h = Harness::new(
            Playing {
                path: path.clone(),
                position_ms: 0,
            },
            800,
            600,
        );
        let r = h
            .intent("video.play", json!({"path": "/data/home/Videos/film.mp4"}))
            .unwrap();
        assert!(r.ok && r.text.contains("Playing film.mp4"), "{r:?}");
        assert_eq!(
            r.entities,
            vec![Entity::new(
                "file",
                "/data/home/Videos/film.mp4",
                "film.mp4"
            )]
        );
        let r = h
            .intent("video.play", json!({"path": "/data/home/Videos/other.mp4"}))
            .unwrap();
        assert!(!r.ok && r.text.contains("not the file playing"), "{r:?}");
        let r = h.intent("video.now", json!({})).unwrap();
        assert!(
            r.ok && r.value["playing"] == json!(true)
                && r.value["path"] == json!("/data/home/Videos/film.mp4"),
            "{r:?}"
        );
    }
}
