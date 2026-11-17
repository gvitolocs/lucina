//! The gesture-driven space switcher and overview as a pure state machine: gesture records and time go in, a *scene* (which space's surface
//! goes into which rectangle this frame) comes out. No I/O, no clock (time is passed in), no allocation, no knowledge of the compositor or the
//! device. Behaviour follows niri (`swipe_tracker.rs`, workspace switch and overview gesture), GNOME Shell (`swipeTracker.js`,
//! `WORKSPACE_SPACING`) and KWin's partial activation; no code is taken from them.
//!
//! Springs are advanced by absolute time (`now_us - t0`), so skipping frames never changes what a given instant looks like.
//! Layers are never zero-sized, but they are not clipped to the screen: a layer may lie fully outside it (an overview neighbour), and the
//! caller clips. The keyboard switcher (Meta+Tab / Alt+Tab) is the exception: once its popup is up, every tile of that row lies inside the screen.

use crate::overview::{space_rect, zoom_at};
use crate::rubber::Band;
use crate::spaces::{Space, Spaces};
use crate::spring::{Params, Spring};
use crate::switcher::{AppId, MAX_APPS, Mru, Switcher};
use crate::tile::Rect;
use crate::tracker::{Axis, AxisLock, Tracker, commit_overview, release_target};

/// Most layers a scene can hold: more than `spaces::MAX_SPACES` (16), so every space of the overview fits.
pub const MAX_LAYERS: usize = 24;
/// Swipe distance (libinput 1000-dpi units, already divided by 256 by the caller) that moves one space. GNOME Shell's
/// `TOUCHPAD_BASE_WIDTH` (400). To be calibrated on the real pad.
pub const SWIPE_DISTANCE: f64 = 400.0;
/// Gap in pixels between neighbouring spaces while swiping (GNOME Shell `WORKSPACE_SPACING`).
pub const GAP: i32 = 100;
/// Swipe distance (same units) that opens the overview completely (niri `OVERVIEW_GESTURE_MOVEMENT`, 300).
pub const OVERVIEW_DISTANCE: f64 = 300.0;
/// Zoom of the fully open overview in per-mille of the screen (niri's default overview zoom of 0.6 is of the same size).
pub const OVERVIEW_ZOOM_PERMILLE: i32 = 600;
/// Fewest fingers that make a swipe a shell gesture (GNOME and niri use three for workspace and overview swipes).
pub const MIN_FINGERS: u8 = 3;
/// How long Meta or Alt is held before the switcher popup is drawn. A quicker tap shows none (GNOME Shell, 150 ms).
pub const POPUP_DELAY_US: u64 = 150_000;
/// Opacity of a switcher tile that is not the selected one.
const SWITCHER_DIM: u8 = 160;
/// Gap in pixels between switcher tiles when the row has room for it.
const SWITCHER_GAP: i32 = 24;

/// Phase of a gesture record (`lucina_gesture` begin / update / end).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Begin,
    Update,
    End,
}

/// What the shell feeds in. `dx`, `dy` are libinput 1000-dpi units as f64, already divided by 256 by the caller; `time_us` is monotonic.
/// `swipe == false` is a pinch or hold, which this controller ignores.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GestureIn {
    pub swipe: bool,
    pub phase: Phase,
    pub fingers: u8,
    pub cancelled: bool,
    pub dx: f64,
    pub dy: f64,
    pub time_us: u64,
}

/// What the frame shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Spaces,
    Overview,
    /// Meta+Tab / Alt+Tab. Drawn only after [`POPUP_DELAY_US`]; before that the scene stays the ordinary one.
    Switcher,
}

/// One layer of the frame, in draw order: the surface of the app in space slot `space` (index into `Spaces`), drawn from its full
/// (screen-sized) surface into `to`, with `opacity` 0..=255.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layer {
    pub space: usize,
    pub to: Rect,
    pub opacity: u8,
}

/// The frame: `layers[..len]` in draw order (back to front).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scene {
    pub layers: [Layer; MAX_LAYERS],
    pub len: usize,
    /// More frames are needed (a finger is down or a spring is running).
    pub animating: bool,
    pub mode: Mode,
}

impl Scene {
    /// An empty scene.
    pub const fn new() -> Scene {
        Scene {
            layers: [Layer {
                space: 0,
                to: Rect {
                    x: 0,
                    y: 0,
                    w: 0,
                    h: 0,
                },
                opacity: 0,
            }; MAX_LAYERS],
            len: 0,
            animating: false,
            mode: Mode::Spaces,
        }
    }

    /// The used layers.
    pub fn as_slice(&self) -> &[Layer] {
        &self.layers[..self.len]
    }

    fn push(&mut self, space: usize, to: Rect) {
        self.push_opacity(space, to, 255);
    }

    fn push_opacity(&mut self, space: usize, to: Rect, opacity: u8) {
        if self.len < MAX_LAYERS {
            self.layers[self.len] = Layer { space, to, opacity };
            self.len += 1;
        }
    }
}

impl Default for Scene {
    fn default() -> Scene {
        Scene::new()
    }
}

#[derive(Clone, Debug)]
enum State {
    /// Nothing moves. Position is the active space; the overview is open or closed according to `Controller::open`.
    Rest,
    /// Gesture begun, axis not locked yet: the frame holds the position it began at.
    Pending {
        lock: AxisLock,
        start: usize,
        base_p: f64,
        base_o: f64,
    },
    DragSpaces {
        start: usize,
        base_p: f64,
        tracker: Tracker,
    },
    DragOverview {
        base_o: f64,
        tracker: Tracker,
    },
    /// A gesture the controller does not act on (horizontal while the overview is open): the frame holds `base_o`.
    Ignoring {
        base_o: f64,
    },
    SettleSpaces {
        spring: Spring,
        t0: u64,
        duration_s: f64,
    },
    SettleOverview {
        spring: Spring,
        t0: u64,
        duration_s: f64,
        open: bool,
    },
}

/// A Meta+Tab / Alt+Tab session held over the gesture state, so cancel can put that state back unchanged.
#[derive(Clone, Copy, Debug)]
struct SwitcherHold {
    switcher: Switcher,
    begun_us: u64,
    /// `overview_at` was above 0 when the session started. A commit that lands returns to that view.
    overview: bool,
}

/// The gesture-driven space switch and overview, plus the keyboard app switcher.
#[derive(Clone, Debug)]
pub struct Controller {
    screen: Rect,
    state: State,
    /// Whether the overview is open while in [`State::Rest`].
    open: bool,
    /// Focused app first. Seeded from the row the first time it is needed, then updated on each commit.
    mru: Mru,
    switcher: Option<SwitcherHold>,
}

fn elapsed_s(now_us: u64, t0: u64) -> f64 {
    now_us.saturating_sub(t0) as f64 / 1_000_000.0
}

fn clamp_f(x: f64, lo: f64, hi: f64) -> f64 {
    if x < lo {
        lo
    } else if x > hi {
        hi
    } else {
        x
    }
}

fn clamp_i64(v: i64, lo: i64, hi: i64) -> i64 {
    if lo > hi || v < lo {
        lo
    } else if v > hi {
        hi
    } else {
        v
    }
}

fn sat_i32(v: i64) -> i32 {
    if v > i32::MAX as i64 {
        i32::MAX
    } else if v < i32::MIN as i64 {
        i32::MIN
    } else {
        v as i32
    }
}

fn primary_app(space: Space) -> Option<AppId> {
    match space {
        Space::Home => None,
        Space::App(id) => Some(id),
        Space::Tiles(t) => t.get(0),
    }
}

/// Live apps, left to right, Home skipped, at most [`MAX_APPS`], duplicates dropped.
fn live_apps(spaces: &Spaces, out: &mut [AppId; MAX_APPS]) -> usize {
    let mut n = 0;
    let mut i = 0;
    while i < spaces.len() && n < MAX_APPS {
        let Some(space) = spaces.get(i) else {
            break;
        };
        match space {
            Space::Home => {}
            Space::App(id) => {
                if !listed(out, n, id) {
                    out[n] = id;
                    n += 1;
                }
            }
            Space::Tiles(t) => {
                let mut k = 0;
                while k < t.len() && n < MAX_APPS {
                    if let Some(id) = t.get(k)
                        && !listed(out, n, id)
                    {
                        out[n] = id;
                        n += 1;
                    }
                    k += 1;
                }
            }
        }
        i += 1;
    }
    n
}

