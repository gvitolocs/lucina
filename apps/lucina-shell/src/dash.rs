//! Home's first page on Juno: the glass dashboard (`lucina_glass::home`), drawn from real data only.
//!
//! - **Pages.** Home has two: the dashboard and the apps (the tile grid of `ui.rs`, with the power buttons). PgDn, Tab or the
//!   "Open" action go to the apps, which slide in over the dashboard (screen-forward); Esc, Backspace or PgUp come back
//!   (screen-back). Each move is composed whole pixels at a time ([`lucina_glass::home::slide_compose`]), which keeps up with
//!   the panel.
//! - **Frames.** Only what changed or moves is drawn: [`Dash::draw`] asks the view for its damage, draws each rectangle under a
//!   clip into the canvas and copies it into the shell's frame, and [`Dash::next_frame`] says when the next frame is due, or
//!   never while nothing moves.
//! - **First paint.** The backdrop (the night sky) and its copy for the glass are made at start, in a few tens of milliseconds, with
//!   nothing read from the disk; the first frame copies the sky and draws the
//!   cards that need no network (the clock, the actions, the volume). The cards whose data comes from /data or the network
//!   come in as their sources answer ([`crate::home_feed`]), with their entrance.
//! - **Data.** Each source's last answer is kept, and [`Dash::compose`] puts them together with what the shell knows (the
//!   status bar, the installed apps, the volume) into one `HomeData`. A source with nothing leaves its cards out.

use std::sync::mpsc::Receiver;
use std::time::{Instant, SystemTime};

use lucina_app::gfx::{Frame, Rect};
use lucina_app::manifest::Manifest;
use lucina_glass::home::{
    self, Action, Countdown, Device, Home, HomeData, Key, Note, Outcome, Pointer, Stat, StatItem,
    StatusBar, Target,
};
use lucina_glass::icons::Icon;
use lucina_glass::{Canvas, R, Type};

use crate::home_feed::{self, Ask, Feed, Update};

/// The page of Home on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Dash,
    Apps,
}

/// A move between the pages.
struct Slide {
    start: f64,
    /// 1: to the apps (they come from the right); -1: back.
    dir: i32,
    back: Vec<u32>,
    front: Vec<u32>,
}

/// How long a page move takes, seconds (the concept's screen transition).
const SLIDE: f64 = 0.62;

/// The sources' last answers.
#[derive(Default)]
struct Parts {
    calendar: Option<(
        Option<home::Deadline>,
        Option<home::Week>,
        Option<SystemTime>,
    )>,
    decisions: Option<(u32, Vec<Note>)>,
    agents: Option<(u32, Option<home::Build>)>,
    tasks: Option<(u32, u32, Vec<(String, String)>, Vec<(String, String)>)>,
    events: Vec<Note>,
    media: Option<home::Media>,
    uploads: Option<home::Upload>,
    /// Notifications cleared on the screen, by id: they stay away until the source drops them.
    cleared: Vec<String>,
}

/// What the shell knows that the dashboard shows.
pub struct Ctx<'a> {
    pub clock: &'a str,
    pub online: bool,
    pub battery: Option<(u8, bool)>,
    pub camera: bool,
    pub apps: &'a [Manifest],
    pub volume: Option<(f32, bool)>,
    pub device_line: Option<String>,
}

pub struct Dash {
    pub home: Home,
    pub page: Page,
    cv: Canvas,
    ty: Type,
    /// The backdrop and its blurred copy, the canvas's size; `None` until there is one.
    bg: Option<(Canvas, Canvas)>,
    /// The backdrop being made on a thread, when there was no prebuilt one.
    bg_job: Option<Receiver<(Canvas, Canvas)>>,
    t0: Instant,
    slide: Option<Slide>,
    pub feed: Option<Feed>,
    parts: Parts,
    /// Pixels per dp, and where the 1280 x 720 dp screen starts on the frame (a panel of another shape is letterboxed).
    k: f32,
    off: (i32, i32),
    /// Frame times for the trace: (when, ms in all, pixels, ms of that spent copying into the frame).
    pub last_draw: Option<(f64, f64, usize, f64)>,
    /// How long the backdrop took to read and blur at the start, ms (`None`: none was there to read).
    pub backdrop_ms: Option<f64>,
    /// A second canvas and glyph cache for drawing the lower halves of large frames on another core (`LUCINA_HOME_THREADS=1`
    /// keeps one).
    helper: Option<(Canvas, Type)>,
    threads: usize,
    /// The first frame showed the plain ground: the sky is still to make.
    sky_pending: bool,
    /// The first frame's pixels went to the frame by a swap (once).
    handed: bool,
}

