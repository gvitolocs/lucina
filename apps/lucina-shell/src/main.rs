//! lucina-shell — the graphical interface of Juno (LucinaOS for PCs), on the firmware framebuffer.
//!
//! A home screen with a status bar (clock, battery, network), a terminal with BusyBox `sh` on a
//! pty, a Settings page and a tile per app (`apps.rs`), all software-rendered into one frame and copied to `/dev/fb0`.
//! Keys and the pointer come from evdev. One thread and `poll`: it idles at no cost and stays
//! light on one CPU.
//!
//! ```text
//! lucina-shell [--fb /dev/fb0] [--trace] [--log <file>] [--shell <program> [args...]]
//! lucina-shell --render <dir> [--size WxH]     # host: write the three views as PPM files
//! lucina-shell --text-console                  # give the screen and keyboard back to the kernel's console
//! ```
//!
//! Keys: Super+1/2/3 (or Fn+F1/F2/F3: the Galaxy Book Go row gives its hot keys first, kernel patch 1169) Home, Terminal, Settings;
//! the brightness keys anywhere; on Home the arrows and Enter; in Settings Up/Down and
//! Esc, and Enter or Right into the rows that can be changed (the light and when the screen goes off), then Left/Right; in the terminal Shift+PgUp/PgDn look through the history. Everything else in the terminal
//! goes to the shell.
//!
//! `--trace` logs to stderr what a test needs to follow the interface without a screen: views drawn,
//! input devices, the pointer position, every line the terminal completes (`term| ...`), and how long each
//! input took to reach the screen (`latency ...`). `--log` sends all of the shell's messages to a file, for reading over SSH.

#![allow(
    clippy::type_complexity,
    clippy::field_reassign_with_default,
    clippy::items_after_test_module
)]

// Juno with no C library: Eyra is the libc and the program start, in Rust (docs/research/juno-pure-rust.md).
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
extern crate eyra;

mod app_groups;
mod apps;
mod ax;
mod backlight;
mod control;
mod dash;
mod events;
mod fnkeys;
mod home_feed;
mod net;
mod power_watch;
mod pty;
mod settings;
mod standby;
mod system;
mod term;
mod ui;
mod volume;

use std::time::{Duration, Instant};

use apps::{Ended, Running};
use lucina_app::fb::Surface;
use lucina_app::gfx::{Frame, Rect, Text};
use lucina_app::input::{
    EV_ABS, EV_KEY, EV_REL, EV_SYN, Inputs, Pointer, REL_X, REL_Y, SYN_DROPPED,
};
use lucina_app::keys::{Keyboard, Press};
use lucina_app::manifest::{self, Manifest};
use lucina_app::screen::Screen;
use lucina_app::time::{self as tz, Zone};
use lucina_app::{fb, gfx, input, input_watch, keys};
use settings::Display;
use system::Status;
use term::Terminal;
use ui::{Home, Icon, Layout, SettingsPage, Tile, TileAction, View};

const DEFAULT_SHELL: &[&str] = &["/bin/sh", "-l"];
/// How often the status bar looks at the network and the battery.
const STATUS_EVERY: Duration = Duration::from_secs(5);
/// How often the camera mark is checked: a privacy indicator must not lag behind who captures.
const CAMERA_EVERY: Duration = Duration::from_millis(500);
const ASLEEP_RECHECK: Duration = Duration::from_secs(600);
/// Linux's KEY_WAKEUP (input-event-codes.h): the kernel woke the machine by itself (patch 1097).
const KEY_WAKEUP: u16 = 143;

struct Options {
    fb: String,
    trace: bool,
    log: Option<String>,
    shell: Vec<String>,
    render: Option<String>,
    size: (u32, u32),
}

fn usage() -> ! {
    eprintln!(
        "usage: lucina-shell [--fb <device>] [--trace] [--log <file>] [--shell <program> [args...]]"
    );
    eprintln!("       lucina-shell --render <dir> [--size WxH]");
    std::process::exit(2);
}

fn parse_args() -> Options {
    let mut o = Options {
        fb: "/dev/fb0".into(),
        trace: false,
        log: None,
        shell: Vec::new(),
        render: None,
        size: (1024, 768),
    };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--text-console" => {
                // session.sh, before it starts sh on the console.
                if let Err(e) = fb::text_console() {
                    eprintln!("lucina-shell: {e}");
                    std::process::exit(1);
                }
                std::process::exit(0);
            }
            "--fb" => o.fb = args.next().unwrap_or_else(|| usage()),
            "--trace" => o.trace = true,
            "--log" => o.log = Some(args.next().unwrap_or_else(|| usage())),
            "--shell" => {
                o.shell = args.by_ref().collect();
                if o.shell.is_empty() {
                    usage();
                }
            }
            "--render" => o.render = Some(args.next().unwrap_or_else(|| usage())),
            "--size" => {
                let s = args.next().unwrap_or_else(|| usage());
                let (w, h) = s.split_once('x').unwrap_or_else(|| usage());
                o.size = (
                    w.parse().unwrap_or_else(|_| usage()),
                    h.parse().unwrap_or_else(|_| usage()),
                );
            }
            _ => usage(),
        }
    }
    o
}

struct Session {
    /// None when the shell could not be started.
    pty: Option<pty::Pty>,
    term: Terminal,
    exited: bool,
}

/// The tiles: the shell's own views, then an app per manifest, by group and title.
fn home_tiles(apps: &[Manifest], l: &Layout) -> Vec<Tile> {
    let mut tiles = vec![Tile {
        id: "settings".into(),
        title: "Settings".into(),
        subtitle: "This device".into(),
        icon: Icon::Named("settings".into(), "S".into()),
        action: TileAction::Open(View::Settings),
    }];
    // Diana's image has no BusyBox, so no shell for a terminal to run.
    if std::path::Path::new(DEFAULT_SHELL[0]).exists() {
        tiles.insert(
            0,
            Tile {
                id: "terminal".into(),
                title: "Terminal".into(),
                subtitle: "BusyBox shell".into(),
                icon: Icon::Named("terminal".into(), "T".into()),
                action: TileAction::Open(View::Terminal),
            },
        );
    }
    let side = l.px(46.0) as u32;
    for (i, m) in apps.iter().enumerate() {
        let letter: String = m.title.chars().take(1).collect();
        let icon = if m.icon.starts_with('/') {
            match lucina_app::Image::open(&m.icon) {
                Ok(img) => Icon::Picture(img.fit_within(side, side)),
                Err(e) => {
                    eprintln!("lucina-shell: {}: icon {}: {e}", m.id, m.icon);
                    Icon::Text(letter)
                }
            }
        } else if lucina_app::icons::is_name(&m.icon) {
            Icon::Named(m.icon.clone(), letter)
        } else {
            Icon::Text(m.icon.clone())
        };
        tiles.push(Tile {
            id: m.id.clone(),
            title: m.title.clone(),
            subtitle: String::new(),
            icon,
            action: TileAction::App(i),
        });
    }
    sort_home_tiles(&mut tiles);
    tiles
}

/// The apps page's order: by group, then by title, case-insensitively.
fn sort_home_tiles(tiles: &mut [Tile]) {
    tiles.sort_by(|a, b| {
        app_groups::group_of(&a.id)
            .cmp(&app_groups::group_of(&b.id))
            .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
            .then_with(|| a.id.cmp(&b.id))
    });
}

/// An app Lucina asked for while another had the screen: its tile, its args and its environment (an intent's call).
type PendingOpen = (usize, Vec<String>, Vec<(String, String)>);

struct App {
    opts: Options,
    text: Text,
    pub(crate) layout: Layout,
    frame: Frame,
    fb: Option<Screen>,
    display: Display,
    inputs: Inputs,
    kb: Keyboard,
    pointer: Pointer,
    pointer_moved: bool,
    pointer_shown: Option<Rect>,
    pub(crate) view: View,
    pub(crate) home: Home,
    apps: Vec<Manifest>,
    /// The thread reading the disk's apps: its answer, and the pipe that wakes the loop for it.
    disk_apps: Option<(
        std::sync::mpsc::Receiver<Result<(String, Vec<Manifest>), String>>,
        std::os::fd::OwnedFd,
    )>,
    /// The thread reading the apps on /data: its answer, and the pipe that wakes the loop for it.
    /// There is one while a scan has been asked and its answer has not been taken.
    data_apps: Option<(
        std::sync::mpsc::Receiver<Vec<Manifest>>,
        std::os::fd::OwnedFd,
    )>,
    /// The app on screen. While there is one the shell does not draw.
    pub(crate) running: Option<Running>,
    pub(crate) ended: Option<Ended>,
    /// The confirmation dialog, if open: what it would do, and which button has the focus.
    pub(crate) confirm: Option<(ui::Power, usize)>,
    /// The system volume the volume keys move (`volume`).
    volume: volume::Volume,
    /// The level a function key just set, shown for a moment over the screen.
    osd: Option<ui::Osd>,
    /// The touchpad, as its hot key last left it: the kernel drops its events while it is off (patch 1169).
    touchpad_on: bool,
    pub(crate) settings_sel: usize,
    /// The row of Settings that Left/Right change, when the focus is in the panel.
    settings_row: Option<usize>,
    pub(crate) settings: SettingsPage,
    crashes: Option<String>,
    pub(crate) status: Status,
    network: net::Network,
    zone: Zone,
    zone_name: String,
    model: String,
    session: Option<Session>,
    /// The whole screen must be drawn again: another view. Its copy is cheaper than finding out what changed.
    full: bool,
    /// Parts of it must: only these are composed and copied, so a change of focus costs a fraction of a millisecond.
    pub(crate) damage: Vec<Rect>,
    bar_dirty: bool,
    drawn_view: Option<View>,
    /// The screen is off: the power button asked for it. Only the power button counts then.
    asleep: bool,
    /// The backlight class device, when the kernel has one: brightness, and the screen going dark after idling.
    backlight: Option<backlight::Backlight>,
    /// When the last input event of any kind was read, on CLOCK_MONOTONIC: the idle timer counts from it.
    last_input_us: i64,
    /// When the last screen hold ended, on CLOCK_MONOTONIC: the idle timer counts from it too.
    hold_ended_us: i64,
    /// Idle standby (`lucina_standby`): the holds and the times the screen-off and the sleep count from. Its screen is not
    /// used: the shell's own `asleep` and the backlight's `dark` are the truth, put in before every decision.
    standby: lucina_standby::Standby,
    /// Audio playback holds the sleep off (a system hold in `standby`), and when to look again whether it still plays.
    audio_hold: Option<Instant>,
    /// Asleep and gone on to standby (`standby::enter`): the wake undoes it.
    in_standby: bool,
    /// Who holds the screen on now: the app's title and its reason (`lucina_app::hold`).
    held_by: Option<(String, String)>,
    /// When the key that woke the screen was pressed, until the first frame after it.
    wake_key_at: Option<i64>,
    /// Just woken: read the status now, the clock has moved on.
    status_stale: bool,
    /// With `--trace`: the input still waiting to reach the screen, and when the kernel took it.
    input_waiting: Option<(String, i64)>,
    pointer_waiting: Option<i64>,
    /// `/run/lucina/shell.sock`, for `lucina-devd` (`control.rs`). Bound after the first frame.
    control: Option<control::Control>,
    /// An app asked for through the socket while another had the screen: it starts once that one has closed.
    pending_open: Option<PendingOpen>,
    /// The AccessKit socket Lucina reads the shell's screen from, bound after the first frame.
    ax: Option<lucina_ax::Server>,
    /// Home's first page: the glass dashboard (src/dash.rs). The tile grid above is its second page.
    pub(crate) dash: dash::Dash,
    /// The level panel as the glass vertical slider.
    osd_glass: dash::GlassOsd,
    /// The panel's frame time, seconds: 60 Hz.
    frame_period: f64,
}

/// A line in the kernel's log, with the kernel's stamp on it.
fn kmsg(line: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().write(true).open("/dev/kmsg") {
        let _ = writeln!(f, "<6>{line}");
    }
}

/// CLOCK_MONOTONIC in microseconds, the clock the input events are stamped with.
/// Which app is open, on /data, for the line after a crash (lucina-trial). Only where /data has come up: an app opened before that is not told.
fn note_open_app(title: Option<&str>) {
    use std::io::Write;
    const PATH: &str = "/data/lucina/open-app";
    if !std::path::Path::new("/run/lucina/data-ready").exists() {
        return;
    }
    match title {
        Some(title) => {
            if let Ok(mut f) = std::fs::File::create(PATH) {
                let _ = f.write_all(title.as_bytes()).and_then(|()| f.sync_all());
            }
        }
        None => {
            let _ = std::fs::remove_file(PATH);
        }
    }
}

/// A line the system has for the person: what lucina-trial writes.
fn system_notice() -> Option<String> {
    let text = std::fs::read_to_string("/run/lucina/notice").ok()?;
    Some(text.trim().replace('\n', ". ")).filter(|t| !t.is_empty())
}

