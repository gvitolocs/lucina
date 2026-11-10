//! What a Precision Touchpad's frames become: pointer motion, two-finger scrolling, taps and clicks. This is libinput's
//! touchpad code (1.28.1: `evdev-mt-touchpad.c`, `-gestures.c`, `evdev.c:evdev_post_scroll`, MIT), the subset this pad
//! uses, plus kinetic scrolling as GTK 4.16 does it (`gtkeventcontrollerscroll.c` scroll history, `gtkkineticscrolling.c`,
//! `gtkscrolledwindow.c` deceleration, LGPL-2.1: behaviour only). It runs where Juno has its one input layer, the kernel
//! driver, so every evdev reader gets it; libinput runs it in user space. Taps are [`crate::tp_tap`], pointer
//! acceleration [`crate::tp_accel`].
//!
//! Ported (`docs/research/juno-touchpad.md` has the whole table): touch begin/update/end, motion history, the
//! finger-count reset of the motion history, tool palms (`tp_palm_detect_tool_triggered`), clickpad pinning
//! (`tp_pin_fingers`, `tp_unpin_finger`), the finger count and its debounce (`tp_gesture_update_finger_state`), the
//! gesture states NONE / UNKNOWN / POINTER_MOTION / SCROLL_START / SCROLL and the scroll-or-pinch decision
//! (`tp_gesture_detect_motion_gestures`, `tp_gesture_is_pinch`), scroll constraints, `evdev_post_scroll` /
//! `evdev_stop_scroll`, natural scrolling. Pinches and swipes are told apart from scrolling as libinput does, and then
//! send nothing (no reader takes them yet). Not ported: hold gestures (they only notify clients; a touch stops kinetic
//! scrolling instead, as libinput's hold begin does for GTK), the pressure, size, edge, typing and trackpoint palm
//! detectors and thumb detection (this pad has no pressure or size; the firmware's confidence bit marks palms and
//! thumbs), edge scrolling, 3-finger drag, jump detection and hysteresis (quirks for other models). The clickpad's
//! click becomes left, middle or right by where the fingers are, libinput's default click method for it (software
//! button areas, [`crate::tp_buttons`]; Windows' default puts right-click in the same corner).

use crate::ptp::{Frame, MAX_SLOTS, MT_TOOL_PALM};
use crate::tp_accel::TouchpadAccel;
pub use crate::tp_rules::GestureState;
use crate::tp_rules::{GestureEvent, finger_update, gesture_step};
use crate::tp_buttons::{ButtonPhase, ButtonTouch, ClickButton, SoftButtons};
use crate::tp_tap::{Tap, TapButton, TapConfig, TapEvent, TapTouch, TouchPhase};

/// `TOUCHPAD_HISTORY_LENGTH`.
const HISTORY: usize = 4;
/// `DEFAULT_MOUSE_DPI` (`evdev.h`).
const DEFAULT_MOUSE_DPI: f64 = 1000.0;
/// `DEFAULT_GESTURE_SWITCH_TIMEOUT`, `DEFAULT_GESTURE_SWIPE_TIMEOUT`, `DEFAULT_GESTURE_PINCH_TIMEOUT`.
const GESTURE_SWITCH_TIMEOUT_US: u64 = 100_000;
const GESTURE_SWIPE_TIMEOUT_US: u64 = 150_000;
const GESTURE_PINCH_TIMEOUT_US: u64 = 300_000;
/// `PINCH_DISAMBIGUATION_MOVE_THRESHOLD` (mm).
const PINCH_DISAMBIGUATION_MOVE_MM: f64 = 1.5;
/// `tp_unpin_finger`: 1.5 mm movement unpins.
const UNPIN_MM: f64 = 1.5;
/// `evdev_device_init_pointer_acceleration` / `tp_init_scroll`: a touchpad's scroll threshold is 0, the direction lock 5.
const SCROLL_THRESHOLD: f64 = 0.0;
const SCROLL_DIRECTION_LOCK_THRESHOLD: f64 = 5.0;
/// libinput's legacy wheel: one detent is 15 units of axis value (`libinput_event_pointer_get_axis_value` with the
/// default 15 degree click angle), and 120 in v120 units. A finger scroll of `v` units is `v * 8` in REL_WHEEL_HI_RES.
pub const V120_PER_UNIT: f64 = 120.0 / 15.0;
/// GTK's scroll history window for the release velocity (`SCROLL_CAPTURE_THRESHOLD_MS`).
const SCROLL_CAPTURE_US: u64 = 150_000;
/// GTK's `DECELERATION_FRICTION` (per second) and the velocity below which deceleration stops (0.1 in
/// `gtk_kinetic_scrolling_tick`), in GTK pixels per second. GTK's pixels are libinput units times
/// `MAGIC_SCROLL_FACTOR` (2.5, `gtkscrolledwindow.c`), so the stop is 0.1 / 2.5 units per second here.
const DECELERATION_FRICTION: f64 = 4.0;
const MAGIC_SCROLL_FACTOR: f64 = 2.5;
const KINETIC_STOP_VELOCITY: f64 = 0.1 / MAGIC_SCROLL_FACTOR;
/// GTK's `VELOCITY_ACCUMULATION_FLOOR`, `_CEIL`, `_MAX`: a fling in the same direction as one just stopped adds to it.
const VELOCITY_ACCUMULATION_FLOOR: f64 = 0.33;
const VELOCITY_ACCUMULATION_CEIL: f64 = 1.0;
const VELOCITY_ACCUMULATION_MAX: f64 = 6.0;
/// How often the kinetic scroll moves: the driver's poll tick.
pub const KINETIC_TICK_US: u64 = 10_000;
/// Scroll history entries kept (150 ms of reports at up to ~250 Hz).
const SCROLL_HISTORY: usize = 48;

/// How the pad behaves. libinput's configuration knobs, with Juno's defaults.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    pub tap: TapConfig,
    /// libinput's natural scrolling (off by default there; on by default on Windows and macOS, so on here).
    pub natural_scroll: bool,
    /// GTK's kinetic scrolling (on by default in GTK).
    pub kinetic: bool,
}

impl Config {
    /// Juno: tapping on (Windows' default; libinput's is off), natural scrolling on, kinetic scrolling on.
    pub fn juno() -> Config {
        Config {
            tap: TapConfig::libinput_default(true),
            natural_scroll: true,
            kinetic: true,
        }
    }
}