fn listed(ids: &[AppId], n: usize, id: AppId) -> bool {
    let mut i = 0;
    while i < n && i < ids.len() {
        if ids[i] == id {
            return true;
        }
        i += 1;
    }
    false
}

/// How many different spaces the snapshot still points at.
fn distinct_spaces(sw: &Switcher, spaces: &Spaces) -> usize {
    let mut seen = [usize::MAX; MAX_APPS];
    let mut n = 0;
    let items = sw.items();
    let mut i = 0;
    while i < items.len() {
        if let Some(sp) = spaces.space_of(items[i])
            && !listed_usize(&seen, n, sp)
            && n < seen.len()
        {
            seen[n] = sp;
            n += 1;
        }
        i += 1;
    }
    n
}

fn listed_usize(ids: &[usize], n: usize, id: usize) -> bool {
    let mut i = 0;
    while i < n && i < ids.len() {
        if ids[i] == id {
            return true;
        }
        i += 1;
    }
    false
}

/// One centred row of `n` equal tiles. `space_rect` pins the active space to the middle and lets neighbours leave the
/// screen; `overview::layout` may split into several rows. The switcher is a single row, kept inside a positive screen
/// by shrinking the gap and then overlapping when the screen is narrower than the row.
fn switcher_row(screen: Rect, n: usize, out: &mut [Rect; MAX_LAYERS]) {
    let n = n.min(MAX_LAYERS).min(out.len());
    if n == 0 {
        return;
    }
    let sx = screen.x as i64;
    let sy = screen.y as i64;
    let sw = (screen.w as i64).max(0);
    let sh = (screen.h as i64).max(0);
    let box_w = sw.max(1);
    let box_h = sh.max(1);
    let n_i = n as i64;
    let gap_pref = (SWITCHER_GAP as i64).max(0);
    let gaps = gap_pref.saturating_mul(n_i.saturating_sub(1));
    let tile_w = if sw >= n_i.saturating_add(gaps) {
        (sw - gaps) / n_i
    } else if sw >= n_i {
        sw / n_i
    } else {
        1
    };
    let tile_w = tile_w.max(1).min(box_w);
    let gap = if sw >= n_i.saturating_add(gaps) {
        gap_pref
    } else {
        0
    };
    let tile_h = if sh >= 3 {
        (sh.saturating_mul(2) / 3).clamp(1, box_h)
    } else {
        box_h
    };
    let natural = tile_w.saturating_add(gap);
    let stride = if n_i <= 1 {
        0
    } else {
        let room = box_w.saturating_sub(tile_w);
        let max_stride = room / (n_i - 1);
        natural.min(max_stride)
    };
    let row_w = tile_w
        .saturating_add(stride.saturating_mul(n_i - 1))
        .min(box_w);
    let x0 = sx.saturating_add((box_w - row_w) / 2);
    let y0 = sy.saturating_add((box_h - tile_h) / 2);
    let x_hi = sx.saturating_add(box_w.saturating_sub(tile_w));
    let y_hi = sy.saturating_add(box_h.saturating_sub(tile_h));
    let mut i = 0;
    while i < n {
        let x = clamp_i64(x0.saturating_add(stride.saturating_mul(i as i64)), sx, x_hi);
        let y = clamp_i64(y0, sy, y_hi);
        out[i] = Rect {
            x: sat_i32(x),
            y: sat_i32(y),
            w: sat_i32(tile_w),
            h: sat_i32(tile_h),
        };
        i += 1;
    }
}

/// `(active, last)` of the row, both inside it.
fn indices(spaces: &Spaces) -> (usize, usize) {
    let last = spaces.len().saturating_sub(1);
    (spaces.active().min(last), last)
}

fn spring_state_spaces(from: f64, to: usize, velocity: f64, now_us: u64) -> State {
    let spring = Spring::new(from, to as f64, velocity, Params::WORKSPACE_SWITCH);
    State::SettleSpaces {
        spring,
        t0: now_us,
        duration_s: spring.duration_s(),
    }
}

fn spring_state_overview(from: f64, open: bool, velocity: f64, now_us: u64) -> State {
    let to = if open { 1.0 } else { 0.0 };
    let spring = Spring::new(from, to, velocity, Params::VIEW);
    State::SettleOverview {
        spring,
        t0: now_us,
        duration_s: spring.duration_s(),
        open,
    }
}

impl Controller {
    /// A controller for a screen of the given rectangle, resting on the active space with the overview closed.
    pub fn new(screen: Rect) -> Controller {
        Controller {
            screen,
            state: State::Rest,
            open: false,
            mru: Mru::new(),
            switcher: None,
        }
    }

    /// Rebuild the MRU from the row. Apps the controller has already focused keep that order; apps it has never seen are
    /// older, left to right. The app in the active space, when there is one, is focused last so it sits at the front.
    fn sync_mru(&mut self, spaces: &Spaces) {
        let old = self.mru;
        let mut live = [AppId(0); MAX_APPS];
        let n = live_apps(spaces, &mut live);
        self.mru = Mru::new();
        let mut i = 0;
        while i < n {
            self.mru.focus(live[i]);
            i += 1;
        }
        let mut i = old.len();
        while i > 0 {
            i -= 1;
            let id = old.as_slice()[i];
            if spaces.space_of(id).is_some() {
                self.mru.focus(id);
            }
        }
        if let Some(id) = spaces.get(spaces.active()).and_then(primary_app) {
            self.mru.focus(id);
        }
    }

    fn popup_due(&self, now_us: u64) -> bool {
        match self.switcher {
            Some(hold) => now_us.saturating_sub(hold.begun_us) >= POPUP_DELAY_US,
            None => false,
        }
    }

    fn write_switcher(&self, spaces: &Spaces, out: &mut Scene) {
        out.len = 0;
        out.mode = Mode::Switcher;
        out.animating = true;
        let Some(hold) = self.switcher else {
            return;
        };
        let items = hold.switcher.items();
        let sel = hold.switcher.selected();
        let mut ids = [AppId(0); MAX_LAYERS];
        let mut n = 0;
        let mut i = 0;
        while i < items.len() && n < MAX_LAYERS {
            if spaces.space_of(items[i]).is_some() {
                ids[n] = items[i];
                n += 1;
            }
            i += 1;
        }
        let mut rects = [Rect {
            x: 0,
            y: 0,
            w: 1,
            h: 1,
        }; MAX_LAYERS];
        switcher_row(self.screen, n, &mut rects);
        i = 0;
        while i < n {
            if let Some(space) = spaces.space_of(ids[i]) {
                let opacity = if sel == Some(ids[i]) {
                    255
                } else {
                    SWITCHER_DIM
                };
                out.push_opacity(space, rects[i], opacity);
            }
            i += 1;
        }
    }

    /// Overview progress (0 closed, 1 open, rubber-banded a little beyond while dragging) at `now_us`.
    fn overview_at(&self, now_us: u64) -> f64 {
        match &self.state {
            State::Rest => {
                if self.open {
                    1.0
                } else {
                    0.0
                }
            }
            State::Pending { base_o, .. } | State::Ignoring { base_o } => *base_o,
            State::DragOverview { base_o, tracker } => {
                Band::GESTURE.clamp(0.0, 1.0, base_o - tracker.pos() / OVERVIEW_DISTANCE)
            }
            State::SettleOverview { spring, t0, .. } => {
                spring.value_at(elapsed_s(now_us, *t0)).clamp(-1.0, 2.0)
            }
            State::DragSpaces { .. } | State::SettleSpaces { .. } => 0.0,
        }
    }

    /// Position in space-index units at `now_us`, inside the row (plus the rubber band while dragging).
    fn position_at(&self, now_us: u64, active: usize, last: usize) -> f64 {
        let last_f = last as f64;
        match &self.state {
            State::Pending { base_p, .. } => clamp_f(*base_p, 0.0, last_f),
            State::DragSpaces {
                base_p, tracker, ..
            } => Band::GESTURE.clamp(0.0, last_f, base_p - tracker.pos() / SWIPE_DISTANCE),
            State::SettleSpaces { spring, t0, .. } => {
                clamp_f(spring.value_at(elapsed_s(now_us, *t0)), 0.0, last_f)
            }
            _ => active as f64,
        }
    }

