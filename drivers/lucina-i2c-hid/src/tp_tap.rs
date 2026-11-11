//! libinput 1.28.1's touchpad tap state machine (`src/evdev-mt-touchpad-tap.c`), ported to `no_std` Rust.
//!
//! Behaviour is copied state by state from the C original; see the state diagram in libinput's
//! `doc/touchpad-tap-state-machine.svg`. Device facts for the Galaxy Book Go touchpad
//! (14e5:650e, Windows Precision Touchpad, clickpad): `tp->buttons.is_clickpad = true`, no hover
//! states, up to [`MAX_TOUCHES`] contacts, positions already in mm.

/// libinput's `MAX_TOUCHES` for this pad: the Contact Identifier field is 3 bits, logical max 5.
pub const MAX_TOUCHES: usize = 5;

/// `DEFAULT_TAP_TIMEOUT_PERIOD` (ms2us(180)).
const TAP_TIMEOUT_PERIOD_US: u64 = 180_000;
/// `DEFAULT_DRAG_TIMEOUT_PERIOD_BASE` (ms2us(160)).
const DRAG_TIMEOUT_PERIOD_BASE_US: u64 = 160_000;
/// `DEFAULT_DRAG_TIMEOUT_PERIOD_PERFINGER` (ms2us(20)).
const DRAG_TIMEOUT_PERIOD_PERFINGER_US: u64 = 20_000;
/// `DEFAULT_DRAGLOCK_TIMEOUT_PERIOD` (ms2us(300)).
const DRAGLOCK_TIMEOUT_PERIOD_US: u64 = 300_000;
/// `DEFAULT_TAP_MOVE_THRESHOLD` (1.3 mm).
const TAP_MOVE_THRESHOLD_MM: f64 = 1.3;

/// Which button 1, 2 and 3 finger taps press (libinput's LIBINPUT_CONFIG_TAP_MAP_LRM / _LMR).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TapMap {
    Lrm,
    Lmr,
}

/// libinput's LIBINPUT_CONFIG_DRAG_LOCK_DISABLED / _ENABLED_TIMEOUT / _ENABLED_STICKY.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DragLock {
    Off,
    Timeout,
    Sticky,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TapConfig {
    pub enabled: bool,
    pub map: TapMap,
    pub drag: bool,
    pub drag_lock: DragLock,
}

impl TapConfig {
    /// libinput's defaults for a touchpad (tp_tap_default, tp_tap_config_get_default_map, drag and drag lock defaults),
    /// except `enabled`, which the caller passes (libinput leaves tapping off by default; Windows has it on).
    pub fn libinput_default(enabled: bool) -> TapConfig {
        TapConfig {
            enabled,
            map: TapMap::Lrm,         // tp_tap_config_get_default_map
            drag: true,               // tp_drag_default: LIBINPUT_CONFIG_DRAG_ENABLED
            drag_lock: DragLock::Off, // tp_drag_lock_default: LIBINPUT_CONFIG_DRAG_LOCK_DISABLED
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TouchPhase {
    Begin,
    Update,
    End,
}

/// One touch that changed in this frame (libinput's `t->dirty` touches), position in mm from the pad's top left.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TapTouch {
    pub slot: u8,
    pub phase: TouchPhase,
    pub x_mm: f64,
    pub y_mm: f64,
    /// libinput's palm state != PALM_NONE for this touch (the confidence bit said palm).
    pub palm: bool,
    /// libinput's tp_thumb_ignored_for_tap(); always false on this pad (no pressure/size), keep the path.
    pub thumb: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TapButton {
    Left,
    Right,
    Middle,
}

/// A button event the tap machine posts (tp_tap_notify), with the time libinput gives it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TapEvent {
    pub time_us: u64,
    pub button: TapButton,
    pub pressed: bool,
}

/// libinput's `enum tp_tap_state`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Idle,
    Hold,
    Touch,
    Tap1Tapped,
    Tap2Tapped,
    Tap3Tapped,
    Touch2,
    Touch2Hold,
    Touch2Release,
    Touch3,
    Touch3Hold,
    Touch3Release,
    Touch3Release2,
    Drag1OrDoubletap,
    Drag2OrDoubletap,
    Drag3OrDoubletap,
    Drag1OrTap,
    Drag2OrTap,
    Drag3OrTap,
    Drag1,
    Drag2,
    Drag3,
    Drag1Wait,
    Drag2Wait,
    Drag3Wait,
    Drag1Second,
    Drag2Second,
    Drag3Second,
    Dead,
}

/// libinput's `enum tp_tap_touch_state`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TouchState {
    Idle,
    Touch,
    Dead,
}

/// libinput's `enum tap_event`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Event {
    Touch,
    Motion,
    Release,
    Button,
    Timeout,
    Thumb,
    Palm,
    PalmUp,
}

/// libinput's `struct tp_touch`'s `tap` member, per slot.
#[derive(Clone, Copy, Debug)]
struct SlotTap {
    state: TouchState,
    /// `t->tap.initial`, in mm (positions arrive in mm on this pad).
    initial: (f64, f64),
    is_thumb: bool,
    is_palm: bool,
}

impl SlotTap {
    const NEW: SlotTap = SlotTap {
        state: TouchState::Idle,
        initial: (0.0, 0.0),
        is_thumb: false,
        is_palm: false,
    };
}

pub struct Tap {
    config: TapConfig,
    state: State,
    /// `tp->tap.buttons_pressed`.
    buttons_pressed: u32,
    saved_press_time: u64,
    saved_release_time: u64,
    /// `tp->tap.timer`, armed deadline in us.
    timer: Option<u64>,
    /// `tp->tap.nfingers_down` (excludes thumb/palm).
    nfingers_down: u32,
    touches: [SlotTap; MAX_TOUCHES],
    /// A tap timeout reached a hold state, where libinput calls `tp_gesture_tap_timeout`.
    gesture_timeout: bool,
}

impl Tap {
    pub fn new(config: TapConfig) -> Tap {
        Tap {
            config,
            state: State::Idle,
            buttons_pressed: 0,
            saved_press_time: 0,
            saved_release_time: 0,
            timer: None,
            nfingers_down: 0,
            touches: [SlotTap::NEW; MAX_TOUCHES],
            gesture_timeout: false,
        }
    }