/// A button of the pointer device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Left,
    Right,
    Middle,
}

/// What the pad did, as the pointer device sends it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Output {
    /// REL_X, REL_Y in pointer pixels (libinput's accelerated, normalized units).
    Motion { dx: i32, dy: i32 },
    /// REL_HWHEEL_HI_RES, REL_WHEEL_HI_RES (v120): `dx` positive = right, `dy` positive = up (the wheel's sense).
    Scroll { dx: i32, dy: i32 },
    /// The scroll ended (libinput's zero axis event, `evdev_stop_scroll`), and whether kinetic scrolling follows.
    ScrollStop { kinetic: bool },
    Button { button: Button, pressed: bool },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
enum TouchState {
    #[default]
    None,
    Begin,
    Update,
    End,
}

#[derive(Clone, Copy, Debug, Default)]
struct Touch {
    state: TouchState,
    tracking_id: i32,
    point: (i32, i32),
    dirty: bool,
    is_tool_palm: bool,
    palm: bool,
    pinned: Option<(i32, i32)>,
    history: [(i32, i32); HISTORY],
    history_count: usize,
    history_index: usize,
    gesture_initial: (i32, i32),
}

impl Touch {
    /// `tp_motion_history_push`.
    fn history_push(&mut self) {
        let i = (self.history_index + 1) % HISTORY;
        if self.history_count < HISTORY {
            self.history_count += 1;
        }
        self.history[i] = self.point;
        self.history_index = i;
    }

    fn history_offset(&self, offset: usize) -> (i32, i32) {
        self.history[(self.history_index + HISTORY - offset) % HISTORY]
    }

    /// `tp_get_delta`.
    fn delta(&self) -> (f64, f64) {
        if self.history_count <= 1 {
            return (0.0, 0.0);
        }
        let (a, b) = (self.history_offset(0), self.history_offset(1));
        (f64::from(a.0 - b.0), f64::from(a.1 - b.1))
    }

    fn down(&self) -> bool {
        matches!(self.state, TouchState::Begin | TouchState::Update)
    }
}


/// `tp->scroll` and the scrolling half of `device->scroll`.
#[derive(Clone, Copy, Debug, Default)]
struct Scroll {
    active_h: bool,
    active_v: bool,
    duration_h: u64,
    duration_v: u64,
    vector: (f64, f64),
    time_prev: u64,
    buildup: (f64, f64),
    /// `device->scroll.direction`: which axes are scrolling.
    scrolling_h: bool,
    scrolling_v: bool,
}

/// GTK's scroll history and one axis's deceleration.
#[derive(Clone, Copy, Debug)]
struct Kinetic {
    history: [(f64, f64, u64); SCROLL_HISTORY],
    nhistory: usize,
    /// Per axis: `c1`, `c2` of `gtk_kinetic_scrolling_new`'s decelerating phase, its start, the position reached.
    axes: [Option<Decel>; 2],
    /// The velocities of the deceleration a touch stopped, for GTK's velocity accumulation.
    stopped: [Option<f64>; 2],
    last_tick: u64,
}

#[derive(Clone, Copy, Debug)]
struct Decel {
    c1: f64,
    c2: f64,
    start: u64,
    position: f64,
    velocity: f64,
}

impl Decel {
    /// `gtk_kinetic_scrolling_new` from position 0 (no overshoot: the kernel has no content bounds).
    fn new(velocity: f64, start: u64) -> Decel {
        Decel {
            c1: velocity / DECELERATION_FRICTION,
            c2: -velocity / DECELERATION_FRICTION,
            start,
            position: 0.0,
            velocity,
        }
    }

    /// `gtk_kinetic_scrolling_tick`'s decelerating phase at `now`: the position moved since the last tick, and whether it
    /// goes on.
    fn tick(&mut self, now: u64) -> (f64, bool) {
        let t = now.saturating_sub(self.start) as f64 / 1e6;
        let e = exp_neg(DECELERATION_FRICTION * t);
        let position = self.c1 + self.c2 * e;
        self.velocity = -DECELERATION_FRICTION * self.c2 * e;
        let moved = position - self.position;
        self.position = position;
        (moved, self.velocity.abs() >= KINETIC_STOP_VELOCITY)
    }
}

impl Default for Kinetic {
    fn default() -> Self {
        Kinetic {
            history: [(0.0, 0.0, 0); SCROLL_HISTORY],
            nhistory: 0,
            axes: [None; 2],
            stopped: [None; 2],
            last_tick: 0,
        }
    }
}

impl Kinetic {
    /// `scroll_history_push`: drop what is older than 150 ms before this event, then add it.
    fn push(&mut self, dx: f64, dy: f64, time: u64) {
        let keep_from = self.history[..self.nhistory]
            .iter()
            .position(|e| e.2 + SCROLL_CAPTURE_US >= time)
            .unwrap_or(self.nhistory);
        self.history.copy_within(keep_from..self.nhistory, 0);
        self.nhistory -= keep_from;
        if self.nhistory == SCROLL_HISTORY {
            self.history.copy_within(1..SCROLL_HISTORY, 0);
            self.nhistory -= 1;
        }
        self.history[self.nhistory] = (dx, dy, time);
        self.nhistory += 1;
    }

    /// `scroll_history_finish`: units per second over what the history holds, 0 if it spans no time.
    fn finish(&mut self) -> (f64, f64) {
        let h = &self.history[..self.nhistory];
        self.nhistory = 0;
        let (Some(first), Some(last)) = (h.first(), h.last()) else {
            return (0.0, 0.0);
        };
        if last.2 == first.2 {
            return (0.0, 0.0);
        }
        let (sx, sy) = h.iter().fold((0.0, 0.0), |(x, y), e| (x + e.0, y + e.1));
        // GTK's event times are milliseconds: `accum * 1000 / (last - first)`.
        let ms = (last.2 / 1000) as f64 - (first.2 / 1000) as f64;
        if ms == 0.0 {
            return (0.0, 0.0);
        }
        (sx * 1000.0 / ms, sy * 1000.0 / ms)
    }

