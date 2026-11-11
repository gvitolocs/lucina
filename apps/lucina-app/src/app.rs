//! The runtime: the loop every app runs in. It owns the screen and the input devices while the app
//! is on screen, turns evdev into [`Input`], and calls the app's `view`.
//!
//! One thread and `poll`: idle, an app costs nothing. Other threads talk to it through a [`Sender`],
//! whose messages arrive in [`App::message`] on the main thread and redraw afterwards.
//!
//! Under `lucina-shell` (which sets `LUCINA_SHELL`) the shell keeps the VT in graphics mode and owns
//! F1: it asks the app to end with SIGTERM, so [`App::on_quit`] has about half a second to save.
//! Started by hand, on a console, an app sets the graphics mode itself and F1 ends it.

use std::collections::VecDeque;
use std::os::fd::RawFd;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::dirty::Tiles;
use crate::fb::{Fb, GraphicsMode, Surface};
use crate::gfx::Recording;
use crate::gfx::{self, Frame, Rect, Text};
use crate::input::{self, EV_ABS, EV_KEY, EV_REL, EV_SYN, Inputs, Pointer};
use crate::input_watch;
use crate::keys::{self, Keyboard};
use crate::screen::Screen;
use crate::ui::{AxPass, Input, Theme, Ui, UiState};
use lucina_ax::{Builder, Refused, Request, Snapshot};

/// An app: its state, and how that state looks.
pub trait App {
    /// What other threads send it. `()` when it has none.
    type Msg: Send + 'static;

    /// Lay out and draw everything from the state, and react to the widgets. See [`crate::ui`].
    fn view(&mut self, ui: &mut Ui);

    /// A message from a [`Sender`], on the main thread. The screen is redrawn afterwards.
    fn message(&mut self, _msg: Self::Msg) {}

    /// The app is about to end because the user went home. Save what must be saved, quickly.
    fn on_quit(&mut self) {}

    /// The whole screen for something that cannot go through `view`, such as video: asked after each round of the loop. The closure
    /// has the screen and the input devices until it returns; then the app draws again, from nothing. It must return soon after
    /// [`FullScreen::terminating`].
    #[allow(clippy::type_complexity)]
    fn take_screen(&mut self) -> Option<Box<dyn FnOnce(&mut FullScreen)>> {
        None
    }

    /// The intents this app offers (see [`crate::intent`]). Listed without starting the app, by `<program> --intents`.
    fn intents() -> &'static [crate::intent::IntentSpec]
    where
        Self: Sized,
    {
        &[]
    }

    /// One of [`App::intents`], with its arguments already checked against its schema. Answer through `reply`, now or later
    /// (keep it in the state and answer from [`App::message`]). Call the same functions the app's own buttons call.
    fn intent(&mut self, call: crate::intent::Call, reply: crate::intent::Responder) {
        let text = format!("{}: this app has no intents", call.name);
        reply.send(crate::intent::Reply::failed(text));
    }
}

/// What an app holds while it has the whole screen ([`App::take_screen`]).
pub struct FullScreen<'a> {
    pub screen: &'a mut Screen,
    pub inputs: &'a mut Inputs,
}

impl FullScreen<'_> {
    /// The user went home: time to return.
    pub fn terminating(&self) -> bool {
        TERMINATE.load(Ordering::SeqCst)
    }
}

struct Shared<M> {
    queue: Mutex<VecDeque<M>>,
    wake_fd: RawFd,
}

/// Sends messages to the app from any thread; each one wakes the main loop. Clone freely.
pub struct Sender<M> {
    shared: Arc<Shared<M>>,
}

impl<M> Clone for Sender<M> {
    fn clone(&self) -> Self {
        Sender {
            shared: self.shared.clone(),
        }
    }
}

fn wake(fd: RawFd) {
    if fd >= 0 {
        let b = 1u8;
        // SAFETY: writes one byte from a live local. The pipe is non-blocking: when it is full the
        // main loop is awake already, so failing is fine.
        unsafe { libc::write(fd, (&b as *const u8).cast(), 1) };
    }
}

pub(crate) fn wake_fd(fd: RawFd) {
    wake(fd);
}

/// The write end of the main loop's wake pipe, for a thread outside the loop that has to wake it
/// (the camera watcher in [`crate::camera_mark`]).
pub(crate) fn signal_fd() -> RawFd {
    SIGNAL_FD.load(Ordering::SeqCst)
}

impl<M: Send + 'static> Sender<M> {
    pub fn send(&self, msg: M) {
        if let Ok(mut q) = self.shared.queue.lock() {
            q.push_back(msg);
        }
        wake(self.shared.wake_fd);
    }

    /// Run `f` on a thread of its own and send its result: network calls, decoding pictures, reading
    /// large files.
    pub fn spawn(&self, f: impl FnOnce() -> M + Send + 'static) {
        let s = self.clone();
        std::thread::spawn(move || s.send(f()));
    }
}

/// Something that comes from a thread: what a view shows at once, whatever the network does. Keep one in
/// the app's state, start the work with [`Sender::spawn`] and [`Load::start`], put the result in with
/// [`Load::finish`] from `message`. `view` matches on it, so it cannot forget the waiting state and
/// has no way to wait.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Load<T> {
    /// Nothing asked for yet.
    #[default]
    Idle,
    /// On its way. Holds what to show meanwhile: the last result or a cached copy.
    Loading(Option<T>),
    Ready(T),
    /// It failed. Holds the message, and what to go on showing.
    Failed(String, Option<T>),
}

impl<T> Load<T> {
    /// The work has started; what is known stays on screen.
    pub fn start(&mut self) {
        *self = match std::mem::take(self) {
            Load::Ready(v) | Load::Loading(Some(v)) | Load::Failed(_, Some(v)) => {
                Load::Loading(Some(v))
            }
            _ => Load::Loading(None),
        };
    }

    /// The work's result.
    pub fn finish(&mut self, r: Result<T, String>) {
        *self = match r {
            Ok(v) => Load::Ready(v),
            Err(e) => match std::mem::take(self) {
                Load::Ready(v) | Load::Loading(Some(v)) | Load::Failed(_, Some(v)) => {
                    Load::Failed(e, Some(v))
                }
                _ => Load::Failed(e, None),
            },
        };
    }

    /// What there is to show now, fresh or not.
    pub fn value(&self) -> Option<&T> {
        match self {
            Load::Ready(v) | Load::Loading(Some(v)) | Load::Failed(_, Some(v)) => Some(v),
            _ => None,
        }
    }

    pub fn is_loading(&self) -> bool {
        matches!(self, Load::Loading(_))
    }
}

/// A `view` longer than this froze the screen for a person to notice: the log says so.
const SLOW_VIEW: Duration = Duration::from_millis(50);

/// The app with its frame and text: all of the runtime that needs no device, so that tests can drive
/// an app, and look at what it draws, on the host ([`crate::Harness`]).
pub struct Core<A: App> {
    pub app: A,
    pub frame: Frame,
    pub text: Text,
    pub(crate) st: UiState,
    theme: Theme,
    tiles: Tiles,
    /// Kept between frames for its memory.
    recording: Option<Box<Recording>>,
    /// `LUCINA_APP_FULL_REDRAW`: draw everything and hash the pixels, as the shell does. For comparing.
    full_redraw: bool,
}

impl<A: App> Core<A> {
    pub fn new(app: A, w: u32, h: u32) -> Core<A> {
        let mut c = Core {
            app,
            frame: Frame::new(w, h),
            text: Text::new(),
            st: UiState::default(),
            theme: Theme::new(w, h),
            tiles: Tiles::default(),
            recording: Some(Box::default()),
            full_redraw: std::env::var_os("LUCINA_APP_FULL_REDRAW").is_some(),
        };
        c.st.want_paint = true;
        // Hover and focus settle with one pass before anything is drawn.
        c.pass(None, false);
        c
    }