/// Where the time to the first frame goes: each step of the start, logged once in one line after the first frame.
static START_MARKS: std::sync::Mutex<Vec<(&'static str, i64)>> = std::sync::Mutex::new(Vec::new());

fn start_mark(what: &'static str) {
    if let Ok(mut m) = START_MARKS.lock() {
        m.push((what, monotonic_us()));
    }
}

/// The marks as "step +ms", each from the one before.
fn start_marks_line() -> String {
    let m = START_MARKS.lock().map(|m| m.clone()).unwrap_or_default();
    let mut out = String::from("start:");
    let mut last = m.first().map_or(0, |x| x.1);
    for (what, at) in m {
        out.push_str(&format!(" {what} +{:.1}", (at - last) as f64 / 1000.0));
        last = at;
    }
    out
}

fn monotonic_us() -> i64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: clock_gettime fills the timespec.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec * 1_000_000 + ts.tv_nsec / 1000
}

impl App {
    fn new(opts: Options, fb: Option<fb::Fb>, w: u32, h: u32, display: Display) -> App {
        start_mark("new");
        let mut text = Text::new();
        let layout = Layout::new(w, h, &mut text);
        start_mark("text");
        let (zone_name, zone) = tz::load();
        start_mark("zone");
        // Not read yet: the first frame waits for nothing. It comes straight after that frame.
        let network = net::Network::default();
        // The backlight is opened after the first frame (`after_first_frame`): the screen is lit already, and opening it took 32 ms
        // once on the laptop.
        let backlight: Option<backlight::Backlight> = None;
        start_mark("backlight");
        let settings = settings::page(
            &display,
            layout.k,
            &[],
            &zone_name,
            &network,
            None,
            backlight.as_ref(),
        );
        start_mark("settings");
        let apps = manifest::scan(std::path::Path::new(manifest::MANIFEST_DIR), &mut |e| {
            eprintln!("lucina-shell: {e}")
        });
        start_mark("manifests");
        let mut home = Home {
            tiles: home_tiles(&apps, &layout),
            sel: 0,
            note: None,
            ..Home::default()
        };
        home.reflow(&layout);
        start_mark("tiles");
        let frame = Frame::new(w, h);
        let screen = fb.map(Screen::new);
        start_mark("frame");
        let dash = dash::Dash::new(w, h);
        start_mark("dash");
        let osd_glass = dash::GlassOsd::new(
            (w as f32 / lucina_glass::home::W).min(h as f32 / lucina_glass::home::H),
        );
        start_mark("osd");
        App {
            opts,
            text,
            frame,
            fb: screen,
            display,
            inputs: Inputs::default(),
            kb: Keyboard::default(),
            pointer: Pointer::new(w as i32, h as i32),
            pointer_moved: false,
            pointer_shown: None,
            view: View::Home,
            home,
            apps,
            disk_apps: None,
            data_apps: None,
            running: None,
            ended: None,
            confirm: None,
            volume: volume::Volume::read(),
            osd: None,
            touchpad_on: true,
            settings_sel: 0,
            settings_row: None,
            settings,
            crashes: None,
            status: Status {
                network: String::new(),
                ..system::status(&zone, &network)
            },
            network,
            zone,
            zone_name,
            model: system::model().unwrap_or_else(|| "Lucina Juno".into()),
            session: None,
            full: true,
            damage: Vec::new(),
            bar_dirty: false,
            drawn_view: None,
            asleep: false,
            backlight,
            last_input_us: monotonic_us(),
            hold_ended_us: 0,
            standby: lucina_standby::start(monotonic_us()),
            audio_hold: None,
            in_standby: false,
            held_by: None,
            wake_key_at: None,
            status_stale: false,
            input_waiting: None,
            pointer_waiting: None,
            control: None,
            pending_open: None,
            ax: None,
            dash,
            osd_glass,
            frame_period: 1.0 / 60.0,
            layout,
        }
    }

    /// What the first frame did not wait for: the sky (drawn at the next frame) and the backlight.
    fn after_first_frame(&mut self) {
        if let Some(ms) = self.dash.finish_backdrop() {
            self.log(&format!(
                "home: sky made in {ms:.0} ms, after the first frame"
            ));
        }
        // Only on a real screen: a preview must not change the host's light.
        if self.fb.is_some() && self.backlight.is_none() {
            self.backlight = backlight::Backlight::open(backlight::Paths::system());
            if let Some(b) = &self.backlight {
                self.log(&format!("backlight: level {} of {}", b.level, b.max));
            }
            self.settings = self.build_settings();
        }
    }

    /// Whether the dashboard has the screen: Home, its first page, no app, no dialog.
    fn on_dash(&self) -> bool {
        self.view == View::Home
            && self.running.is_none()
            && self.confirm.is_none()
            && (self.dash.page == dash::Page::Dash || self.dash.sliding())
    }

    /// The dashboard's data again from what the shell knows and the sources' last answers.
    fn compose_dash(&mut self) {
        let battery = self
            .status
            .battery
            .as_ref()
            .map(|b| (b.percent, b.status == "Charging"));
        let volume = std::path::Path::new("/dev/lucina-audio-ctl")
            .exists()
            .then(|| {
                (
                    f32::from(self.volume.level) / f32::from(fnkeys::VOLUME_MAX),
                    self.volume.muted,
                )
            });
        let device_line = system::generation_line()
            .or_else(|| (!self.status.network.is_empty()).then(|| self.status.network.clone()));
        let clock = self
            .status
            .clock
            .split(' ')
            .next()
            .unwrap_or("")
            .to_string();
        let ctx = dash::Ctx {
            clock: &clock,
            online: self.status.online,
            battery,
            camera: self.status.camera.is_some(),
            apps: &self.apps,
            volume,
            device_line,
        };
        self.dash.compose(Instant::now(), &ctx);
    }

    /// The apps page drawn whole into a buffer, for the slide between the pages.
    fn apps_page_pixels(&mut self) -> Vec<u32> {
        let mut f = Frame::new(self.frame.w, self.frame.h);
        let term = self.session.as_ref().map(|s| &s.term.screen);
        ui::draw_all(
            &mut f,
            &mut self.text,
            &self.layout,
            View::Home,
            &self.status,
            &self.model,
            &self.home,
            self.ended.as_ref(),
            &self.settings,
            self.settings_sel,
            self.settings_row,
            term,
            None,
        );
        f.px.to_vec()
    }

    /// Slide to the other page of Home.
    pub(crate) fn go_page(&mut self, to: dash::Page) {
        if self.view != View::Home || self.dash.page == to {
            return;
        }
        let other = self.apps_page_pixels();
        self.dash.go(Instant::now(), to, other);
        self.log(&format!(
            "home page {}",
            if to == dash::Page::Dash {
                "dashboard"
            } else {
                "apps"
            }
        ));
    }

    /// Do item `i` of the dashboard, as Lucina asked through the AccessKit tree.
    pub(crate) fn dash_activate(&mut self, i: usize) {
        let o = self.dash.activate(Instant::now(), i);
        self.dash_outcome(o);
    }

    /// Carry out what the dashboard asked.
    pub(crate) fn dash_outcome(&mut self, o: lucina_glass::home::Outcome) {
        use lucina_glass::home::{Outcome, Target};
        match o {
            Outcome::Open(Target::Apps) => self.go_page(dash::Page::Apps),
            Outcome::Open(Target::Settings) => self.open(View::Settings),
            // Tasks opens on that task (LUCINA_TASKS_OPEN).
            Outcome::Open(Target::Task(id)) => match self.apps.iter().position(|m| m.id == "tasks")
            {
                Some(a) => self.start_app_with(a, &[], &[("LUCINA_TASKS_OPEN".to_string(), id)]),
                None => self.log("home: no tasks app"),
            },
            Outcome::Open(Target::App(id)) | Outcome::Media(id) => {
                match self.apps.iter().position(|m| m.id == id) {
                    Some(a) => self.start_app(a),
                    None => self.log(&format!("home: no app {id}")),
                }
            }
            Outcome::Volume(v) => {
                let level = (v.clamp(0.0, 1.0) * f32::from(fnkeys::VOLUME_MAX)).round() as u8;
                self.volume.set_level(level);
                self.compose_dash();
            }
            Outcome::Dismissed(id) => {
                self.dash.cleared(id);
                self.compose_dash();
            }
            Outcome::None | Outcome::Redraw | Outcome::Unused => {}
        }
    }

    /// The power button: the screen goes off, or comes back. Everything else stays as it is, so that it comes back as it was.
    /// If the kernel does not do this (`lucina.power=on`), nothing changes.
    fn on_power_key(&mut self, at_us: i64) {
        let asleep = !self.asleep;
        if let Err(err) = fb::blank(asleep) {
            self.log(&format!("power button: blanking refused: {err}"));
            return;
        }
        self.asleep = asleep;
        self.kb.release_all();
        if let Some(r) = self.running.as_ref() {
            r.set_paused(asleep);
        }
        if asleep {
            self.log("power button: screen off");
            self.dash.sleep();
        } else {
            self.dash.wake(Instant::now());
            // Waking is input: the screen-off, the sleep and the standby count again from now.
            self.standby = lucina_standby::input(self.standby, monotonic_us());
            if self.in_standby {
                self.in_standby = false;
                self.log(&format!("out of standby: {}", standby::leave()));
            }
            self.log(&format!(
                "power button: screen on {} us after the key",
                monotonic_us() - at_us
            ));
            self.wake_key_at = Some(at_us);
            self.status_stale = true;
            self.bar_dirty = true;
        }
    }

    /// The lid closed or opened. As logind's `HandleLidSwitch=suspend` and Windows' lid action (Sleep): a closed lid sleeps the
    /// way the power key does, an opened one wakes, and the light comes back at its level even if it had gone dark from idling.
    fn on_lid(&mut self, closed: bool, at_us: i64) {
        match lid_action(closed, self.asleep) {
            Some(LidAction::Sleep) => {
                self.log("lid closed: screen off");
                self.on_power_key(at_us);
            }
            Some(LidAction::Wake) => {
                self.last_input_us = monotonic_us();
                if let Some(b) = self.backlight.as_mut().filter(|b| b.dark) {
                    b.wake();
                }
                self.log("lid open: screen on");
                self.on_power_key(at_us);
            }
            None => {}
        }
    }

    pub(crate) fn log(&self, msg: &str) {
        eprintln!("lucina-shell: {msg}");
    }

    fn build_settings(&self) -> SettingsPage {
        settings::page(
            &self.display,
            self.layout.k,
            &self.input_list(),
            &self.zone_name,
            &self.network,
            self.crashes.as_deref(),
            self.backlight.as_ref(),
        )
    }

    /// A brightness key, or Left/Right on the Brightness row.
    fn step_brightness(&mut self, up: bool) {
        if self.backlight.as_mut().is_some_and(|b| b.step(up)) {
            self.refresh_screen_rows();
        }
    }

    /// A function key (`fnkeys::action`): brightness, volume, mute; the level is shown for a moment. Whether it was one.
    fn function_key(&mut self, code: u16) -> bool {
        let shown = match fnkeys::action(code) {
            fnkeys::Action::Brightness(up) => {
                self.step_brightness(up);
                let Some(b) = self.backlight.as_ref() else {
                    return true;
                };
                ui::Osd::new(ui::OsdKind::Brightness, b.level, b.max)
            }
            fnkeys::Action::Volume(up) => {
                self.volume.step(up);
                ui::Osd::new(
                    ui::OsdKind::Volume,
                    u32::from(self.volume.shown()),
                    u32::from(fnkeys::VOLUME_MAX),
                )
            }
            fnkeys::Action::Mute => {
                self.volume.toggle_mute();
                let kind = if self.volume.muted {
                    ui::OsdKind::Muted
                } else {
                    ui::OsdKind::Volume
                };
                ui::Osd::new(
                    kind,
                    u32::from(self.volume.shown()),
                    u32::from(fnkeys::VOLUME_MAX),
                )
            }
            // Juno has no airplane mode yet: the key is logged, so that it can be mapped once there is one.
            fnkeys::Action::Radio => {
                self.log("function key: Wireless Radio (KEY_RFKILL): no airplane mode yet");
                return true;
            }
            fnkeys::Action::Settings => {
                // Over an app the shell does not change its view: Fn+F1 (or Super+1) ends the app first.
                if self.running.is_none() {
                    self.open(View::Settings);
                }
                return true;
            }
            fnkeys::Action::Touchpad => {
                self.touchpad_on = !self.touchpad_on;
                ui::Osd::new(
                    ui::OsdKind::Touchpad(self.touchpad_on),
                    u32::from(self.touchpad_on),
                    1,
                )
            }
            fnkeys::Action::DisplayOff => {
                // "Turn the display on or off": dark as after idling; the next input brings it back.
                self.log("function key: display off");
                if self.backlight.as_mut().is_some_and(|b| b.go_dark()) {
                    self.osd = None;
                }
                // While dark, input only brings the light back and is otherwise dropped, the release of this key too: without
                // this the key would count as held and repeat, darkening the screen again and again (laptop, 2026-11-08).
                self.kb.release_all();
                return true;
            }
            fnkeys::Action::DisplaySwitch => {
                self.log("function key: display switch: no external display on Juno yet");
                return true;
            }
            fnkeys::Action::Other => return false,
        };
        self.log(&format!(
            "function key {code}: {} {} of {}",
            shown.kind.name(),
            shown.level,
            shown.max
        ));
        self.osd_glass.show(Instant::now(), shown.level, shown.max);
        self.osd = Some(shown);
        if self.on_dash() {
            self.compose_dash();
            self.dash_damage(self.osd_glass.rect(self.frame.w, self.frame.h));
        } else {
            self.full = true;
        }
        true
    }