    /// `gtk_scrolled_window_accumulate_velocity`, then the deceleration starts (`gtk_scrolled_window_start_deceleration`).
    fn start(&mut self, velocity: (f64, f64), now: u64) {
        for (i, v) in [velocity.0, velocity.1].into_iter().enumerate() {
            let mut v = v;
            if let Some(last) = self.stopped[i].take()
                && (v >= 0.0) == (last >= 0.0)
                && v.abs() >= last.abs() * VELOCITY_ACCUMULATION_FLOOR
            {
                let min = last * VELOCITY_ACCUMULATION_FLOOR;
                let max = last * VELOCITY_ACCUMULATION_CEIL;
                let multiplier = (v - min) / (max - min);
                v += last * multiplier.min(VELOCITY_ACCUMULATION_MAX);
            }
            self.axes[i] = (v != 0.0).then(|| Decel::new(v, now));
        }
        self.last_tick = now;
    }

    /// A touch: `stop_kinetic_scrolling_cb`. The velocity at that moment is kept for the next fling.
    fn stop(&mut self) {
        for i in 0..2 {
            if let Some(d) = self.axes[i].take() {
                self.stopped[i] = Some(d.velocity);
            }
        }
    }

    fn running(&self) -> bool {
        self.axes.iter().any(Option::is_some)
    }
}

/// The touchpad: libinput's `tp_dispatch` for one Precision Touchpad.
pub struct Touchpad {
    config: Config,
    res: (f64, f64),
    touches: [Touch; MAX_SLOTS],
    num_slots: usize,
    nfingers_down: u32,
    old_nfingers_down: u32,
    button: bool,
    accel: TouchpadAccel,
    tap: Tap,
    buttons: SoftButtons,
    state: GestureState,
    finger_count: u32,
    finger_count_pending: u32,
    switch_deadline: Option<u64>,
    initial_time: u64,
    gesture_touches: [usize; 2],
    scroll: Scroll,
    kinetic: Kinetic,
    /// What did not make a whole pixel or v120 unit yet.
    motion_rest: (f64, f64),
    scroll_rest: (f64, f64),
    /// Whether this frame had motion (`TOUCHPAD_EVENT_MOTION`).
    queued_motion: bool,
}

impl Touchpad {
    /// A pad with `num_slots` slots, axes from 0 to `x_max`, `y_max` with `res_x`, `res_y` units per mm
    /// (`hidinput_calc_abs_res`).
    pub fn new(x_max: i32, y_max: i32, res_x: i32, res_y: i32, num_slots: usize, config: Config) -> Touchpad {
        let res = (f64::from(res_x.max(1)), f64::from(res_y.max(1)));
        // `device->dpi = absinfo_x->resolution * 25.4` (evdev-mt-touchpad.c tp_init), then `int dpi = device->dpi`.
        let dpi = (res.0 * 25.4) as i32;
        Touchpad {
            config,
            res,
            touches: [Touch::default(); MAX_SLOTS],
            num_slots: num_slots.min(MAX_SLOTS),
            nfingers_down: 0,
            old_nfingers_down: 0,
            button: false,
            accel: TouchpadAccel::new(dpi),
            tap: Tap::new(config.tap),
            buttons: SoftButtons::new(x_max, y_max, res_x.max(1), res_y.max(1)),
            state: GestureState::None,
            finger_count: 0,
            finger_count_pending: 0,
            switch_deadline: None,
            initial_time: 0,
            gesture_touches: [0, 0],
            scroll: Scroll::default(),
            kinetic: Kinetic::default(),
            motion_rest: (0.0, 0.0),
            scroll_rest: (0.0, 0.0),
            queued_motion: false,
        }
    }

    pub fn config(&self) -> Config {
        self.config
    }

    pub fn gesture_state(&self) -> GestureState {
        self.state
    }