    fn pass(&mut self, input: Option<Input>, painting: bool) -> bool {
        self.st.begin_pass();
        let mut ui = Ui::new(
            &mut self.frame,
            &mut self.text,
            &mut self.st,
            self.theme,
            input,
            painting,
        );
        let started = Instant::now();
        self.app.view(&mut ui);
        let took = started.elapsed();
        if took > SLOW_VIEW {
            eprintln!(
                "lucina-app: view took {} ms: slow work belongs on a thread (Sender::spawn)",
                took.as_millis()
            );
        }
        let left = ui.input_left();
        self.st.end_pass(left)
    }

    /// One event: `view` runs with drawing off, so that it reaches its widget.
    pub fn input(&mut self, i: Input) {
        match i {
            Input::Move(x, y) | Input::Down(x, y) | Input::Up(x, y) | Input::Secondary(x, y) => {
                self.st.pointer = (x, y)
            }
            _ => {}
        }
        // Anything but a plain pointer move shows on the screen.
        if !matches!(i, Input::Move(..)) {
            self.st.want_paint = true;
        }
        if self.pass(Some(i), false) {
            self.st.want_paint = true;
        }
    }

    /// One event, `injected` meaning that it came from Lucina's injection device: widgets inside [`Ui::owner_only`] ignore it.
    pub fn input_from(&mut self, i: Input, injected: bool) {
        self.st.injected = injected;
        self.input(i);
        self.st.injected = false;
    }

    /// One pass of `view` that records the tree and carries `pending` to its widget. Never draws.
    fn ax_pass(&mut self, app: &str, pending: Option<AxPass>) -> AxPass {
        let pass = pending.unwrap_or_else(|| AxPass {
            builder: Builder::new(app, self.frame.w as i32, self.frame.h as i32),
            pending: None,
            value: None,
            action: None,
            taken: false,
        });
        self.st.ax = Some(pass);
        if self.pass(None, false) {
            self.st.want_paint = true;
        }
        self.st.ax.take().expect("the pass keeps its recorder")
    }

    /// What is on screen now, as the tree Lucina reads. Costs one pass of `view` with drawing off.
    pub fn tree(&mut self, app: &str) -> Snapshot {
        self.ax_pass(app, None).builder.finish()
    }

    /// Lucina's action on the node `id`, against the tree of `version`: checked against the tree on screen now, carried to its
    /// widget in one pass, settled in another. The reply's tree is the one after it; a refusal carries the tree now.
    // The refusal is as large as a tree because it carries one: the socket sends it with the refusal.
    #[allow(clippy::result_large_err)]
    pub fn act(
        &mut self,
        app: &str,
        id: &str,
        action: &str,
        value: Option<&str>,
        version: &str,
    ) -> Result<Snapshot, (Refused, Snapshot)> {
        let now = self.tree(app);
        let planned = match now.plan(id, action, value, version) {
            Ok(p) => p,
            Err(r) => return Err((r, now)),
        };
        let pass = self.ax_pass(
            app,
            Some(AxPass {
                builder: Builder::new(app, self.frame.w as i32, self.frame.h as i32),
                pending: Some(planned.pending),
                value: planned.value,
                action: Some(planned.action),
                taken: false,
            }),
        );
        if pass.pending.is_some() || !pass.taken {
            return Err((
                Refused::Bad("the widget did not take the action".into()),
                self.tree(app),
            ));
        }
        // As after an input: the focus settles, and the screen shows what changed.
        self.st.want_paint = true;
        if self.pass(None, false) {
            self.st.want_paint = true;
        }
        Ok(self.tree(app))
    }

    /// A message from a [`Sender`].
    pub fn message(&mut self, m: A::Msg) {
        self.app.message(m);
        self.st.want_paint = true;
        if self.pass(None, false) {
            self.st.want_paint = true;
        }
    }

    /// Whether the app has asked to end.
    /// Whether a password field has the keyboard.
    pub fn typing_secret(&self) -> bool {
        self.st.typing_secret
    }

    pub fn quit_requested(&self) -> bool {
        self.st.quit
    }

    /// When the app wants to run again if nothing else happens, taking the request.
    fn take_wake(&mut self) -> Option<Instant> {
        self.st.wake
    }

    /// The screen is someone else's now: when it comes back, every pixel has to be sent again.
    fn screen_was_lost(&mut self) {
        self.tiles = Tiles::default();
        self.st.want_paint = true;
    }

    /// Whether something is waiting to be drawn.
    pub fn needs_paint(&self) -> bool {
        self.st.want_paint
    }

    /// Draw if anything changed; returns the rectangles to copy to the screen.
    pub fn paint(&mut self) -> Vec<Rect> {
        if !self.st.want_paint {
            return Vec::new();
        }
        self.st.want_paint = false;
        if !self.full_redraw {
            self.frame
                .start_recording(self.recording.take().unwrap_or_default());
        }
        // A pass can move the focus (its widget went away): draw until that has settled.
        for _ in 0..3 {
            self.frame.restart_recording();
            if !self.pass(None, true) {
                break;
            }
        }
        self.st.want_paint = false;
        self.frame.set_clip(self.frame.bounds());
        let Some(rec) = self.frame.take_recording() else {
            return self.tiles.diff(&self.frame);
        };
        let dirty = self
            .tiles
            .diff_ops(self.frame.w as usize, self.frame.h as usize, &rec);
        self.frame.replay(&rec, &mut self.text, &dirty);
        self.recording = Some(rec);
        dirty
    }
}

/// Drives an app without a device, in tests: send events, then look at the frame.
pub struct Harness<A: App> {
    pub core: Core<A>,
    pending: Option<Arc<Mutex<Option<crate::intent::Reply>>>>,
}

impl<A: App> Harness<A> {
    pub fn new(app: A, w: u32, h: u32) -> Harness<A> {
        let mut core = Core::new(app, w, h);
        core.paint();
        Harness {
            core,
            pending: None,
        }
    }

    pub fn app(&mut self) -> &mut A {
        &mut self.core.app
    }

    /// A key press with no modifiers, by evdev code.
    pub fn key(&mut self, code: u16) -> Vec<Rect> {
        self.press(code, keys::Mods::default())
    }

    pub fn press(&mut self, code: u16, mods: keys::Mods) -> Vec<Rect> {
        self.core.input(Input::Key(keys::Press { code, mods }));
        self.core.paint()
    }

    /// Type text on the US layout (what `char_for` can produce).
    pub fn type_str(&mut self, s: &str) {
        for c in s.chars() {
            let (code, shift) = keys::code_for(c).unwrap_or_else(|| panic!("cannot type {c:?}"));
            self.press(
                code,
                keys::Mods {
                    shift,
                    ..keys::Mods::default()
                },
            );
        }
    }

    pub fn click(&mut self, x: i32, y: i32) -> Vec<Rect> {
        self.core.input(Input::Move(x, y));
        self.core.input(Input::Down(x, y));
        self.core.input(Input::Up(x, y));
        self.core.paint()
    }

    /// A click from Lucina's injection device.
    pub fn click_injected(&mut self, x: i32, y: i32) -> Vec<Rect> {
        self.core.input_from(Input::Move(x, y), true);
        self.core.input_from(Input::Down(x, y), true);
        self.core.input_from(Input::Up(x, y), true);
        self.core.paint()
    }

    /// The tree Lucina would read now.
    pub fn tree(&mut self) -> Snapshot {
        self.core.tree("harness")
    }

    /// Lucina's action on node `id`, against the tree on screen now; then the frame is drawn.
    pub fn act(&mut self, id: u64, action: &str, value: Option<&str>) -> Result<Snapshot, Refused> {
        let version = self.core.tree("harness").version;
        let id = lucina_ax::tree::id_text(id);
        let out = self
            .core
            .act("harness", &id, action, value, &version)
            .map_err(|(r, _)| r);
        self.core.paint();
        out
    }

    /// [`act`](Self::act) on the first node with this role and name. Panics with the tree when there is none.
    pub fn act_on(
        &mut self,
        role: lucina_ax::accesskit::Role,
        name: &str,
        action: &str,
        value: Option<&str>,
    ) -> Result<Snapshot, Refused> {
        let tree = self.tree();
        let Some(id) = tree.find(role, name) else {
            panic!("no {role:?} {name:?} in the tree:\n{}", tree.text());
        };
        self.act(id, action, value)
    }