    pub fn config(&self) -> TapConfig {
        self.config
    }

    /// libinput's tp_tap_handle_state for one frame: `touches` are the dirty touches in slot order; `button` is
    /// Some(pressed) when the physical button changed in this frame (TOUCHPAD_EVENT_BUTTON_PRESS / _RELEASE);
    /// `fingers_down` is tp->nfingers_down after this frame. Calls `out` for each button event. Returns libinput's
    /// return value: true when pointer motion must be filtered (not posted) for this frame.
    pub fn handle_frame(
        &mut self,
        now_us: u64,
        touches: &[TapTouch],
        button: Option<bool>,
        fingers_down: u32,
        out: &mut dyn FnMut(TapEvent),
    ) -> bool {
        // tp_tap_enabled: enabled && !suspended. This interface has no suspend, so only `enabled` gates.
        if !self.config.enabled {
            return false;
        }

        // tp->buttons.is_clickpad = true (device fact: one physical button under the surface).
        // libinput only queues TOUCHPAD_EVENT_BUTTON_PRESS for clickpads; a button release never
        // reaches the tap machine, so only Some(true) generates TAP_EVENT_BUTTON.
        let button_press = button == Some(true);
        if button_press {
            self.handle_event(None, Event::Button, now_us, out);
        }

        for t in touches {
            let slot = t.slot as usize;
            if slot >= MAX_TOUCHES {
                continue;
            }

            if button_press {
                self.touches[slot].state = TouchState::Dead;
            }

            // If a touch was considered thumb for tapping once, ignore it for the rest of its lifetime.
            if self.touches[slot].is_thumb {
                continue;
            }

            // A palm tap needs to be properly released; otherwise any event from it is ignored.
            if self.touches[slot].is_palm {
                if t.phase == TouchPhase::End {
                    self.handle_event(Some(slot), Event::PalmUp, now_us, out);
                }
                continue;
            }

            // No hover on this pad: touches are never TOUCH_STATE_HOVERING (device fact).

            if t.palm {
                self.touches[slot].is_palm = true;
                self.touches[slot].state = TouchState::Dead;
                if t.phase != TouchPhase::Begin {
                    self.handle_event(Some(slot), Event::Palm, now_us, out);
                    self.nfingers_down -= 1;
                }
            } else if t.phase == TouchPhase::Begin {
                if t.thumb {
                    self.touches[slot].is_thumb = true;
                    continue;
                }
                self.touches[slot].state = TouchState::Touch;
                self.touches[slot].initial = (t.x_mm, t.y_mm);
                self.nfingers_down += 1;
                self.handle_event(Some(slot), Event::Touch, now_us, out);
            } else if t.phase == TouchPhase::End {
                // A touch that ends was down (t->was_down), so it always counts as a release.
                self.nfingers_down -= 1;
                self.handle_event(Some(slot), Event::Release, now_us, out);
                self.touches[slot].state = TouchState::Idle;
            } else if self.state != State::Idle && t.thumb {
                self.handle_event(Some(slot), Event::Thumb, now_us, out);
            } else if self.state != State::Idle && self.exceeds_motion_threshold(slot, t) {
                // Any touch exceeding the threshold turns all touches into DEAD.
                for ts in &mut self.touches {
                    if ts.state == TouchState::Touch {
                        ts.state = TouchState::Dead;
                    }
                }
                self.handle_event(Some(slot), Event::Motion, now_us, out);
            }
        }

        debug_assert!(self.nfingers_down <= fingers_down);

        // In any state where motion exceeding the move threshold would move to the next state,
        // filter that motion until we actually exceed it.
        matches!(
            self.state,
            State::Touch
                | State::Tap1Tapped
                | State::Tap2Tapped
                | State::Tap3Tapped
                | State::Drag1OrDoubletap
                | State::Drag2OrDoubletap
                | State::Drag3OrDoubletap
                | State::Drag1OrTap
                | State::Drag2OrTap
                | State::Drag3OrTap
                | State::Touch2
                | State::Touch3
        )
    }

    /// When the timer must fire (libinput's tap.timer), if armed.
    pub fn deadline(&self) -> Option<u64> {
        self.timer
    }

    /// The timer fired (tp_tap_handle_timeout). Call with now >= deadline().
    pub fn timeout(&mut self, now_us: u64, out: &mut dyn FnMut(TapEvent)) {
        // A libinput timer fires once: libinput_timer_handler clears its expiry before calling the handler.
        self.timer = None;
        self.gesture_timeout = false;
        self.handle_event(None, Event::Timeout, now_us, out);
        for ts in &mut self.touches {
            if ts.state != TouchState::Idle {
                ts.state = TouchState::Dead;
            }
        }
    }

    /// Whether the last [`Tap::timeout`] made libinput call `tp_gesture_tap_timeout` (the touch timed out into HOLD,
    /// TOUCH_2_HOLD or TOUCH_3_HOLD); reading it clears it.
    pub fn take_gesture_timeout(&mut self) -> bool {
        core::mem::take(&mut self.gesture_timeout)
    }

    /// `tp->tap.state` is HOLD, TOUCH_2_HOLD or TOUCH_3_HOLD (`tp_gesture_use_hold_timer`).
    pub fn holding(&self) -> bool {
        matches!(
            self.state,
            State::Hold | State::Touch2Hold | State::Touch3Hold
        )
    }

    /// `tp->tap.state` is DEAD (`tp_gesture_use_hold_timer`).
    pub fn dead(&self) -> bool {
        self.state == State::Dead
    }

    /// tp_tap_dragging_or_double_tapping.
    pub fn dragging_or_double_tapping(&self) -> bool {
        matches!(
            self.state,
            State::Drag1OrDoubletap | State::Drag2OrDoubletap | State::Drag3OrDoubletap
        )
    }

    /// tp_tap_dragging.
    pub fn dragging(&self) -> bool {
        matches!(
            self.state,
            State::Drag1
                | State::Drag2
                | State::Drag3
                | State::Drag1Second
                | State::Drag2Second
                | State::Drag3Second
                | State::Drag1Wait
                | State::Drag2Wait
                | State::Drag3Wait
                | State::Drag1OrTap
                | State::Drag2OrTap
                | State::Drag3OrTap
        )
    }