    /// Feed a gesture record. `spaces` is read for the count and the active index; a committed switch calls `spaces.go_to(target)` at the
    /// moment the finger-tracked part ends (on release), not when the animation ends.
    ///
    /// Ignored: pinch and hold records, `Begin` and `Update` with fewer than [`MIN_FINGERS`], `Update` and `End` without a `Begin`. A `Begin`
    /// during a gesture or a settle takes over from the position shown at that instant. An `End` ends the gesture in progress whatever
    /// its finger count says (the fingers of one gesture do not change).
    /// When a gesture begun during a settle locks on the other axis, the held position of the first axis snaps to its rest value.
    /// While a switcher session is open the keyboard owns the controller: every record is ignored.
    pub fn gesture(&mut self, g: GestureIn, spaces: &mut Spaces) {
        if self.switcher.is_some() {
            return;
        }
        if !g.swipe {
            return;
        }
        let (active, last) = indices(spaces);
        match g.phase {
            Phase::Begin => {
                if g.fingers < MIN_FINGERS {
                    return;
                }
                let base_p = self.position_at(g.time_us, active, last);
                let base_o = self.overview_at(g.time_us);
                self.state = State::Pending {
                    lock: AxisLock::new(),
                    start: active,
                    base_p,
                    base_o,
                };
            }
            Phase::Update => {
                if g.fingers < MIN_FINGERS {
                    return;
                }
                self.update(g);
            }
            Phase::End => self.end(g, spaces, active, last),
        }
    }

    fn update(&mut self, g: GestureIn) {
        match &mut self.state {
            State::Pending {
                lock,
                start,
                base_p,
                base_o,
            } => {
                let Some(axis) = lock.update(g.dx, g.dy) else {
                    return;
                };
                let (cx, cy) = lock.cumulative();
                let (start, base_p, base_o) = (*start, *base_p, *base_o);
                let mut tracker = Tracker::new();
                self.state = match axis {
                    Axis::Horizontal if base_o <= 0.0 => {
                        tracker.push(cx, g.time_us);
                        State::DragSpaces {
                            start,
                            base_p,
                            tracker,
                        }
                    }
                    Axis::Horizontal => State::Ignoring { base_o },
                    Axis::Vertical => {
                        tracker.push(cy, g.time_us);
                        State::DragOverview { base_o, tracker }
                    }
                };
            }
            State::DragSpaces { tracker, .. } => tracker.push(g.dx, g.time_us),
            State::DragOverview { tracker, .. } => tracker.push(g.dy, g.time_us),
            _ => {}
        }
    }

    fn end(&mut self, g: GestureIn, spaces: &mut Spaces, active: usize, last: usize) {
        let t = g.time_us;
        let last_f = last as f64;
        let state = core::mem::replace(&mut self.state, State::Rest);
        match state {
            State::DragSpaces {
                start,
                base_p,
                mut tracker,
            } => {
                tracker.push(0.0, t);
                let p = Band::GESTURE.clamp(0.0, last_f, base_p - tracker.pos() / SWIPE_DISTANCE);
                let start = start.min(last);
                let (target, velocity) = if g.cancelled {
                    (start, 0.0)
                } else {
                    let r =
                        release_target(p, -tracker.velocity() / SWIPE_DISTANCE, start, last, false);
                    (r.target, r.velocity)
                };
                spaces.go_to(target);
                self.open = false;
                self.state = spring_state_spaces(p, target, velocity, t);
                self.sync_mru(spaces);
            }
            State::DragOverview {
                base_o,
                mut tracker,
            } => {
                tracker.push(0.0, t);
                let o = Band::GESTURE.clamp(0.0, 1.0, base_o - tracker.pos() / OVERVIEW_DISTANCE);
                let v = -tracker.velocity() / OVERVIEW_DISTANCE;
                let (open, velocity) = if g.cancelled {
                    (base_o >= 0.5, 0.0)
                } else {
                    (
                        commit_overview(o, v),
                        v * Band::GESTURE.clamp_derivative(0.0, 1.0, o),
                    )
                };
                self.state = spring_state_overview(o, open, velocity, t);
            }
            State::Pending { base_p, base_o, .. } => {
                self.settle_held(t, base_p, base_o, active, last)
            }
            State::Ignoring { base_o } => self.settle_held(t, active as f64, base_o, active, last),
            other => self.state = other,
        }
    }

    /// A gesture ended without moving anything: bring the position it held (from a takeover of a running spring) to rest.
    fn settle_held(&mut self, t: u64, p: f64, o: f64, active: usize, last: usize) {
        let p = clamp_f(p, 0.0, last as f64);
        if o > 0.0 && o < 1.0 {
            self.state = spring_state_overview(o, commit_overview(o, 0.0), 0.0, t);
        } else if o >= 1.0 {
            self.open = true;
        } else {
            self.open = false;
            if p != active as f64 {
                self.state = spring_state_spaces(p, active, 0.0, t);
            }
        }
    }

    /// Build the frame for time `now_us`; call once per vblank while the last scene had `animating`, and once after any input. A pure
    /// function of the controller state and `now_us`: springs run on absolute time, so skipped frames do not change the result.
    /// The indices are clamped to the row every frame, so a row that shrank between frames is safe.
    pub fn scene(&mut self, now_us: u64, spaces: &Spaces, out: &mut Scene) {
        let (active, last) = indices(spaces);
        match &self.state {
            State::SettleSpaces { t0, duration_s, .. } if elapsed_s(now_us, *t0) >= *duration_s => {
                self.state = State::Rest;
                self.open = false;
            }
            State::SettleOverview {
                t0,
                duration_s,
                open,
                ..
            } if elapsed_s(now_us, *t0) >= *duration_s => {
                self.open = *open;
                self.state = State::Rest;
            }
            _ => {}
        }
        if self.popup_due(now_us) {
            self.write_switcher(spaces, out);
            return;
        }
        let o = self.overview_at(now_us);
        let p = self.position_at(now_us, active, last);
        out.len = 0;
        out.animating = !matches!(self.state, State::Rest);
        let s = self.screen;
        if o <= 0.0 {
            out.mode = Mode::Spaces;
            let lo = libm::floor(p) as i64;
            let hi = libm::ceil(p) as i64;
            let step = (s.w as i64) + GAP as i64;
            for i in lo..=hi.min(lo + 1) {
                if i < 0 || i > last as i64 {
                    continue;
                }
                let offset = libm::round((i as f64 - p) * step as f64) as i32;
                out.push(
                    i as usize,
                    Rect {
                        x: s.x.saturating_add(offset),
                        ..s
                    },
                );
            }
        } else {
            out.mode = Mode::Overview;
            let zoom = zoom_at(libm::round(o * 1000.0) as i32, OVERVIEW_ZOOM_PERMILLE);
            for i in (0..=last).filter(|&i| i != active) {
                out.push(i, space_rect(s, zoom, i, active));
            }
            out.push(active, space_rect(s, zoom, active, active));
        }
        if self.switcher.is_some() {
            out.animating = true;
        }
    }

    /// Keyboard and other triggers: switch to space `target` (clamped to the row) with the spring from rest, from the position shown now.
    /// `spaces.go_to` is called at once. With the overview open or moving the active space just changes (the overview recentres on it).
    pub fn go_to(&mut self, now_us: u64, target: usize, spaces: &mut Spaces) {
        let (active, last) = indices(spaces);
        let target = target.min(last);
        let o = self.overview_at(now_us);
        let p = self.position_at(now_us, active, last);
        spaces.go_to(target);
        self.sync_mru(spaces);
        if o > 0.0 {
            return;
        }
        self.open = false;
        self.state = spring_state_spaces(p, target, 0.0, now_us);
    }

    /// Open or close the overview with the spring from rest, from the progress shown now. Does nothing when it is there already. A space
    /// switch that is still settling is cut short (the view jumps to the new active space).
    pub fn set_overview(&mut self, now_us: u64, open: bool) {
        let o = self.overview_at(now_us);
        let target = if open { 1.0 } else { 0.0 };
        if o == target {
            return;
        }
        self.state = spring_state_overview(o, open, 0.0, now_us);
    }