    pub fn right_click(&mut self, x: i32, y: i32) -> Vec<Rect> {
        self.core.input(Input::Move(x, y));
        self.core.input(Input::Secondary(x, y));
        self.core.paint()
    }

    pub fn wheel(&mut self, notches: i32) -> Vec<Rect> {
        self.core.input(Input::Wheel(notches));
        self.core.paint()
    }

    /// Smooth scroll, in screen pixels.
    pub fn scroll(&mut self, dx: i32, dy: i32) -> Vec<Rect> {
        self.core.input(Input::Scroll { dx, dy });
        self.core.paint()
    }

    /// Call one of the app's intents as lucina-devd would, then draw. The answer, if the app gave it at once; otherwise
    /// [`Harness::reply`] after the messages it waits for.
    pub fn intent(&mut self, name: &str, args: serde_json::Value) -> Option<crate::intent::Reply> {
        let call = crate::intent::Call {
            call: "test".into(),
            name: name.into(),
            args,
            undo: None,
        };
        self.intent_call(call)
    }

    /// [`Harness::intent`] with the whole call (an undo carries its entry).
    pub fn intent_call(&mut self, call: crate::intent::Call) -> Option<crate::intent::Reply> {
        let (responder, slot) = crate::intent::Responder::slot(&call.name);
        crate::intent::dispatch(&mut self.core.app, call, responder);
        self.core.paint();
        self.pending = Some(slot);
        self.reply()
    }

    /// The answer to the last [`Harness::intent`], once there is one.
    pub fn reply(&mut self) -> Option<crate::intent::Reply> {
        let slot = self.pending.as_ref()?;
        let r = slot.lock().ok()?.take();
        if r.is_some() {
            self.pending = None;
        }
        r
    }

    /// The frame as a binary PPM, to write out and look at.
    pub fn ppm(&self) -> Vec<u8> {
        self.core.frame.to_ppm()
    }
}

static TERMINATE: AtomicBool = AtomicBool::new(false);
static SIGNAL_FD: AtomicI32 = AtomicI32::new(-1);

extern "C" fn on_sigterm(_: libc::c_int) {
    TERMINATE.store(true, Ordering::SeqCst);
    wake(SIGNAL_FD.load(Ordering::SeqCst));
}

/// Resident memory in KiB, from /proc/self/status (for the trace).
fn rss_kib() -> i64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            let line = s.lines().find(|l| l.starts_with("VmRSS:"))?.to_string();
            line.split_whitespace().nth(1)?.parse().ok()
        })
        .unwrap_or(-1)
}

fn monotonic_us() -> i64 {
    let mut t = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: fills a live timespec.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut t) };
    t.tv_sec * 1_000_000 + t.tv_nsec / 1000
}

/// What one EV_REL wheel event means on a device: hi-res units to accumulate, or one [`Input`] to
/// send now. A hi-res device also sends whole notches for programs that only know them; reading
/// both would scroll twice, so on such a device the notches are ignored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WheelDelta {
    /// Hi-res units, (dx, dy): 1/120 of a notch each.
    Hires(i64, i64),
    Send(Input),
}

fn wheel_delta(hires: bool, code: u16, value: i32, notch_px: i32) -> Option<WheelDelta> {
    match code {
        input::REL_WHEEL | input::REL_HWHEEL if hires => None,
        input::REL_WHEEL => Some(WheelDelta::Send(Input::Wheel(value))),
        input::REL_HWHEEL => Some(WheelDelta::Send(Input::Scroll {
            dx: value * notch_px,
            dy: 0,
        })),
        input::REL_WHEEL_HI_RES => Some(WheelDelta::Hires(0, value as i64)),
        input::REL_HWHEEL_HI_RES => Some(WheelDelta::Hires(value as i64, 0)),
        _ => None,
    }
}

/// Hi-res units per logical pixel: a 120-unit notch is the 40 px a wheel notch scrolls a list. (GTK, the original for
/// what a touchpad scroll moves, takes libinput's units times its MAGIC_SCROLL_FACTOR 2.5, and the driver sends 8 hi-res
/// units per libinput unit: 120 units = 37.5 GTK pixels, the same within a few per cent.)
const HIRES_PER_PX: f32 = 3.0;

/// Whole screen pixels from hi-res wheel units at theme scale `k`, with the remainder in hi-res units, to carry to the
/// next batch so slow motion is not lost.
fn hires_pixels(units: i64, rem: i64, k: f32) -> (i32, i64) {
    let total = (units + rem) as f32 * k;
    let px = (total / HIRES_PER_PX).floor() as i32;
    let rem = ((total - px as f32 * HIRES_PER_PX) / k).round() as i64;
    (px, rem)
}

/// Run an app until it ends; never returns. `build` makes it, and gets the [`Sender`] for its
/// threads. `name` appears in its log lines. The process exits 0 when the app quits, and 1 when
/// there is no screen.
pub fn run<A: App>(name: &str, build: impl FnOnce(Sender<A::Msg>) -> A) -> ! {
    if std::env::args().nth(1).as_deref() == Some("--intents") {
        println!("{}", crate::intent::listing::<A>());
        std::process::exit(0);
    }
    let headless = std::env::var_os("LUCINA_INTENT").is_some()
        && std::env::var_os("LUCINA_INTENT_HEADLESS").is_some();
    let result = if headless {
        run_headless(name, build)
    } else {
        run_inner(name, build)
    };
    let code = match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{name}: {e}");
            1
        }
    };
    std::process::exit(code);
}

/// An intent that doesn't show the app: no screen, no input, only the app's messages, until it has answered.
fn run_headless<A: App>(
    name: &str,
    build: impl FnOnce(Sender<A::Msg>) -> A,
) -> std::io::Result<()> {
    let mut pipe = [-1; 2];
    // SAFETY: pipe2 fills the two-element array.
    #[cfg(not(target_os = "macos"))]
    if unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_NONBLOCK | libc::O_CLOEXEC) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // macOS has no pipe2: pipe, then the same two flags on both ends.
    #[cfg(target_os = "macos")]
    // SAFETY: pipe fills the two-element array; fcntl takes the descriptors it just made.
    unsafe {
        if libc::pipe(pipe.as_mut_ptr()) < 0 {
            return Err(std::io::Error::last_os_error());
        }
        for fd in pipe {
            libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK);
            libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
        }
    }
    let [wake_r, wake_w] = pipe;
    SIGNAL_FD.store(wake_w, Ordering::SeqCst);
    // SAFETY: the handler only touches atomics and write(2), which are async-signal-safe.
    unsafe {
        libc::signal(
            libc::SIGTERM,
            on_sigterm as extern "C" fn(libc::c_int) as libc::sighandler_t,
        )
    };
    let shared = Arc::new(Shared {
        queue: Mutex::new(VecDeque::new()),
        wake_fd: wake_w,
    });
    let mut app = build(Sender {
        shared: shared.clone(),
    });
    let slot = Arc::new(Mutex::new(None));
    let Some((call, reply)) = crate::intent::from_env(Some((slot.clone(), wake_w))) else {
        return Ok(());
    };
    let intent = call.name.clone();
    eprintln!("{name}: intent {intent} without the screen");
    crate::intent::dispatch(&mut app, call, reply);
    let deadline = Instant::now() + crate::intent::headless_timeout();
    let answer = loop {
        if let Some(r) = slot.lock().ok().and_then(|mut s| s.take()) {
            break r;
        }
        let now = Instant::now();
        if now >= deadline || TERMINATE.load(Ordering::SeqCst) {
            break crate::intent::Reply::failed(format!("{intent}: no answer in time"));
        }
        let mut fds = [libc::pollfd {
            fd: wake_r,
            events: libc::POLLIN,
            revents: 0,
        }];
        let ms = (deadline - now).as_millis().min(1000) as i32;
        // SAFETY: one live pollfd.
        unsafe { libc::poll(fds.as_mut_ptr(), 1, ms) };
        let mut buf = [0u8; 64];
        // SAFETY: reads into a live local buffer; the pipe is non-blocking.
        while unsafe { libc::read(wake_r, buf.as_mut_ptr().cast(), buf.len()) } > 0 {}
        let msgs: Vec<A::Msg> = shared
            .queue
            .lock()
            .map(|mut q| q.drain(..).collect())
            .unwrap_or_default();
        for m in msgs {
            app.message(m);
        }
    };
    app.on_quit();
    if let Some(path) = crate::intent::reply_path() {
        crate::intent::write_to(&path, &answer);
    }
    Ok(())
}