    /// The tap state's name, for logs and tests (libinput's tap_state_to_str without the "TAP_STATE_" prefix).
    pub fn state_name(&self) -> &'static str {
        match self.state {
            State::Idle => "IDLE",
            State::Hold => "HOLD",
            State::Touch => "TOUCH",
            State::Tap1Tapped => "1FGTAP_TAPPED",
            State::Tap2Tapped => "2FGTAP_TAPPED",
            State::Tap3Tapped => "3FGTAP_TAPPED",
            State::Touch2 => "TOUCH_2",
            State::Touch2Hold => "TOUCH_2_HOLD",
            State::Touch2Release => "TOUCH_2_RELEASE",
            State::Touch3 => "TOUCH_3",
            State::Touch3Hold => "TOUCH_3_HOLD",
            State::Touch3Release => "TOUCH_3_RELEASE",
            State::Touch3Release2 => "TOUCH_3_RELEASE_2",
            State::Drag1OrDoubletap => "1FGTAP_DRAGGING_OR_DOUBLETAP",
            State::Drag2OrDoubletap => "2FGTAP_DRAGGING_OR_DOUBLETAP",
            State::Drag3OrDoubletap => "3FGTAP_DRAGGING_OR_DOUBLETAP",
            State::Drag1OrTap => "1FGTAP_DRAGGING_OR_TAP",
            State::Drag2OrTap => "2FGTAP_DRAGGING_OR_TAP",
            State::Drag3OrTap => "3FGTAP_DRAGGING_OR_TAP",
            State::Drag1 => "1FGTAP_DRAGGING",
            State::Drag2 => "2FGTAP_DRAGGING",
            State::Drag3 => "3FGTAP_DRAGGING",
            State::Drag1Wait => "1FGTAP_DRAGGING_WAIT",
            State::Drag2Wait => "2FGTAP_DRAGGING_WAIT",
            State::Drag3Wait => "3FGTAP_DRAGGING_WAIT",
            State::Drag1Second => "1FGTAP_DRAGGING_2",
            State::Drag2Second => "2FGTAP_DRAGGING_2",
            State::Drag3Second => "3FGTAP_DRAGGING_2",
            State::Dead => "DEAD",
        }
    }

    // ---- tp_tap_notify ----

    fn notify(
        &mut self,
        time_us: u64,
        nfingers: u32,
        pressed: bool,
        out: &mut dyn FnMut(TapEvent),
    ) {
        if !(1..=3).contains(&nfingers) {
            return;
        }
        // button_map[tp->tap.map][nfingers - 1]; tp_gesture_cancel/evdev_pointer_notify_button are
        // outside this machine, the `out` callback is the button notification.
        let button = match (self.config.map, nfingers) {
            (TapMap::Lrm, 1) => TapButton::Left,
            (TapMap::Lrm, 2) => TapButton::Right,
            (TapMap::Lrm, 3) => TapButton::Middle,
            (TapMap::Lmr, 1) => TapButton::Left,
            (TapMap::Lmr, 2) => TapButton::Middle,
            (TapMap::Lmr, 3) => TapButton::Right,
            _ => unreachable!(),
        };
        if pressed {
            self.buttons_pressed |= 1 << nfingers;
        } else {
            self.buttons_pressed &= !(1 << nfingers);
        }
        out(TapEvent {
            time_us,
            button,
            pressed,
        });
    }

    // ---- timer helpers ----

    fn set_timer(&mut self, time: u64) {
        self.timer = Some(time + TAP_TIMEOUT_PERIOD_US);
    }

    fn set_drag_timer(&mut self, time: u64, nfingers_tapped: u32) {
        self.timer = Some(
            time + DRAG_TIMEOUT_PERIOD_BASE_US
                + u64::from(nfingers_tapped) * DRAG_TIMEOUT_PERIOD_PERFINGER_US,
        );
    }

    fn set_draglock_timer(&mut self, time: u64) {
        self.timer = Some(time + DRAGLOCK_TIMEOUT_PERIOD_US);
    }

    fn clear_timer(&mut self) {
        self.timer = None;
    }

    fn move_to_dead(&mut self, slot: Option<usize>, out: &mut dyn FnMut(TapEvent)) {
        let _ = out;
        self.state = State::Dead;
        if let Some(s) = slot {
            self.touches[s].state = TouchState::Dead;
        }
        self.clear_timer();
    }

    // ---- tp_tap_exceeds_motion_threshold ----
    //
    // Positions are already in mm on this pad, so tp_phys_delta/length_in_mm reduce to the raw delta.
    // core has no hypot: dx*dx + dy*dy > T*T is the same test as hypot(dx, dy) > T.
    // The Synaptics-serial and semi-mt special cases do not apply to this device (not those models).
    fn exceeds_motion_threshold(&self, slot: usize, t: &TapTouch) -> bool {
        let (ix, iy) = self.touches[slot].initial;
        let dx = t.x_mm - ix;
        let dy = t.y_mm - iy;
        dx * dx + dy * dy > TAP_MOVE_THRESHOLD_MM * TAP_MOVE_THRESHOLD_MM
    }

    // ---- tp_tap_handle_event and the per-state handlers ----

    fn handle_event(
        &mut self,
        slot: Option<usize>,
        event: Event,
        time: u64,
        out: &mut dyn FnMut(TapEvent),
    ) {
        match self.state {
            State::Idle => self.idle_handle_event(slot, event, time, out),
            State::Touch => self.touch_handle_event(slot, event, time, out),
            State::Hold => self.hold_handle_event(slot, event, time, out),
            State::Tap1Tapped => self.tapped_handle_event(slot, event, time, 1, out),
            State::Tap2Tapped => self.tapped_handle_event(slot, event, time, 2, out),
            State::Tap3Tapped => self.tapped_handle_event(slot, event, time, 3, out),
            State::Touch2 => self.touch2_handle_event(slot, event, time, out),
            State::Touch2Hold => self.touch2_hold_handle_event(slot, event, time, out),
            State::Touch2Release => self.touch2_release_handle_event(slot, event, time, out),
            State::Touch3 => self.touch3_handle_event(slot, event, time, out),
            State::Touch3Hold => self.touch3_hold_handle_event(slot, event, time, out),
            State::Touch3Release => self.touch3_release_handle_event(slot, event, time, out),
            State::Touch3Release2 => self.touch3_release2_handle_event(slot, event, time, out),
            State::Drag1OrDoubletap => {
                self.dragging_or_doubletap_handle_event(slot, event, time, 1, out)
            }
            State::Drag2OrDoubletap => {
                self.dragging_or_doubletap_handle_event(slot, event, time, 2, out)
            }
            State::Drag3OrDoubletap => {
                self.dragging_or_doubletap_handle_event(slot, event, time, 3, out)
            }
            State::Drag1 => self.dragging_handle_event(slot, event, time, 1, out),
            State::Drag2 => self.dragging_handle_event(slot, event, time, 2, out),
            State::Drag3 => self.dragging_handle_event(slot, event, time, 3, out),
            State::Drag1Wait => self.dragging_wait_handle_event(slot, event, time, 1, out),
            State::Drag2Wait => self.dragging_wait_handle_event(slot, event, time, 2, out),
            State::Drag3Wait => self.dragging_wait_handle_event(slot, event, time, 3, out),
            State::Drag1OrTap => self.dragging_tap_handle_event(slot, event, time, 1, out),
            State::Drag2OrTap => self.dragging_tap_handle_event(slot, event, time, 2, out),
            State::Drag3OrTap => self.dragging_tap_handle_event(slot, event, time, 3, out),
            State::Drag1Second => self.dragging2_handle_event(slot, event, time, 1, out),
            State::Drag2Second => self.dragging2_handle_event(slot, event, time, 2, out),
            State::Drag3Second => self.dragging2_handle_event(slot, event, time, 3, out),
            State::Dead => self.dead_handle_event(slot, event, time, out),
        }
        if self.state == State::Idle || self.state == State::Dead {
            self.clear_timer();
        }
    }

    fn idle_handle_event(
        &mut self,
        _slot: Option<usize>,
        event: Event,
        time: u64,
        _out: &mut dyn FnMut(TapEvent),
    ) {
        match event {
            Event::Touch => {
                self.state = State::Touch;
                self.saved_press_time = time;
                self.set_timer(time);
            }
            Event::Release | Event::Timeout | Event::PalmUp => {}
            Event::Motion | Event::Thumb => {} // log_tap_bug
            Event::Button => self.state = State::Dead,
            Event::Palm => self.state = State::Idle,
        }
    }

    fn touch_handle_event(
        &mut self,
        slot: Option<usize>,
        event: Event,
        time: u64,
        out: &mut dyn FnMut(TapEvent),
    ) {
        match event {
            Event::Touch => {
                self.state = State::Touch2;
                self.saved_press_time = time;
                self.set_timer(time);
            }
            Event::Release => {
                self.notify(self.saved_press_time, 1, true, out);
                if self.config.drag {
                    self.state = State::Tap1Tapped;
                    self.saved_release_time = time;
                    self.set_drag_timer(time, 1);
                } else {
                    self.notify(time, 1, false, out);
                    self.state = State::Idle;
                }
            }
            Event::Motion => self.move_to_dead(slot, out),
            Event::Timeout => {
                self.state = State::Hold;
                self.clear_timer();
                // tp_gesture_tap_timeout is called here; the gesture side reads it with take_gesture_timeout.
                self.gesture_timeout = true;
            }
            Event::Button => self.state = State::Dead,
            Event::Thumb => {
                self.state = State::Idle;
                if let Some(s) = slot {
                    self.touches[s].is_thumb = true;
                }
                self.nfingers_down -= 1;
                if let Some(s) = slot {
                    self.touches[s].state = TouchState::Dead;
                }
                self.clear_timer();
            }
            Event::Palm => {
                self.state = State::Idle;
                self.clear_timer();
            }
            Event::PalmUp => {}
        }
    }

    fn hold_handle_event(
        &mut self,
        slot: Option<usize>,
        event: Event,
        time: u64,
        _out: &mut dyn FnMut(TapEvent),
    ) {
        match event {
            Event::Touch => {
                self.state = State::Touch2;
                self.saved_press_time = time;
                self.set_timer(time);
            }
            Event::Release => self.state = State::Idle,
            Event::Motion => self.move_to_dead(slot, _out),
            Event::Timeout => {}
            Event::Button => self.state = State::Dead,
            Event::Thumb => {
                self.state = State::Idle;
                if let Some(s) = slot {
                    self.touches[s].is_thumb = true;
                    self.touches[s].state = TouchState::Dead;
                }
                self.nfingers_down -= 1;
            }
            Event::Palm => self.state = State::Idle,
            Event::PalmUp => {}
        }
    }

    fn tapped_handle_event(
        &mut self,
        _slot: Option<usize>,
        event: Event,
        time: u64,
        nfingers_tapped: u32,
        out: &mut dyn FnMut(TapEvent),
    ) {
        match event {
            Event::Motion | Event::Release | Event::Thumb | Event::Palm => {} // log_tap_bug
            Event::Touch => {
                self.state = match nfingers_tapped {
                    1 => State::Drag1OrDoubletap,
                    2 => State::Drag2OrDoubletap,
                    _ => State::Drag3OrDoubletap,
                };
                self.saved_press_time = time;
                self.set_timer(time);
            }
            Event::Timeout => {
                self.state = State::Idle;
                self.notify(self.saved_release_time, nfingers_tapped, false, out);
            }
            Event::Button => {
                self.state = State::Dead;
                self.notify(self.saved_release_time, nfingers_tapped, false, out);
            }
            Event::PalmUp => {}
        }
    }

    fn touch2_handle_event(
        &mut self,
        slot: Option<usize>,
        event: Event,
        time: u64,
        _out: &mut dyn FnMut(TapEvent),
    ) {
        match event {
            Event::Touch => {
                self.state = State::Touch3;
                self.saved_press_time = time;
                self.set_timer(time);
            }
            Event::Release => {
                self.state = State::Touch2Release;
                self.saved_release_time = time;
                self.set_timer(time);
            }
            Event::Motion => self.move_to_dead(slot, _out),
            Event::Timeout => {
                self.state = State::Touch2Hold;
                // tp_gesture_tap_timeout is called here; the gesture side reads it with take_gesture_timeout.
                self.gesture_timeout = true;
            }
            Event::Button => self.state = State::Dead,
            Event::Thumb => {}
            Event::Palm => self.state = State::Touch,
            Event::PalmUp => {}
        }
    }

    fn touch2_hold_handle_event(
        &mut self,
        slot: Option<usize>,
        event: Event,
        time: u64,
        _out: &mut dyn FnMut(TapEvent),
    ) {
        match event {
            Event::Touch => {
                self.state = State::Touch3;
                self.saved_press_time = time;
                self.set_timer(time);
            }
            Event::Release => self.state = State::Hold,
            Event::Motion => self.move_to_dead(slot, _out),
            Event::Timeout => self.state = State::Touch2Hold,
            Event::Button => self.state = State::Dead,
            Event::Thumb => {}
            Event::Palm => self.state = State::Hold,
            Event::PalmUp => {}
        }
    }

    fn touch2_release_handle_event(
        &mut self,
        slot: Option<usize>,
        event: Event,
        time: u64,
        out: &mut dyn FnMut(TapEvent),
    ) {
        match event {
            Event::Touch => {
                self.state = State::Touch2Hold;
                if let Some(s) = slot {
                    self.touches[s].state = TouchState::Dead;
                }
                self.clear_timer();
            }
            Event::Release => {
                self.notify(self.saved_press_time, 2, true, out);
                if self.config.drag {
                    self.state = State::Tap2Tapped;
                    self.set_drag_timer(time, 2);
                } else {
                    self.notify(self.saved_release_time, 2, false, out);
                    self.state = State::Idle;
                }
            }
            Event::Motion => self.move_to_dead(slot, out),
            Event::Timeout => self.state = State::Hold,
            Event::Button => self.state = State::Dead,
            Event::Thumb => {}
            Event::Palm => {
                // One saved press time, overwritten by the last touch down; see the comment in
                // tp_tap_touch2_release_handle_event.
                self.notify(self.saved_press_time, 1, true, out);
                if self.config.drag {
                    self.state = State::Tap1Tapped;
                } else {
                    self.notify(self.saved_release_time, 1, false, out);
                    self.state = State::Idle;
                }
            }
            Event::PalmUp => {}
        }
    }

    fn touch3_handle_event(
        &mut self,
        slot: Option<usize>,
        event: Event,
        time: u64,
        _out: &mut dyn FnMut(TapEvent),
    ) {
        match event {
            Event::Touch => {
                self.state = State::Dead;
                self.clear_timer();
            }
            Event::Motion => self.move_to_dead(slot, _out),
            Event::Timeout => {
                self.state = State::Touch3Hold;
                self.clear_timer();
                // tp_gesture_tap_timeout is called here; the gesture side reads it with take_gesture_timeout.
                self.gesture_timeout = true;
            }
            Event::Release => {
                self.state = State::Touch3Release;
                self.saved_release_time = time;
                self.set_timer(time);
            }
            Event::Button => self.state = State::Dead,
            Event::Thumb => {}
            Event::Palm => self.state = State::Touch2,
            Event::PalmUp => {}
        }
    }

    fn touch3_hold_handle_event(
        &mut self,
        slot: Option<usize>,
        event: Event,
        time: u64,
        _out: &mut dyn FnMut(TapEvent),
    ) {
        match event {
            Event::Touch => {
                self.state = State::Dead;
                self.set_timer(time);
            }
            Event::Release => self.state = State::Touch2Hold,
            Event::Motion => self.move_to_dead(slot, _out),
            Event::Timeout => {}
            Event::Button => self.state = State::Dead,
            Event::Thumb => {}
            Event::Palm => self.state = State::Touch2Hold,
            Event::PalmUp => {}
        }
    }

    fn touch3_release_handle_event(
        &mut self,
        slot: Option<usize>,
        event: Event,
        time: u64,
        out: &mut dyn FnMut(TapEvent),
    ) {
        match event {
            Event::Touch => {
                self.notify(self.saved_press_time, 3, true, out);
                self.notify(self.saved_release_time, 3, false, out);
                self.state = State::Touch3;
                self.saved_press_time = time;
                self.set_timer(time);
            }
            Event::Release => {
                self.state = State::Touch3Release2;
                self.set_timer(time);
            }
            Event::Motion => {
                self.notify(self.saved_press_time, 3, true, out);
                self.notify(self.saved_release_time, 3, false, out);
                self.move_to_dead(slot, out);
            }
            Event::Timeout => {
                self.notify(self.saved_press_time, 3, true, out);
                self.notify(self.saved_release_time, 3, false, out);
                self.state = State::Touch2Hold;
            }
            Event::Button => {
                self.notify(self.saved_press_time, 3, true, out);
                self.notify(self.saved_release_time, 3, false, out);
                self.state = State::Dead;
            }
            Event::Thumb => {}
            Event::Palm => self.state = State::Touch2Release,
            Event::PalmUp => {}
        }
    }

    fn touch3_release2_handle_event(
        &mut self,
        slot: Option<usize>,
        event: Event,
        time: u64,
        out: &mut dyn FnMut(TapEvent),
    ) {
        match event {
            Event::Touch => {
                self.notify(self.saved_press_time, 3, true, out);
                self.notify(self.saved_release_time, 3, false, out);
                self.state = State::Touch2;
                self.saved_press_time = time;
                self.set_timer(time);
            }
            Event::Release => {
                self.notify(self.saved_press_time, 3, true, out);
                if self.config.drag {
                    self.state = State::Tap3Tapped;
                    self.set_drag_timer(time, 3);
                } else {
                    self.notify(self.saved_release_time, 3, false, out);
                    self.state = State::Idle;
                }
            }
            Event::Motion => {
                self.notify(self.saved_press_time, 3, true, out);
                self.notify(self.saved_release_time, 3, false, out);
                self.move_to_dead(slot, out);
            }
            Event::Timeout => {
                self.notify(self.saved_press_time, 3, true, out);
                self.notify(self.saved_release_time, 3, false, out);
                self.state = State::Hold;
            }
            Event::Button => {
                self.notify(self.saved_press_time, 3, true, out);
                self.notify(self.saved_release_time, 3, false, out);
                self.state = State::Dead;
            }
            Event::Thumb => {}
            Event::Palm => {
                self.notify(self.saved_press_time, 2, true, out);
                if self.config.drag {
                    // The timestamp of the last real finger release is lost; the in-progress
                    // delay for the finger that became a palm has to do (see C comment).
                    self.state = State::Tap2Tapped;
                } else {
                    self.notify(self.saved_release_time, 2, false, out);
                    self.state = State::Idle;
                }
            }
            Event::PalmUp => {}
        }
    }

    fn dragging_or_doubletap_handle_event(
        &mut self,
        _slot: Option<usize>,
        event: Event,
        time: u64,
        nfingers_tapped: u32,
        out: &mut dyn FnMut(TapEvent),
    ) {
        match event {
            Event::Touch => {
                self.notify(self.saved_release_time, nfingers_tapped, false, out);
                self.state = State::Touch2;
                self.saved_press_time = time;
                self.set_timer(time);
            }
            Event::Release => {
                self.state = State::Tap1Tapped;
                self.notify(self.saved_release_time, nfingers_tapped, false, out);
                self.notify(self.saved_press_time, 1, true, out);
                self.saved_release_time = time;
                self.set_timer(time);
            }
            Event::Motion | Event::Timeout => {
                self.state = match nfingers_tapped {
                    1 => State::Drag1,
                    2 => State::Drag2,
                    _ => State::Drag3,
                };
            }
            Event::Button => {
                self.state = State::Dead;
                self.notify(self.saved_release_time, nfingers_tapped, false, out);
            }
            Event::Thumb => {}
            Event::Palm => {
                self.state = match nfingers_tapped {
                    1 => State::Tap1Tapped,
                    2 => State::Tap2Tapped,
                    _ => State::Tap3Tapped,
                };
            }
            Event::PalmUp => {}
        }
    }

    fn dragging_handle_event(
        &mut self,
        _slot: Option<usize>,
        event: Event,
        time: u64,
        nfingers_tapped: u32,
        out: &mut dyn FnMut(TapEvent),
    ) {
        match event {
            Event::Touch => {
                self.state = match nfingers_tapped {
                    1 => State::Drag1Second,
                    2 => State::Drag2Second,
                    _ => State::Drag3Second,
                };
            }
            Event::Release => {
                if self.config.drag_lock != DragLock::Off {
                    self.state = match nfingers_tapped {
                        1 => State::Drag1Wait,
                        2 => State::Drag2Wait,
                        _ => State::Drag3Wait,
                    };
                    if self.config.drag_lock == DragLock::Timeout {
                        self.set_draglock_timer(time);
                    }
                } else {
                    self.notify(time, nfingers_tapped, false, out);
                    self.state = State::Idle;
                }
            }
            Event::Motion | Event::Timeout => {}
            Event::Button => {
                self.state = State::Dead;
                self.notify(time, nfingers_tapped, false, out);
            }
            Event::Thumb => {}
            Event::Palm => {
                self.notify(self.saved_release_time, nfingers_tapped, false, out);
                self.state = State::Idle;
            }
            Event::PalmUp => {}
        }
    }

    fn dragging_wait_handle_event(
        &mut self,
        _slot: Option<usize>,
        event: Event,
        time: u64,
        nfingers_tapped: u32,
        out: &mut dyn FnMut(TapEvent),
    ) {
        match event {
            Event::Touch => {
                self.state = match nfingers_tapped {
                    1 => State::Drag1OrTap,
                    2 => State::Drag2OrTap,
                    _ => State::Drag3OrTap,
                };
                self.set_timer(time);
            }
            Event::Release | Event::Motion | Event::Thumb | Event::Palm => {} // log_tap_bug
            Event::Timeout => {
                self.state = State::Idle;
                self.notify(time, nfingers_tapped, false, out);
            }
            Event::Button => {
                self.state = State::Dead;
                self.notify(time, nfingers_tapped, false, out);
            }
            Event::PalmUp => {}
        }
    }

    fn dragging_tap_handle_event(
        &mut self,
        slot: Option<usize>,
        event: Event,
        time: u64,
        nfingers_tapped: u32,
        out: &mut dyn FnMut(TapEvent),
    ) {
        match event {
            Event::Touch => {
                self.notify(time, nfingers_tapped, false, out);
                self.clear_timer();
                self.move_to_dead(slot, out);
            }
            Event::Release => {
                self.state = State::Idle;
                self.notify(time, nfingers_tapped, false, out);
            }
            Event::Motion | Event::Timeout => {
                self.state = match nfingers_tapped {
                    1 => State::Drag1,
                    2 => State::Drag2,
                    _ => State::Drag3,
                };
            }
            Event::Button => {
                self.state = State::Dead;
                self.notify(time, nfingers_tapped, false, out);
            }
            Event::Thumb => {}
            Event::Palm => {
                self.state = match nfingers_tapped {
                    1 => State::Drag1Wait,
                    2 => State::Drag2Wait,
                    _ => State::Drag3Wait,
                };
            }
            Event::PalmUp => {}
        }
    }

    fn dragging2_handle_event(
        &mut self,
        _slot: Option<usize>,
        event: Event,
        time: u64,
        nfingers_tapped: u32,
        out: &mut dyn FnMut(TapEvent),
    ) {
        match event {
            Event::Release => {
                self.state = match nfingers_tapped {
                    1 => State::Drag1,
                    2 => State::Drag2,
                    _ => State::Drag3,
                };
            }
            Event::Touch => {
                self.state = State::Dead;
                self.notify(time, nfingers_tapped, false, out);
            }
            Event::Motion | Event::Timeout => {}
            Event::Button => {
                self.state = State::Dead;
                self.notify(time, nfingers_tapped, false, out);
            }
            Event::Thumb => {}
            Event::Palm => {
                self.state = match nfingers_tapped {
                    1 => State::Drag1,
                    2 => State::Drag2,
                    _ => State::Drag3,
                };
            }
            Event::PalmUp => {}
        }
    }

    fn dead_handle_event(
        &mut self,
        _slot: Option<usize>,
        event: Event,
        _time: u64,
        _out: &mut dyn FnMut(TapEvent),
    ) {
        match event {
            Event::Release => {
                if self.nfingers_down == 0 {
                    self.state = State::Idle;
                }
            }
            Event::Touch | Event::Motion | Event::Timeout | Event::Button | Event::Thumb => {}
            Event::Palm | Event::PalmUp => {
                if self.nfingers_down == 0 {
                    self.state = State::Idle;
                }
            }
        }
    }

    // ---- tp_release_all_taps / tp_tap_suspend / tp_tap_resume ----
    //
    // The public interface above has no suspend/resume entry point, so these are kept private.

    #[allow(dead_code)]
    fn release_all_taps(&mut self, now: u64, out: &mut dyn FnMut(TapEvent)) {
        for i in 1..=3u32 {
            if self.buttons_pressed & (1 << i) != 0 {
                self.notify(now, i, false, out);
            }
        }
        // To neutralize all current touches, make them all palms.
        for ts in &mut self.touches {
            ts.is_palm = true;
            ts.state = TouchState::Dead;
        }
        self.state = State::Idle;
        self.nfingers_down = 0;
    }

    #[allow(dead_code)]
    fn suspend(&mut self, time: u64, out: &mut dyn FnMut(TapEvent)) {
        // tp_tap_enabled_update(tp, true, enabled, time): disabling releases all taps.
        self.release_all_taps(time, out);
    }

    #[allow(dead_code)]
    fn resume(&mut self) {
        // tp_tap_enabled_update(tp, false, enabled, time): on resume all touches are
        // considered palms and the machine goes idle.
        for ts in &mut self.touches {
            ts.is_palm = true;
            ts.state = TouchState::Dead;
        }
        self.state = State::Idle;
        self.nfingers_down = 0;
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use std::vec;
    use std::vec::Vec;

    fn touch(slot: u8, phase: TouchPhase, x: f64, y: f64) -> TapTouch {
        TapTouch {
            slot,
            phase,
            x_mm: x,
            y_mm: y,
            palm: false,
            thumb: false,
        }
    }

    fn run(
        tap: &mut Tap,
        now: u64,
        touches: &[TapTouch],
        button: Option<bool>,
        fingers: u32,
    ) -> (Vec<TapEvent>, bool) {
        let mut events = Vec::new();
        let filter = tap.handle_frame(now, touches, button, fingers, &mut |e| events.push(e));
        (events, filter)
    }

    fn enabled() -> TapConfig {
        TapConfig::libinput_default(true)
    }

    // Mirrors START_TEST(touchpad_1fg_tap): one finger down and up within the timeout presses
    // and releases the left button; the press carries the time of the touch down.
    #[test]
    fn one_finger_tap() {
        let mut tap = Tap::new(enabled());
        let (ev, _) = run(
            &mut tap,
            0,
            &[touch(0, TouchPhase::Begin, 10.0, 10.0)],
            None,
            1,
        );
        assert!(ev.is_empty());
        assert_eq!(tap.state_name(), "TOUCH");
        assert_eq!(tap.deadline(), Some(180_000));

        let (ev, _) = run(
            &mut tap,
            100_000,
            &[touch(0, TouchPhase::End, 10.0, 10.0)],
            None,
            0,
        );
        assert_eq!(
            ev,
            vec![TapEvent {
                time_us: 0,
                button: TapButton::Left,
                pressed: true
            }]
        );
        assert_eq!(tap.state_name(), "1FGTAP_TAPPED");
        // drag timeout: 160 ms + 1 * 20 ms after the release
        assert_eq!(tap.deadline(), Some(100_000 + 180_000));

        let mut ev = Vec::new();
        tap.timeout(280_000, &mut |e| ev.push(e));
        assert_eq!(
            ev,
            vec![TapEvent {
                time_us: 100_000,
                button: TapButton::Left,
                pressed: false
            }]
        );
        assert_eq!(tap.state_name(), "IDLE");
    }

    // Mirrors START_TEST(touchpad_2fg_tap) and START_TEST(touchpad_2fg_tap_inverted):
    // two fingers tap the right button with LRM, the middle button with LMR.
    #[test]
    fn two_finger_tap() {
        for (map, want) in [
            (TapMap::Lrm, TapButton::Right),
            (TapMap::Lmr, TapButton::Middle),
        ] {
            let mut cfg = enabled();
            cfg.map = map;
            let mut tap = Tap::new(cfg);
            run(
                &mut tap,
                0,
                &[touch(0, TouchPhase::Begin, 10.0, 10.0)],
                None,
                1,
            );
            run(
                &mut tap,
                10_000,
                &[touch(1, TouchPhase::Begin, 20.0, 10.0)],
                None,
                2,
            );
            run(
                &mut tap,
                50_000,
                &[touch(0, TouchPhase::End, 10.0, 10.0)],
                None,
                1,
            );
            let (ev, _) = run(
                &mut tap,
                60_000,
                &[touch(1, TouchPhase::End, 20.0, 10.0)],
                None,
                0,
            );
            assert_eq!(
                ev,
                vec![TapEvent {
                    time_us: 10_000,
                    button: want,
                    pressed: true
                }]
            );
            let mut ev = Vec::new();
            tap.timeout(tap.deadline().unwrap(), &mut |e| ev.push(e));
            assert_eq!(
                ev,
                // saved_release_time is the first finger's release (TOUCH_2_RELEASE), as in libinput
                vec![TapEvent {
                    time_us: 50_000,
                    button: want,
                    pressed: false
                }]
            );
        }
    }

    // Mirrors START_TEST(touchpad_3fg_tap): three fingers tap the middle button with LRM.
    #[test]
    fn three_finger_tap() {
        let mut tap = Tap::new(enabled());
        run(
            &mut tap,
            0,
            &[touch(0, TouchPhase::Begin, 10.0, 10.0)],
            None,
            1,
        );
        run(
            &mut tap,
            10_000,
            &[touch(1, TouchPhase::Begin, 20.0, 10.0)],
            None,
            2,
        );
        run(
            &mut tap,
            20_000,
            &[touch(2, TouchPhase::Begin, 30.0, 10.0)],
            None,
            3,
        );
        run(
            &mut tap,
            50_000,
            &[touch(0, TouchPhase::End, 10.0, 10.0)],
            None,
            2,
        );
        run(
            &mut tap,
            60_000,
            &[touch(1, TouchPhase::End, 20.0, 10.0)],
            None,
            1,
        );
        let (ev, _) = run(
            &mut tap,
            70_000,
            &[touch(2, TouchPhase::End, 30.0, 10.0)],
            None,
            0,
        );
        assert_eq!(
            ev,
            vec![TapEvent {
                time_us: 20_000,
                button: TapButton::Middle,
                pressed: true
            }]
        );
    }

    // Mirrors START_TEST(touchpad_no_first_fg_tap_after_move): a touch that moves more than
    // 1.3 mm from where it began is not a tap.
    #[test]
    fn no_tap_after_move() {
        let mut tap = Tap::new(enabled());
        run(
            &mut tap,
            0,
            &[touch(0, TouchPhase::Begin, 10.0, 10.0)],
            None,
            1,
        );
        let (ev, filter) = run(
            &mut tap,
            10_000,
            &[touch(0, TouchPhase::Update, 12.0, 10.0)],
            None,
            1,
        );
        assert!(ev.is_empty());
        assert_eq!(tap.state_name(), "DEAD");
        assert!(!filter);
        let (ev, _) = run(
            &mut tap,
            20_000,
            &[touch(0, TouchPhase::End, 12.0, 10.0)],
            None,
            0,
        );
        assert!(ev.is_empty());
        assert_eq!(tap.state_name(), "IDLE");
    }

    // Mirrors START_TEST(touchpad_no_2fg_tap_after_timeout): a touch held longer than 180 ms
    // is not a tap.
    #[test]
    fn no_tap_after_timeout() {
        let mut tap = Tap::new(enabled());
        run(
            &mut tap,
            0,
            &[touch(0, TouchPhase::Begin, 10.0, 10.0)],
            None,
            1,
        );
        let mut ev = Vec::new();
        tap.timeout(180_000, &mut |e| ev.push(e));
        assert!(ev.is_empty());
        assert_eq!(tap.state_name(), "HOLD");
        let (ev, _) = run(
            &mut tap,
            200_000,
            &[touch(0, TouchPhase::End, 10.0, 10.0)],
            None,
            0,
        );
        assert!(ev.is_empty());
        assert_eq!(tap.state_name(), "IDLE");
    }

    // Mirrors START_TEST(touchpad_tap_n_drag): tap, touch again within the drag timeout, move
    // (button held), lift (released at lift time, drag lock off).
    #[test]
    fn tap_and_drag() {
        let mut tap = Tap::new(enabled());
        run(
            &mut tap,
            0,
            &[touch(0, TouchPhase::Begin, 10.0, 10.0)],
            None,
            1,
        );
        let (ev, _) = run(
            &mut tap,
            100_000,
            &[touch(0, TouchPhase::End, 10.0, 10.0)],
            None,
            0,
        );
        assert_eq!(ev.len(), 1);
        // second touch within the drag timeout (deadline 280_000)
        run(
            &mut tap,
            200_000,
            &[touch(0, TouchPhase::Begin, 10.0, 10.0)],
            None,
            1,
        );
        assert_eq!(tap.state_name(), "1FGTAP_DRAGGING_OR_DOUBLETAP");
        let (ev, _) = run(
            &mut tap,
            250_000,
            &[touch(0, TouchPhase::Update, 15.0, 10.0)],
            None,
            1,
        );
        assert!(ev.is_empty());
        assert_eq!(tap.state_name(), "1FGTAP_DRAGGING");
        assert!(tap.dragging());
        let (ev, _) = run(
            &mut tap,
            300_000,
            &[touch(0, TouchPhase::End, 15.0, 10.0)],
            None,
            0,
        );
        assert_eq!(
            ev,
            vec![TapEvent {
                time_us: 300_000,
                button: TapButton::Left,
                pressed: false
            }]
        );
        assert_eq!(tap.state_name(), "IDLE");
        assert!(!tap.dragging());
    }

    // Mirrors the palm handling in tp_tap_handle_state (palm touches are never taps; exercised
    // by the palm paths of test-touchpad-tap.c): a palm touch produces no tap.
    #[test]
    fn palm_is_not_a_tap() {
        let mut tap = Tap::new(enabled());
        let mut p = touch(0, TouchPhase::Begin, 10.0, 10.0);
        p.palm = true;
        let (ev, _) = run(&mut tap, 0, &[p], None, 1);
        assert!(ev.is_empty());
        assert_eq!(tap.state_name(), "IDLE");
        let mut p = touch(0, TouchPhase::End, 10.0, 10.0);
        p.palm = true;
        let (ev, _) = run(&mut tap, 50_000, &[p], None, 0);
        assert!(ev.is_empty());
        assert_eq!(tap.state_name(), "IDLE");
    }

    // Mirrors START_TEST(clickpad_1fg_tap_click): a physical clickpad press during a tap kills
    // the tap (this pad is a clickpad, so the press reaches the tap machine).
    #[test]
    fn click_during_tap() {
        let mut tap = Tap::new(enabled());
        run(
            &mut tap,
            0,
            &[touch(0, TouchPhase::Begin, 10.0, 10.0)],
            None,
            1,
        );
        let (ev, _) = run(&mut tap, 50_000, &[], Some(true), 1);
        assert!(ev.is_empty());
        assert_eq!(tap.state_name(), "DEAD");
        let (ev, _) = run(
            &mut tap,
            100_000,
            &[touch(0, TouchPhase::End, 10.0, 10.0)],
            None,
            0,
        );
        assert!(ev.is_empty());
        assert_eq!(tap.state_name(), "IDLE");
    }
}