    /// Open a Meta+Tab / Alt+Tab session. The list is the MRU snapshot ([`Switcher::begin`]): the focused app first, the
    /// selection on the previous one. The popup stays hidden until [`POPUP_DELAY_US`] has passed. A session already open
    /// is replaced. The gesture state underneath is left as it is, so [`Self::switcher_cancel`] puts the previous mode back.
    pub fn switcher_begin(&mut self, spaces: &Spaces, now_us: u64) {
        self.sync_mru(spaces);
        let overview = self.overview_at(now_us) > 0.0;
        self.switcher = Some(SwitcherHold {
            switcher: Switcher::begin(&self.mru, false),
            begun_us: now_us,
            overview,
        });
    }

    /// Tab (`forward`) or Shift+Tab moves the selection, wrapping at either end. Does nothing when no session is open.
    pub fn switcher_step(&mut self, forward: bool) {
        if let Some(hold) = self.switcher.as_mut() {
            hold.switcher.step(!forward);
        }
    }

    /// Release. Goes to the selected app's space and returns that index. `None`, with the mode put back and no space
    /// changed, when no session is open, the list is empty or covers a single space, or the selected app is gone.
    pub fn switcher_commit(&mut self, spaces: &mut Spaces) -> Option<usize> {
        let hold = self.switcher.take()?;
        if distinct_spaces(&hold.switcher, spaces) < 2 {
            return None;
        }
        let app = hold.switcher.selected()?;
        let idx = spaces.space_of(app)?;
        spaces.go_to(idx);
        self.sync_mru(spaces);
        self.mru.focus(app);
        self.state = State::Rest;
        self.open = hold.overview;
        Some(idx)
    }