    /// Draw a frame rectangle of the dashboard again at the next frame.
    fn dash_damage(&mut self, r: Rect) {
        let k = (self.frame.w as f32 / lucina_glass::home::W)
            .min(self.frame.h as f32 / lucina_glass::home::H);
        self.dash.home.redraw(lucina_glass::R::new(
            r.x as f32 / k,
            r.y as f32 / k,
            r.w as f32 / k,
            r.h as f32 / k,
        ));
    }

    /// Left/Right, or a click on − or +, on row `i` of the rows that can be changed (`settings::screen_rows`).
    fn adjust(&mut self, i: usize, up: bool) {
        if i == 0 {
            return self.step_brightness(up);
        }
        if let Some(b) = self.backlight.as_mut() {
            match i {
                1 | 2 => b.step_minutes(i == 2, up),
                // "Sleep after" and "Standby after": a standby before the sleep moves with it (`lucina_standby::settle`).
                3 | 4 => b.step_sleep(i == 4, up),
                _ => return,
            }
            self.refresh_screen_rows();
        }
    }

    /// Who holds the screen on, after the app was looked at: a new hold lights a dark screen (Windows' request does), and the end of
    /// one starts the idle timer from then.
    fn update_hold(&mut self) {
        let now_held = self.running.as_ref().and_then(|r| {
            r.display_hold()
                .map(|why| (r.manifest.title.clone(), why.to_string()))
        });
        if now_held == self.held_by {
            return;
        }
        match (&self.held_by, &now_held) {
            (_, Some((app, why))) => self.log(&format!("screen held on by {app}: {why}")),
            (Some((app, _)), None) => self.log(&format!("screen hold by {app} ended")),
            (None, None) => {}
        }
        if now_held.is_none() {
            self.hold_ended_us = monotonic_us();
        }
        // The app's hold is one display hold in standby: it keeps the screen on and the machine awake.
        match (self.held_by.is_some(), now_held.is_some()) {
            (false, true) => {
                self.standby = lucina_standby::take(self.standby, lucina_standby::Kind::Display)
            }
            (true, false) => {
                self.standby = lucina_standby::give(
                    self.standby,
                    lucina_standby::Kind::Display,
                    monotonic_us(),
                )
            }
            _ => {}
        }
        if now_held.is_some()
            && let Some(b) = self.backlight.as_mut().filter(|b| b.dark)
        {
            b.wake();
            self.log("screen on by a hold");
        }
        self.held_by = now_held;
        if let Some(b) = self.backlight.as_mut() {
            b.held_by = self.held_by.as_ref().map(|(app, why)| {
                if why.is_empty() {
                    app.clone()
                } else {
                    format!("{app}: {why}")
                }
            });
        }
        self.refresh_screen_rows();
    }

    /// The light's rows again, in place: cheaper than reading the whole page, so a held key does not read the disks.
    fn refresh_screen_rows(&mut self) {
        let (Some(b), Some((s, n))) = (self.backlight.as_ref(), self.settings.adjustable) else {
            return;
        };
        let rows = settings::screen_rows(b);
        if let Some((_, section)) = self.settings.sections.get_mut(s) {
            let _ = n;
            let n = rows.len().min(section.len());
            section.splice(..n, rows);
        }
        if self.view == View::Settings && self.settings_sel == s {
            self.damage.push(self.layout.panel);
        }
    }

    /// How many rows of the section on screen can be changed.
    fn adjustable_rows(&self) -> usize {
        self.settings
            .adjustable
            .filter(|&(s, _)| s == self.settings_sel)
            .map_or(0, |(_, n)| n)
    }

    /// The focus goes to a row that can be changed, or (None) back to the list of sections.
    fn focus_row(&mut self, row: Option<usize>) {
        if self.settings_row != row {
            self.settings_row = row;
            self.damage.push(self.layout.panel);
        }
    }

    /// Standby's state with the screen as the shell has it now.
    fn standby_now(&self) -> lucina_standby::Standby {
        let screen = if self.asleep && self.in_standby {
            lucina_standby::Screen::Standby
        } else if self.asleep {
            lucina_standby::Screen::Asleep
        } else if self.backlight.as_ref().is_some_and(|b| b.dark) {
            lucina_standby::Screen::Dark
        } else {
            lucina_standby::Screen::Lit
        };
        lucina_standby::Standby {
            screen,
            ..self.standby
        }
    }