/// What one paint changed in the tree, as events: a node each for a few, one `view.changed` for a new view. Held-back values and
/// text are in no tree (stage B), so none can reach an event.
fn tree_events(changes: &[lucina_ax::tree::Change]) {
    const NODES_AT_MOST: usize = 12;
    if changes.is_empty() {
        return;
    }
    if changes.len() > NODES_AT_MOST {
        let count = |k: lucina_ax::tree::ChangeKind| changes.iter().filter(|c| c.kind == k).count();
        lucina_events::emit("view.changed")
            .field("added", count(lucina_ax::tree::ChangeKind::Added))
            .field("removed", count(lucina_ax::tree::ChangeKind::Removed))
            .field("changed", count(lucina_ax::tree::ChangeKind::Changed))
            .send();
        return;
    }
    for c in changes {
        let change = match c.kind {
            lucina_ax::tree::ChangeKind::Added => "added",
            lucina_ax::tree::ChangeKind::Removed => "removed",
            lucina_ax::tree::ChangeKind::Changed => "changed",
        };
        let role = serde_json::to_value(c)
            .ok()
            .and_then(|v| v["role"].as_str().map(str::to_owned))
            .unwrap_or_default();
        let mut e = lucina_events::emit("node.changed")
            .subject(&format!("node:{:x}", c.id))
            .field("change", change)
            .field("role", role);
        if let Some(n) = &c.name {
            e = e.field("name", n.as_str());
        }
        e.send();
    }
}