    pub fn tap_state(&self) -> &'static str {
        self.tap.state_name()
    }

    pub fn finger_count(&self) -> u32 {
        self.finger_count
    }

    /// When [`Touchpad::tick`] must run next: a tap or finger-switch timer, or the next kinetic step.
    pub fn deadline(&self) -> Option<u64> {
        let kinetic = self
            .kinetic
            .running()
            .then_some(self.kinetic.last_tick + KINETIC_TICK_US);
        [self.tap.deadline(), self.buttons.deadline(), self.switch_deadline, kinetic]
            .into_iter()
            .flatten()
            .min()
    }

    /// Timers due at `now` (libinput's timer handlers, GTK's frame clock for the deceleration).
    pub fn tick(&mut self, now: u64, out: &mut dyn FnMut(Output)) {
        self.timers(now, out);
        if self.kinetic.running() && now >= self.kinetic.last_tick + KINETIC_TICK_US {
            self.kinetic.last_tick = now;
            let mut moved = [0.0; 2];
            for (i, axis) in self.kinetic.axes.iter_mut().enumerate() {
                if let Some(d) = axis {
                    let (m, more) = d.tick(now);
                    moved[i] = m;
                    if !more {
                        *axis = None;
                    }
                }
            }
            self.emit_scroll(moved[0], moved[1], out);
        }
    }

    fn timers(&mut self, now: u64, out: &mut dyn FnMut(Output)) {
        if self.tap.deadline().is_some_and(|d| now >= d) {
            self.tap.timeout(now, &mut |e| out(tap_output(e)));
        }
        if self.buttons.deadline().is_some_and(|d| now >= d) {
            self.buttons.timeout(now);
        }
        if self.switch_deadline.is_some_and(|d| now >= d) {
            self.switch_deadline = None;
            // tp_gesture_finger_count_switch_timeout.
            if self.finger_count_pending != 0 {
                self.gesture_event(GestureEvent::FingerSwitchTimeout, now, out);
                self.finger_count = self.finger_count_pending;
                self.finger_count_pending = 0;
            }
        }
    }

    /// One frame (SYN_REPORT) at `time` µs: libinput's `tp_handle_state`.
    pub fn frame(&mut self, frame: &Frame, time: u64, out: &mut dyn FnMut(Output)) {
        self.timers(time, out);
        let button_change = self.read_frame(frame);
        // tp_pre_process_state: an ending touch keeps the last position of its history.
        for t in self.touches[..self.num_slots].iter_mut() {
            if t.state == TouchState::End && t.history_count > 0 {
                t.point = t.history_offset(0);
            }
        }
        self.process_state(time, button_change, out);
        self.post_events(time, button_change, out);
        self.post_process_state();
    }

    /// The frame as libinput would have read it from evdev: tracking IDs begin and end touches, positions and tool types
    /// make them dirty. Returns the physical button's change.
    fn read_frame(&mut self, frame: &Frame) -> Option<bool> {
        self.queued_motion = false;
        for (t, s) in self.touches[..self.num_slots].iter_mut().zip(frame.slots.iter()) {
            let was = t.down() && t.tracking_id >= 0;
            if s.active() && (!was || s.tracking_id != t.tracking_id) {
                // A slot that went straight to another contact: the old one's finger is gone, the new one counts once.
                if !was {
                    self.nfingers_down += 1;
                }
                // tp_new_touch, tp_begin_touch (this pad has no hover: a new touch begins at once).
                *t = Touch {
                    state: TouchState::Begin,
                    tracking_id: s.tracking_id,
                    point: (s.x, s.y),
                    dirty: true,
                    is_tool_palm: s.tool == MT_TOOL_PALM,
                    ..Touch::default()
                };
                self.queued_motion = true;
                if self.kinetic.running() {
                    self.kinetic.stop();
                }
            } else if s.active() {
                if (s.x, s.y) != t.point {
                    t.point = (s.x, s.y);
                    t.dirty = true;
                    self.queued_motion = true;
                }
                let palm = s.tool == MT_TOOL_PALM;
                if palm != t.is_tool_palm {
                    t.is_tool_palm = palm;
                    t.dirty = true;
                }
            } else if was {
                // tp_end_sequence -> tp_end_touch.
                t.state = TouchState::End;
                t.tracking_id = -1;
                t.dirty = true;
                t.palm = false;
                t.pinned = None;
                self.nfingers_down = self.nfingers_down.saturating_sub(1);
                self.queued_motion = true;
            }
        }
        let change = (frame.left != self.button).then_some(frame.left);
        self.button = frame.left;
        change
    }

    /// `tp_process_state`.
    fn process_state(&mut self, time: u64, button_change: Option<bool>, out: &mut dyn FnMut(Output)) {
        let want_motion_reset = self.nfingers_down != self.old_nfingers_down;
        let mut restart_filter = false;
        let res = self.res;
        for t in self.touches[..self.num_slots].iter_mut() {
            if t.state == TouchState::None {
                continue;
            }
            if want_motion_reset {
                t.history_count = 0;
            }
            if !t.dirty {
                t.history_push();
                continue;
            }
            // tp_palm_detect: only the tool detector applies to this pad.
            if !t.palm && t.is_tool_palm {
                t.palm = true;
            } else if t.palm && !t.is_tool_palm {
                t.palm = false;
            }
            t.history_push();
            // tp_unpin_finger.
            if let Some(center) = t.pinned {
                let dx = f64::from((t.point.0 - center.0).abs()) / res.0;
                let dy = f64::from((t.point.1 - center.1).abs()) / res.1;
                if dx * dx + dy * dy >= UNPIN_MM * UNPIN_MM {
                    t.pinned = None;
                }
            }
            if t.state == TouchState::Begin {
                restart_filter = true;
            }
        }
        if restart_filter {
            self.accel.restart(time);
        }
        // tp_button_handle_state: the dirty touches through the software-button state machine.
        let mut bt = [ButtonTouch { slot: 0, phase: ButtonPhase::Begin, x: 0, y: 0 }; MAX_SLOTS];
        let mut nb = 0;
        for (i, t) in self.touches[..self.num_slots].iter().enumerate() {
            let phase = match (t.dirty, t.state) {
                (true, TouchState::Begin) => ButtonPhase::Begin,
                (true, TouchState::Update) => ButtonPhase::Update,
                (true, TouchState::End) => ButtonPhase::End,
                _ => continue,
            };
            bt[nb] = ButtonTouch { slot: i as u8, phase, x: t.point.0, y: t.point.1 };
            nb += 1;
        }
        self.buttons.handle_frame(time, &bt[..nb]);
        // A physical click on a clickpad pins every finger, so the clicking finger does not move the pointer.
        if button_change == Some(true) {
            for t in self.touches[..self.num_slots].iter_mut() {
                t.pinned = Some(t.point);
            }
        }
        self.update_finger_state(time, out);
    }

    /// `tp_touch_active_for_gesture` (no thumb or edge-scroll state on this pad).
    fn active_for(&self, i: usize) -> bool {
        let t = &self.touches[i];
        t.down() && !t.palm && t.pinned.is_none() && self.buttons.touch_active(i as u8)
    }

    /// `tp_gesture_update_finger_state` ([`finger_update`] decides).
    fn update_finger_state(&mut self, time: u64, out: &mut dyn FnMut(Output)) {
        let active = (0..self.num_slots).filter(|&i| self.active_for(i)).count() as u32;
        let u = finger_update(
            active,
            self.finger_count,
            self.finger_count_pending,
            self.debounce_finger_changes(),
            matches!(self.state, GestureState::Unknown | GestureState::PointerMotion),
        );
        if u.end {
            self.gesture_end(time, false, out);
        }
        self.finger_count = u.count;
        self.finger_count_pending = u.pending;
        if u.reset {
            self.gesture_event(GestureEvent::Reset, time, out);
        }
        if u.arm {
            self.switch_deadline = Some(time + GESTURE_SWITCH_TIMEOUT_US);
        }
    }

    /// `tp_gesture_debounce_finger_changes`.
    fn debounce_finger_changes(&self) -> bool {
        matches!(self.state, GestureState::Scroll | GestureState::Pinch | GestureState::Swipe)
    }

    /// `tp_post_events`.
    fn post_events(&mut self, time: u64, button_change: Option<bool>, out: &mut dyn FnMut(Output)) {
        let mut touches = [TapTouch {
            slot: 0,
            phase: TouchPhase::Begin,
            x_mm: 0.0,
            y_mm: 0.0,
            palm: false,
            thumb: false,
        }; MAX_SLOTS];
        let mut n = 0;
        for (i, t) in self.touches[..self.num_slots].iter().enumerate() {
            if !t.dirty {
                continue;
            }
            let phase = match t.state {
                TouchState::Begin => TouchPhase::Begin,
                TouchState::Update => TouchPhase::Update,
                TouchState::End => TouchPhase::End,
                TouchState::None => continue,
            };
            touches[n] = TapTouch {
                slot: i as u8,
                phase,
                x_mm: f64::from(t.point.0) / self.res.0,
                y_mm: f64::from(t.point.1) / self.res.1,
                palm: t.palm,
                thumb: false,
            };
            n += 1;
        }
        let mut ignore_motion =
            self.tap
                .handle_frame(time, &touches[..n], button_change, self.nfingers_down, &mut |e| out(tap_output(e)));
        // tp_post_button_events on a clickpad: tp_post_softbutton_buttons picks the button by area.
        if let Some(pressed) = button_change {
            ignore_motion |= self.buttons.click(time, pressed, &mut |b, p| {
                out(Output::Button {
                    button: match b {
                        ClickButton::Left => Button::Left,
                        ClickButton::Right => Button::Right,
                        ClickButton::Middle => Button::Middle,
                    },
                    pressed: p,
                })
            });
        }
        if ignore_motion {
            self.cancel_motion_gestures(time, out);
            self.gesture_post_events(time, true, out);
            return;
        }
        self.gesture_post_events(time, false, out);
    }

    /// `tp_post_process_state`.
    fn post_process_state(&mut self) {
        for t in self.touches[..self.num_slots].iter_mut() {
            if !t.dirty {
                continue;
            }
            match t.state {
                TouchState::End => t.state = TouchState::None,
                TouchState::Begin => t.state = TouchState::Update,
                _ => {}
            }
            t.dirty = false;
        }
        self.old_nfingers_down = self.nfingers_down;
    }

    /// `tp_gesture_post_events`.
    fn gesture_post_events(&mut self, time: u64, ignore_motion: bool, out: &mut dyn FnMut(Output)) {
        if self.finger_count == 0 {
            return;
        }
        // Tap-and-drag, or a click held by a finger (no thumb detection here: thumb.state is always FINGER), is 1-finger
        // pointer motion.
        if self.tap.dragging() || self.button {
            if self.state != GestureState::PointerMotion {
                self.gesture_end(time, true, out);
                self.gesture_event(GestureEvent::PointerMotionStart, time, out);
            }
            self.finger_count = 1;
            self.finger_count_pending = 0;
        }
        if self.finger_count_pending != 0 {
            return;
        }
        if self.finger_count <= 4 {
            self.handle_state(time, ignore_motion, out);
        }
    }

    /// `tp_gesture_handle_state`.
    fn handle_state(&mut self, time: u64, ignore_motion: bool, out: &mut dyn FnMut(Output)) {
        if self.state == GestureState::None {
            self.handle_state_none(time, out);
        }
        if self.state == GestureState::Unknown && !ignore_motion {
            self.detect_motion_gestures(time, out);
        }
        if self.state == GestureState::PointerMotion && self.queued_motion {
            self.post_pointer_motion(time, out);
        }
        if self.state == GestureState::Scroll {
            self.handle_state_scroll(time, out);
        }
        if self.state == GestureState::ScrollStart {
            self.handle_state_scroll_start(time, out);
        }
        // Swipes and pinches: recognised, nothing sent (libinput's _START states move on at their first motion).
        if self.state == GestureState::SwipeStart {
            self.state = GestureState::Swipe;
        }
        if self.state == GestureState::PinchStart {
            self.state = GestureState::Pinch;
        }
    }

    /// The active touches in slot order (`tp_gesture_get_active_touches`), at most 4.
    fn active_touches(&self) -> ([usize; 4], usize) {
        let mut out = [0; 4];
        let mut n = 0;
        for i in 0..self.num_slots {
            if self.active_for(i) {
                out[n] = i;
                n += 1;
                if n == out.len() {
                    break;
                }
            }
        }
        (out, n)
    }

    /// `tp_gesture_handle_state_none`.
    fn handle_state_none(&mut self, time: u64, out: &mut dyn FnMut(Output)) {
        let (touches, n) = self.active_touches();
        if n == 0 {
            return;
        }
        let mut first = touches[0];
        let mut second = touches[1];
        if n == 1 {
            self.touches[first].gesture_initial = self.touches[first].point;
            self.gesture_touches[0] = first;
            self.gesture_event(GestureEvent::FingerDetected, time, out);
            return;
        }
        if n > 2 {
            second = touches[0];
            for &i in &touches[1..n.min(self.num_slots)] {
                if self.touches[i].point.1 < self.touches[first].point.1 {
                    first = i;
                } else if self.touches[i].point.1 >= self.touches[second].point.1 {
                    second = i;
                }
            }
            if first == second {
                return;
            }
        }
        self.initial_time = time;
        self.touches[first].gesture_initial = self.touches[first].point;
        self.touches[second].gesture_initial = self.touches[second].point;
        self.gesture_touches = [first, second];
        self.gesture_event(GestureEvent::FingerDetected, time, out);
    }

    /// `tp_gesture_mm_moved`, as (x, y) mm.
    fn mm_moved(&self, i: usize) -> (f64, f64) {
        let t = &self.touches[i];
        (
            f64::from((t.point.0 - t.gesture_initial.0).abs()) / self.res.0,
            f64::from((t.point.1 - t.gesture_initial.1).abs()) / self.res.1,
        )
    }

    /// `tp_gesture_get_direction`: the direction since the gesture began, in mm (`tp_phys_delta`).
    fn direction(&self, i: usize) -> u32 {
        let t = &self.touches[i];
        xy_get_direction(
            f64::from(t.point.0 - t.gesture_initial.0) / self.res.0,
            f64::from(t.point.1 - t.gesture_initial.1) / self.res.1,
        )
    }

    /// `tp_gesture_detect_motion_gestures` for 1 to 4 fingers (no 3-finger drag, no thumb detection).
    fn detect_motion_gestures(&mut self, time: u64, out: &mut dyn FnMut(Output)) {
        let [first, second] = self.gesture_touches;
        let len = |(x, y): (f64, f64)| sqrt(x * x + y * y);
        let first_mm = len(self.mm_moved(first));
        if self.finger_count == 1 {
            if !self.has_pending_pointer_motion() {
                return;
            }
            self.gesture_event(GestureEvent::PointerMotionStart, time, out);
            return;
        }
        let extra = f64::from(self.finger_count - 2);
        let max_move = 4.0 + 2.0 * extra;
        let min_move = 1.5 + 0.5 * extra;
        let second_mm = len(self.mm_moved(second));
        let (a, b) = (self.touches[first].point, self.touches[second].point);
        let distance = (
            f64::from((a.0 - b.0).abs()) / self.res.0,
            f64::from((a.1 - b.1).abs()) / self.res.1,
        );
        if first_mm < 1.0 && second_mm < 1.0 {
            return;
        }
        if distance.0 < 40.0 && distance.1 < 7.0 && time > self.initial_time + GESTURE_SWIPE_TIMEOUT_US {
            let e = if self.finger_count == 2 {
                GestureEvent::ScrollStart
            } else {
                GestureEvent::SwipeStart
            };
            self.gesture_event(e, time, out);
            return;
        }
        if first_mm >= max_move || second_mm >= max_move {
            // The lower touch is the thumb.
            let finger_mm = if a.1 > b.1 { second_mm } else { first_mm };
            if finger_mm < min_move && self.finger_count == 2 {
                self.gesture_event(GestureEvent::ScrollStart, time, out);
                return;
            }
        }
        if first_mm < min_move || second_mm < min_move {
            return;
        }
        let (dir1, dir2) = (self.direction(first), self.direction(second));
        if self.finger_count as usize > self.num_slots || same_directions(dir1, dir2) {
            let e = if self.finger_count == 2 {
                GestureEvent::ScrollStart
            } else {
                GestureEvent::SwipeStart
            };
            self.gesture_event(e, time, out);
            return;
        }
        self.gesture_event(GestureEvent::PinchStart, time, out);
    }

    /// `tp_gesture_is_pinch`.
    fn is_pinch(&self) -> bool {
        let [first, second] = self.gesture_touches;
        if same_directions(self.direction(first), self.direction(second)) {
            return false;
        }
        let len2 = |(x, y): (f64, f64)| x * x + y * y;
        let t = PINCH_DISAMBIGUATION_MOVE_MM * PINCH_DISAMBIGUATION_MOVE_MM;
        len2(self.mm_moved(first)) >= t && len2(self.mm_moved(second)) >= t
    }

    /// The average delta of the active touches (`tp_get_average_touches_delta`), or their sum while the clickpad is
    /// clicked (`tp_get_combined_touches_delta`, for pointer motion).
    fn touches_delta(&self, average: bool) -> (f64, f64) {
        let mut d = (0.0, 0.0);
        let mut n = 0.0;
        for (i, t) in self.touches[..self.num_slots].iter().enumerate() {
            if !self.active_for(i) {
                continue;
            }
            n += 1.0;
            if t.dirty {
                let (x, y) = t.delta();
                d.0 += x;
                d.1 += y;
            }
        }
        if !average || n == 0.0 {
            return d;
        }
        (d.0 / n, d.1 / n)
    }

    fn raw_pointer_motion(&self) -> (f64, f64) {
        self.touches_delta(!self.button)
    }

    /// `tp_has_pending_pointer_motion`.
    fn has_pending_pointer_motion(&self) -> bool {
        if !self.queued_motion {
            return false;
        }
        let (x, y) = self.raw_pointer_motion();
        x != 0.0 || y != 0.0
    }

    /// `tp_scale_to_xaxis`.
    fn to_xaxis(&self, (x, y): (f64, f64)) -> (f64, f64) {
        (x, y * self.res.0 / self.res.1)
    }

    /// `tp_gesture_post_pointer_motion` with `tp_filter_motion`.
    fn post_pointer_motion(&mut self, time: u64, out: &mut dyn FnMut(Output)) {
        let raw = self.raw_pointer_motion();
        if raw == (0.0, 0.0) {
            return;
        }
        let (x, y) = self.to_xaxis(raw);
        let (dx, dy) = self.accel.filter(x, y, time);
        let (dx, dy) = take_whole(&mut self.motion_rest, dx, dy);
        if dx != 0 || dy != 0 {
            out(Output::Motion { dx, dy });
        }
    }

    /// `tp_filter_scroll`: the constant filter of the x-axis-scaled delta.
    fn filter_scroll(&self, raw: (f64, f64)) -> (f64, f64) {
        if raw == (0.0, 0.0) {
            return raw;
        }
        let (x, y) = self.to_xaxis(raw);
        self.accel.constant(x, y)
    }

    /// `tp_gesture_handle_state_scroll_start`.
    fn handle_state_scroll_start(&mut self, time: u64, out: &mut dyn FnMut(Output)) {
        if time < self.initial_time + GESTURE_PINCH_TIMEOUT_US && self.is_pinch() {
            self.gesture_event(GestureEvent::PinchStart, time, out);
            return;
        }
        let delta = self.filter_scroll(self.touches_delta(true));
        if delta == (0.0, 0.0) {
            return;
        }
        // tp_gesture_init_scroll.
        self.scroll.active_h = false;
        self.scroll.active_v = false;
        self.scroll.duration_h = 0;
        self.scroll.duration_v = 0;
        self.scroll.vector = (0.0, 0.0);
        self.scroll.time_prev = 0;
        self.state = GestureState::Scroll;
    }

    /// `tp_gesture_handle_state_scroll`.
    fn handle_state_scroll(&mut self, time: u64, out: &mut dyn FnMut(Output)) {
        if time < self.initial_time + GESTURE_PINCH_TIMEOUT_US && self.is_pinch() {
            self.gesture_event(GestureEvent::PinchStart, time, out);
            return;
        }
        let raw = self.touches_delta(true);
        let mut delta = self.filter_scroll(raw);
        if delta == (0.0, 0.0) {
            return;
        }
        self.apply_scroll_constraints(raw, &mut delta, time);
        self.post_scroll(time, delta, out);
    }

    /// `tp_gesture_apply_scroll_constraints`.
    fn apply_scroll_constraints(&mut self, raw: (f64, f64), delta: &mut (f64, f64), time: u64) {
        const ACTIVE_THRESHOLD: u64 = 100_000;
        const INACTIVE_THRESHOLD: u64 = 50_000;
        const EVENT_TIMEOUT: u64 = 100_000;
        const DEGREE_75: f64 = 3.73;
        const DEGREE_60: f64 = 1.73;
        const DEGREE_30: f64 = 0.57;
        const DEGREE_15: f64 = 0.27;
        const MIN_VECTOR: f64 = 0.15;
        let s = &mut self.scroll;
        if s.active_h && s.active_v {
            return;
        }
        let mut tdelta = 0;
        if s.time_prev != 0 {
            tdelta = time.saturating_sub(s.time_prev);
        }
        if tdelta > EVENT_TIMEOUT {
            tdelta = 0;
        }
        s.time_prev = time;
        // tp_phys_delta: mm.
        let delta_mm = (raw.0 / self.res.0, raw.1 / self.res.1);
        let vector_decay = if tdelta > 0 {
            let td = tdelta as f64;
            let et = EVENT_TIMEOUT as f64;
            let recent = ((et / 2.0) - td) / (et / 2.0);
            let later = (et - td) / (et * 2.0);
            if td <= 0.33 * et { recent } else { later }
        } else {
            0.0
        };
        let vector = (
            s.vector.0 * vector_decay + delta_mm.0,
            s.vector.1 * vector_decay + delta_mm.1,
        );
        let vector_length = sqrt(vector.0 * vector.0 + vector.1 * vector.1);
        s.vector = vector;
        let slope = if vector.0 != 0.0 {
            (vector.1 / vector.0).abs()
        } else {
            f64::INFINITY
        };
        if slope >= DEGREE_30 && vector_length > MIN_VECTOR {
            s.duration_v = (s.duration_v + tdelta).min(ACTIVE_THRESHOLD);
            if slope >= DEGREE_75 {
                s.duration_h = s.duration_h.saturating_sub(tdelta);
            }
        }
        if slope < DEGREE_60 && vector_length > MIN_VECTOR {
            s.duration_h = (s.duration_h + tdelta).min(ACTIVE_THRESHOLD);
            if slope < DEGREE_15 {
                s.duration_v = s.duration_v.saturating_sub(tdelta);
            }
        }
        if s.duration_h == ACTIVE_THRESHOLD {
            s.active_h = true;
            if s.duration_v < INACTIVE_THRESHOLD {
                s.active_v = false;
            }
        }
        if s.duration_v == ACTIVE_THRESHOLD {
            s.active_v = true;
            if s.duration_h < INACTIVE_THRESHOLD {
                s.active_h = false;
            }
        }
        if vector_length > 5.0 && (DEGREE_30..DEGREE_60).contains(&slope) {
            s.active_v = true;
            s.active_h = true;
        }
        if !s.active_h && s.active_v {
            delta.0 = 0.0;
        }
        if s.active_h && !s.active_v {
            delta.1 = 0.0;
        }
        if !s.active_h && !s.active_v {
            if slope >= DEGREE_60 {
                delta.0 = 0.0;
            }
            if slope < DEGREE_30 {
                delta.1 = 0.0;
            }
        }
    }

    /// `evdev_post_scroll` with the finger source, then `evdev_notify_axis_finger` (natural scrolling).
    fn post_scroll(&mut self, time: u64, delta: (f64, f64), out: &mut dyn FnMut(Output)) {
        let s = &mut self.scroll;
        if !s.scrolling_v {
            s.buildup.1 += delta.1;
        }
        if !s.scrolling_h {
            s.buildup.0 += delta.0;
        }
        if !s.scrolling_h && !s.scrolling_v {
            if s.buildup.1.abs() >= SCROLL_THRESHOLD {
                s.scrolling_v = true;
            }
            if s.buildup.0.abs() >= SCROLL_THRESHOLD {
                s.scrolling_h = true;
            }
        } else if !s.scrolling_v {
            if delta.1.abs() >= SCROLL_DIRECTION_LOCK_THRESHOLD {
                s.scrolling_v = true;
            }
        } else if !s.scrolling_h && delta.0.abs() >= SCROLL_DIRECTION_LOCK_THRESHOLD {
            s.scrolling_h = true;
        }
        let mut event = delta;
        if !s.scrolling_v {
            event.1 = 0.0;
        }
        if !s.scrolling_h {
            event.0 = 0.0;
        }
        if event == (0.0, 0.0) {
            return;
        }
        if self.config.natural_scroll {
            event = (-event.0, -event.1);
        }
        if self.config.kinetic {
            self.kinetic.push(event.0, event.1, time);
        }
        self.emit_scroll(event.0, event.1, out);
    }

    /// An axis value (libinput units: y positive = down) as REL_HWHEEL_HI_RES / REL_WHEEL_HI_RES (y positive = up).
    fn emit_scroll(&mut self, x: f64, y: f64, out: &mut dyn FnMut(Output)) {
        let (dx, dy) = take_whole(&mut self.scroll_rest, x * V120_PER_UNIT, -y * V120_PER_UNIT);
        if dx != 0 || dy != 0 {
            out(Output::Scroll { dx, dy });
        }
    }

    /// `evdev_stop_scroll`, and GTK's deceleration from the release velocity.
    fn stop_scroll(&mut self, time: u64, out: &mut dyn FnMut(Output)) {
        let s = &mut self.scroll;
        let was = s.scrolling_h || s.scrolling_v;
        s.buildup = (0.0, 0.0);
        s.scrolling_h = false;
        s.scrolling_v = false;
        if !was {
            return;
        }
        let mut kinetic = false;
        if self.config.kinetic {
            let v = self.kinetic.finish();
            self.kinetic.start(v, time);
            kinetic = self.kinetic.running();
        }
        out(Output::ScrollStop { kinetic });
    }

    /// `tp_gesture_set_scroll_buildup`: the average movement since the gesture began, normalized.
    fn set_scroll_buildup(&mut self) {
        let [a, b] = self.gesture_touches;
        let d = |i: usize| {
            let t = &self.touches[i];
            (
                f64::from(t.point.0 - t.gesture_initial.0),
                f64::from(t.point.1 - t.gesture_initial.1),
            )
        };
        let (d0, d1) = (d(a), d(b));
        let avg = ((d0.0 + d1.0) / 2.0, (d0.1 + d1.1) / 2.0);
        // tp_normalize_delta: (DEFAULT_MOUSE_DPI / 25.4) / res per axis.
        self.scroll.buildup = (
            avg.0 * (DEFAULT_MOUSE_DPI / 25.4) / self.res.0,
            avg.1 * (DEFAULT_MOUSE_DPI / 25.4) / self.res.1,
        );
    }

    /// `tp_gesture_end`.
    fn gesture_end(&mut self, time: u64, cancelled: bool, out: &mut dyn FnMut(Output)) {
        let e = match self.state {
            GestureState::None
            | GestureState::Unknown
            | GestureState::ScrollStart
            | GestureState::PinchStart
            | GestureState::SwipeStart => GestureEvent::Reset,
            _ if cancelled => GestureEvent::Cancel,
            _ => GestureEvent::End,
        };
        self.gesture_event(e, time, out);
    }

    /// `tp_gesture_cancel_motion_gestures`.
    fn cancel_motion_gestures(&mut self, time: u64, out: &mut dyn FnMut(Output)) {
        if matches!(
            self.state,
            GestureState::PointerMotion | GestureState::Scroll | GestureState::Pinch | GestureState::Swipe
        ) {
            self.gesture_end(time, true, out);
        }
    }

    /// `tp_gesture_handle_event`: [`gesture_step`] decides; the scroll's stop and buildup happen here.
    fn gesture_event(&mut self, event: GestureEvent, time: u64, out: &mut dyn FnMut(Output)) {
        let step = gesture_step(self.state, event);
        if step.buildup {
            self.set_scroll_buildup();
        }
        if step.stop_scroll {
            self.stop_scroll(time, out);
        }
        self.state = step.next;
    }
}