/// From this many pixels a frame is drawn on two cores: below it the second thread's start costs more than it saves.
const TWO_CORES_FROM: usize = 120_000;

/// A picture installed in place of the generated sky, and its blurred copy at half the size: two reads.
fn load_backdrop(w: u32, h: u32, k: f32) -> Option<(Canvas, Canvas)> {
    let (a, b) = home::backdrop_names(w, h);
    {
        let dir = "/usr/share/lucina/home";
        let dir = std::path::Path::new(dir);
        if let (Some(img), Some(blurred)) = (
            home::read_raw(&dir.join(&a), w, h, k),
            home::read_raw(&dir.join(&b), w / 2, h / 2, k / 2.0),
        ) {
            return Some((img, blurred));
        }
    }
    None
}

impl Dash {
    /// The dashboard for a `w` x `h` frame. Nothing here waits for a disk beyond the image or for the network.
    pub fn new(w: u32, h: u32) -> Dash {
        let k = (w as f32 / home::W).min(h as f32 / home::H);
        let off = (
            ((w as f32 - home::W * k) / 2.0).round() as i32,
            ((h as f32 - home::H * k) / 2.0).round() as i32,
        );
        let t0 = Instant::now();
        let started = Instant::now();
        let mut cv = Canvas::new(w, h, k);
        cv.origin = (off.0 as f32 / k, off.1 as f32 / k);
        // A picture installed in place of the sky (`/usr/share/lucina/home/sky-WxH.raw` and its `-blur` copy) is read; otherwise
        // the sky is made here, which at boot is much quicker than reading 10 MB from a cold disk (557 ms measured).
        // The first frame cannot wait for the sky (30-50 ms on the laptop): it shows the sky's plain ground, and `finish_backdrop`
        // makes the sky right after it. A picture installed in its place is read now.
        let installed = load_backdrop(w, h, k);
        let sky_pending = installed.is_none();
        let bg = Some(installed.unwrap_or_else(|| home::plain_backdrop(w, h, k)));
        let backdrop_ms = Some(started.elapsed().as_secs_f64() * 1000.0);
        let bg_job: Option<Receiver<(Canvas, Canvas)>> = None;
        Dash {
            home: Home::new(0.0),
            page: Page::Dash,
            cv,
            ty: Type::new(),
            bg,
            bg_job,
            t0,
            slide: None,
            feed: None,
            parts: Parts::default(),
            k,
            off,
            last_draw: None,
            backdrop_ms,
            helper: None,
            sky_pending,
            handed: false,
            threads: std::env::var("LUCINA_HOME_THREADS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(1)
                .clamp(1, 2),
        }
    }

    /// The view's clock: seconds since the dashboard was made.
    pub fn secs(&self, at: Instant) -> f64 {
        at.saturating_duration_since(self.t0).as_secs_f64()
    }

    /// The instant of view time `s`.
    pub fn instant(&self, s: f64) -> Instant {
        self.t0 + std::time::Duration::from_secs_f64(s.max(0.0))
    }

    /// Start the sources (after the first frame).
    pub fn start_feed(&mut self) {
        if self.feed.is_none() {
            self.feed = Feed::start();
        }
    }

    /// Take what the sources sent; true when something changed.
    pub fn take_feed(&mut self) -> bool {
        let Some(f) = self.feed.as_mut() else {
            return false;
        };
        let ups = f.drain();
        let any = !ups.is_empty();
        for u in ups {
            match u {
                Update::Calendar {
                    deadline,
                    week,
                    next_start,
                } => self.parts.calendar = Some((deadline, week, next_start)),
                Update::Decisions { pending, notes } => {
                    self.parts.decisions = Some((pending, notes))
                }
                Update::Agents { running, build } => self.parts.agents = Some((running, build)),
                Update::Tasks {
                    due_today,
                    overdue,
                    today,
                    overdue_tasks,
                } => self.parts.tasks = Some((due_today, overdue, today, overdue_tasks)),
                Update::Events { notes } => self.parts.events = notes,
                Update::Media(m) => self.parts.media = m,
                Update::Uploads(u) => self.parts.uploads = u,
                Update::Gone(s) => match s {
                    home_feed::Source::Calendar => self.parts.calendar = None,
                    home_feed::Source::Decisions => self.parts.decisions = None,
                    home_feed::Source::Agents => self.parts.agents = None,
                    home_feed::Source::Tasks => self.parts.tasks = None,
                },
            }
        }
        any
    }

    /// Make the sky after the first frame (it showed the plain ground) and draw everything again over it. Returns how long it
    /// took, in ms, when it did something.
    pub fn finish_backdrop(&mut self) -> Option<f64> {
        if !std::mem::take(&mut self.sky_pending) {
            return None;
        }
        let started = Instant::now();
        self.bg = Some(home::sky_backdrop(self.cv.w, self.cv.h, self.k));
        self.home.invalidate();
        Some(started.elapsed().as_secs_f64() * 1000.0)
    }

    /// The backdrop made on its thread, when it is ready: everything is drawn again over it.
    pub fn take_backdrop(&mut self) -> bool {
        let Some(rx) = self.bg_job.as_ref() else {
            return false;
        };
        match rx.try_recv() {
            Ok(b) => {
                self.bg = Some(b);
                self.bg_job = None;
                self.home.invalidate();
                true
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.bg_job = None;
                false
            }
            Err(_) => false,
        }
    }

    /// Put everything together and hand it to the view.
    pub fn compose(&mut self, now: Instant, c: &Ctx) {
        let t = self.secs(now);
        let has = |id: &str| c.apps.iter().any(|m| m.id == id);
        let act = |icon: Icon, label: &str, id: &str| {
            has(id).then(|| Action {
                icon,
                label: label.into(),
                target: Target::App(id.into()),
            })
        };
        let mut work = vec![Action {
            icon: Icon::Doc,
            label: "Open".into(),
            target: Target::Apps,
        }];
        work.extend(
            [
                act(Icon::Clock, "Plan", "calendar"),
                act(Icon::Pen, "Write", "notes"),
                act(Icon::Play, "Play", "player"),
            ]
            .into_iter()
            .flatten(),
        );
        let lucina: Vec<Action> = [
            act(Icon::Send, "Ask", "chat"),
            act(Icon::Bell, "Decide", "decisions"),
            act(Icon::Code, "Build", "agents"),
            act(Icon::Bolt, "Do", "tasks"),
        ]
        .into_iter()
        .flatten()
        .collect();
        let mut segments = vec![("Work".to_string(), work)];
        if !lucina.is_empty() {
            segments.push(("Lucina".to_string(), lucina));
        }

        let p = &self.parts;
        let (deadline, week, next_start) = p.calendar.clone().unwrap_or((None, None, None));
        let countdown = next_start
            .and_then(|s| s.duration_since(SystemTime::now()).ok())
            .filter(|d| d.as_secs() < 3600)
            .map(|d| Countdown {
                at: t + d.as_secs_f64(),
                window: 3600,
            });
        let mut stats_left = Vec::new();
        if let Some((due, overdue, today, late)) = &p.tasks {
            let lines = |v: &Vec<(String, String)>| {
                v.iter()
                    .map(|(id, t)| StatItem {
                        title: t.clone(),
                        target: Target::Task(id.clone()),
                    })
                    .collect::<Vec<_>>()
            };
            stats_left.push(Stat {
                icon: Icon::Doc,
                caption: "Due today".into(),
                value: due.to_string(),
                app: Some("tasks".into()),
                items: lines(today),
            });
            stats_left.push(Stat {
                icon: Icon::Bolt,
                caption: "Overdue".into(),
                value: overdue.to_string(),
                app: Some("tasks".into()),
                items: lines(late),
            });
        }
        let mut stats_right = Vec::new();
        if let Some((n, _)) = &p.decisions {
            stats_right.push(Stat {
                icon: Icon::Bell,
                caption: "Decisions".into(),
                value: n.to_string(),
                app: Some("decisions".into()),
                items: Vec::new(),
            });
        }
        if let Some((n, _)) = &p.agents {
            stats_right.push(Stat {
                icon: Icon::Code,
                caption: "Agents".into(),
                value: n.to_string(),
                app: Some("agents".into()),
                items: Vec::new(),
            });
        }
        // Notifications: the machine's own first (newest first), then Lucina's questions; a cleared one stays away.
        let notes = if p.decisions.is_some() || self.feed.is_some() {
            let mut v: Vec<Note> = p.events.clone();
            if let Some((_, d)) = &p.decisions {
                v.extend(d.iter().cloned());
            }
            v.retain(|n| !p.cleared.contains(&n.id));
            // The age from the event's own time, worded now: compose runs on the minute tick, so it stays true.
            let now_wall = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis() as i64);
            for n in &mut v {
                if let Some(at) = n.at {
                    n.line = home::age_line(now_wall - at, &n.line);
                }
            }
            Some(v)
        } else {
            None
        };
        let data = HomeData {
            status: Some(StatusBar {
                clock: c.clock.to_string(),
                online: c.online,
                battery: c.battery.map(|b| b.0),
                charging: c.battery.is_some_and(|b| b.1),
                camera: c.camera,
            }),
            build: p.agents.as_ref().and_then(|a| a.1.clone()),
            segments,
            stats_left,
            device: c.device_line.clone().map(|line| Device {
                line,
                busy: self.bg_job.is_some(),
            }),
            person: None,
            deadline,
            week,
            stats_right,
            notes,
            upload: p.uploads.clone(),
            countdown,
            media: p.media.clone(),
            volume: c.volume,
        };
        self.home.set_data(t, data);
    }