    /// When standby next has something to do (the screen off, the sleep, the standby), or audio that holds the sleep is looked
    /// at again. None without a backlight to switch, held, or set to Never.
    fn idle_deadline(&self, now: Instant) -> Option<Instant> {
        let b = self.backlight.as_ref()?;
        let idle = lucina_standby::deadline(self.standby_now(), b.times()).map(|at| {
            // A millisecond late rather than early: poll's timeout is in whole milliseconds.
            let wait = (at - monotonic_us()).max(0) as u64;
            now + Duration::from_micros(wait) + Duration::from_millis(1)
        });
        match (idle, self.audio_hold) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// Idle standby (`lucina_standby::decide`): after the screen-off minutes the light goes off (the level is kept, nothing is
    /// blanked, apps keep running, input brings it back); after "Sleep after", with nothing held, the machine sleeps the same way
    /// as for the lid; after "Standby after", asleep and with nothing held, it goes on to standby (`standby::enter`). All count
    /// from idle. The screen-off minutes are those for the adapter's state as it is now.
    fn check_idle(&mut self) {
        let now_us = monotonic_us();
        if let Some(at) = self.audio_hold
            && Instant::now() >= at
        {
            if standby::read_audio_busy() {
                self.audio_hold = Some(Instant::now() + standby::AUDIO_RECHECK);
            } else {
                self.audio_hold = None;
                self.standby =
                    lucina_standby::give(self.standby, lucina_standby::Kind::System, now_us);
                self.log("sleep hold by audio playback ended");
            }
        }
        let Some(t) = self.backlight.as_ref().map(|b| b.times()) else {
            return;
        };
        if lucina_standby::decide(self.standby_now(), now_us, t) == lucina_standby::Decision::Stay {
            return;
        }
        // The charger may have come or gone since the status last read it.
        let t = match self.backlight.as_mut() {
            Some(b) => {
                b.read_adapter();
                b.times()
            }
            None => return,
        };
        match lucina_standby::decide(self.standby_now(), now_us, t) {
            lucina_standby::Decision::Stay => {}
            lucina_standby::Decision::ScreenOff => {
                if self.backlight.as_mut().is_some_and(|b| b.go_dark()) {
                    self.log(&format!("screen off after {} min idle", t.off));
                } else {
                    // Refused: try again after another while, not at once.
                    self.standby = lucina_standby::refused(self.standby, now_us);
                }
            }
            lucina_standby::Decision::Sleep => {
                if standby::read_audio_busy() {
                    self.audio_hold = Some(Instant::now() + standby::AUDIO_RECHECK);
                    self.standby = lucina_standby::take(self.standby, lucina_standby::Kind::System);
                    self.log("sleep held off by audio playback");
                    return;
                }
                self.log(&format!("asleep after {} min idle", t.sleep));
                self.on_power_key(now_us);
                if !self.asleep {
                    self.standby = lucina_standby::refused(self.standby, now_us);
                }
            }
            lucina_standby::Decision::Standby => {
                if standby::read_audio_busy() {
                    self.audio_hold = Some(Instant::now() + standby::AUDIO_RECHECK);
                    self.standby = lucina_standby::take(self.standby, lucina_standby::Kind::System);
                    self.log("standby held off by audio playback");
                    return;
                }
                let done = standby::enter();
                self.in_standby = true;
                self.log(&format!("standby after {} min idle: {done}", t.standby));
            }
        }
    }

    /// The Crashes section runs a program, so it is read when that section is opened.
    pub(crate) fn select_section(&mut self, i: usize) {
        self.settings_sel = i;
        self.settings_row = None;
        if self
            .settings
            .sections
            .get(i)
            .is_some_and(|(n, _)| *n == "Crashes")
        {
            self.crashes = settings::read_crash_report();
            self.settings = self.build_settings();
        }
        self.damage.push(self.layout.list.union(&self.layout.panel));
    }

    fn input_list(&self) -> Vec<(String, String)> {
        self.inputs
            .devices
            .iter()
            .map(|d| (d.node.clone(), d.name.clone()))
            .collect()
    }

    pub(crate) fn open(&mut self, v: View) {
        if v == View::Settings {
            self.settings = self.build_settings();
        }
        if v == View::Terminal {
            self.ensure_session();
        }
        self.kb.release_all();
        if v == View::Home && self.view != View::Home {
            self.dash.page = dash::Page::Dash;
            self.dash.wake(Instant::now());
        }
        self.view = v;
        self.full = true;
    }

    fn ensure_session(&mut self) {
        if self.session.as_ref().is_some_and(|s| !s.exited) {
            return;
        }
        let (cols, rows) = (self.layout.cols, self.layout.rows);
        let argv: Vec<&str> = if self.opts.shell.is_empty() {
            DEFAULT_SHELL.to_vec()
        } else {
            self.opts.shell.iter().map(|s| s.as_str()).collect()
        };
        match pty::Pty::spawn(&argv, cols as u16, rows as u16, &self.zone.rule) {
            Ok(p) => {
                let mut term = Terminal::new(cols, rows);
                if self.opts.trace {
                    term.screen.trace = Some(Vec::new());
                }
                self.log(&format!(
                    "terminal ready: {cols}x{rows}, {}",
                    argv.join(" ")
                ));
                self.session = Some(Session {
                    pty: Some(p),
                    term,
                    exited: false,
                });
            }
            Err(e) => {
                self.log(&format!("cannot start {}: {e}", argv.join(" ")));
                let mut term = Terminal::new(cols, rows);
                term.feed(
                    format!("lucina-shell: cannot start {}: {e}\r\n", argv.join(" ")).as_bytes(),
                );
                // A session that is already over: Enter tries again.
                self.session = Some(Session {
                    pty: None,
                    term,
                    exited: true,
                });
            }
        }
    }

    /// What the tile or the power button does.
    pub(crate) fn activate(&mut self, i: usize) {
        self.home.sel = i;
        self.full = true;
        match self.home.tiles.get(i).map(|t| t.action) {
            Some(TileAction::Open(v)) => self.open(v),
            Some(TileAction::App(a)) => self.start_app(a),
            None => {
                if let Some(&p) = ui::POWER_BUTTONS.get(i - self.home.tiles.len()) {
                    self.ask(p);
                }
            }
        }
    }

    /// Start reading the apps on the disk, behind the first frame.
    fn start_disk_apps(&mut self) {
        let mut pipe = [-1; 2];
        // SAFETY: pipe2 fills the two-element array.
        if unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_NONBLOCK | libc::O_CLOEXEC) } < 0 {
            self.log("no pipe for the disk's apps");
            return;
        }
        // SAFETY: Both descriptors were just made; each gets one owner.
        let (r, w) = unsafe {
            use std::os::fd::FromRawFd;
            (
                std::os::fd::OwnedFd::from_raw_fd(pipe[0]),
                std::os::fd::OwnedFd::from_raw_fd(pipe[1]),
            )
        };
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(apps::read_disk_apps());
            // The write end closing wakes the poll.
            drop(w);
        });
        self.disk_apps = Some((rx, r));
    }

    /// The disk's answer, when it has come: the tiles grow, after the ones from the image.
    fn take_disk_apps(&mut self) {
        let Some(answer) = self
            .disk_apps
            .as_ref()
            .and_then(|(rx, _)| rx.try_recv().ok())
        else {
            return;
        };
        self.disk_apps = None;
        match answer {
            Ok((generation, found)) => {
                self.log(&format!(
                    "apps from the disk: {} in {generation}, {} ms after the kernel started",
                    found.len(),
                    monotonic_us() / 1000
                ));
                let start = self.apps.len();
                let found: Vec<Manifest> = found
                    .into_iter()
                    .filter(|m| !self.apps.iter().any(|a| a.id == m.id))
                    .collect();
                let tiles = home_tiles(&found, &self.layout)
                    .into_iter()
                    .filter(|t| matches!(t.action, TileAction::App(_)));
                for t in tiles {
                    let action = match t.action {
                        TileAction::App(i) => TileAction::App(start + i),
                        other => other,
                    };
                    self.home.tiles.push(Tile { action, ..t });
                }
                self.apps.extend(found);
                sort_home_tiles(&mut self.home.tiles);
                self.home.sel = 0;
                self.home.scroll = 0;
                self.home.reflow(&self.layout);
                self.update_tiles();
                self.compose_dash();
                if self.opts.trace {
                    for (i, t) in self.home.tiles.iter().enumerate() {
                        if let TileAction::App(a) = t.action {
                            self.log(&format!("tile-index {i} {}", self.apps[a].id));
                        }
                    }
                }
            }
            Err(e) => {
                self.log(&format!("apps from the disk: {e}"));
                if self.apps.is_empty() {
                    self.home.note = Some(format!("Apps are not available: {e}."));
                }
            }
        }
        // The order may have changed: the first tile takes the focus.
        self.full = true;
    }

    /// Start reading the apps on /data, behind the drawing loop: the folder is small, but /data
    /// has just come up and may still be busy. There is no answer until it has come up.
    fn start_data_apps(&mut self) {
        if self.data_apps.is_some() || !std::path::Path::new(apps::DATA_READY).exists() {
            return;
        }
        let mut pipe = [-1; 2];
        // SAFETY: pipe2 fills the two-element array.
        if unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_NONBLOCK | libc::O_CLOEXEC) } < 0 {
            self.log("no pipe for the apps on /data");
            return;
        }
        // SAFETY: Both descriptors were just made; each gets one owner.
        let (r, w) = unsafe {
            use std::os::fd::FromRawFd;
            (
                std::os::fd::OwnedFd::from_raw_fd(pipe[0]),
                std::os::fd::OwnedFd::from_raw_fd(pipe[1]),
            )
        };
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(apps::read_data_apps(std::path::Path::new(apps::DATA_APPS)));
            // The write end closing wakes the poll.
            drop(w);
        });
        self.data_apps = Some((rx, r));
    }

    /// The answer from /data, when it has come: the new apps join the tiles after the others, and
    /// an id that is already there wins (`merge_data_apps` says so). Asked again each time the
    /// home screen comes back, so an app that is installed while another one runs appears without
    /// a reboot.
    fn take_data_apps(&mut self) {
        let Some(found) = self
            .data_apps
            .as_ref()
            .and_then(|(rx, _)| rx.try_recv().ok())
        else {
            return;
        };
        self.data_apps = None;
        let start = self.apps.len();
        let merged = apps::merge_data_apps(std::mem::take(&mut self.apps), found);
        // The tail is what the merge added: it gets its tiles after the old ones.
        let new = merged[start..].to_vec();
        let tiles = home_tiles(&new, &self.layout)
            .into_iter()
            .filter(|t| matches!(t.action, TileAction::App(_)));
        for t in tiles {
            let action = match t.action {
                TileAction::App(i) => TileAction::App(start + i),
                other => other,
            };
            self.home.tiles.push(Tile { action, ..t });
        }
        self.apps = merged;
        sort_home_tiles(&mut self.home.tiles);
        self.home.sel = 0;
        self.home.scroll = 0;
        self.home.reflow(&self.layout);
        self.update_tiles();
        self.full = true;
    }

    /// Hand the screen and the input to an app.
    fn start_app(&mut self, a: usize) {
        self.start_app_with(a, &[], &[]);
    }

    /// The same with arguments after the program's path, and environment of its own (an app intent's call and reply file).
    fn start_app_with(&mut self, a: usize, args: &[String], env: &[(String, String)]) {
        let Some(m) = self.apps.get(a) else { return };
        if m.terminal {
            let m = m.clone();
            return self.start_in_terminal(a, &m);
        }
        self.home.note = None;
        self.kb.release_all();
        let t0 = monotonic_us();
        let started = apps::prepare(m).and_then(|(path, copy)| {
            let mut run = m.clone();
            run.exec = path;
            let dir = apps::app_dir(m, copy);
            let r = Running::start(a, &run, &dir, &self.zone.rule, self.opts.trace, args, env);
            if copy {
                // It runs from its own pages now.
                let _ = std::fs::remove_file(&run.exec);
            }
            r
        });
        if self.opts.trace {
            self.log(&format!("app prepared in {} us", monotonic_us() - t0));
        }
        match started {
            Ok(r) => {
                self.log(&format!("app {} started at_us={}", m.id, monotonic_us()));
                events::opened(&m.id, &m.title, false);
                note_open_app(Some(&m.title));
                self.running = Some(r);
            }
            Err(e) => {
                self.log(&format!("app {}: cannot start {}: {e}", m.id, m.exec));
                events::failed_to_start(&m.id, &format!("It could not start: {e}."));
                self.ended = Some(Ended {
                    app: a,
                    title: m.title.clone(),
                    what: format!("It could not start: {e}."),
                    tail: Vec::new(),
                    failed: true,
                });
                self.view = View::Ended;
                self.full = true;
            }
        }
    }

    /// A program for the terminal: typed into the terminal's shell, so it keeps running there while the screen is
    /// elsewhere, and the shell is there when it ends. The copy stays until it is next started: the shell starts it later.
    fn start_in_terminal(&mut self, a: usize, m: &Manifest) {
        self.home.note = None;
        match apps::prepare(m) {
            Ok((path, copy)) => {
                let line = apps::terminal_command(&path, &apps::app_dir(m, copy));
                self.ensure_session();
                if let Some(pty) = self.session.as_mut().and_then(|s| s.pty.as_mut()) {
                    pty.write(line.as_bytes());
                }
                self.log(&format!("app {} typed into the terminal", m.id));
                events::opened(&m.id, &m.title, true);
                self.open(View::Terminal);
            }
            Err(e) => {
                self.log(&format!("app {}: cannot start {}: {e}", m.id, m.exec));
                self.ended = Some(Ended {
                    app: a,
                    title: m.title.clone(),
                    what: format!("It could not start: {e}."),
                    tail: Vec::new(),
                    failed: true,
                });
                self.view = View::Ended;
                self.full = true;
            }
        }
    }

    /// Take the screen back from an app that ended.
    fn app_ended(&mut self, e: Ended) {
        self.log(&format!("app ended: {}: {}", e.title, e.what));
        if let Some(m) = self.apps.get(e.app) {
            events::ended(&m.id, &e.what, e.failed);
        }
        // An app killed after its half second leaves its tree socket behind: it is gone, so is the socket.
        if let Some(r) = self.running.take() {
            let _ = std::fs::remove_file(lucina_ax::proto::socket_path(r.pid()));
        }
        self.kb.release_all();
        self.pointer_shown = None;
        self.drawn_view = None;
        // The app drew over everything: all of the frame has to go to the screen again.
        if e.failed {
            self.view = View::Ended;
            self.ended = Some(e);
        } else {
            self.view = View::Home;
            self.dash.page = dash::Page::Dash;
            self.dash.wake(Instant::now());
            note_open_app(None);
            // An app may have been put on /data while this one had the screen.
            self.start_data_apps();
            self.home.note = Some(format!("{}: {}", e.title, e.what));
            self.ended = None;
        }
        self.full = true;
        // Lucina asked for another app while this one had the screen.
        if let Some((a, args, env)) = self.pending_open.take() {
            self.view = View::Home;
            self.ended = None;
            self.start_app_with(a, &args, &env);
        }
    }

    /// One request from `/run/lucina/shell.sock`.
    fn answer(
        &mut self,
        request: Result<control::Request, String>,
        now: Instant,
    ) -> serde_json::Value {
        use control::Request;
        use serde_json::json;
        let request = match request {
            Ok(r) => r,
            Err(why) => return control::refusal(&why),
        };
        let front = self.running.as_ref().map(|r| r.manifest.id.clone());
        match request {
            Request::List => {
                let apps: Vec<serde_json::Value> = self
                    .apps
                    .iter()
                    .map(|m| json!({"id": m.id, "title": m.title, "terminal": m.terminal}))
                    .collect();
                json!({"ok": true, "apps": apps, "front": front})
            }
            Request::Open { id, args, intent } => {
                let Some(a) = self.apps.iter().position(|m| m.id == id) else {
                    return control::refusal(&format!("there is no app {id:?}"));
                };
                if self.apps[a].terminal && !args.is_empty() {
                    return control::refusal("a terminal program takes no args here");
                }
                let env: Vec<(String, String)> = match &intent {
                    Some(i) => {
                        if self.apps[a].terminal {
                            return control::refusal("a terminal program takes no intents");
                        }
                        let reply = std::path::Path::new(&i.reply);
                        if let Err(why) =
                            control::reply_dir_ok(std::path::Path::new(control::INTENT_DIR), reply)
                        {
                            return control::refusal(&why);
                        }
                        vec![
                            ("LUCINA_INTENT".into(), i.call.clone()),
                            ("LUCINA_INTENT_REPLY".into(), i.reply.clone()),
                        ]
                    }
                    None => Vec::new(),
                };
                // An intent for the app in front starts it again for that call: lucina-devd sends it over the app's socket when
                // it has one.
                if front.as_deref() == Some(id.as_str()) && args.is_empty() && intent.is_none() {
                    return json!({"ok": true, "opening": id, "closing": null, "already": true});
                }
                self.log(&format!(
                    "control: open {id}{}",
                    if intent.is_some() {
                        " for an intent"
                    } else {
                        ""
                    }
                ));
                self.confirm = None;
                if let Some(r) = self.running.as_mut() {
                    // As F1 does; the new one starts when this one has gone (`app_ended`).
                    r.stop(now);
                    self.pending_open = Some((a, args, env));
                    return json!({"ok": true, "opening": id, "closing": front});
                }
                self.view = View::Home;
                self.start_app_with(a, &args, &env);
                match (&self.running, &self.ended) {
                    (Some(_), _) => json!({"ok": true, "opening": id, "closing": null}),
                    (None, Some(e)) if e.failed => {
                        control::refusal(&format!("{}: {}", e.title, e.what))
                    }
                    _ => json!({"ok": true, "opening": id, "closing": null, "terminal": true}),
                }
            }
            Request::Close => {
                self.pending_open = None;
                if let Some(id) = &front {
                    self.log(&format!("control: close {id}"));
                }
                if let Some(r) = self.running.as_mut() {
                    r.stop(now);
                }
                json!({"ok": true, "closing": front})
            }
            Request::Notify { title, body } => {
                self.home.note = Some(control::notice_line(&title, &body));
                self.full = true;
                json!({"ok": true})
            }
            Request::Notices => json!({
                "ok": true,
                "notice": self.home.note,
                "system": system_notice(),
                "front": front
            }),
            Request::Inhibit { .. } => control::refusal("inhibit is answered by the socket"),
            Request::Holds => {
                // Windows' `powercfg /requests`: who keeps what, and when standby acts next by itself (the socket adds the
                // inhibitors).
                let s = self.standby_now();
                let t = self.backlight.as_ref().map(|b| b.times());
                let next_s = t
                    .and_then(|t| lucina_standby::deadline(s, t))
                    .map(|at| ((at - monotonic_us()).max(0) / 1_000_000) as u64);
                json!({
                    "ok": true,
                    "state": format!("{:?}", s.screen).to_lowercase(),
                    "screen_held_by": self.held_by.as_ref().map(|(app, why)| format!("{app}: {why}")),
                    "audio": self.audio_hold.is_some(),
                    "display_holds": s.display_holds,
                    "system_holds": s.system_holds,
                    "minutes": t.map(|t| json!({"screen_off": t.off, "sleep": t.sleep, "standby": t.standby})),
                    "next_in_s": next_s,
                })
            }
        }
    }

    fn pump_control(&mut self, now: Instant) {
        if let Some(mut c) = self.control.take() {
            let pumped = c.pump(now, &mut |r| self.answer(r, now));
            self.control = Some(c);
            // Inhibitors (`lucina-inhibit`): "idle" keeps the screen on and the machine awake, "sleep" the machine awake (and a
            // sleeping one out of standby). Each lasts as long as its connection.
            let kind = |what| match what {
                control::What::Idle => lucina_standby::Kind::Display,
                control::What::Sleep => lucina_standby::Kind::System,
            };
            for h in &pumped.began {
                self.standby = lucina_standby::take(self.standby, kind(h.what));
                let what = match h.what {
                    control::What::Idle => "screen and sleep",
                    control::What::Sleep => "sleep",
                };
                self.log(&format!("{what} held by {}: {}", h.who, h.why));
            }
            for h in &pumped.ended {
                self.standby = lucina_standby::give(self.standby, kind(h.what), monotonic_us());
                self.log(&format!("hold by {} ended", h.who));
            }
        }
    }

    /// Reopen the app that stopped.
    pub(crate) fn reopen(&mut self) {
        if let Some(e) = self.ended.take() {
            self.view = View::Home;
            self.start_app(e.app);
        }
    }

    /// The network came or went: tiles of apps that need it say so.
    fn update_tiles(&mut self) {
        for t in &mut self.home.tiles {
            if let TileAction::App(a) = t.action {
                let needs = self.apps[a].needs_network && !self.status.online;
                let subtitle = if needs { "needs network" } else { "" };
                if t.subtitle != subtitle {
                    t.subtitle = subtitle.into();
                    // The status bar alone is about to be redrawn: this shows on the tile too.
                    self.full = true;
                }
            }
        }
    }

    fn on_key(&mut self, p: Press) {
        if self.opts.trace {
            self.log(&format!("key {}", p.code));
        }
        if self.confirm.is_some() {
            return self.on_confirm_key(p);
        }
        // The function row of a laptop may send brightness and volume until Fn is held, so Super+1/2/3 does the same.
        match (p.code, p.mods.meta) {
            // Ctrl+Tab and Ctrl+Shift+Tab go round Home, Terminal and Settings, as a browser's tabs do; the F-row of this laptop
            // gives its hot keys first (kernel 1169), so F1-F3 below come with Fn.
            (keys::KEY_TAB, _) if p.mods.ctrl => {
                let order = [View::Home, View::Terminal, View::Settings];
                let at = order.iter().position(|v| *v == self.view).unwrap_or(0);
                let next = if p.mods.shift {
                    (at + order.len() - 1) % order.len()
                } else {
                    (at + 1) % order.len()
                };
                return self.open(order[next]);
            }
            (keys::KEY_F1, _) | (keys::KEY_1, true) if self.view == View::Home => {
                return self.go_page(dash::Page::Dash);
            }
            (keys::KEY_F1, _) | (keys::KEY_1, true) => return self.open(View::Home),
            (keys::KEY_F2, _) | (keys::KEY_2, true) => return self.open(View::Terminal),
            (keys::KEY_F3, _) | (keys::KEY_3, true) => return self.open(View::Settings),
            _ if self.function_key(p.code) => return,
            _ => {}
        }
        match self.view {
            View::Home if self.dash.page == dash::Page::Dash => {
                use lucina_glass::home::{Key, Outcome};
                let k = match p.code {
                    keys::KEY_LEFT => Some(Key::Left),
                    keys::KEY_RIGHT => Some(Key::Right),
                    keys::KEY_UP => Some(Key::Up),
                    keys::KEY_DOWN => Some(Key::Down),
                    keys::KEY_ENTER | keys::KEY_KPENTER | keys::KEY_SPACE => Some(Key::Enter),
                    keys::KEY_DELETE => Some(Key::Delete),
                    keys::KEY_ESC => Some(Key::Esc),
                    keys::KEY_PAGEDOWN | keys::KEY_TAB => {
                        self.go_page(dash::Page::Apps);
                        None
                    }
                    _ => None,
                };
                if let Some(k) = k {
                    let o = self.dash.key(Instant::now(), k);
                    if o != Outcome::Unused {
                        self.dash_outcome(o);
                    }
                }
            }
            View::Home
                if matches!(
                    p.code,
                    keys::KEY_ESC | keys::KEY_BACKSPACE | keys::KEY_PAGEUP
                ) =>
            {
                self.go_page(dash::Page::Dash);
            }
            View::Home => {
                // The tiles, then the power buttons.
                let tiles = self.home.tiles.len();
                let n = tiles + ui::POWER_BUTTONS.len();
                let sel = self.home.sel;
                let tile_x = self
                    .home
                    .placed
                    .get(sel)
                    .map(|r| r.x + r.w / 2)
                    .unwrap_or(0);
                let next = match p.code {
                    keys::KEY_RIGHT if sel < tiles => Some((sel + 1) % tiles.max(1)),
                    keys::KEY_RIGHT => Some(tiles + (sel - tiles + 1) % ui::POWER_BUTTONS.len()),
                    keys::KEY_TAB if !p.mods.shift => Some((sel + 1) % n),
                    keys::KEY_LEFT | keys::KEY_TAB if sel < tiles => {
                        Some((sel + tiles - 1) % tiles.max(1))
                    }
                    keys::KEY_LEFT | keys::KEY_TAB => Some(
                        tiles
                            + (sel - tiles + ui::POWER_BUTTONS.len() - 1) % ui::POWER_BUTTONS.len(),
                    ),
                    keys::KEY_DOWN if sel < tiles => {
                        let row = self.home.row_of(sel).unwrap_or(0);
                        self.home.nearest_in_row(row + 1, tile_x).or_else(|| {
                            let a = self.layout.power[0].x + self.layout.power[0].w / 2;
                            let b = self.layout.power[1].x + self.layout.power[1].w / 2;
                            let k = if (b - tile_x).abs() < (a - tile_x).abs() {
                                1
                            } else {
                                0
                            };
                            Some(tiles + k)
                        })
                    }
                    keys::KEY_UP if sel >= tiles => {
                        let k = (sel - tiles).min(ui::POWER_BUTTONS.len() - 1);
                        let x = self.layout.power[k].x + self.layout.power[k].w / 2;
                        self.home
                            .last_row()
                            .and_then(|row| self.home.nearest_in_row(row, x))
                    }
                    keys::KEY_UP => {
                        let row = self.home.row_of(sel).unwrap_or(0);
                        if row == 0 {
                            Some(sel)
                        } else {
                            self.home.nearest_in_row(row - 1, tile_x)
                        }
                    }
                    keys::KEY_ENTER | keys::KEY_KPENTER | keys::KEY_SPACE => {
                        self.activate(sel);
                        None
                    }
                    _ => None,
                };
                if let Some(i) = next {
                    self.focus_home(i);
                }
            }
            View::Ended => match p.code {
                keys::KEY_ENTER | keys::KEY_KPENTER => self.reopen(),
                keys::KEY_ESC | keys::KEY_BACKSPACE => self.open(View::Home),
                _ => {}
            },
            View::Settings => {
                let n = self.settings.sections.len();
                let rows = self.adjustable_rows();
                match (self.settings_row, p.code) {
                    (Some(r), keys::KEY_LEFT | keys::KEY_RIGHT) => {
                        self.adjust(r, p.code == keys::KEY_RIGHT)
                    }
                    (Some(r), keys::KEY_DOWN) => self.focus_row(Some((r + 1).min(rows - 1))),
                    (Some(0), keys::KEY_UP) => self.focus_row(None),
                    (Some(r), keys::KEY_UP) => self.focus_row(Some(r - 1)),
                    (Some(_), keys::KEY_ESC | keys::KEY_BACKSPACE | keys::KEY_TAB) => {
                        self.focus_row(None)
                    }
                    (Some(_), _) => {}
                    (None, keys::KEY_ENTER | keys::KEY_KPENTER | keys::KEY_RIGHT) if rows > 0 => {
                        self.focus_row(Some(0))
                    }
                    (None, keys::KEY_DOWN | keys::KEY_TAB) => {
                        self.select_section((self.settings_sel + 1) % n)
                    }
                    (None, keys::KEY_UP) => self.select_section((self.settings_sel + n - 1) % n),
                    (None, keys::KEY_ESC | keys::KEY_BACKSPACE) => self.open(View::Home),
                    (None, _) => {}
                }
            }
            View::Terminal => {
                let Some(s) = self.session.as_mut() else {
                    return;
                };
                if s.exited {
                    if matches!(p.code, keys::KEY_ENTER | keys::KEY_KPENTER) {
                        self.ensure_session();
                        self.full = true;
                    }
                    return;
                }
                if p.mods.shift && matches!(p.code, keys::KEY_PAGEUP | keys::KEY_PAGEDOWN) {
                    let page = s.term.screen.rows.saturating_sub(1).max(1) as isize;
                    let rows = if p.code == keys::KEY_PAGEUP {
                        page
                    } else {
                        -page
                    };
                    if s.term.screen.scroll_view(rows) {
                        let back = s.term.screen.view_back();
                        self.full = true;
                        if self.opts.trace {
                            self.log(&format!("history {back} rows back"));
                        }
                    }
                    return;
                }
                let bytes = keys::terminal_bytes(p, s.term.screen.app_cursor);
                if let Some(pty) = s.pty.as_mut().filter(|_| !bytes.is_empty()) {
                    pty.write(&bytes);
                    if s.term.screen.scroll_view(isize::MIN) {
                        self.full = true;
                    }
                }
            }
        }
    }

    /// Item `i` of Home's focus order on the screen: the tiles, then the power buttons.
    fn home_item_rect(&self, i: usize) -> Rect {
        let tiles = self.home.tiles.len();
        if i < tiles {
            self.home.placed.get(i).copied().unwrap_or_default()
        } else {
            self.layout.power[(i - tiles).min(self.layout.power.len() - 1)]
        }
    }

    /// The focus moves to item `i` of Home: only the two items are drawn, unless the page scrolls.
    pub(crate) fn focus_home(&mut self, i: usize) {
        if i == self.home.sel {
            return;
        }
        let old = self.home_item_rect(self.home.sel);
        self.home.sel = i;
        if self.home.ensure_visible() {
            self.full = true;
        } else {
            self.damage.push(old);
            self.damage.push(self.home_item_rect(i));
        }
    }

    fn ask(&mut self, action: ui::Power) {
        self.kb.release_all();
        self.confirm = Some((action, ui::CONFIRM_CANCEL));
        self.damage.push(self.layout.dialog);
        self.log(&format!("asking before: {}", action.name()));
    }

    fn on_confirm_key(&mut self, p: Press) {
        let Some((action, sel)) = self.confirm else {
            return;
        };
        match p.code {
            keys::KEY_LEFT | keys::KEY_RIGHT | keys::KEY_TAB | keys::KEY_UP | keys::KEY_DOWN => {
                self.confirm = Some((action, 1 - sel));
                self.damage.push(self.layout.dialog);
            }
            keys::KEY_ESC => {
                self.confirm = None;
                self.damage.push(self.layout.dialog);
            }
            keys::KEY_ENTER | keys::KEY_KPENTER | keys::KEY_SPACE => {
                self.confirm = None;
                self.damage.push(self.layout.dialog);
                if sel == ui::CONFIRM_DO {
                    self.leave(action);
                }
            }
            _ => {}
        }
    }

    /// Restart or power off. /data, if this boot has it, is flushed and marked clean first, or the next boot spends seconds on
    /// `e2fsck`. If that fails (something still has a file open) the next boot checks it: the machine still goes.
    pub(crate) fn leave(&mut self, action: ui::Power) {
        self.log(&format!("{}: asked for and confirmed", action.name()));
        let dirty = ui::draw_leaving(&mut self.frame, &mut self.text, &self.layout, action);
        if let Some(fb) = self.fb.as_mut() {
            fb.present(&self.frame, dirty);
        }
        if std::path::Path::new("/run/lucina/data-ready").exists() {
            match std::process::Command::new("/sbin/lucina-data")
                .arg("clean")
                .status()
            {
                Ok(s) if s.success() => self.log("/data marked clean"),
                Ok(s) => self.log(&format!("/data not marked clean: lucina-data {s}")),
                Err(e) => self.log(&format!("/data not marked clean: {e}")),
            }
        }
        let cmd = match action {
            ui::Power::Restart => libc::RB_AUTOBOOT,
            ui::Power::ShutDown => libc::RB_POWER_OFF,
        };
        // SAFETY: reboot takes one integer and returns only if it failed.
        let r = unsafe { libc::reboot(cmd) };
        self.log(&format!(
            "{}: reboot returned {r}: {}",
            action.name(),
            std::io::Error::last_os_error()
        ));
        self.full = true;
    }

    /// A wheel step: the terminal's history, or the next Settings section.
    fn on_wheel(&mut self, steps: i32) {
        match self.view {
            View::Terminal => {
                if let Some(s) = self.session.as_mut()
                    && s.term.screen.scroll_view(steps as isize * 3)
                {
                    self.full = true;
                }
            }
            View::Settings => {
                let n = self.settings.sections.len() as i32;
                let next = (self.settings_sel as i32 - steps).rem_euclid(n);
                self.select_section(next as usize);
            }
            View::Home if !self.on_dash() => {
                if self.home.scroll_by(-steps) {
                    self.full = true;
                }
            }
            View::Home | View::Ended => {}
        }
    }

    /// What the pointer is over takes the focus, so the highlight follows the pointer as it follows the keys.
    fn hover(&mut self) {
        let (x, y) = (self.pointer.x, self.pointer.y);
        if let Some((action, sel)) = self.confirm {
            if let Some(i) = self.layout.dialog_button_at(x, y)
                && i != sel
            {
                self.confirm = Some((action, i));
                self.damage.push(self.layout.dialog);
            }
            return;
        }
        if self.view != View::Home {
            return;
        }
        if self.on_dash() {
            let o = self.dash.pointer(Instant::now(), x, y, 0);
            self.dash_outcome(o);
            return;
        }
        if let Some(i) = self.home_item_at(x, y) {
            self.focus_home(i);
        }
    }

    /// The tile or power button under (`x`, `y`), as an index for `Home::sel`.
    fn home_item_at(&self, x: i32, y: i32) -> Option<usize> {
        let tiles = self.home.tiles.len();
        if self.home.area.contains(x, y)
            && let Some(i) = self.home.placed.iter().position(|r| r.contains(x, y))
        {
            return Some(i);
        }
        self.layout.power_at(x, y).map(|i| tiles + i)
    }

    fn on_click(&mut self, x: i32, y: i32) {
        if let Some((action, _)) = self.confirm {
            match self.layout.dialog_button_at(x, y) {
                Some(ui::CONFIRM_DO) => {
                    self.confirm = None;
                    self.damage.push(self.layout.dialog);
                    self.leave(action);
                }
                Some(_) => {
                    self.confirm = None;
                    self.damage.push(self.layout.dialog);
                }
                // A click elsewhere neither confirms nor closes it.
                None => {}
            }
            return;
        }
        if self.layout.bar_home.contains(x, y) {
            return self.open(View::Home);
        }
        match self.view {
            View::Home => {
                if let Some(i) = self.home_item_at(x, y) {
                    self.activate(i);
                }
            }
            View::Ended => {
                let (reopen, home) = ui::ended_buttons(&self.layout);
                if reopen.contains(x, y) {
                    self.reopen();
                } else if home.contains(x, y) {
                    self.open(View::Home);
                }
            }
            View::Settings => {
                if let Some(i) = self.layout.section_at(x, y, self.settings.sections.len()) {
                    self.select_section(i);
                } else if let Some((i, step)) = self.layout.stepper_at(x, y, self.adjustable_rows())
                {
                    self.focus_row(Some(i));
                    if let Some(up) = step {
                        self.adjust(i, up);
                    }
                }
            }
            View::Terminal => {}
        }
    }

    /// Rescan through the input watch: the kernel said an input device came or went (`input_watch`), so the directory is
    /// read now; without the hub the timed rescan covers it.
    fn handle_input(&mut self, now: Instant, watch: Option<&mut input_watch::InputWatch>) {
        let rescanned = input_watch::rescan(watch, &mut self.inputs, now);
        if rescanned {
            for d in &self.inputs.devices {
                if self.opts.trace {
                    self.log(&format!(
                        "input {}: {} (EVIOCGNAME returned {})",
                        d.node, d.name, d.name_ioctl
                    ));
                } else {
                    self.log(&format!("input {}: {}", d.node, d.name));
                }
                if d.has_abs_ranges {
                    self.log(&format!(
                        "absolute ranges x {}..{} y {}..{}",
                        d.abs_x.min, d.abs_x.max, d.abs_y.min, d.abs_y.max
                    ));
                }
            }
            if self.view == View::Settings {
                self.settings = self.build_settings();
                self.full = true;
            }
        }
        let mut evs = Vec::new();
        if self.inputs.read(&mut evs) {
            self.kb.release_all();
            self.log("an input device went away");
        }
        // The lid (Linux's SW_LID, from the kernel's gpio-keys device) first: never swallowed by the idle wake below, and only
        // an opening counts as input.
        let mut lid = None;
        evs.retain(|(_, ev)| {
            let is_lid = ev.type_ == EV_SW && ev.code == SW_LID;
            if is_lid {
                lid = Some((ev.value != 0, ev.at_us));
            }
            !is_lid
        });
        if let Some((closed, at_us)) = lid {
            self.on_lid(closed, at_us);
        }
        // Asleep (the lid, the power key or idle standby): a key press wakes it, and so does the kernel's KEY_WAKEUP when it
        // woke itself for the alarm, the keyboard or the touchpad (both `SharedAndWake` in this laptop's DSDT, as Windows'
        // Modern Standby wakes on them). The press does nothing else. The power key keeps its own path below.
        if self.asleep
            && let Some(at_us) = evs
                .iter()
                .find(|(_, ev)| ev.type_ == EV_KEY && ev.value == 1 && ev.code != keys::KEY_POWER)
                .map(|(_, ev)| ev.at_us)
        {
            if let Some(b) = self.backlight.as_mut().filter(|b| b.dark) {
                b.wake();
            }
            self.log("woken by a key");
            self.on_power_key(at_us);
            evs.clear();
        }
        // KEY_WAKEUP only says the machine woke: no app sees it.
        evs.retain(|(_, ev)| !(ev.type_ == EV_KEY && ev.code == KEY_WAKEUP));
        if !evs.is_empty() {
            self.last_input_us = monotonic_us();
            self.standby = lucina_standby::input(self.standby, self.last_input_us);
            // Dark from idling: whatever came brings the light back and does nothing else, so a key that wakes the
            // screen does not type and a tap does not click.
            if let Some(b) = self.backlight.as_mut().filter(|b| b.dark) {
                b.wake();
                self.log("screen on by input");
                evs.clear();
            }
        }
        for (dev, ev) in evs {
            // Lucina's injection device cannot press the power buttons or the dialog's buttons.
            let injected = self
                .inputs
                .devices
                .get(dev)
                .is_some_and(|d| d.name == "lucina-inject");
            if ev.type_ == EV_KEY && ev.code == keys::KEY_POWER {
                if ev.value == 1 {
                    self.on_power_key(ev.at_us);
                }
                continue;
            }
            if self.asleep {
                continue;
            }
            match ev.type_ {
                // The kernel's buffer overflowed: releases may be lost, and a held key must not
                // repeat forever.
                EV_SYN if ev.code == SYN_DROPPED => self.kb.release_all(),
                // An app has the screen: only F1 (or Super+1) and the brightness keys are ours.
                EV_KEY if self.running.is_some() => {
                    let Some(p) = self.kb.feed(ev.code, ev.value, now) else {
                        continue;
                    };
                    if self.function_key(p.code) {
                    } else if (p.code == keys::KEY_F1 || (p.code == keys::KEY_1 && p.mods.meta))
                        && let Some(r) = self.running.as_mut()
                    {
                        eprintln!("lucina-shell: F1: stopping {}", r.manifest.id);
                        r.stop(now);
                    }
                }
                _ if self.running.is_some() => {}
                EV_KEY
                    if (ev.code == keys::BTN_LEFT || ev.code == keys::BTN_TOUCH)
                        && self.on_dash()
                        && !self.dash.sliding() =>
                {
                    let (x, y) = (self.pointer.x, self.pointer.y);
                    if ev.value == 1 || ev.value == 0 {
                        self.input_waiting = Some((format!("click {x},{y}"), ev.at_us));
                        let o = self
                            .dash
                            .pointer(now, x, y, if ev.value == 1 { 1 } else { 2 });
                        self.dash_outcome(o);
                    }
                }
                EV_KEY if ev.code == keys::BTN_LEFT || ev.code == keys::BTN_TOUCH => {
                    if ev.value == 1 {
                        let (x, y) = (self.pointer.x, self.pointer.y);
                        let guarded = if self.confirm.is_some() {
                            self.layout.dialog_button_at(x, y).is_some()
                        } else {
                            self.view == View::Home && self.layout.power_at(x, y).is_some()
                        };
                        if guarded && !lucina_ax::rules::input_reaches(true, injected) {
                            continue;
                        }
                        self.input_waiting = Some((format!("click {x},{y}"), ev.at_us));
                        if self.opts.trace {
                            self.log(&format!("click {x},{y}"));
                        }
                        self.on_click(x, y);
                    }
                }
                EV_KEY => {
                    if let Some(p) = self.kb.feed(ev.code, ev.value, now) {
                        let enter = matches!(
                            p.code,
                            keys::KEY_ENTER | keys::KEY_KPENTER | keys::KEY_SPACE
                        );
                        let guarded = self.confirm.is_some()
                            || (self.view == View::Home && self.home.sel >= self.home.tiles.len());
                        if enter && guarded && !lucina_ax::rules::input_reaches(true, injected) {
                            continue;
                        }
                        self.input_waiting = Some((format!("key {}", p.code), ev.at_us));
                        self.on_key(p);
                    }
                }
                EV_REL => {
                    if ev.code == input::REL_WHEEL {
                        self.on_wheel(ev.value);
                        continue;
                    }
                    match ev.code {
                        REL_X => self.pointer.moved(ev.value, 0),
                        REL_Y => self.pointer.moved(0, ev.value),
                        _ => continue,
                    }
                    self.pointer_moved = true;
                    self.pointer_waiting = Some(ev.at_us);
                }
                EV_ABS => {
                    let d =
                        &self.inputs.devices[dev.min(self.inputs.devices.len().saturating_sub(1))];
                    let range = if ev.code == input::ABS_X {
                        d.abs_x
                    } else {
                        d.abs_y
                    };
                    self.pointer.absolute(ev.code, ev.value, range);
                    self.pointer_moved = true;
                }
                _ => {}
            }
        }
        if self.pointer_moved {
            self.hover();
        }
        while let Some(p) = self.kb.repeat(now) {
            // A held function key repeats only when it is a level (`fnkeys::repeats`: volume, brightness), and does so over an
            // app too, as Windows does; a toggle (mute, touchpad, display off) never repeats.
            let action = fnkeys::action(p.code);
            if action != fnkeys::Action::Other {
                if fnkeys::repeats(action) && !self.asleep {
                    self.function_key(p.code);
                }
                continue;
            }
            if self.running.is_none() && !self.asleep {
                self.on_key(p);
            }
        }
        if let Some(r) = self.running.as_mut()
            && let Some(e) = r.check(now)
        {
            self.app_ended(e);
        }
        self.update_hold();
    }

    fn pump_pty(&mut self) {
        let trace = self.opts.trace;
        let Some(s) = self.session.as_mut() else {
            return;
        };
        if s.exited {
            return;
        }
        let Some(pty) = s.pty.as_mut() else { return };
        let mut buf = Vec::new();
        let gone = match pty.read(&mut buf) {
            Ok(Some(_)) => false,
            Ok(None) => true,
            Err(e) => {
                eprintln!("lucina-shell: terminal read: {e}");
                true
            }
        };
        if !buf.is_empty() {
            s.term.feed(&buf);
            if s.term.screen.view_back() > 0 {
                self.full = true; // The title strip counts the rows back.
            }
            let replies = std::mem::take(&mut s.term.screen.replies);
            if !replies.is_empty() {
                pty.write(&replies);
            }
            if trace && let Some(lines) = s.term.screen.trace.as_mut() {
                for l in lines.drain(..) {
                    eprintln!("lucina-shell: term| {l}");
                }
            }
        }
        if gone || pty.exited() {
            s.exited = true;
            s.term
                .feed(b"\r\n[the shell exited: press Enter for a new one]");
            eprintln!("lucina-shell: shell exited");
            if self.view == View::Terminal {
                self.full = true;
            }
        }
    }

    fn render(&mut self) {
        // The first frame goes to the screen by the CPU: moving the frame into a GPU buffer first costs more than it saves once.
        if self.drawn_view.is_some()
            && let Some(fb) = self.fb.as_mut()
        {
            fb.prepare(&mut self.frame);
        }
        let started = monotonic_us();
        let mut dirty = Rect::default();
        let mut rects: Vec<Rect> = Vec::new();
        let dash_on = self.on_dash();
        if let Some(fb) = self.fb.as_mut() {
            fb.set_vsync(dash_on);
        }
        if dash_on {
            let now = Instant::now();
            if self.dash.sliding() {
                let t = Instant::now();
                self.dash.draw_slide(&mut self.frame, now);
                if self.opts.trace {
                    self.log(&format!(
                        "slide frame {:.1} ms",
                        t.elapsed().as_secs_f64() * 1000.0
                    ));
                }
                rects.push(self.frame.bounds());
                self.full = false;
                self.damage.clear();
            } else {
                if std::mem::take(&mut self.full) {
                    self.dash.home.invalidate();
                }
                self.damage.clear();
                self.bar_dirty = false;
                if self.osd.is_some() && self.osd_glass.moving(now).is_some() {
                    self.dash_damage(self.osd_glass.rect(self.frame.w, self.frame.h));
                }
                rects = self.dash.draw(&mut self.frame, now);
                if self.osd.is_some() {
                    let osd = self.osd_glass.rect(self.frame.w, self.frame.h);
                    if rects.iter().any(|r| !r.intersect(&osd).is_empty()) {
                        let r = self.osd_glass.draw(&mut self.frame, now);
                        rects.push(r);
                    }
                }
                if self.opts.trace
                    && !rects.is_empty()
                    && let Some((_, ms, px, copy)) = self.dash.last_draw
                {
                    self.log(&format!(
                        "dash frame {ms:.1} ms, {px} pixels, {} rects, copy {copy:.1} ms",
                        rects.len()
                    ));
                }
            }
        } else {
            self.render_views(&mut rects, &mut dirty);
        }
        self.present_frame(started, rects, dirty);
    }

    /// Draw the shell's own views (not the dashboard): all of it, or the damaged parts.
    fn render_views(&mut self, rects: &mut Vec<Rect>, dirty: &mut Rect) {
        let note = self.session.as_ref().and_then(|s| {
            if s.term.screen.view_back() > 0 {
                Some(format!(
                    "{} rows back: any key returns",
                    s.term.screen.view_back()
                ))
            } else {
                s.exited.then(|| "exited".to_string())
            }
        });
        if self.full {
            let term = self.session.as_ref().map(|s| &s.term.screen);
            ui::draw_all(
                &mut self.frame,
                &mut self.text,
                &self.layout,
                self.view,
                &self.status,
                &self.model,
                &self.home,
                self.ended.as_ref(),
                &self.settings,
                self.settings_sel,
                self.settings_row,
                term,
                note.as_deref(),
            );
            if let Some(s) = self.session.as_mut() {
                s.term.screen.take_dirty();
            }
            if let Some((action, sel)) = self.confirm {
                ui::draw_confirm(&mut self.frame, &mut self.text, &self.layout, action, sel);
            }
            if self.osd.is_some() {
                self.osd_glass.draw(&mut self.frame, Instant::now());
            }
            self.full = false;
            self.bar_dirty = false;
            self.damage.clear();
            rects.push(self.frame.bounds());
        } else {
            let term = self.session.as_ref().map(|s| &s.term.screen);
            for r in std::mem::take(&mut self.damage) {
                // The same drawing as a whole redraw, kept inside `r`.
                self.frame.set_clip(r);
                ui::draw_all(
                    &mut self.frame,
                    &mut self.text,
                    &self.layout,
                    self.view,
                    &self.status,
                    &self.model,
                    &self.home,
                    self.ended.as_ref(),
                    &self.settings,
                    self.settings_sel,
                    self.settings_row,
                    term,
                    note.as_deref(),
                );
                if let Some((action, sel)) = self.confirm {
                    ui::draw_confirm(&mut self.frame, &mut self.text, &self.layout, action, sel);
                }
                if self.osd.is_some() {
                    self.osd_glass.draw(&mut self.frame, Instant::now());
                }
                rects.push(r);
            }
            self.frame.set_clip(self.frame.bounds());
            if self.bar_dirty {
                *dirty = dirty.union(&ui::draw_bar(
                    &mut self.frame,
                    &mut self.text,
                    &self.layout,
                    self.view,
                    &self.status,
                ));
                self.bar_dirty = false;
            }
            if self.view == View::Terminal
                && let Some(s) = self.session.as_mut()
            {
                let rows = s.term.screen.take_dirty();
                if !rows.is_empty() {
                    let a = ui::draw_term_rows(
                        &mut self.frame,
                        &mut self.text,
                        &self.layout,
                        &s.term.screen,
                        &rows,
                        true,
                    );
                    *dirty = dirty.union(&a);
                }
            }
        }
    }

    /// Show what was drawn: the rectangles, the pointer over them, and the trace's latency lines.
    fn present_frame(&mut self, started: i64, mut rects: Vec<Rect>, dirty: Rect) {
        let composed = monotonic_us();
        if !dirty.is_empty() {
            rects.push(dirty);
        }
        let drew = !rects.is_empty();
        let drawn_px: i64 = rects.iter().map(|r| r.w as i64 * r.h as i64).sum();
        if let Some(fb) = self.fb.as_mut() {
            fb.present_all(&self.frame, &rects);
            // The pointer goes straight to the screen over the frame; where it was, the frame is
            // copied back.
            let n = if self.layout.k >= 1.25 { 2 } else { 1 };
            let (pw, ph) = gfx::pointer_size(n);
            let at = Rect::new(self.pointer.x, self.pointer.y, pw, ph);
            let overlaps = self
                .pointer_shown
                .is_some_and(|p| rects.iter().any(|r| !r.intersect(&p).is_empty()));
            if self.pointer.visible && (self.pointer_moved || overlaps) {
                if let Some(old) = self.pointer_shown.take() {
                    fb.present(&self.frame, old);
                }
                let img = gfx::pointer_image(&self.frame, at.x, at.y, n, ui::NAVY);
                fb.present_image(at.x, at.y, pw as usize, &img);
                self.pointer_shown = Some(at);
            }
        }
        if self.opts.trace {
            let now = monotonic_us();
            if self.pointer_moved {
                let waited = self.pointer_waiting.take().map_or(0, |t| now - t);
                self.log(&format!(
                    "pointer {},{} latency {} us",
                    self.pointer.x, self.pointer.y, waited
                ));
            }
            // What the input changed is on the screen once something was drawn. In the terminal that may be a little
            // later than the key, when the program's echo comes back through the pty.
            if drew && let Some((what, t)) = self.input_waiting.take() {
                self.log(&format!(
                    "latency {what}: {} us to the screen (compose {} us, copy {} us, {} rects, {} by the GPU, {drawn_px} pixels)",
                    now - t,
                    composed - started,
                    now - composed,
                    rects.len(),
                    self.fb.as_ref().map_or(0, |fb| fb.last_gpu_rects),
                ));
            }
        }
        self.pointer_moved = false;
        if self.drawn_view != Some(self.view) {
            self.drawn_view = Some(self.view);
            self.log(&format!("view {} drawn", self.view.name()));
        }
    }

    fn run(&mut self) -> ! {
        // A shell that starts while the screen is off would otherwise draw for nobody.
        let _ = fb::blank(false);
        let mut next_clock =
            Instant::now() + Duration::from_secs(system::until_next_minute(&self.zone));
        // The first frame comes before anything is read: the status follows at once. The dashboard has what needs no disk or
        // network: the actions, the volume, the clock if set.
        self.compose_dash();
        start_mark("compose");
        self.render();
        start_mark("drawn");
        let first_frame = format!(
            "first frame {} ms after the kernel started",
            monotonic_us() / 1000
        );
        self.log(&first_frame);
        let marks = start_marks_line();
        self.log(&marks);
        self.after_first_frame();
        match self.dash.backdrop_ms {
            Some(ms) => self.log(&format!("home: backdrop ready in {ms:.0} ms")),
            None => self.log("home: no backdrop to read: it is being made"),
        }
        // In the kernel log too, where a boot's time can be read over the network. One write is one record.
        let _ = std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/kmsg")
            .and_then(|mut kmsg| {
                std::io::Write::write_all(
                    &mut kmsg,
                    format!(
                        "lucina-shell: {first_frame}
"
                    )
                    .as_bytes(),
                )
            });
        // Everything that can wait starts now: /data, the services, the modem (boot/remote/init.sh, patch 0842).
        if std::path::Path::new("/run/lucina").is_dir() {
            let _ = std::fs::write("/run/lucina/first-frame", b"");
            // A shell that ended in standby left Wi-Fi down: it comes back as at a wake.
            if let Some(done) = standby::leave_if_left_behind() {
                self.log(&format!("standby mark left behind: {done}"));
            }
            match control::Control::bind(control::SOCKET) {
                Ok(c) => self.control = Some(c),
                Err(e) => self.log(&format!("{}: {e}", control::SOCKET)),
            }
            match lucina_ax::Server::bind_at(std::path::Path::new(ax::SOCKET)) {
                Ok(s) => {
                    self.log(&format!("ax: listening on {}", ax::SOCKET));
                    self.ax = Some(s);
                }
                Err(e) => self.log(&format!("ax: cannot listen on {}: {e}", ax::SOCKET)),
            }
        }
        // The dashboard's sources: the calendar, Lucina, the event hub (src/home_feed.rs). Each card comes as its source answers.
        self.dash.start_feed();
        // Apps in the image or on the root file system are the whole set: the EFI partition is not needed.
        if self.apps.is_empty() {
            self.start_disk_apps();
        }
        // The big apps on the persistent partition, when it has come up.
        self.start_data_apps();
        let mut next_status = Instant::now();
        let mut next_camera = Instant::now();
        // The plug's events from the hub, so the battery indicator follows the cable at once.
        let mut power = power_watch::PowerWatch::start();
        // Input devices by event: the kernel's input.added and input.removed through the hub (patch 1192).
        let mut input_watch = input_watch::InputWatch::start();
        self.handle_input(next_status, input_watch.as_mut());
        loop {
            // The level of a function key goes after its moment: the loop's deadline wakes it for that (below).
            if self.osd.as_ref().is_some_and(|o| Instant::now() >= o.until) {
                self.osd = None;
                self.full = true;
            }
            // While an app has the screen, the shell draws nothing.
            if self.running.is_none() && !self.asleep {
                self.render();
                if let Some(at_us) = self.wake_key_at.take() {
                    kmsg(&format!(
                        "lucina-shell: wake: first frame {} us after the power key",
                        monotonic_us() - at_us
                    ));
                }
            }
            let now = Instant::now();
            // Asleep, only input and what it runs can wake the shell: no clock, no status, no looking for new devices.
            let mut deadline = if self.asleep {
                now + ASLEEP_RECHECK
            } else {
                let d = next_clock.min(next_status).min(next_camera);
                // With the kernel's input events, no timer after the first seconds: a new device comes as an event.
                match self.inputs.next_rescan(now) {
                    Some(r) => d.min(r),
                    None => d,
                }
            };
            if let Some(r) = self.running.as_ref() {
                deadline = deadline.min(r.next_check(now));
            }
            if let Some(r) = self.kb.next_repeat() {
                deadline = deadline.min(r);
            }
            if let Some(o) = self.osd.as_ref() {
                deadline = deadline.min(o.until);
            }
            if let Some(idle) = self.idle_deadline(now) {
                deadline = deadline.min(idle);
            }
            if let Some(t) = self.control.as_ref().and_then(|c| c.next_deadline()) {
                deadline = deadline.min(t);
            }
            // The dashboard's next frame while something moves on it; none while it rests.
            if !self.asleep && self.on_dash() {
                if let Some(t) = self.dash.next_frame(now, self.frame_period) {
                    deadline = deadline.min(t);
                }
                if self.osd.is_some()
                    && let Some(t) = self.osd_glass.moving(now)
                {
                    deadline = deadline.min(t);
                }
            } else if self.osd.is_some()
                && let Some(t) = self.osd_glass.moving(now)
            {
                deadline = deadline.min(t);
                self.full = true;
            }
            let timeout = deadline
                .saturating_duration_since(now)
                .as_millis()
                .min(i32::MAX as u128) as i32;
            let mut fds: Vec<libc::pollfd> = self
                .inputs
                .fds()
                .into_iter()
                .map(|fd| libc::pollfd {
                    fd,
                    events: libc::POLLIN,
                    revents: 0,
                })
                .collect();
            if let Some(pty) = self
                .session
                .as_ref()
                .filter(|s| !s.exited)
                .and_then(|s| s.pty.as_ref())
            {
                fds.push(libc::pollfd {
                    fd: pty.fd(),
                    events: libc::POLLIN,
                    revents: 0,
                });
            }
            if let Some((_, r)) = self.disk_apps.as_ref() {
                use std::os::fd::AsRawFd;
                fds.push(libc::pollfd {
                    fd: r.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                });
            }
            if let Some((_, r)) = self.data_apps.as_ref() {
                use std::os::fd::AsRawFd;
                fds.push(libc::pollfd {
                    fd: r.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                });
            }
            if let Some(r) = self.running.as_ref() {
                fds.extend(r.fds().into_iter().map(|fd| libc::pollfd {
                    fd,
                    events: libc::POLLIN,
                    revents: 0,
                }));
            }
            if let Some(c) = self.control.as_ref() {
                fds.extend(c.fds().into_iter().map(|fd| libc::pollfd {
                    fd,
                    events: libc::POLLIN,
                    revents: 0,
                }));
            }
            if let Some(s) = self.ax.as_ref() {
                fds.push(libc::pollfd {
                    fd: s.fd(),
                    events: libc::POLLIN,
                    revents: 0,
                });
            }
            if let Some(p) = power.as_ref() {
                fds.push(libc::pollfd {
                    fd: p.fd(),
                    events: libc::POLLIN,
                    revents: 0,
                });
            }
            if let Some(f) = self.dash.feed.as_ref() {
                fds.push(libc::pollfd {
                    fd: f.fd(),
                    events: libc::POLLIN,
                    revents: 0,
                });
            }
            if let Some(w) = input_watch.as_ref() {
                fds.push(libc::pollfd {
                    fd: w.fd(),
                    events: libc::POLLIN,
                    revents: 0,
                });
            }
            // SAFETY: `fds` is a valid array of pollfd for the duration of the call.
            unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout.max(1)) };
            let now = Instant::now();
            self.handle_input(now, input_watch.as_mut());
            self.pump_control(now);
            if power.as_mut().is_some_and(|p| p.drain()) {
                next_status = now;
            }
            if let Some(mut s) = self.ax.take() {
                s.pump(|r| ax::answer(self, r));
                self.ax = Some(s);
            }
            self.take_disk_apps();
            self.take_data_apps();
            if self.dash.take_feed() | self.dash.take_backdrop() {
                self.compose_dash();
                if self.opts.trace {
                    self.log(&format!(
                        "home data at {} ms after the kernel started",
                        monotonic_us() / 1000
                    ));
                }
            }
            self.pump_pty();
            self.check_idle();
            // The camera mark alone, every 500 ms: on a change the bar is redrawn and one line is
            // logged, so the owner sees who captures without waiting for the rest of the status.
            if !self.asleep && now >= next_camera {
                next_camera = now + CAMERA_EVERY;
                let camera = lucina_app::camera_mark::read().map(|(_, program)| program);
                if camera != self.status.camera {
                    self.status.camera = camera.clone();
                    self.bar_dirty = true;
                    match &camera {
                        Some(program) => self.log(&format!("camera on: {program}")),
                        None => self.log("camera off"),
                    }
                }
            }
            let stale = std::mem::take(&mut self.status_stale);
            if !self.asleep && (stale || now >= next_clock || now >= next_status) {
                let started = monotonic_us();
                self.network = net::Network::read();
                if self.opts.trace {
                    self.log(&format!("status read in {} us", monotonic_us() - started));
                }
                if let Some(b) = self.backlight.as_mut() {
                    // A level saved on /data comes back once /data has come up.
                    b.sync();
                    b.read_adapter();
                }
                self.refresh_screen_rows();
                let st = system::status(&self.zone, &self.network);
                if st != self.status {
                    self.status = st;
                    self.bar_dirty = true;
                    self.update_tiles();
                    self.compose_dash();
                }
                if self.view == View::Settings {
                    let page = self.build_settings();
                    if page.sections != self.settings.sections {
                        self.settings = page;
                        self.damage.push(self.layout.panel);
                    }
                }
                next_clock =
                    now + Duration::from_secs(system::until_next_minute(&self.zone).max(1));
                next_status = now + STATUS_EVERY;
                if self.home.note.is_none()
                    && let Some(n) = system_notice()
                {
                    events::notice(&n);
                    self.home.note = Some(n);
                    self.full = true;
                }
            }
        }
    }
}