fn tap_output(e: TapEvent) -> Output {
    Output::Button {
        button: match e.button {
            TapButton::Left => Button::Left,
            TapButton::Right => Button::Right,
            TapButton::Middle => Button::Middle,
        },
        pressed: e.pressed,
    }
}

/// Adds `(x, y)` to the remainder and takes the whole units out of it, rounding toward zero.
fn take_whole(rest: &mut (f64, f64), x: f64, y: f64) -> (i32, i32) {
    rest.0 += x;
    rest.1 += y;
    let wx = rest.0 as i32;
    let wy = rest.1 as i32;
    rest.0 -= f64::from(wx);
    rest.1 -= f64::from(wy);
    (wx, wy)
}

/// `tp_gesture_same_directions`.
fn same_directions(dir1: u32, dir2: u32) -> bool {
    ((dir1 | (dir1 >> 1)) & dir2) != 0
        || ((dir2 | (dir2 >> 1)) & dir1) != 0
        || ((dir1 & 0x80) != 0 && (dir2 & 0x01) != 0)
        || ((dir2 & 0x80) != 0 && (dir1 & 0x01) != 0)
}

/// libinput's direction bits (`libinput-private.h`): N = 1, NE = 2, E = 4, ... NW = 128.
const N: u32 = 1 << 0;
const NE: u32 = 1 << 1;
const E: u32 = 1 << 2;
const SE: u32 = 1 << 3;
const S: u32 = 1 << 4;
const SW: u32 = 1 << 5;
const W: u32 = 1 << 6;
const NW: u32 = 1 << 7;
const UNDEFINED_DIRECTION: u32 = 0xff;