    /// A notification was cleared on the screen.
    pub fn cleared(&mut self, id: String) {
        self.parts.cleared.push(id);
        if let Some(f) = &self.feed {
            f.ask(Ask::Dismissed);
        }
    }

    /// When the next frame is due, if ever.
    pub fn next_frame(&self, now: Instant, period: f64) -> Option<Instant> {
        if self.slide.is_some() {
            return Some(now + std::time::Duration::from_secs_f64(period));
        }
        if self.page != Page::Dash {
            return None;
        }
        let t = self.secs(now);
        self.home.next_frame(t, period).map(|s| self.instant(s))
    }

    /// Draw what is damaged into `frame`; returns the frame rectangles to show.
    pub fn draw(&mut self, frame: &mut Frame, now: Instant) -> Vec<Rect> {
        let t = self.secs(now);
        let started = Instant::now();
        let rects = self.home.damage(t);
        if rects.is_empty() {
            return Vec::new();
        }
        let black;
        let (img, blurred) = match &self.bg {
            Some((a, b)) => (a, b),
            None => {
                black = Canvas::new(self.cv.w, self.cv.h, self.k);
                (&black, &black)
            }
        };
        let k = self.k;
        let px_of = |r: &R| ((r.w * k).ceil() as usize + 2) * ((r.h * k).ceil() as usize + 2);
        let total: usize = rects.iter().map(px_of).sum();
        let Dash {
            home,
            cv,
            ty,
            helper,
            threads,
            ..
        } = self;
        if *threads > 1 && total >= TWO_CORES_FROM {
            // Each rectangle's top half here, its bottom half on another core with a copy of the view (its changes are dropped:
            // this view stays the one that answers input), into its own canvas; then those rows are copied over.
            let (tops, bottoms): (Vec<R>, Vec<R>) = rects
                .iter()
                .map(|r| {
                    (
                        R::new(r.x, r.y, r.w, r.h / 2.0),
                        R::new(r.x, r.y + r.h / 2.0, r.w, r.h - r.h / 2.0),
                    )
                })
                .unzip();
            let (cv2, ty2) = helper.get_or_insert_with(|| {
                let mut c = Canvas::new(cv.w, cv.h, cv.k);
                c.origin = cv.origin;
                (c, Type::new())
            });
            let mut other = home.clone();
            let mut lower: Vec<(i32, i32, i32, i32)> = Vec::new();
            std::thread::scope(|sc| {
                let job = sc.spawn(|| {
                    let mut boxes = Vec::new();
                    for r in &bottoms {
                        cv2.push_clip(*r, 0.0);
                        boxes.push(cv2.clip_px());
                        other.draw(cv2, ty2, t, img, blurred);
                        cv2.pop_clip();
                    }
                    boxes
                });
                for r in &tops {
                    cv.push_clip(*r, 0.0);
                    home.draw(cv, ty, t, img, blurred);
                    cv.pop_clip();
                }
                lower = job.join().unwrap_or_default();
            });
            let w = cv.w as usize;
            for (x0, y0, x1, y1) in lower {
                for y in y0.max(0)..y1 {
                    let row = y as usize * w;
                    cv.px[row + x0 as usize..row + x1 as usize]
                        .copy_from_slice(&cv2.px[row + x0 as usize..row + x1 as usize]);
                }
            }
        } else {
            for r in &rects {
                cv.push_clip(*r, 0.0);
                home.draw(cv, ty, t, img, blurred);
                cv.pop_clip();
            }
        }
        let mut out = Vec::with_capacity(rects.len());
        let mut px = 0usize;
        let copy_started = Instant::now();
        // The first frame is the whole canvas: hand its pixels to the frame instead of copying them into memory never touched
        // yet (8 MB of first-touch faults, 24 ms on the laptop). The canvas gets the frame's buffer and is filled again after.
        let whole = rects.len() == 1
            && rects[0].x <= 0.0
            && rects[0].y <= 0.0
            && rects[0].w >= home::W - 0.5
            && rects[0].h >= home::H - 0.5;
        if !self.handed
            && whole
            && frame.w == self.cv.w
            && frame.h == self.cv.h
            && let lucina_app::gfx::Pixels::Heap(v) = &mut frame.px
            && v.len() == self.cv.px.len()
        {
            std::mem::swap(v, &mut self.cv.px);
            self.handed = true;
            // The canvas now holds the frame's old buffer: the next frame draws it whole again (with the sky, when it is made).
            self.home.invalidate();
            self.last_draw = Some((
                t,
                started.elapsed().as_secs_f64() * 1000.0,
                frame.px.len(),
                copy_started.elapsed().as_secs_f64() * 1000.0,
            ));
            return vec![Rect::new(0, 0, frame.w as i32, frame.h as i32)];
        }
        for r in rects {
            self.cv.push_clip(r, 0.0);
            let (x0, y0, x1, y1) = self.cv.clip_px();
            self.cv.pop_clip();
            let fr = Rect::new(x0, y0, x1 - x0, y1 - y0);
            copy_rect(&self.cv, frame, fr);
            px += (fr.w.max(0) * fr.h.max(0)) as usize;
            out.push(fr);
        }
        self.last_draw = Some((
            t,
            started.elapsed().as_secs_f64() * 1000.0,
            px,
            copy_started.elapsed().as_secs_f64() * 1000.0,
        ));
        out
    }