fn run_inner<A: App>(name: &str, build: impl FnOnce(Sender<A::Msg>) -> A) -> std::io::Result<()> {
    let trace = std::env::var_os("LUCINA_APP_TRACE").is_some();
    let under_shell = std::env::var_os("LUCINA_SHELL").is_some();
    let fb_path = std::env::var("LUCINA_FB").unwrap_or_else(|_| "/dev/fb0".into());
    let fb = Fb::open(&fb_path)
        .map_err(|e| std::io::Error::new(e.kind(), format!("no screen: {fb_path}: {e}")))?;
    let _graphics = if under_shell {
        None
    } else {
        GraphicsMode::enter().ok()
    };

    let mut pipe = [-1; 2];
    // SAFETY: pipe2 fills the two-element array.
    #[cfg(not(target_os = "macos"))]
    if unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_NONBLOCK | libc::O_CLOEXEC) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // macOS has no pipe2: pipe, then the same two flags on both ends.
    #[cfg(target_os = "macos")]
    // SAFETY: pipe fills the two-element array; fcntl takes the descriptors it just made.
    unsafe {
        if libc::pipe(pipe.as_mut_ptr()) < 0 {
            return Err(std::io::Error::last_os_error());
        }
        for fd in pipe {
            libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK);
            libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
        }
    }
    let [wake_r, wake_w] = pipe;
    SIGNAL_FD.store(wake_w, Ordering::SeqCst);
    // SAFETY: the handler only touches atomics and write(2), which are async-signal-safe.
    unsafe {
        libc::signal(
            libc::SIGTERM,
            on_sigterm as extern "C" fn(libc::c_int) as libc::sighandler_t,
        )
    };
    let shared = Arc::new(Shared {
        queue: Mutex::new(VecDeque::new()),
        wake_fd: wake_w,
    });
    let sender = Sender {
        shared: shared.clone(),
    };

    let (w, h) = (fb.width, fb.height);
    let mut core = Core::new(build(sender), w, h);
    if let Some((call, reply)) = crate::intent::from_env(None) {
        crate::intent::dispatch(&mut core.app, call, reply);
    }
    // The tree Lucina reads, on /run/lucina/ax/<pid>.sock; none on a system without /run/lucina.
    let mut ax = match lucina_ax::Server::bind() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{name}: ax: {e}");
            None
        }
    };
    // A key repeat comes from where the key that started it came from.
    let mut last_key_injected = false;
    let mut screen = Screen::new(fb);
    let mut inputs = Inputs::default();
    // Input devices by event: the kernel's input.added and input.removed through the hub.
    let mut watch = input_watch::InputWatch::start();
    let mut kb = Keyboard::default();
    let mut pointer = Pointer::new(w as i32, h as i32);
    let mut pointer_shown: Option<Rect> = None;
    let mut evs = Vec::new();
    // Hi-res wheel remainder, in units, kept across batches so slow motion is not lost.
    let mut scroll_rem = (0i64, 0i64);
    // The last tree, while a reader wants tree events (stage D): each paint after it says what changed.
    let mut last_tree: Option<lucina_ax::tree::Snapshot> = None;
    let mut first_frame_shown = false;
    let scale = if Theme::new(w, h).k >= 1.25 { 2 } else { 1 };
    let (pw, ph) = gfx::pointer_size(scale);
    eprintln!("{name}: ready {w}x{h}");

    loop {
        let now = Instant::now();
        input_watch::rescan(watch.as_mut(), &mut inputs, now);
        // Draw.
        let mut pointer_moved = false;
        let mut earliest_input: Option<i64> = None;
        let mut pending_move = false;
        // Hi-res wheel units accumulated in this batch, (dx, dy).
        let mut scroll_acc = (0i64, 0i64);
        let mut scroll_injected = false;
        evs.clear();
        if inputs.read(&mut evs) {
            kb.release_all();
        }
        for (dev, ev) in evs.drain(..) {
            if matches!(ev.type_, EV_KEY | EV_REL | EV_ABS) {
                earliest_input.get_or_insert(ev.at_us);
            }
            // Lucina's input comes through a device of its own.
            let injected = inputs
                .devices
                .get(dev)
                .is_some_and(|d| d.name == "lucina-inject");
            match ev.type_ {
                EV_SYN if ev.code == input::SYN_DROPPED => kb.release_all(),
                EV_KEY if ev.code == keys::BTN_LEFT || ev.code == keys::BTN_TOUCH => {
                    if pending_move {
                        core.input_from(Input::Move(pointer.x, pointer.y), injected);
                        pending_move = false;
                    }
                    if ev.value == 1 {
                        core.input_from(Input::Down(pointer.x, pointer.y), injected);
                    } else if ev.value == 0 {
                        core.input_from(Input::Up(pointer.x, pointer.y), injected);
                    }
                }
                EV_KEY if ev.code == keys::BTN_RIGHT => {
                    if pending_move {
                        core.input_from(Input::Move(pointer.x, pointer.y), injected);
                        pending_move = false;
                    }
                    if ev.value == 1 {
                        core.input_from(Input::Secondary(pointer.x, pointer.y), injected);
                    }
                }
                EV_KEY => {
                    if let Some(p) = kb.feed(ev.code, ev.value, now) {
                        if p.code == keys::KEY_F1 {
                            // The shell's key. On a console it is the way out.
                            if !under_shell {
                                TERMINATE.store(true, Ordering::SeqCst);
                            }
                            continue;
                        }
                        // Never the keys of a password field: the codes would spell it.
                        if trace && !core.typing_secret() {
                            eprintln!("{name}: key {}", p.code);
                        }
                        last_key_injected = injected;
                        core.input_from(Input::Key(p), injected);
                    }
                }
                EV_REL => match ev.code {
                    input::REL_X => {
                        pointer.moved(ev.value, 0);
                        pending_move = true;
                        pointer_moved = true;
                    }
                    input::REL_Y => {
                        pointer.moved(0, ev.value);
                        pending_move = true;
                        pointer_moved = true;
                    }
                    input::REL_WHEEL
                    | input::REL_HWHEEL
                    | input::REL_WHEEL_HI_RES
                    | input::REL_HWHEEL_HI_RES => {
                        let hires = inputs.devices.get(dev).is_some_and(|d| d.hires);
                        match wheel_delta(hires, ev.code, ev.value, core.theme.px(40.0)) {
                            Some(WheelDelta::Hires(dx, dy)) => {
                                scroll_acc.0 += dx;
                                scroll_acc.1 += dy;
                                scroll_injected = injected;
                            }
                            Some(WheelDelta::Send(i)) => core.input_from(i, injected),
                            None => {}
                        }
                    }
                    _ => {}
                },
                EV_ABS => {
                    if let Some(d) = inputs.devices.get(dev) {
                        let range = if ev.code == input::ABS_X {
                            d.abs_x
                        } else {
                            d.abs_y
                        };
                        pointer.absolute(ev.code, ev.value, range);
                        pending_move = true;
                        pointer_moved = true;
                    }
                }
                _ => {}
            }
        }
        if pending_move {
            core.input(Input::Move(pointer.x, pointer.y));
        }
        // The batch's hi-res units become whole pixels; the remainder stays for the next batch.
        let (sdx, rx) = hires_pixels(scroll_acc.0, scroll_rem.0, core.theme.k);
        let (sdy, ry) = hires_pixels(scroll_acc.1, scroll_rem.1, core.theme.k);
        scroll_rem = (rx, ry);
        if sdx != 0 || sdy != 0 {
            core.input_from(Input::Scroll { dx: sdx, dy: sdy }, scroll_injected);
        }
        while let Some(p) = kb.repeat(now) {
            if p.code != keys::KEY_F1 {
                core.input_from(Input::Key(p), last_key_injected);
            }
        }
        // Messages from other threads.
        let msgs: Vec<A::Msg> = shared
            .queue
            .lock()
            .map(|mut q| q.drain(..).collect())
            .unwrap_or_default();
        for m in msgs {
            core.message(m);
        }
        // The camera state changed while the loop was waiting: the header's "camera on" mark shows
        // it, so the whole screen is drawn again.
        if crate::camera_mark::CHANGED.swap(false, Ordering::SeqCst) {
            core.screen_was_lost();
        }
        // Lucina's questions, between passes; an action sets want_paint, so the frame below shows it.
        if let Some(s) = ax.as_mut() {
            serve_ax(s, &mut core, name);
        }
        // A wake the app asked for.
        if core.st.wake.is_some_and(|t| t <= now) {
            core.st.wake = None;
            core.st.want_paint = true;
        }

        if TERMINATE.load(Ordering::SeqCst) || core.quit_requested() {
            core.app.on_quit();
            return Ok(());
        }

        if let Some(take) = core.app.take_screen() {
            take(&mut FullScreen {
                screen: &mut screen,
                inputs: &mut inputs,
            });
            // What the screen shows now is the closure's. Everything is drawn again, and keys held across it are gone.
            core.tiles = Tiles::default();
            core.st.want_paint = true;
            pointer_shown = None;
            kb.release_all();
            continue;
        }

        let painting = core.needs_paint();
        screen.prepare(&mut core.frame);
        let t0 = monotonic_us();
        let dirty = core.paint();
        let t1 = monotonic_us();
        let out: &mut dyn Surface = &mut screen;
        out.present_all(&core.frame, &dirty);
        if painting && !dirty.is_empty() && lucina_events::client::tree_wanted() {
            let tree = core.tree(name);
            if let Some(old) = &last_tree {
                tree_events(&old.diff(&tree));
            }
            last_tree = Some(tree);
        } else if last_tree.is_some() && !lucina_events::client::tree_wanted() {
            last_tree = None;
        }
        // The pointer goes straight to the screen over the frame, as in the shell.
        let overlaps =
            pointer_shown.is_some_and(|p| dirty.iter().any(|d| !d.intersect(&p).is_empty()));
        if pointer.visible && (pointer_moved || overlaps) {
            if let Some(old) = pointer_shown.take() {
                out.present(&core.frame, old);
            }
            let img = gfx::pointer_image(&core.frame, pointer.x, pointer.y, scale, core.app_bg());
            out.present_image(pointer.x, pointer.y, pw as usize, &img);
            pointer_shown = Some(Rect::new(pointer.x, pointer.y, pw, ph));
        }
        let t2 = monotonic_us();
        if !first_frame_shown {
            first_frame_shown = true;
            // The shell logs when it started the program: the difference is what a person waits.
            eprintln!("{name}: first frame on screen at_us={t2}");
        }
        if trace && painting {
            let px: i64 = dirty.iter().map(|r| r.w as i64 * r.h as i64).sum();
            let latency = earliest_input.map_or(-1, |t| t2 - t);
            eprintln!(
                "{name}: frame at_us={t2} draw_us={} present_us={} rects={} gpu_rects={} px={px} input_to_screen_us={latency} rss_kib={}",
                t1 - t0,
                t2 - t1,
                dirty.len(),
                screen.last_gpu_rects,
                rss_kib(),
            );
        }

        if let Some(program) = core.st.program.take() {
            let ended = run_over(&program, wake_r);
            // What was typed at it was not for this app.
            evs.clear();
            inputs.read(&mut evs);
            evs.clear();
            kb.release_all();
            pointer_shown = None;
            core.st.ended = Some(ended);
            core.screen_was_lost();
            if core.pass(None, false) {
                core.st.want_paint = true;
            }
            continue;
        }

        // Sleep until something happens.
        let now = Instant::now();
        // No rescan deadline (event-driven, past the boot window): the poll then waits only for
        // input; the timeout is clamped to i32::MAX ms below.
        let mut deadline = inputs
            .next_rescan(now)
            .unwrap_or(now + Duration::from_millis(i32::MAX as u64));
        if let Some(r) = kb.next_repeat() {
            deadline = deadline.min(r);
        }
        if let Some(t) = core.take_wake() {
            deadline = deadline.min(t);
        }
        let timeout = deadline
            .saturating_duration_since(now)
            .as_millis()
            .min(i32::MAX as u128) as i32;
        let mut fds: Vec<libc::pollfd> = inputs
            .fds()
            .into_iter()
            .chain(ax.as_ref().map(|s| s.fd()))
            .chain(watch.as_ref().map(|w| w.fd()))
            .chain([wake_r])
            .map(|fd| libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            })
            .collect();
        // SAFETY: `fds` is a valid array for the duration of the call. An interrupted poll (EINTR,
        // SIGTERM) just goes round the loop.
        unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout.max(1)) };
        if fds.last().is_some_and(|f| f.revents & libc::POLLIN != 0) {
            let mut sink = [0u8; 64];
            // SAFETY: reads into a live local buffer; the pipe is non-blocking.
            while unsafe { libc::read(wake_r, sink.as_mut_ptr().cast(), sink.len()) } > 0 {}
        }
    }
}

/// Lucina's requests waiting on the socket: trees and actions answered at once, intents handed to the app, which may answer
/// after more frames (the [`crate::intent::Responder`] holds the connection).
pub(crate) fn serve_ax<A: App>(s: &mut lucina_ax::Server, core: &mut Core<A>, name: &str) -> usize {
    let mut n = 0;
    while let Some((waiting, req)) = s.take_request() {
        n += 1;
        match req {
            Ok(Request::Intent { call }) => {
                crate::intent::from_socket(&mut core.app, call, waiting);
                core.st.want_paint = true;
            }
            other => waiting.reply(&answer(core, name, other)),
        }
    }
    n
}

/// The reply to one of Lucina's requests on the socket. Never logs a value she sends: it may be anything she types.
fn answer<A: App>(
    core: &mut Core<A>,
    name: &str,
    req: Result<Request, String>,
) -> serde_json::Value {
    let pid = std::process::id();
    match req {
        Err(why) => lucina_ax::proto::refusal("bad_request", &why, None),
        Ok(Request::Tree) => core.tree(name).reply(pid),
        Ok(Request::Act {
            id,
            action,
            value,
            version,
            ..
        }) => match core.act(name, &id, &action, value.as_deref(), &version) {
            Ok(s) => {
                eprintln!("{name}: ax act {action} {id} -> ok");
                let mut v = s.reply(pid);
                v["done"] = serde_json::Value::String(action);
                v
            }
            Err((r, s)) => {
                eprintln!("{name}: ax act {action} {id} -> refused");
                let error = r.code();
                lucina_ax::proto::refusal(error, &r.why(), Some(s.reply(pid)))
            }
        },
        // `serve_ax` hands intents to the app.
        Ok(Request::Intent { .. }) => {
            lucina_ax::proto::refusal("bad_request", "an intent goes to the app", None)
        }
    }
}