/// `xy_get_direction`.
fn xy_get_direction(x: f64, y: f64) -> u32 {
    if x.abs() < 2.0 && y.abs() < 2.0 {
        return if x > 0.0 && y > 0.0 {
            S | SE | E
        } else if x > 0.0 && y < 0.0 {
            N | NE | E
        } else if x < 0.0 && y > 0.0 {
            S | SW | W
        } else if x < 0.0 && y < 0.0 {
            N | NW | W
        } else if x > 0.0 {
            NE | E | SE
        } else if x < 0.0 {
            NW | W | SW
        } else if y > 0.0 {
            SE | S | SW
        } else if y < 0.0 {
            NE | N | NW
        } else {
            UNDEFINED_DIRECTION
        };
    }
    let pi = core::f64::consts::PI;
    let mut r = atan2(y, x);
    r = fmod(r + 2.5 * pi, 2.0 * pi);
    r *= 4.0 * core::f64::consts::FRAC_1_PI;
    let d1 = ((r + 0.9) as i32) % 8;
    let d2 = ((r + 0.1) as i32) % 8;
    (1 << d1) | (1 << d2)
}

/// `fmod` for a positive divisor.
fn fmod(a: f64, b: f64) -> f64 {
    let q = (a / b) as i64 as f64;
    let r = a - q * b;
    if r < 0.0 { r + b } else { r }
}