    /// The whole dashboard as it is now, for a page move. The canvas already holds it: only what is still due is drawn first.
    fn snapshot(&mut self, t: f64) -> Vec<u32> {
        let black;
        let (img, blurred) = match &self.bg {
            Some((a, b)) => (a, b),
            None => {
                black = Canvas::new(self.cv.w, self.cv.h, self.k);
                (&black, &black)
            }
        };
        for r in self.home.damage(t) {
            self.cv.push_clip(r, 0.0);
            self.home.draw(&mut self.cv, &mut self.ty, t, img, blurred);
            self.cv.pop_clip();
        }
        self.cv.px.clone()
    }

    /// Start moving to `to`: `other` is the apps page drawn whole into a frame-sized buffer.
    pub fn go(&mut self, now: Instant, to: Page, other: Vec<u32>) {
        if to == self.page {
            return;
        }
        let t = self.secs(now);
        let dash = self.snapshot(t);
        let (dir, back, front) = if to == Page::Apps {
            (1, dash, other)
        } else {
            (-1, other, dash)
        };
        self.slide = Some(Slide {
            start: t,
            dir,
            back,
            front,
        });
        self.page = to;
        if let Some(f) = &self.feed
            && to == Page::Dash
        {
            f.ask(Ask::Refresh);
        }
    }

