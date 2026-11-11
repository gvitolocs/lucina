//! The gesture-driven space switcher and overview as a pure state machine: gesture records and time go in, a *scene* (which space's surface
//! goes into which rectangle this frame) comes out. No I/O, no clock (time is passed in), no allocation, no knowledge of the compositor or the
//! device. Behaviour follows niri (`swipe_tracker.rs`, workspace switch and overview gesture), GNOME Shell (`swipeTracker.js`,
//! `WORKSPACE_SPACING`) and KWin's partial activation; no code is taken from them.
//!
//! Springs are advanced by absolute time (`now_us - t0`), so skipping frames never changes what a given instant looks like.
//! Layers are never zero-sized, but they are not clipped to the screen: a layer may lie fully outside it (an overview neighbour), and the
//! caller clips.

use crate::overview::{space_rect, zoom_at};
use crate::rubber::Band;
use crate::spaces::Spaces;
use crate::spring::{Params, Spring};
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
        if self.len < MAX_LAYERS {
            self.layers[self.len] = Layer {
                space,
                to,
                opacity: 255,
            };
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

/// The space switcher and overview.
#[derive(Clone, Debug)]
pub struct Controller {
    screen: Rect,
    state: State,
    /// Whether the overview is open while in [`State::Rest`].
    open: bool,
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
    pub fn gesture(&mut self, g: GestureIn, spaces: &mut Spaces) {
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
    }

    /// Keyboard and other triggers: switch to space `target` (clamped to the row) with the spring from rest, from the position shown now.
    /// `spaces.go_to` is called at once. With the overview open or moving the active space just changes (the overview recentres on it).
    pub fn go_to(&mut self, now_us: u64, target: usize, spaces: &mut Spaces) {
        let (active, last) = indices(spaces);
        let target = target.min(last);
        let o = self.overview_at(now_us);
        let p = self.position_at(now_us, active, last);
        spaces.go_to(target);
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
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