/// Square root by Newton's method (core has no `sqrt`): exact to the last bit or one off.
pub(crate) fn sqrt(x: f64) -> f64 {
    if x <= 0.0 || x.is_nan() {
        return 0.0;
    }
    if x.is_infinite() {
        return x;
    }
    // A first guess from the exponent: halve it.
    let bits = x.to_bits();
    let mut g = f64::from_bits((bits >> 1) + (1023u64 << 51));
    for _ in 0..8 {
        g = 0.5 * (g + x / g);
    }
    g
}

/// e^-x for x >= 0: e^-x = 2^-k e^-r with r in [0, ln 2), the series to full precision.
pub(crate) fn exp_neg(x: f64) -> f64 {
    if x.is_nan() || x <= 0.0 {
        return 1.0;
    }
    if x > 745.0 {
        return 0.0;
    }
    let ln2 = core::f64::consts::LN_2;
    let k = (x / ln2) as u32;
    let r = x - f64::from(k) * ln2;
    let mut term = 1.0;
    let mut sum = 1.0;
    for n in 1..30 {
        term *= -r / f64::from(n);
        sum += term;
    }
    let mut scale = 1.0;
    for _ in 0..k {
        scale *= 0.5;
    }
    sum * scale
}

/// atan2 from atan on [0, 1] (two argument halvings, then the series).
fn atan2(y: f64, x: f64) -> f64 {
    let pi = core::f64::consts::PI;
    if x == 0.0 && y == 0.0 {
        return 0.0;
    }
    let (ax, ay) = (x.abs(), y.abs());
    let a = if ay <= ax { atan01(ay / ax) } else { pi / 2.0 - atan01(ax / ay) };
    let a = if x < 0.0 { pi - a } else { a };
    if y < 0.0 { -a } else { a }
}

fn atan01(t: f64) -> f64 {
    // atan(t) = 2 atan(t / (1 + sqrt(1 + t^2))), twice: |t| <= tan(pi/16) ~ 0.199.
    let h = |t: f64| t / (1.0 + sqrt(1.0 + t * t));
    let u = h(h(t));
    let u2 = u * u;
    let mut term = u;
    let mut sum = u;
    for n in 1..24 {
        term *= -u2;
        sum += term / f64::from(2 * n + 1);
    }
    4.0 * sum
}

#[cfg(test)]
mod tests;