    /// One frame of a page move into `frame`; true while it goes on.
    pub fn draw_slide(&mut self, frame: &mut Frame, now: Instant) -> bool {
        let Some(s) = self.slide.as_ref() else {
            return false;
        };
        let t = self.secs(now);
        let k = ((t - s.start) / SLIDE).clamp(0.0, 1.0) as f32;
        let (w, h) = (frame.w as usize, frame.h as usize);
        if s.back.len() == w * h && s.front.len() == w * h {
            home::slide_compose(&mut frame.px[..], &s.back, &s.front, w, h, k, s.dir);
        }
        if k >= 1.0 {
            self.slide = None;
            self.home.invalidate();
            return false;
        }
        true
    }

    pub fn sliding(&self) -> bool {
        self.slide.is_some()
    }

    /// The dashboard's items for the AccessKit tree: names and frame rectangles.
    pub fn ax_items(&self) -> Vec<(String, Rect)> {
        self.home
            .items()
            .into_iter()
            .map(|(n, r)| {
                let px = |v: f32| (v * self.k).round() as i32;
                (
                    n,
                    Rect::new(px(r.x) + self.off.0, px(r.y) + self.off.1, px(r.w), px(r.h)),
                )
            })
            .collect()
    }

    /// Do item `i` of [`Dash::ax_items`].
    pub fn activate(&mut self, now: Instant, i: usize) -> Outcome {
        let t = self.secs(now);
        self.home.activate_item(t, i)
    }