/// `--render`: the three views of a `w` x `h` screen as PPM files, without a device.
fn render_previews(dir: &str, w: u32, h: u32) -> std::io::Result<()> {
    let display = Display {
        width: w,
        height: h,
        format: "XRGB8888".into(),
        driver: "preview".into(),
    };
    let opts = Options {
        fb: String::new(),
        trace: false,
        log: None,
        shell: Vec::new(),
        render: None,
        size: (w, h),
    };
    let mut app = App::new(opts, None, w, h, display);
    app.status = Status {
        clock: "14:05 CET".into(),
        battery: Some(system::Battery {
            percent: 83,
            status: "Charging".into(),
            details: Vec::new(),
        }),
        network: "eth0 192.168.1.47".into(),
        online: true,
        camera: None,
    };
    app.model = "Samsung Galaxy Book Go".into();
    std::fs::create_dir_all(dir)?;
    let mut term = Terminal::new(app.layout.cols, app.layout.rows);
    term.feed(
        b"\x1b[1;33mJuno\x1b[0m on LucinaJuno\r\n\r\n~ # uname -a\r\nLinux 5.13.0-LucinaJuno aarch64\r\n\
          ~ # ls /\r\n\x1b[1;34mbin\x1b[0m  \x1b[1;34mdev\x1b[0m  \x1b[1;34metc\x1b[0m  \x1b[1;34mproc\x1b[0m  \x1b[1;34msys\x1b[0m  \x1b[1;34musr\x1b[0m\r\n\
          ~ # echo LUCINA-TYPED-$((6*7))\r\nLUCINA-TYPED-42\r\n~ # ",
    );
    // A full page: forty apps, ten of them craft apps, and one with only a letter.
    app.apps = [
        ("artcraft", "Art Craft"),
        ("woodcraft", "Wood Craft"),
        ("stonecraft", "Stone Craft"),
        ("metalcraft", "Metal Craft"),
        ("glasscraft", "Glass Craft"),
        ("papercraft", "Paper Craft"),
        ("foodcraft", "Food Craft"),
        ("soundcraft", "Sound Craft"),
        ("lightcraft", "Light Craft"),
        ("gamecraft", "Game Craft"),
        ("camera", "Camera"),
        ("recorder", "Recorder"),
        ("gallery", "Gallery"),
        ("viewer", "Viewer"),
        ("video", "Video"),
        ("player", "Player"),
        ("library", "Library"),
        ("gameboy", "Game Boy"),
        ("agents", "Agents"),
        ("server-health", "Server Health"),
        ("secrets", "Secrets"),
        ("reliability", "Reliability"),
        ("vault", "Vault"),
        ("desk", "Desk"),
        ("codecor", "Codecor"),
        ("chat", "Lucina"),
        ("calendar", "Calendar"),
        ("tasks", "Tasks"),
        ("notes", "Notes"),
        ("decisions", "Decisions"),
        ("drive", "Drive"),
        ("office", "Office"),
        ("notebooks", "Notebooks"),
        ("net", "Net"),
        ("discord", "Discord"),
        ("shopping", "Shopping"),
        ("hello", "Hello"),
        ("maps", "Maps"),
        ("mail", "Mail"),
        ("music", "Music"),
    ]
    .iter()
    .map(|(id, title)| {
        let icon = if *id == "hello" { "H" } else { id };
        Manifest::parse(&format!(
            "id = {id}\ntitle = {title}\nicon = {icon}\nnetwork = true"
        ))
        .unwrap()
    })
    .collect();
    app.home.tiles = home_tiles(&app.apps, &app.layout);
    app.home.sel = 0;
    app.home.scroll = 0;
    app.home.reflow(&app.layout);
    let ended = Ended {
        app: 0,
        title: "Hello".into(),
        what: "It stopped because of an abort, usually a panic (SIGABRT).".into(),
        tail: vec![
            "thread 'main' panicked at apps/lucina-app-hello/src/main.rs:154:25:".into(),
            "deliberate panic from the Crash button".into(),
        ],
        failed: true,
    };
    for (name, view) in [
        ("home", View::Home),
        ("settings", View::Settings),
        ("terminal", View::Terminal),
        ("ended", View::Ended),
    ] {
        app.view = view;
        app.full = true;
        let screen = (view == View::Terminal).then_some(&term.screen);
        ui::draw_all(
            &mut app.frame,
            &mut app.text,
            &app.layout,
            view,
            &app.status,
            &app.model,
            &app.home,
            Some(&ended),
            &app.settings,
            app.settings_sel,
            app.settings_row,
            screen,
            None,
        );
        std::fs::write(format!("{dir}/{name}.ppm"), app.frame.to_ppm())?;
    }
    // Home with the level a volume key shows (17 of 20).
    app.view = View::Home;
    ui::draw_all(
        &mut app.frame,
        &mut app.text,
        &app.layout,
        View::Home,
        &app.status,
        &app.model,
        &app.home,
        Some(&ended),
        &app.settings,
        app.settings_sel,
        app.settings_row,
        None,
        None,
    );
    app.osd_glass
        .show(Instant::now() - Duration::from_secs(2), 17, 20);
    app.osd_glass.draw(&mut app.frame, Instant::now());
    std::fs::write(format!("{dir}/volume.ppm"), app.frame.to_ppm())?;
    // The dashboard as the shell has it with no source answering yet: the actions of the installed apps, the clock, the battery.
    app.dash.page = dash::Page::Dash;
    app.compose_dash();
    app.dash.home.invalidate();
    // Long after the entrance, with the sky made as after a first frame.
    app.dash.finish_backdrop();
    let _ = app
        .dash
        .draw(&mut app.frame, Instant::now() + Duration::from_secs(5));
    std::fs::write(format!("{dir}/dashboard.ppm"), app.frame.to_ppm())?;
    Ok(())
}