/// Runs `program` until it ends. If this app is told to end meanwhile, so is it. Its error output goes on to this app's, and the end of it
/// is kept to say what went wrong.
fn run_over(program: &crate::program::Program, wake_r: RawFd) -> crate::program::Ended {
    let mut cmd = program.command();
    cmd.stderr(std::process::Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return crate::program::ended_from(Err(e), ""),
    };
    let tail = Arc::new(Mutex::new(String::new()));
    let reader = child.stderr.take().map(|err| {
        let tail = tail.clone();
        std::thread::spawn(move || {
            use std::io::BufRead;
            for line in std::io::BufReader::new(err).lines().map_while(Result::ok) {
                eprintln!("{line}");
                if let Ok(mut t) = tail.lock() {
                    t.push_str(&line);
                    t.push('\n');
                    let keep = t.len().saturating_sub(2048);
                    if keep > 0 {
                        let cut = (keep..t.len())
                            .find(|i| t.is_char_boundary(*i))
                            .unwrap_or(0);
                        t.drain(..cut);
                    }
                }
            }
        })
    });
    let mut told = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Ok(s),
            Ok(None) => {}
            Err(e) => break Err(e),
        }
        if TERMINATE.load(Ordering::SeqCst) && !told {
            told = true;
            // SAFETY: signals the child this owns and has not yet reaped.
            unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
        }
        let mut fds = [libc::pollfd {
            fd: wake_r,
            events: libc::POLLIN,
            revents: 0,
        }];
        // SAFETY: a valid array of one for the call. Messages stay queued for afterwards; this only wakes the loop for the signal.
        unsafe { libc::poll(fds.as_mut_ptr(), 1, 50) };
        if fds[0].revents & libc::POLLIN != 0 {
            let mut sink = [0u8; 64];
            // SAFETY: reads into a live local buffer; the pipe is non-blocking.
            while unsafe { libc::read(wake_r, sink.as_mut_ptr().cast(), sink.len()) } > 0 {}
        }
    };
    if let Some(r) = reader {
        let _ = r.join();
    }
    let text = tail.lock().map(|t| t.clone()).unwrap_or_default();
    crate::program::ended_from(status, &text)
}

impl<A: App> Core<A> {
    fn app_bg(&self) -> gfx::Rgb {
        self.theme.bg
    }
}

#[cfg(test)]
mod redraw_tests {
    //! Drawing only what changed has to come out exactly as drawing everything does.

    use super::*;
    use crate::gfx::Text;
    use crate::{Face, Image, Rgb, TextEdit, keys};

    struct Everything {
        filter: TextEdit,
        note: TextEdit,
        list: crate::ListState,
        dialog: bool,
        image: Image,
        page: Vec<u32>,
        ticks: u32,
    }

    impl Everything {
        fn new() -> Everything {
            let px = (0..64 * 64u32)
                .map(|i| 0x8000_0000 | i.wrapping_mul(0x0001_0107))
                .collect();
            Everything {
                filter: TextEdit::new(),
                note: TextEdit::multiline(),
                list: crate::ListState::new(),
                dialog: false,
                image: Image { w: 64, h: 64, px },
                page: vec![0x00334455; 100 * 40],
                ticks: 0,
            }
        }
    }

    impl App for Everything {
        type Msg = ();
        fn view(&mut self, ui: &mut Ui) {
            ui.header("Everything");
            ui.pad(10.0);
            let t = ui.theme();
            ui.text_field("filter", &mut self.filter);
            ui.text_area("note", &mut self.note, 3);
            let mut flag = self.ticks.is_multiple_of(2);
            ui.checkbox("A box", &mut flag);
            ui.row(36.0, |ui| {
                if ui.button("Dialog") {
                    self.dialog = true;
                }
                if ui.primary_button("Tick") {
                    self.ticks += 1;
                }
            });
            // Text clipped by a neighbour, a picture with alpha over a colour, and pixels that change.
            let band = ui.cut_top(t.px(70.0));
            ui.fill(band, t.panel);
            ui.image_in(Rect::new(band.x + 3, band.y + 3, 40, 40), &self.image);
            let c = 0x00102030 + (self.ticks & 0xff);
            self.page.iter_mut().step_by(7).for_each(|p| *p = c);
            ui.pixels(Rect::new(band.x + 80, band.y, 100, 40), 100, &self.page);
            ui.fill_round(Rect::new(band.x + 200, band.y + 4, 90, 40), 9, t.accent);
            ui.text_in(
                Rect::new(band.x + 300, band.y, 30, 30),
                Face::Bold,
                18,
                Rgb(0xffffff),
                "clipped by its box",
            );
            let r = ui.area();
            let n = 400 + (self.ticks as usize % 3);
            ui.list("rows", &mut self.list, r, n, 28.0, |ui, i, selected| {
                let a = ui.area();
                if selected {
                    ui.fill_round(a, 6, t.panel_hi);
                }
                ui.text_in(a, Face::Sans, 15, t.text, &format!("Row {i} áéîõü ✓"));
            });
            if self.dialog {
                let mut close = false;
                ui.modal(300.0, 150.0, |ui| {
                    ui.heading("A dialog");
                    close = ui.button("Close");
                });
                if close {
                    self.dialog = false;
                }
            }
        }
    }

    fn both() -> (Core<Everything>, Core<Everything>) {
        let quick = Core::new(Everything::new(), 800, 600);
        let mut full = Core::new(Everything::new(), 800, 600);
        full.full_redraw = true;
        full.recording = None;
        (quick, full)
    }

    fn same(step: &str, quick: &Core<Everything>, full: &Core<Everything>) {
        let (a, b): (&[u32], &[u32]) = (&quick.frame.px, &full.frame.px);
        if let Some(i) = a.iter().zip(b).position(|(x, y)| x != y) {
            let (x, y) = (i % 800, i / 800);
            panic!(
                "{step}: differ first at {x},{y}: {:08x} and {:08x}",
                a[i], b[i]
            );
        }
    }

    #[test]
    fn drawing_what_changed_comes_out_as_drawing_everything() {
        let (mut quick, mut full) = both();
        quick.paint();
        full.paint();
        same("first", &quick, &full);
        let press = |code| {
            Input::Key(keys::Press {
                code,
                mods: Default::default(),
            })
        };
        let mut steps: Vec<(&str, Input)> = vec![
            ("pointer on the list", Input::Move(300, 450)),
            ("wheel down", Input::Wheel(-1)),
            ("wheel down", Input::Wheel(-3)),
            ("wheel up", Input::Wheel(1)),
            ("wheel at the top", Input::Wheel(40)),
            ("wheel at the top", Input::Wheel(1)),
            ("tab", press(keys::KEY_TAB)),
            ("tab", press(keys::KEY_TAB)),
            ("tab", press(keys::KEY_TAB)),
            ("tab", press(keys::KEY_TAB)),
        ];
        for c in ["a", "b", "c"] {
            steps.push((
                "typing",
                press(keys::code_for(c.chars().next().unwrap()).unwrap().0),
            ));
        }
        steps.extend([
            ("backspace", press(keys::KEY_BACKSPACE)),
            ("enter on a button", press(keys::KEY_ENTER)),
            ("tab", press(keys::KEY_TAB)),
            ("to the list", press(keys::KEY_TAB)),
            ("down", press(keys::KEY_DOWN)),
            ("down", press(keys::KEY_DOWN)),
            ("page", press(keys::KEY_PAGEDOWN)),
            ("pointer over a row", Input::Move(200, 500)),
            ("press", Input::Down(200, 500)),
            ("release", Input::Up(200, 500)),
        ]);
        for (step, i) in steps {
            for c in [&mut quick as &mut dyn Feed, &mut full] {
                c.feed(i);
            }
            same(step, &quick, &full);
        }
        // The dialog: everything dims, and comes back.
        for c in [&mut quick as &mut dyn Feed, &mut full] {
            c.open_dialog();
        }
        same("dialog", &quick, &full);
        for c in [&mut quick as &mut dyn Feed, &mut full] {
            c.close_dialog();
        }
        same("dialog closed", &quick, &full);
        for _ in 0..4 {
            for c in [&mut quick as &mut dyn Feed, &mut full] {
                c.tick();
            }
            same("pixels and rows changed", &quick, &full);
        }
    }