    /// A key on the dashboard.
    pub fn key(&mut self, now: Instant, k: Key) -> Outcome {
        let t = self.secs(now);
        self.home.key(t, k)
    }

    /// The pointer, in frame pixels.
    pub fn pointer(&mut self, now: Instant, x: i32, y: i32, ev: u8) -> Outcome {
        let t = self.secs(now);
        let (dx, dy) = (
            (x - self.off.0) as f32 / self.k,
            (y - self.off.1) as f32 / self.k,
        );
        let p = match ev {
            0 => Pointer::Move(dx, dy),
            1 => Pointer::Down(dx, dy),
            _ => Pointer::Up(dx, dy),
        };
        self.home.pointer(t, p)
    }

    /// The screen came on, or Home came back from an app: play the entrance and ask the sources again.
    pub fn wake(&mut self, now: Instant) {
        let t = self.secs(now);
        self.home.enter(t);
        if let Some(f) = &self.feed {
            f.ask(Ask::Refresh);
        }
    }

    /// The screen went off.
    pub fn sleep(&mut self) {
        if let Some(f) = &self.feed {
            f.ask(Ask::Sleep);
        }
    }
}

/// Copy rectangle `r` (frame pixels) of the canvas into the frame.
fn copy_rect(cv: &Canvas, frame: &mut Frame, r: Rect) {
    let w = cv.w as usize;
    if frame.w as usize != w || frame.h != cv.h {
        return;
    }
    let (x0, x1) = (
        r.x.max(0) as usize,
        (r.x + r.w).min(cv.w as i32).max(0) as usize,
    );
    for y in r.y.max(0)..(r.y + r.h).min(cv.h as i32) {
        let row = y as usize * w;
        frame.px[row + x0..row + x1].copy_from_slice(&cv.px[row + x0..row + x1]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dashboard_with_no_data_draws_only_what_the_shell_knows() {
        let mut d = Dash::new(1280, 720);
        let ctx = Ctx {
            clock: "12:00",
            online: false,
            battery: None,
            camera: false,
            apps: &[],
            volume: None,
            device_line: None,
        };
        d.compose(Instant::now(), &ctx);
        assert!(
            d.home.data.week.is_none()
                && d.home.data.build.is_none()
                && d.home.data.notes.is_none()
        );
        // Only "Open" (the apps page) is there: no app is installed.
        assert_eq!(d.home.data.segments.len(), 1);
        assert_eq!(d.home.data.segments[0].1.len(), 1);
        let mut f = Frame::new(1280, 720);
        let rects = d.draw(&mut f, Instant::now());
        assert_eq!(rects.len(), 1);
        assert!(f.px.iter().any(|&p| p != 0));
    }

    #[test]
    fn quick_blur_keeps_the_size() {
        let c = Canvas::new(64, 36, 1.0);
        let b = home::quick_blur(&c);
        assert_eq!((b.w, b.h), (32, 18));
    }
}

/// The level panel after a brightness or volume key, as the concept's vertical slider (vertical-slider): a glass pill at the
/// right edge whose thumb moves to the new level on a spring. Drawn over whatever is on the frame, which the caller has just
/// drawn there afresh.
pub struct GlassOsd {
    slider: lucina_glass::widgets::chrome::VSlider,
    max: u32,
    cv: Canvas,
    ty: Type,
    t0: Instant,
    /// The thumb moves until then (view seconds).
    pub moving_until: f64,
    k: f32,
}

/// The panel in dp: 56 wide, 240 tall, 28 from the right edge, centred.
const OSD_W: f32 = 56.0;
const OSD_H: f32 = 270.0;

impl GlassOsd {
    pub fn new(k: f32) -> GlassOsd {
        let (w, h) = ((OSD_W * k).ceil() as u32 + 2, (OSD_H * k).ceil() as u32 + 2);
        GlassOsd {
            slider: lucina_glass::widgets::chrome::VSlider::new(2, 0),
            max: 1,
            cv: Canvas::new(w, h, k),
            ty: Type::new(),
            t0: Instant::now(),
            moving_until: 0.0,
            k,
        }
    }

    fn secs(&self, at: Instant) -> f64 {
        at.saturating_duration_since(self.t0).as_secs_f64()
    }

    /// Where it goes on a `w` x `h` frame.
    pub fn rect(&self, w: u32, h: u32) -> Rect {
        let (cw, ch) = (self.cv.w as i32, self.cv.h as i32);
        Rect::new(
            w as i32 - cw - (28.0 * self.k) as i32,
            (h as i32 - ch) / 2,
            cw,
            ch,
        )
    }

    /// Show `level` of `max` from `now`: the thumb moves there (the top is the most).
    pub fn show(&mut self, now: Instant, level: u32, max: u32) {
        let t = self.secs(now);
        // At most 20 steps on the track: a backlight may have thousands of levels.
        let steps = max.clamp(1, 20);
        let at = level.min(max.max(1)) * steps / max.max(1);
        if steps != self.max {
            self.max = steps;
            self.slider = lucina_glass::widgets::chrome::VSlider::new(steps + 1, steps - at);
        } else {
            self.slider.value.set_target(t, (steps - at) as f32);
        }
        self.moving_until = t + 0.8;
    }

    pub fn moving(&self, now: Instant) -> Option<Instant> {
        let t = self.secs(now);
        (t < self.moving_until).then(|| now + std::time::Duration::from_millis(16))
    }

    /// Draw over `frame` at `now`; returns the rectangle drawn.
    pub fn draw(&mut self, frame: &mut Frame, now: Instant) -> Rect {
        let r = self.rect(frame.w, frame.h);
        let (cw, ch) = (self.cv.w as usize, self.cv.h as usize);
        let fw = frame.w as usize;
        if r.x < 0 || r.y < 0 || r.x as usize + cw > fw || r.y as usize + ch > frame.h as usize {
            return Rect::default();
        }
        for y in 0..ch {
            let src = (r.y as usize + y) * fw + r.x as usize;
            self.cv.px[y * cw..(y + 1) * cw].copy_from_slice(&frame.px[src..src + cw]);
        }
        let t = self.secs(now);
        let mut p = lucina_glass::Paint {
            cv: &mut self.cv,
            ty: &mut self.ty,
            now: t,
            backdrop: None,
        };
        p.shadow(
            R::new(1.0, 1.0, OSD_W, OSD_H),
            lucina_glass::tokens::radius::PILL,
            10.0,
        );
        self.slider.draw(&mut p, R::new(1.0, 1.0, OSD_W, OSD_H));
        for y in 0..ch {
            let dst = (r.y as usize + y) * fw + r.x as usize;
            frame.px[dst..dst + cw].copy_from_slice(&self.cv.px[y * cw..(y + 1) * cw]);
        }
        r
    }
}