/// `--log`: from here on stderr is this file.
fn log_to(path: &str) {
    use std::os::fd::AsRawFd;
    // Appending, so that emptying the file from outside does not leave a hole.
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        // SAFETY: both descriptors are open; dup2 replaces stderr.
        Ok(f) => unsafe {
            libc::dup2(f.as_raw_fd(), 2);
        },
        Err(e) => eprintln!("lucina-shell: {path}: {e}"),
    }
}

/// Switch events (EV_SW) and the lid's switch (SW_LID, 1 = closed), Linux's numbers.
const EV_SW: u16 = 0x05;
const SW_LID: u16 = 0x00;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LidAction {
    Sleep,
    Wake,
}

/// What the lid does: closing sleeps an awake machine, opening wakes a sleeping one, anything else (the starting state at boot,
/// a repeat) does nothing.
fn lid_action(closed: bool, asleep: bool) -> Option<LidAction> {
    match (closed, asleep) {
        (true, false) => Some(LidAction::Sleep),
        (false, true) => Some(LidAction::Wake),
        _ => None,
    }
}

fn main() {
    let opts = parse_args();
    if let Some(path) = &opts.log {
        log_to(path);
    }
    eprintln!(
        "lucina-shell: started {} ms after the kernel",
        monotonic_us() / 1000
    );
    events::init();
    if let Some(dir) = opts.render.clone() {
        if let Err(e) = render_previews(&dir, opts.size.0, opts.size.1) {
            eprintln!("lucina-shell: {e}");
            std::process::exit(1);
        }
        return;
    }
    let fb = match fb::Fb::open(&opts.fb) {
        Ok(fb) => fb,
        Err(e) => {
            eprintln!("lucina-shell: no screen: {}: {e}", opts.fb);
            std::process::exit(1);
        }
    };
    let display = Display {
        width: fb.width,
        height: fb.height,
        format: fb.layout.describe(),
        driver: format!(
            "{} ({})",
            opts.fb,
            if fb.name.is_empty() {
                "framebuffer"
            } else {
                &fb.name
            }
        ),
    };
    eprintln!(
        "lucina-shell: screen {}x{} {} on {}",
        fb.width, fb.height, display.format, display.driver
    );
    let _graphics = match fb::GraphicsMode::enter() {
        Ok(g) => Some(g),
        Err(e) => {
            eprintln!("lucina-shell: the text console stays on (no KD_GRAPHICS): {e}");
            None
        }
    };
    let (w, h) = (fb.width, fb.height);
    eprintln!(
        "lucina-shell: screen open {} ms after the kernel",
        monotonic_us() / 1000
    );
    let mut app = App::new(opts, Some(fb), w, h, display);
    app.log(&format!(
        "ready to draw {} ms after the kernel",
        monotonic_us() / 1000
    ));
    app.log(&format!("time zone {}: {}", app.zone_name, app.zone.rule));
    if app.opts.trace {
        app.log(&format!("machine {}", app.model));
        for (i, t) in app.home.placed.iter().enumerate() {
            app.log(&format!("tile {i} at {},{}", t.x + t.w / 2, t.y + t.h / 2));
        }
        for (i, t) in app.home.tiles.iter().enumerate() {
            let id = match t.action {
                TileAction::App(a) => app.apps[a].id.as_str(),
                TileAction::Open(_) => t.title.as_str(),
            };
            app.log(&format!("tile-index {i} {id}"));
        }
    }
    app.run();
}