    trait Feed {
        fn feed(&mut self, i: Input);
        fn open_dialog(&mut self);
        fn close_dialog(&mut self);
        fn tick(&mut self);
    }

    impl Feed for Core<Everything> {
        fn feed(&mut self, i: Input) {
            self.input(i);
            self.paint();
        }
        fn open_dialog(&mut self) {
            self.app.dialog = true;
            self.st.want_paint = true;
            self.paint();
        }
        fn close_dialog(&mut self) {
            self.app.dialog = false;
            self.st.want_paint = true;
            self.paint();
        }
        fn tick(&mut self) {
            self.app.ticks += 1;
            self.st.want_paint = true;
            self.paint();
        }
    }

    /// One thing to draw, and where it is clipped.
    #[derive(Clone)]
    enum Thing {
        Fill(Rect, u32),
        Round(Rect, i32, u32),
        Bordered(Rect, i32, u32, u32),
        Disc(i32, i32, i32, u32),
        Blend(i32, i32, u32, u8),
        Dim(u8),
        Text(i32, i32, u16, u32, u8),
        Image(i32, i32, u32),
        Pixels(i32, i32, u32),
    }

    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self, n: i32) -> i32 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((self.0 >> 33) % n as u64) as i32
        }

        fn colour(&mut self) -> u32 {
            [0x00070b18, 0x007f8fe0, 0x00ffffff, 0x001c1c1e, 0x00ff453a][self.next(5) as usize]
        }

        fn rect(&mut self) -> Rect {
            Rect::new(
                self.next(420) - 20,
                self.next(220) - 20,
                self.next(150),
                self.next(90),
            )
        }

        fn thing(&mut self) -> (Thing, Rect) {
            let clip = if self.next(3) == 0 {
                self.rect()
            } else {
                Rect::new(0, 0, 400, 200)
            };
            let thing = match self.next(9) {
                0 => Thing::Fill(self.rect(), self.colour()),
                1 => Thing::Round(self.rect(), self.next(12), self.colour()),
                2 => Thing::Bordered(self.rect(), self.next(12), self.colour(), self.colour()),
                3 => Thing::Disc(self.next(400), self.next(200), self.next(30), self.colour()),
                4 => Thing::Blend(
                    self.next(400),
                    self.next(200),
                    self.colour(),
                    self.next(256) as u8,
                ),
                5 if self.next(8) == 0 => Thing::Dim(self.next(256) as u8),
                6 => Thing::Image(self.next(420) - 20, self.next(220) - 20, self.colour()),
                7 => Thing::Pixels(self.next(420) - 20, self.next(220) - 20, self.colour()),
                _ => Thing::Text(
                    self.next(420) - 20,
                    self.next(220),
                    12 + self.next(10) as u16,
                    self.colour(),
                    self.next(4) as u8,
                ),
            };
            (thing, clip)
        }
    }

    fn draw(f: &mut Frame, text: &mut Text, things: &[(Thing, Rect)]) {
        let all = f.bounds();
        f.set_clip(all);
        f.fill_rect(all, Rgb(0x00070b18));
        for (thing, clip) in things {
            f.set_clip(*clip);
            match *thing {
                Thing::Fill(r, c) => f.fill_rect(r, Rgb(c)),
                Thing::Round(r, rad, c) => f.fill_round_rect(r, rad, Rgb(c)),
                Thing::Bordered(r, rad, a, b) => f.bordered_round_rect(r, rad, 2, Rgb(a), Rgb(b)),
                Thing::Disc(x, y, rad, c) => f.fill_disc(x as f32, y as f32, rad as f32, Rgb(c)),
                Thing::Blend(x, y, c, a) => f.blend(x, y, Rgb(c), a),
                Thing::Dim(a) => f.dim(a),
                Thing::Text(x, y, px, c, which) => {
                    let s = ["Row 12 áéîõü", "gjpqy Wij", "✓ .,;", " "][which as usize];
                    let clip = f.clip();
                    text.draw_clipped(f, clip, x, y, Face::Sans, px, Rgb(c), s);
                }
                Thing::Image(x, y, c) => {
                    let px: Vec<u32> = (0..30 * 20u32).map(|i| (i * 11 % 256) << 24 | c).collect();
                    f.draw_image(x, y, &Image { w: 30, h: 20, px });
                }
                Thing::Pixels(x, y, c) => {
                    let px: Vec<u32> = (0..30 * 20u32).map(|i| c ^ i).collect();
                    f.copy_pixels(x, y, 30, &px);
                }
            }
        }
        f.set_clip(all);
    }

    /// Every kind of drawing, every kind of change, small ones included, against a frame drawn whole.
    #[test]
    fn random_drawing_comes_out_the_same_whatever_changed() {
        let mut text = Text::new();
        for seed in 1..=6u64 {
            let mut rng = Lcg(seed);
            let mut frame = Frame::new(400, 200);
            let mut tiles = Tiles::default();
            let mut rec = Box::<Recording>::default();
            let mut things: Vec<(Thing, Rect)> = (0..30).map(|_| rng.thing()).collect();
            for step in 0..300 {
                frame.start_recording(rec);
                draw(&mut frame, &mut text, &things);
                rec = frame.take_recording().expect("recording");
                let dirty = tiles.diff_ops(400, 200, &rec);
                frame.replay(&rec, &mut text, &dirty);

                let mut whole = Frame::new(400, 200);
                draw(&mut whole, &mut text, &things);
                let (a, b): (&[u32], &[u32]) = (&frame.px, &whole.px);
                if let Some(i) = a.iter().zip(b).position(|(x, y)| x != y) {
                    panic!(
                        "seed {seed} step {step}: differ first at {},{}: {:08x} and {:08x}",
                        i % 400,
                        i / 400,
                        a[i],
                        b[i]
                    );
                }

                // One small change, the kind that a hash or a box could miss.
                let i = rng.next(things.len() as i32) as usize;
                let by = 1 + rng.next(3);
                match rng.next(6) {
                    0 => things[i] = rng.thing(),
                    1 => {
                        things.remove(i);
                        things.push(rng.thing());
                    }
                    2 => things[i].1.w = (things[i].1.w - by).max(1),
                    3 => things[i].1.x += by,
                    4 => {
                        let c = rng.colour();
                        match &mut things[i].0 {
                            Thing::Fill(_, x)
                            | Thing::Round(_, _, x)
                            | Thing::Disc(_, _, _, x)
                            | Thing::Blend(_, _, x, _)
                            | Thing::Text(_, _, _, x, _)
                            | Thing::Image(_, _, x)
                            | Thing::Pixels(_, _, x) => *x = c,
                            Thing::Bordered(_, _, _, x) => *x = c,
                            Thing::Dim(_) => {}
                        }
                    }
                    _ => match &mut things[i].0 {
                        Thing::Fill(r, _) | Thing::Round(r, _, _) | Thing::Bordered(r, _, _, _) => {
                            r.x += by
                        }
                        Thing::Disc(x, _, _, _)
                        | Thing::Blend(x, _, _, _)
                        | Thing::Text(x, _, _, _, _)
                        | Thing::Image(x, _, _)
                        | Thing::Pixels(x, _, _) => *x += by,
                        Thing::Dim(a) => *a = a.wrapping_add(by as u8),
                    },
                }
            }
        }
    }

    #[test]
    fn nothing_changed_copies_nothing_and_a_list_only_its_rows() {
        let (mut quick, _) = both();
        assert!(!quick.paint().is_empty());
        quick.input(Input::Move(300, 450));
        quick.paint();
        // At the top already: an event that changes nothing.
        quick.input(Input::Wheel(1));
        assert_eq!(quick.paint(), Vec::<Rect>::new());
        quick.input(Input::Wheel(-1));
        let rects = quick.paint();
        assert!(!rects.is_empty());
        let top = rects.iter().map(|r| r.y).min().unwrap();
        let area: i64 = rects.iter().map(|r| r.w as i64 * r.h as i64).sum();
        assert!(top > 200, "above the list nothing moved: {rects:?}");
        assert!(area < 800 * 400, "{area}");
    }

    #[test]
    fn a_full_redraw_can_still_be_asked_for() {
        let (_, mut full) = both();
        assert!(!full.paint().is_empty());
        full.st.want_paint = true;
        assert_eq!(full.paint(), Vec::<Rect>::new());
        assert!(full.recording.is_none());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::Hit;

    struct Rows {
        hit: Hit,
        clicks: usize,
        secondary: usize,
        row_secondary: Option<usize>,
        row_hits: usize,
        activated: Option<usize>,
        list: crate::ListState,
    }

    impl App for Rows {
        type Msg = ();
        fn view(&mut self, ui: &mut Ui) {
            let spot = ui.hit("spot", Rect::new(0, 0, 100, 100));
            self.hit = spot;
            self.clicks += spot.clicked as usize;
            self.secondary += spot.secondary as usize;
            let hits = &mut self.row_hits;
            let res = ui.list(
                "rows",
                &mut self.list,
                Rect::new(0, 200, 400, 300),
                100,
                30.0,
                |ui, i, _| {
                    let a = ui.area();
                    // The left half of each row answers on its own.
                    if ui.hit(("half", i), Rect::new(a.x, a.y, 100, a.h)).clicked {
                        *hits += 1;
                    }
                },
            );
            if res.activated.is_some() {
                self.activated = res.activated;
            }
            if res.secondary.is_some() {
                self.row_secondary = res.secondary;
            }
        }
    }

    fn rows() -> Harness<Rows> {
        Harness::new(
            Rows {
                hit: Hit::default(),
                clicks: 0,
                secondary: 0,
                row_secondary: None,
                row_hits: 0,
                activated: None,
                list: crate::ListState::new(),
            },
            1024,
            768,
        )
    }

    #[test]
    fn a_hit_area_answers_the_pointer_only() {
        let mut h = rows();
        h.click(50, 50);
        assert_eq!(h.app().clicks, 1);
        assert!(h.app().hit.hovered);
        // Down inside and up outside is no click.
        h.core.input(Input::Move(50, 50));
        h.core.input(Input::Down(50, 50));
        assert!(h.app().hit.pressed);
        h.core.input(Input::Up(500, 50));
        assert!(!h.app().hit.hovered);
        assert_eq!(h.app().clicks, 1);
        // The secondary button is not a click.
        h.right_click(50, 50);
        assert_eq!(h.app().secondary, 1);
        assert_eq!(h.app().clicks, 1);
        // A list row reports it too, and selects itself.
        h.right_click(300, 260);
        assert_eq!(h.app().row_secondary, Some(1));
        assert_eq!(h.app().list.selected, Some(1));
        h.app().list.selected = None;
        // It is not in the focus order: Tab stays on the list, the only thing that is.
        h.key(keys::KEY_TAB);
        h.key(keys::KEY_DOWN);
        assert_eq!(h.app().list.selected, Some(0));
    }

    #[test]
    fn a_hit_area_in_a_row_takes_the_click_before_the_row() {
        let mut h = rows();
        h.click(50, 210);
        assert_eq!(h.app().row_hits, 1);
        assert_eq!(h.app().activated, None);
        h.click(300, 210);
        assert_eq!(h.app().row_hits, 1);
        assert_eq!(h.app().activated, Some(0));
    }

    #[test]
    fn a_program_runs_and_says_how_it_ended() {
        let sh = crate::program::Program {
            path: "/bin/sh".into(),
            args: vec!["-c".into(), "echo oh no >&2; exit 3".into()],
            env: Vec::new(),
        };
        let mut pipe = [-1; 2];
        // SAFETY: pipe2 fills the two-element array.
        assert_eq!(
            unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_NONBLOCK | libc::O_CLOEXEC) },
            0
        );
        assert_eq!(
            run_over(&sh, pipe[0]),
            crate::program::Ended::Failed("The player ended with status 3: oh no".to_owned())
        );
        let ok = crate::program::Program {
            path: "/bin/true".into(),
            args: Vec::new(),
            env: Vec::new(),
        };
        assert_eq!(run_over(&ok, pipe[0]), crate::program::Ended::Ok);
        let no = crate::program::Program {
            path: "/nonexistent/program".into(),
            args: Vec::new(),
            env: Vec::new(),
        };
        assert!(matches!(
            run_over(&no, pipe[0]),
            crate::program::Ended::NotStarted(_)
        ));
    }

    #[test]
    fn hires_units_become_pixels_and_the_remainder_carries() {
        // Three reports of 5 hi-res units at k = 1.4: 7 px in all, and a remainder left over.
        let mut rem = 0;
        let mut px = 0;
        for _ in 0..3 {
            let (p, r) = hires_pixels(5, rem, 1.4);
            px += p;
            rem = r;
        }
        assert_eq!(px, 7);
        assert!(rem > 0, "the remainder must carry: {rem}");
        // At k = 1, 3 units are exactly one pixel and nothing is lost; a 120-unit notch is 40 px.
        assert_eq!(hires_pixels(3, 0, 1.0), (1, 0));
        assert_eq!(hires_pixels(2, 0, 1.0), (0, 2));
        assert_eq!(hires_pixels(1, 2, 1.0), (1, 0));
        assert_eq!(hires_pixels(120, 0, 1.0), (40, 0));
    }

    #[test]
    fn a_hires_device_ignores_notches_and_a_plain_one_keeps_them() {
        // On a hi-res device the notch events repeat the hi-res ones: ignored, or it scrolls twice.
        assert_eq!(wheel_delta(true, input::REL_WHEEL, 1, 40), None);
        assert_eq!(wheel_delta(true, input::REL_HWHEEL, 1, 40), None);
        assert_eq!(
            wheel_delta(true, input::REL_WHEEL_HI_RES, 5, 40),
            Some(WheelDelta::Hires(0, 5))
        );
        assert_eq!(
            wheel_delta(true, input::REL_HWHEEL_HI_RES, -3, 40),
            Some(WheelDelta::Hires(-3, 0))
        );
        // A device without hi-res: notches as before, horizontal as pixels.
        assert_eq!(
            wheel_delta(false, input::REL_WHEEL, -2, 40),
            Some(WheelDelta::Send(Input::Wheel(-2)))
        );
        assert_eq!(
            wheel_delta(false, input::REL_HWHEEL, 1, 40),
            Some(WheelDelta::Send(Input::Scroll { dx: 40, dy: 0 }))
        );
    }

    #[test]
    fn a_scroll_event_moves_a_list_by_pixels() {
        let mut h = rows();
        h.core.input(Input::Move(200, 300));
        h.scroll(0, -30);
        assert_eq!(h.app().list.scroll, 30);
        h.scroll(0, -30);
        assert_eq!(h.app().list.scroll, 60);
        // Clamped at the bottom, and at the top. Rows are at least 44 px tall.
        h.scroll(0, -100000);
        let most = 100 * 44 - 300;
        assert_eq!(h.app().list.scroll, most);
        h.scroll(0, 100000);
        assert_eq!(h.app().list.scroll, 0);
    }

    #[test]
    fn load_keeps_what_it_can_show() {
        let mut l: Load<u32> = Load::default();
        assert_eq!(l.value(), None);
        l.start();
        assert!(l.is_loading());
        assert_eq!(l.value(), None);
        l.finish(Ok(1));
        assert_eq!(l.value(), Some(&1));
        l.start();
        assert!(l.is_loading());
        assert_eq!(l.value(), Some(&1));
        l.finish(Err("offline".into()));
        assert_eq!(l, Load::Failed("offline".into(), Some(1)));
        assert_eq!(l.value(), Some(&1));
        let mut e: Load<u32> = Load::Loading(None);
        e.finish(Err("offline".into()));
        assert_eq!(e, Load::Failed("offline".into(), None));
    }
}