    /// Drop the session. The space and the mode underneath are unchanged.
    pub fn switcher_cancel(&mut self) {
        self.switcher = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spaces::Space;
    use crate::switcher::AppId;

    const SCREEN: Rect = Rect {
        x: 0,
        y: 0,
        w: 1920,
        h: 1080,
    };

    fn row(active: usize) -> Spaces {
        let mut s = Spaces::new();
        for id in 1..=3u16 {
            s.open_app(AppId(id));
        }
        s.go_to(active);
        s
    }

    fn g(phase: Phase, dx: f64, dy: f64, t_ms: u64) -> GestureIn {
        GestureIn {
            swipe: true,
            phase,
            fingers: 3,
            cancelled: false,
            dx,
            dy,
            time_us: t_ms * 1000,
        }
    }

    fn frame(c: &mut Controller, t_ms: u64, s: &Spaces) -> Scene {
        let mut out = Scene::new();
        c.scene(t_ms * 1000, s, &mut out);
        out
    }

    /// A swipe of `total` units (x or y) in 10 steps of 10 ms from `t0_ms`; returns the time of the last update.
    fn swipe(
        c: &mut Controller,
        s: &mut Spaces,
        t0_ms: u64,
        total: f64,
        vertical: bool,
        step_ms: u64,
    ) -> u64 {
        c.gesture(g(Phase::Begin, 0.0, 0.0, t0_ms), s);
        let d = total / 10.0;
        let mut t = t0_ms;
        for _ in 0..10 {
            t += step_ms;
            let (dx, dy) = if vertical { (0.0, d) } else { (d, 0.0) };
            c.gesture(g(Phase::Update, dx, dy, t), s);
        }
        t
    }

    fn idle_scene(active: usize) -> Scene {
        let mut sc = Scene::new();
        sc.push(active, SCREEN);
        sc
    }

    fn same_layers(a: &Scene, b: &Scene) -> bool {
        a.as_slice() == b.as_slice() && a.mode == b.mode
    }

    #[test]
    fn idle_is_one_full_screen_layer() {
        let s = row(2);
        let mut c = Controller::new(SCREEN);
        let sc = frame(&mut c, 0, &s);
        assert_eq!(sc.as_slice(), idle_scene(2).as_slice());
        assert_eq!(sc.mode, Mode::Spaces);
        assert!(!sc.animating);
    }

    #[test]
    fn slow_swipe_leaves_half_offset() {
        let mut s = row(1);
        let mut c = Controller::new(SCREEN);
        swipe(&mut c, &mut s, 0, 200.0, false, 10);
        let sc = frame(&mut c, 200, &s);
        assert!(sc.animating);
        assert_eq!(sc.len, 2);
        let half = (1920 + GAP) / 2;
        assert_eq!(sc.layers[0].space, 0);
        assert_eq!(sc.layers[0].to, Rect { x: -half, ..SCREEN });
        assert_eq!(sc.layers[1].space, 1);
        assert_eq!(sc.layers[1].to, Rect { x: half, ..SCREEN });
        assert!(sc.as_slice().iter().all(|l| l.opacity == 255));
        assert_eq!(s.active(), 1, "no switch before release");
    }

    #[test]
    fn slow_release_snaps_to_closest() {
        for (dx, want) in [(200.0, 1), (240.0, 0), (-240.0, 2)] {
            let mut s = row(1);
            let mut c = Controller::new(SCREEN);
            swipe(&mut c, &mut s, 0, dx, false, 10);
            c.gesture(g(Phase::End, 0.0, 0.0, 2000), &mut s);
            assert_eq!(s.active(), want, "dx {dx}");
            let sc = frame(&mut c, 2000, &s);
            assert!(sc.animating);
            let sc = frame(&mut c, 5000, &s);
            assert!(!sc.animating);
            assert_eq!(sc.as_slice(), idle_scene(want).as_slice());
        }
    }

    #[test]
    fn fast_flick_moves_exactly_one_space() {
        let mut s = row(0);
        let mut c = Controller::new(SCREEN);
        // 80 units left in 40 ms: p = 0.2, 5 spaces per second, enough for a long fling that is capped to one space.
        swipe(&mut c, &mut s, 0, -80.0, false, 4);
        c.gesture(g(Phase::End, 0.0, 0.0, 41), &mut s);
        assert_eq!(s.active(), 1);
        // Flicking the other way on space 3 moves one space back.
        let mut s = row(3);
        let mut c = Controller::new(SCREEN);
        swipe(&mut c, &mut s, 0, 80.0, false, 4);
        c.gesture(g(Phase::End, 0.0, 0.0, 41), &mut s);
        assert_eq!(s.active(), 2);
    }

    #[test]
    fn past_the_last_space_is_rubber_banded() {
        let mut s = row(3);
        let mut c = Controller::new(SCREEN);
        c.gesture(g(Phase::Begin, 0.0, 0.0, 0), &mut s);
        for i in 1..=100u64 {
            c.gesture(g(Phase::Update, -50.0, 0.0, i * 8), &mut s);
            let sc = frame(&mut c, i * 8, &s);
            // p = 3 + band, shown as the offset of the last layer: x = (3 - p) * (w + gap), which is non-positive.
            let last_layer = sc.layers[sc.len - 1];
            assert_eq!(last_layer.space, 3);
            let p = 3.0 - last_layer.to.x as f64 / 2020.0;
            assert!(p <= 3.05, "p {p}");
        }
        c.gesture(g(Phase::End, 0.0, 0.0, 2000), &mut s);
        assert_eq!(s.active(), 3);
        let sc = frame(&mut c, 6000, &s);
        assert_eq!(sc.as_slice(), idle_scene(3).as_slice());
        assert!(!sc.animating);
    }

    #[test]
    fn rubber_band_at_the_first_space() {
        let mut s = row(0);
        let mut c = Controller::new(SCREEN);
        c.gesture(g(Phase::Begin, 0.0, 0.0, 0), &mut s);
        c.gesture(g(Phase::Update, 5000.0, 0.0, 10), &mut s);
        let sc = frame(&mut c, 10, &s);
        assert_eq!(sc.len, 1, "no space before the first");
        let shift = sc.layers[0].to.x;
        assert!(
            shift > 0 && shift <= (0.05 * 2020.0) as i32 + 1,
            "shift {shift}"
        );
        c.gesture(g(Phase::End, 0.0, 0.0, 2000), &mut s);
        assert_eq!(s.active(), 0);
    }

    #[test]
    fn settling_ends_within_three_seconds() {
        let mut s = row(1);
        let mut c = Controller::new(SCREEN);
        let t = swipe(&mut c, &mut s, 0, -300.0, false, 10);
        c.gesture(g(Phase::End, 0.0, 0.0, t), &mut s);
        assert_eq!(s.active(), 2);
        let mid = frame(&mut c, t + 50, &s);
        assert!(mid.animating);
        let done = frame(&mut c, t + 3000, &s);
        assert!(!done.animating);
        assert!(same_layers(&done, &idle_scene(2)));
    }

    #[test]
    fn vertical_swipe_up_opens_the_overview() {
        let mut s = row(2);
        let mut c = Controller::new(SCREEN);
        swipe(&mut c, &mut s, 0, -150.0, true, 10);
        let sc = frame(&mut c, 100, &s);
        assert_eq!(sc.mode, Mode::Overview);
        assert!(sc.animating);
        assert_eq!(sc.len, 4);
        let zoom = zoom_at(500, OVERVIEW_ZOOM_PERMILLE);
        assert_eq!(zoom, 800);
        assert_eq!(sc.layers[3].space, 2, "the active space is on top");
        for l in sc.as_slice() {
            assert_eq!(l.to, space_rect(SCREEN, zoom, l.space, 2));
        }
        let mut spaces: [usize; 4] = [0; 4];
        for (k, l) in sc.as_slice().iter().enumerate() {
            spaces[k] = l.space;
        }
        spaces.sort_unstable();
        assert_eq!(spaces, [0, 1, 2, 3]);
    }

    #[test]
    fn overview_release_commits_by_the_rules() {
        // Slow release at 0.5 opens (progress >= 0.5), at 0.3 closes.
        for (dy, open) in [(-150.0, true), (-90.0, false)] {
            let mut s = row(1);
            let mut c = Controller::new(SCREEN);
            swipe(&mut c, &mut s, 0, dy, true, 10);
            c.gesture(g(Phase::End, 0.0, 0.0, 3000), &mut s);
            let sc = frame(&mut c, 7000, &s);
            assert!(!sc.animating);
            if open {
                assert_eq!(sc.mode, Mode::Overview);
                let a = sc.layers[sc.len - 1];
                assert_eq!(a.space, 1);
                assert_eq!(a.to, space_rect(SCREEN, OVERVIEW_ZOOM_PERMILLE, 1, 1));
            } else {
                assert!(same_layers(&sc, &idle_scene(1)));
            }
        }
        // A fast flick up (90 units in 36 ms, 2.5/s) opens although progress is only 0.3.
        let mut s = row(1);
        let mut c = Controller::new(SCREEN);
        let t = swipe(&mut c, &mut s, 0, -90.0, true, 4);
        c.gesture(g(Phase::End, 0.0, 0.0, t + 1), &mut s);
        assert_eq!(frame(&mut c, t + 4000, &s).mode, Mode::Overview);
        // Down while open closes.
        c.gesture(g(Phase::Begin, 0.0, 0.0, 9000), &mut s);
        for i in 1..=10u64 {
            c.gesture(g(Phase::Update, 0.0, 30.0, 9000 + i * 10), &mut s);
        }
        c.gesture(g(Phase::End, 0.0, 0.0, 12000), &mut s);
        let sc = frame(&mut c, 16000, &s);
        assert!(same_layers(&sc, &idle_scene(1)));
    }

    #[test]
    fn overview_is_continuous_at_both_ends() {
        let s = row(2);
        let mut c = Controller::new(SCREEN);
        c.set_overview(0, true);
        let open = frame(&mut c, 0, &s);
        assert_eq!(open.mode, Mode::Spaces);
        assert!(
            same_layers(&open, &idle_scene(2)),
            "o = 0 is the idle scene"
        );
        let end = frame(&mut c, 4_000, &s);
        assert!(!end.animating);
        assert_eq!(end.mode, Mode::Overview);
        let a = end.layers[end.len - 1];
        assert_eq!(a.space, 2);
        assert_eq!(a.to, space_rect(SCREEN, 600, 2, 2));
        // A frame just after the start already shows all spaces with the active one almost full screen.
        let mut c = Controller::new(SCREEN);
        c.set_overview(0, true);
        let early = frame(&mut c, 20, &s);
        assert_eq!(early.mode, Mode::Overview);
        let a = early.layers[early.len - 1];
        assert!(a.to.w <= 1920 && a.to.w > 1500);
        c.set_overview(30_000, false);
    }

    #[test]
    fn cancelled_end_returns_to_the_start() {
        let mut s = row(1);
        let mut c = Controller::new(SCREEN);
        swipe(&mut c, &mut s, 0, -300.0, false, 10);
        let mut end = g(Phase::End, 0.0, 0.0, 120);
        end.cancelled = true;
        c.gesture(end, &mut s);
        assert_eq!(s.active(), 1);
        assert!(same_layers(&frame(&mut c, 6000, &s), &idle_scene(1)));

        // Cancelled overview drag closes again.
        swipe(&mut c, &mut s, 7000, -240.0, true, 10);
        let mut end = g(Phase::End, 0.0, 0.0, 7200);
        end.cancelled = true;
        c.gesture(end, &mut s);
        assert!(same_layers(&frame(&mut c, 12000, &s), &idle_scene(1)));
    }

    #[test]
    fn begin_during_settling_continues_from_the_current_position() {
        let mut s = row(1);
        let mut c = Controller::new(SCREEN);
        let t = swipe(&mut c, &mut s, 0, -300.0, false, 10);
        c.gesture(g(Phase::End, 0.0, 0.0, t), &mut s);
        let at = t + 60;
        let before = frame(&mut c, at, &s);
        assert!(before.animating);
        c.gesture(g(Phase::Begin, 0.0, 0.0, at), &mut s);
        c.gesture(g(Phase::Update, 0.0, 0.0, at), &mut s);
        let after = frame(&mut c, at, &s);
        assert_eq!(after.len, before.len);
        for (a, b) in after.as_slice().iter().zip(before.as_slice()) {
            assert_eq!(a.space, b.space);
            assert!((a.to.x - b.to.x).abs() <= 1, "{} vs {}", a.to.x, b.to.x);
        }
        assert!(after.animating, "finger is down");
    }

    #[test]
    fn begin_during_overview_settle_continues_from_the_current_progress() {
        let mut s = row(1);
        let mut c = Controller::new(SCREEN);
        c.set_overview(0, true);
        let before = frame(&mut c, 100, &s);
        c.gesture(g(Phase::Begin, 0.0, 0.0, 100), &mut s);
        let after = frame(&mut c, 100, &s);
        assert_eq!(
            before,
            Scene {
                animating: true,
                ..before
            }
        );
        assert_eq!(after.as_slice(), before.as_slice());
        // Horizontal movement while the overview is (partly) open is ignored.
        for i in 1..=10u64 {
            c.gesture(g(Phase::Update, 40.0, 0.0, 100 + i * 10), &mut s);
        }
        assert_eq!(frame(&mut c, 200, &s).as_slice(), before.as_slice());
        c.gesture(g(Phase::End, 0.0, 0.0, 200), &mut s);
        assert_eq!(s.active(), 1);
        assert_eq!(frame(&mut c, 5000, &s).mode, Mode::Overview);
    }

    #[test]
    fn shrinking_row_does_not_panic() {
        let mut s = row(3);
        let mut c = Controller::new(SCREEN);
        c.gesture(g(Phase::Begin, 0.0, 0.0, 0), &mut s);
        c.gesture(g(Phase::Update, -300.0, 0.0, 10), &mut s);
        s.close_app(AppId(3));
        s.close_app(AppId(2));
        s.close_app(AppId(1));
        assert_eq!(s.len(), 1);
        let sc = frame(&mut c, 20, &s);
        assert!(sc.len >= 1 && sc.len <= MAX_LAYERS);
        c.gesture(g(Phase::Update, -300.0, 0.0, 30), &mut s);
        c.gesture(g(Phase::End, 0.0, 0.0, 40), &mut s);
        assert_eq!(s.active(), 0);
        let sc = frame(&mut c, 5000, &s);
        assert_eq!(sc.as_slice(), idle_scene(0).as_slice());

        let mut s = row(3);
        let mut c = Controller::new(SCREEN);
        c.set_overview(0, true);
        let _ = frame(&mut c, 5000, &s);
        s.close_app(AppId(1));
        s.close_app(AppId(2));
        let sc = frame(&mut c, 5001, &s);
        assert_eq!(sc.len, 2);
        c.go_to(5_002_000, 9, &mut s);
        assert_eq!(s.active(), 1);
    }

    #[test]
    fn ignored_input_changes_nothing() {
        let mut s = row(1);
        let mut c = Controller::new(SCREEN);
        let base = frame(&mut c, 0, &s);
        let mut pinch = g(Phase::Begin, 0.0, 0.0, 1);
        pinch.swipe = false;
        c.gesture(pinch, &mut s);
        pinch.phase = Phase::Update;
        pinch.dx = 300.0;
        c.gesture(pinch, &mut s);
        let mut two = g(Phase::Begin, 0.0, 0.0, 2);
        two.fingers = 2;
        c.gesture(two, &mut s);
        two.phase = Phase::Update;
        two.dx = 300.0;
        c.gesture(two, &mut s);
        c.gesture(g(Phase::Update, 300.0, 0.0, 3), &mut s);
        c.gesture(g(Phase::End, 0.0, 0.0, 4), &mut s);
        assert_eq!(frame(&mut c, 5, &s), base);
        assert_eq!(s.active(), 1);

        // A two-finger update in the middle of a three-finger gesture adds nothing.
        c.gesture(g(Phase::Begin, 0.0, 0.0, 10), &mut s);
        c.gesture(g(Phase::Update, 100.0, 0.0, 20), &mut s);
        let a = frame(&mut c, 20, &s);
        let mut stray = g(Phase::Update, 100.0, 0.0, 30);
        stray.fingers = 2;
        c.gesture(stray, &mut s);
        assert_eq!(frame(&mut c, 20, &s), a);
    }

    #[test]
    fn begin_in_progress_restarts_from_the_shown_position() {
        let mut s = row(1);
        let mut c = Controller::new(SCREEN);
        swipe(&mut c, &mut s, 0, 200.0, false, 10);
        let before = frame(&mut c, 100, &s);
        c.gesture(g(Phase::Begin, 0.0, 0.0, 100), &mut s);
        assert_eq!(frame(&mut c, 100, &s).as_slice(), before.as_slice());
        c.gesture(g(Phase::Update, 100.0, 0.0, 110), &mut s);
        let moved = frame(&mut c, 110, &s);
        assert_ne!(moved.as_slice(), before.as_slice());
    }

    #[test]
    fn go_to_and_set_overview_start_from_rest() {
        let mut s = row(0);
        let mut c = Controller::new(SCREEN);
        c.go_to(0, 2, &mut s);
        assert_eq!(s.active(), 2);
        let start = frame(&mut c, 0, &s);
        assert_eq!(start.len, 1, "starts exactly at the old position");
        assert_eq!(start.layers[0].space, 0);
        assert_eq!(start.layers[0].to, SCREEN);
        assert!(frame(&mut c, 10, &s).animating);
        assert!(same_layers(&frame(&mut c, 4000, &s), &idle_scene(2)));

        c.set_overview(5_000_000, true);
        assert!(frame(&mut c, 5010, &s).animating);
        let open = frame(&mut c, 9000, &s);
        assert_eq!(open.mode, Mode::Overview);
        assert!(!open.animating);
        c.go_to(9_001_000, 0, &mut s);
        assert_eq!(s.active(), 0);
        let sc = frame(&mut c, 9002, &s);
        assert_eq!(sc.mode, Mode::Overview);
        assert_eq!(sc.layers[sc.len - 1].space, 0);
        c.set_overview(9_003_000, false);
        assert!(same_layers(&frame(&mut c, 14000, &s), &idle_scene(0)));
    }

    fn script(seed: u64, c: &mut Controller, s: &mut Spaces, trace: &mut [Scene]) {
        let mut x = seed;
        let mut next = move || {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (x >> 33) as u32
        };
        let mut t: u64 = 0;
        let mut k = 0;
        for _ in 0..12 {
            let fingers = 2 + (next() % 3) as u8;
            let mk = |phase, dx, dy, t, cancelled| GestureIn {
                swipe: true,
                phase,
                fingers,
                cancelled,
                dx,
                dy,
                time_us: t,
            };
            t += 1000 * (1 + (next() % 20) as u64);
            c.gesture(mk(Phase::Begin, 0.0, 0.0, t, false), s);
            for _ in 0..(next() % 12) {
                t += 1000 * (1 + (next() % 30) as u64);
                let dx = (next() % 400) as f64 - 200.0;
                let dy = (next() % 400) as f64 - 200.0;
                c.gesture(mk(Phase::Update, dx, dy, t, false), s);
                c.scene(t, s, &mut trace[k % trace.len()]);
                k += 1;
            }
            t += 1000 * (1 + (next() % 400) as u64);
            c.gesture(mk(Phase::End, 0.0, 0.0, t, next() % 5 == 0), s);
            if next() % 7 == 0 {
                c.go_to(t, (next() % 6) as usize, s);
            }
            if next() % 9 == 0 {
                c.set_overview(t, next() % 2 == 0);
            }
            c.scene(t, s, &mut trace[k % trace.len()]);
            k += 1;
            t += 1000 * (next() % 600) as u64;
        }
    }

    #[test]
    fn determinism() {
        let run = || {
            let mut s = row(1);
            let mut c = Controller::new(SCREEN);
            let mut trace = [Scene::new(); 16];
            script(7, &mut c, &mut s, &mut trace);
            (trace, s.active())
        };
        assert_eq!(run(), run());
    }

    #[test]
    fn frame_skipping_does_not_change_a_frame() {
        let mut s = row(1);
        let mut c = Controller::new(SCREEN);
        let t = swipe(&mut c, &mut s, 0, -300.0, false, 10);
        c.gesture(g(Phase::End, 0.0, 0.0, t), &mut s);
        let mut d = c.clone();
        let _ = frame(&mut c, t + 16, &s);
        let _ = frame(&mut c, t + 32, &s);
        assert_eq!(frame(&mut c, t + 100, &s), frame(&mut d, t + 100, &s));
    }

    #[test]
    fn random_scripts_hold_the_invariants() {
        for seed in 0..200u64 {
            let mut s = row((seed % 4) as usize);
            let mut c = Controller::new(SCREEN);
            let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
            let mut next = move || {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x
            };
            let mut t: u64 = 0;
            for _ in 0..40 {
                let phase = match next() % 8 {
                    0 => Phase::Begin,
                    1 => Phase::End,
                    _ => Phase::Update,
                };
                t += 1000 * (1 + next() % 40);
                let gi = GestureIn {
                    swipe: next() % 10 != 0,
                    phase,
                    fingers: 2 + (next() % 3) as u8,
                    cancelled: next() % 6 == 0,
                    dx: (next() % 600) as f64 - 300.0,
                    dy: (next() % 600) as f64 - 300.0,
                    time_us: t,
                };
                c.gesture(gi, &mut s);
                if next() % 25 == 0 {
                    s.close_app(AppId(1 + (next() % 3) as u16));
                }
                if next() % 25 == 0 {
                    s.open_app(AppId(1 + (next() % 5) as u16));
                }
                let mut sc = Scene::new();
                c.scene(t, &s, &mut sc);
                assert!(sc.len >= 1 && sc.len <= MAX_LAYERS, "seed {seed}");
                for l in sc.as_slice() {
                    // Never zero-sized; not clipped to the screen (an overview neighbour may lie outside it).
                    assert!(l.to.w > 0 && l.to.h > 0, "seed {seed}: {:?}", l.to);
                    assert!(l.space < s.len(), "seed {seed}");
                }
            }
            // Finish whatever is in progress; after an End and enough time the controller must be idle.
            c.gesture(
                GestureIn {
                    swipe: true,
                    phase: Phase::End,
                    fingers: 3,
                    cancelled: false,
                    dx: 0.0,
                    dy: 0.0,
                    time_us: t + 1000,
                },
                &mut s,
            );
            let mut sc = Scene::new();
            c.scene(t + 10_000_000, &s, &mut sc);
            assert!(!sc.animating, "seed {seed}");
        }
    }

    fn apps(ids: &[u16]) -> Spaces {
        let mut s = Spaces::new();
        for id in ids {
            assert!(s.open_app(AppId(*id)).is_some(), "room for {id}");
        }
        s
    }

    fn scene_at(c: &mut Controller, t_us: u64, s: &Spaces) -> Scene {
        let mut out = Scene::new();
        c.scene(t_us, s, &mut out);
        out
    }

    /// The one tile drawn at full opacity. Every other tile is dimmed.
    fn highlighted(sc: &Scene) -> usize {
        let mut found = None;
        for l in sc.as_slice() {
            if l.opacity == 255 {
                assert!(found.is_none(), "one tile at full opacity");
                found = Some(l.space);
            } else {
                assert_eq!(l.opacity, 160, "dimmed tile");
            }
        }
        found.expect("the selected tile")
    }

    fn inside(screen: Rect, r: Rect) -> bool {
        let sx = screen.x as i64;
        let sy = screen.y as i64;
        let rx = r.x as i64;
        let ry = r.y as i64;
        r.w > 0
            && r.h > 0
            && screen.w > 0
            && screen.h > 0
            && rx >= sx
            && ry >= sy
            && rx + r.w as i64 <= sx + screen.w as i64
            && ry + r.h as i64 <= sy + screen.h as i64
    }

    #[test]
    fn begin_then_immediate_commit_selects_the_second_mru_app_and_shows_no_popup() {
        let mut s = apps(&[1, 2, 3]);
        assert_eq!(s.active_space(), Space::App(AppId(3)));
        let mut c = Controller::new(SCREEN);
        let idle = scene_at(&mut c, 10, &s);
        c.switcher_begin(&s, 10);
        let early = scene_at(&mut c, 10, &s);
        assert_eq!(early.mode, Mode::Spaces);
        assert_ne!(early.mode, Mode::Switcher);
        assert_eq!(early.as_slice(), idle.as_slice());
        assert!(early.animating, "the shell has to come back for the popup");
        let idx = c.switcher_commit(&mut s).expect("three apps");
        assert_eq!(s.space_of(AppId(2)), Some(idx), "the previous app");
        assert_eq!(s.active(), idx);
        let landed = scene_at(&mut c, 10, &s);
        assert_eq!(landed.mode, Mode::Spaces);
        assert!(!landed.animating);
        assert_eq!(landed.as_slice(), idle_scene(idx).as_slice());
        // Releasing again flips back: the app just chosen is now the front of the MRU.
        c.switcher_begin(&s, 20);
        let back = c.switcher_commit(&mut s).expect("still three apps");
        assert_eq!(s.space_of(AppId(3)), Some(back));
        assert_eq!(s.active(), back);
    }

    #[test]
    fn commit_after_the_delay_shows_the_popup() {
        let mut s = apps(&[1, 2, 3]);
        let mut c = Controller::new(SCREEN);
        let t0 = 1_000_000u64;
        c.switcher_begin(&s, t0);
        let before = scene_at(&mut c, t0 + POPUP_DELAY_US - 1, &s);
        assert_eq!(before.mode, Mode::Spaces);
        assert_eq!(before.as_slice(), idle_scene(3).as_slice());
        let pop = scene_at(&mut c, t0 + POPUP_DELAY_US, &s);
        assert_eq!(pop.mode, Mode::Switcher);
        assert!(pop.animating);
        assert_eq!(pop.len, 3);
        let order = [
            s.space_of(AppId(3)).unwrap(),
            s.space_of(AppId(2)).unwrap(),
            s.space_of(AppId(1)).unwrap(),
        ];
        for (i, space) in order.iter().enumerate() {
            assert_eq!(pop.layers[i].space, *space);
        }
        assert_eq!(highlighted(&pop), s.space_of(AppId(2)).unwrap());
        let w = pop.layers[0].to.w;
        let h = pop.layers[0].to.h;
        assert!(w > 0 && h > 0);
        for l in pop.as_slice() {
            assert_eq!(l.to.w, w);
            assert_eq!(l.to.h, h);
            assert_eq!(l.to.y, pop.layers[0].to.y);
            assert!(inside(SCREEN, l.to));
        }
        assert!(pop.layers[1].to.x > pop.layers[0].to.x);
        assert!(pop.layers[2].to.x > pop.layers[1].to.x);
        let gap = pop.layers[1].to.x - (pop.layers[0].to.x + pop.layers[0].to.w);
        assert_eq!(gap, 24);
        assert_eq!(
            pop.layers[2].to.x - (pop.layers[1].to.x + pop.layers[1].to.w),
            gap
        );
        let left = pop.layers[0].to.x - SCREEN.x;
        let right = SCREEN.x + SCREEN.w - (pop.layers[2].to.x + pop.layers[2].to.w);
        assert!(
            (left - right).abs() <= 1,
            "centred, left {left} right {right}"
        );
        // Asking again before the delay hides it; the session itself is still open.
        let back = scene_at(&mut c, t0, &s);
        assert_eq!(back.mode, Mode::Spaces);
        let idx = c.switcher_commit(&mut s).unwrap();
        assert_eq!(Some(idx), s.space_of(AppId(2)));
        assert_eq!(scene_at(&mut c, t0 + POPUP_DELAY_US, &s).mode, Mode::Spaces);
    }

    #[test]
    fn step_forward_and_backward_wrap() {
        let s = apps(&[1, 2, 3]);
        let mut c = Controller::new(SCREEN);
        let t0 = 0u64;
        c.switcher_begin(&s, t0);
        let at = t0 + POPUP_DELAY_US;
        let space = |id: u16| s.space_of(AppId(id)).unwrap();
        // Begin sits on the previous app. Forward walks 1, then 3, then back to 2.
        assert_eq!(highlighted(&scene_at(&mut c, at, &s)), space(2));
        c.switcher_step(true);
        assert_eq!(highlighted(&scene_at(&mut c, at, &s)), space(1));
        c.switcher_step(true);
        assert_eq!(highlighted(&scene_at(&mut c, at, &s)), space(3));
        c.switcher_step(true);
        assert_eq!(highlighted(&scene_at(&mut c, at, &s)), space(2));
        c.switcher_step(false);
        assert_eq!(highlighted(&scene_at(&mut c, at, &s)), space(3));
        c.switcher_step(false);
        assert_eq!(highlighted(&scene_at(&mut c, at, &s)), space(1));
        c.switcher_step(false);
        assert_eq!(highlighted(&scene_at(&mut c, at, &s)), space(2));
    }

    #[test]
    fn shift_tab_from_the_first_selects_the_last() {
        let mut s = apps(&[1, 2, 3]);
        let mut c = Controller::new(SCREEN);
        c.switcher_begin(&s, 0);
        let at = POPUP_DELAY_US;
        // One Shift+Tab from the previous app lands on the first MRU entry.
        c.switcher_step(false);
        assert_eq!(
            highlighted(&scene_at(&mut c, at, &s)),
            s.space_of(AppId(3)).unwrap()
        );
        // The next Shift+Tab wraps onto the last entry.
        c.switcher_step(false);
        assert_eq!(
            highlighted(&scene_at(&mut c, at, &s)),
            s.space_of(AppId(1)).unwrap()
        );
        let idx = c.switcher_commit(&mut s).unwrap();
        assert_eq!(Some(idx), s.space_of(AppId(1)));
        assert_eq!(s.active(), idx);
    }

    #[test]
    fn cancel_returns_to_the_previous_mode_and_changes_no_space() {
        let mut s = apps(&[1, 2, 3]);
        let mut c = Controller::new(SCREEN);
        c.switcher_step(true);
        c.switcher_cancel();
        assert!(c.switcher_commit(&mut s).is_none());
        assert_eq!(s.active(), 3);
        let before = scene_at(&mut c, 0, &s);
        let active = s.active();
        c.switcher_begin(&s, 0);
        c.switcher_step(true);
        c.switcher_cancel();
        assert_eq!(s.active(), active);
        let after = scene_at(&mut c, 0, &s);
        assert_eq!(after.mode, Mode::Spaces);
        assert!(!after.animating);
        assert_eq!(after.as_slice(), before.as_slice());

        c.set_overview(1_000_000, true);
        let open = scene_at(&mut c, 5_000_000, &s);
        assert_eq!(open.mode, Mode::Overview);
        assert!(!open.animating);
        let active = s.active();
        c.switcher_begin(&s, 5_000_000);
        c.switcher_step(false);
        let during = scene_at(&mut c, 5_000_000 + POPUP_DELAY_US, &s);
        assert_eq!(during.mode, Mode::Switcher);
        c.switcher_cancel();
        assert_eq!(s.active(), active);
        let back = scene_at(&mut c, 5_000_000 + POPUP_DELAY_US, &s);
        assert_eq!(back.mode, Mode::Overview);
        assert_eq!(back.as_slice(), open.as_slice());
        assert!(!back.animating);
    }

    #[test]
    fn single_space_commit_returns_none() {
        let mut home = Spaces::new();
        let mut c = Controller::new(SCREEN);
        c.switcher_begin(&home, 0);
        assert!(c.switcher_commit(&mut home).is_none());
        assert_eq!(home.active(), 0);
        assert_eq!(scene_at(&mut c, 0, &home).mode, Mode::Spaces);
        assert_eq!(
            scene_at(&mut c, 0, &home).as_slice(),
            idle_scene(0).as_slice()
        );

        let mut one = apps(&[7]);
        let active = one.active();
        c.switcher_begin(&one, 0);
        let pop = scene_at(&mut c, POPUP_DELAY_US, &one);
        assert_eq!(pop.mode, Mode::Switcher);
        assert_eq!(pop.len, 1);
        assert_eq!(pop.layers[0].opacity, 255);
        assert!(inside(SCREEN, pop.layers[0].to));
        assert!(c.switcher_commit(&mut one).is_none());
        assert_eq!(one.active(), active);
        assert_eq!(scene_at(&mut c, POPUP_DELAY_US, &one).mode, Mode::Spaces);

        let mut pair = apps(&[1, 2]);
        assert!(pair.tile_together(AppId(2), AppId(1)));
        let active = pair.active();
        c.set_overview(0, true);
        let open = scene_at(&mut c, 4_000_000, &pair);
        assert_eq!(open.mode, Mode::Overview);
        c.switcher_begin(&pair, 4_000_000);
        c.switcher_step(true);
        assert!(c.switcher_commit(&mut pair).is_none());
        assert_eq!(pair.active(), active);
        let back = scene_at(&mut c, 4_000_000, &pair);
        assert_eq!(back.mode, Mode::Overview);
        assert_eq!(back.as_slice(), open.as_slice());
    }

    #[test]
    fn gestures_are_ignored_while_the_switcher_is_open() {
        let mut s = apps(&[1, 2, 3]);
        let mut c = Controller::new(SCREEN);
        let before = scene_at(&mut c, 0, &s);
        c.switcher_begin(&s, 0);
        swipe(&mut c, &mut s, 0, -400.0, false, 10);
        c.gesture(g(Phase::End, 0.0, 0.0, 200), &mut s);
        let mut up = g(Phase::Begin, 0.0, -80.0, 210);
        c.gesture(up, &mut s);
        up.phase = Phase::Update;
        c.gesture(up, &mut s);
        up.phase = Phase::End;
        c.gesture(up, &mut s);
        assert_eq!(s.active(), 3);
        let held = scene_at(&mut c, 0, &s);
        assert_eq!(held.mode, Mode::Spaces);
        assert_eq!(held.as_slice(), before.as_slice());
        let pop = scene_at(&mut c, POPUP_DELAY_US, &s);
        assert_eq!(pop.mode, Mode::Switcher);
        assert_eq!(highlighted(&pop), s.space_of(AppId(2)).unwrap());
        c.switcher_step(true);
        assert_eq!(
            highlighted(&scene_at(&mut c, POPUP_DELAY_US, &s)),
            s.space_of(AppId(1)).unwrap()
        );
    }

    #[test]
    fn switcher_layers_stay_within_max_layers() {
        let mut s = Spaces::new();
        for id in 1..=15u16 {
            assert!(s.open_app(AppId(id)).is_some());
        }
        assert!(s.tile_together(AppId(15), AppId(14)));
        assert!(s.open_app(AppId(16)).is_some(), "the tile frees a slot");
        let mut c = Controller::new(SCREEN);
        c.switcher_begin(&s, 0);
        let pop = scene_at(&mut c, POPUP_DELAY_US, &s);
        assert_eq!(pop.mode, Mode::Switcher);
        assert_eq!(pop.len, 16, "every app the MRU can hold");
        assert!(pop.len <= MAX_LAYERS);
        let mut full = 0;
        for l in pop.as_slice() {
            assert!(l.space < s.len());
            assert!(l.to.w > 0 && l.to.h > 0);
            assert!(inside(SCREEN, l.to));
            if l.opacity == 255 {
                full += 1;
            } else {
                assert_eq!(l.opacity, 160);
            }
        }
        assert_eq!(full, 1);
    }

    #[test]
    fn switcher_tiles_lie_inside_the_screen() {
        let screens = [
            SCREEN,
            Rect {
                x: 10,
                y: 20,
                w: 640,
                h: 480,
            },
            Rect {
                x: -40,
                y: 5,
                w: 2000,
                h: 100,
            },
            Rect {
                x: 0,
                y: 0,
                w: 3,
                h: 3,
            },
        ];
        for screen in screens {
            let s = apps(&[1, 2, 3, 4, 5]);
            let mut c = Controller::new(screen);
            c.switcher_begin(&s, 0);
            let pop = scene_at(&mut c, POPUP_DELAY_US, &s);
            assert_eq!(pop.mode, Mode::Switcher);
            assert_eq!(pop.len, 5);
            assert!(pop.len <= MAX_LAYERS);
            let w = pop.layers[0].to.w;
            let h = pop.layers[0].to.h;
            for (i, l) in pop.as_slice().iter().enumerate() {
                assert!(inside(screen, l.to), "{screen:?} {:?}", l.to);
                assert_eq!(l.to.w, w);
                assert_eq!(l.to.h, h);
                assert_eq!(l.to.y, pop.layers[0].to.y);
                if i > 0 {
                    assert!(l.to.x >= pop.layers[i - 1].to.x);
                }
            }
            let left = pop.layers[0].to.x as i64 - screen.x as i64;
            let right = screen.x as i64 + screen.w as i64
                - (pop.layers[4].to.x as i64 + pop.layers[4].to.w as i64);
            assert!(
                (left - right).abs() <= 1,
                "{screen:?} left {left} right {right}"
            );
        }
        // Degenerate screens and a clock at the end of time must not panic. A begin at `u64::MAX` can never
        // reach the popup delay; a begin at 0 still draws non-empty tiles.
        for screen in [
            Rect {
                x: 0,
                y: 0,
                w: 0,
                h: 0,
            },
            Rect {
                x: 0,
                y: 0,
                w: -8,
                h: -4,
            },
            Rect {
                x: i32::MAX - 10,
                y: i32::MIN / 2,
                w: 80,
                h: 40,
            },
        ] {
            let mut s = apps(&[1, 2, 3]);
            let mut c = Controller::new(screen);
            c.switcher_begin(&s, u64::MAX);
            assert!(scene_at(&mut c, 0, &s).len <= MAX_LAYERS);
            assert_ne!(scene_at(&mut c, u64::MAX, &s).mode, Mode::Switcher);
            c.switcher_cancel();
            c.switcher_begin(&s, 0);
            let pop = scene_at(&mut c, POPUP_DELAY_US, &s);
            assert_eq!(pop.mode, Mode::Switcher);
            assert!(pop.len <= MAX_LAYERS && pop.len >= 1);
            for l in pop.as_slice() {
                assert!(l.to.w > 0 && l.to.h > 0, "{screen:?} {:?}", l.to);
            }
            c.switcher_step(true);
            c.switcher_step(false);
            let _ = c.switcher_commit(&mut s);
        }
    }

    #[test]
    fn switcher_scene_is_deterministic() {
        let once = || {
            let mut s = apps(&[1, 2, 3, 4]);
            let mut c = Controller::new(SCREEN);
            c.switcher_begin(&s, 50);
            c.switcher_step(true);
            c.switcher_step(false);
            c.switcher_step(true);
            let t = 50 + POPUP_DELAY_US;
            let first = scene_at(&mut c, t, &s);
            let second = scene_at(&mut c, t, &s);
            assert_eq!(first, second);
            let idx = c.switcher_commit(&mut s).unwrap();
            (first, idx, s.active())
        };
        assert_eq!(once(), once());
    }
}