#[cfg(test)]
impl App {
    /// A shell for the tests: no devices, no screen, the given apps as the whole set.
    pub(crate) fn for_test(w: u32, h: u32, apps: Vec<Manifest>) -> App {
        let display = Display {
            width: w,
            height: h,
            format: "XRGB8888".into(),
            driver: "test".into(),
        };
        let opts = Options {
            fb: String::new(),
            trace: false,
            log: None,
            shell: Vec::new(),
            render: None,
            size: (w, h),
        };
        let mut app = App::new(opts, None, w, h, display);
        app.apps = apps;
        app.home.tiles = home_tiles(&app.apps, &app.layout);
        app.home.reflow(&app.layout);
        app
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn previews_render_at_common_sizes() {
        let dir = std::env::temp_dir().join(format!("lucina-shell-test-{}", std::process::id()));
        for (w, h) in [(800, 600), (1920, 1080)] {
            let d = dir.join(format!("{w}x{h}"));
            render_previews(d.to_str().unwrap(), w, h).unwrap();
            for v in ["home", "settings", "terminal", "ended"] {
                let ppm = std::fs::read(d.join(format!("{v}.ppm"))).unwrap();
                let header = format!("P6\n{w} {h}\n255\n");
                assert!(ppm.starts_with(header.as_bytes()));
                assert_eq!(ppm.len(), header.len() + (w * h * 3) as usize);
                // The page's ground and some accent are on every screen.
                let px: Vec<&[u8]> = ppm[header.len()..].chunks(3).collect();
                let ground = [
                    (ui::NAVY.0 >> 16) as u8,
                    (ui::NAVY.0 >> 8) as u8,
                    ui::NAVY.0 as u8,
                ];
                let accent = [
                    (ui::GOLD.0 >> 16) as u8,
                    (ui::GOLD.0 >> 8) as u8,
                    ui::GOLD.0 as u8,
                ];
                assert!(px.iter().any(|p| *p == ground), "{v}: no ground");
                assert!(px.iter().any(|p| *p == accent), "{v}: no accent");
            }
        }
        std::fs::remove_dir_all(dir).ok();
    }
}

#[cfg(test)]
mod lid_tests {
    use super::*;

    #[test]
    fn closing_the_lid_sleeps_and_opening_it_wakes() {
        assert_eq!(lid_action(true, false), Some(LidAction::Sleep));
        assert_eq!(lid_action(false, true), Some(LidAction::Wake));
        // The starting state at boot, and repeats, do nothing.
        assert_eq!(lid_action(false, false), None);
        assert_eq!(lid_action(true, true), None);
    }
}

#[cfg(test)]
mod view_key_tests {
    use super::*;
    use lucina_app::keys::Mods;

    fn press(code: u16, ctrl: bool, shift: bool) -> Press {
        Press {
            code,
            mods: Mods {
                ctrl,
                shift,
                ..Mods::default()
            },
        }
    }

    /// Ctrl+Tab goes round Home, Terminal, Settings; Ctrl+Shift+Tab goes back (the owner, 2026-11-08: the view shortcuts off
    /// the bare F-row, whose keys give their hot keys first on this laptop).
    #[test]
    fn ctrl_tab_goes_round_the_views() {
        let mut app = App::for_test(1920, 1080, Vec::new());
        assert!(app.view == View::Home);
        app.on_key(press(keys::KEY_TAB, true, false));
        assert!(app.view == View::Terminal);
        app.on_key(press(keys::KEY_TAB, true, false));
        assert!(app.view == View::Settings);
        app.on_key(press(keys::KEY_TAB, true, false));
        assert!(app.view == View::Home);
        app.on_key(press(keys::KEY_TAB, true, true));
        assert!(app.view == View::Settings);
    }

    /// The Settings hot key (KEY_CONFIG, the plain F1 of this laptop) opens Settings.
    #[test]
    fn the_settings_key_opens_settings() {
        let mut app = App::for_test(1920, 1080, Vec::new());
        app.on_key(press(fnkeys::KEY_CONFIG, false, false));
        assert!(app.view == View::Settings);
    }
}
