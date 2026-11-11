// The touchpad's rules that must hold whatever the pad sends: which slot a contact goes to, when a frame of a hybrid-mode
// pad is whole, how the gesture state moves (a scroll always ends with its stop, and only a scroll has one; a hold, swipe or pinch begins only
// on entering its state and always ends when left, cancelled or not as libinput says), how the finger
// count settles, and how hi-res scrolling becomes whole notches. Verus checks this very file: `//@` lines hold the proof,
// see scripts/verify-shipped.sh (D-011). ptp.rs, touchpad.rs and the kernel driver call these functions.
//
// The originals: `input_mt_get_slot_by_key` (drivers/input/input-mt.c), `mt_touch_report`'s contact count rule and
// `mt_process_slot`'s MT_QUIRK_CONTACT_CNT_ACCURATE (drivers/hid/hid-multitouch.c), `tp_gesture_handle_event` and
// `tp_gesture_update_finger_state` (libinput evdev-mt-touchpad-gestures.c), `hidinput_handle_scroll`
// (drivers/hid/hid-input.c).
//
// Not proved: the floating-point parts (pointer acceleration, scroll constraints, kinetic deceleration), tested instead.

/// Slots the tracker keeps (`ptp::MAX_SLOTS`).
pub const SLOTS: usize = 10;

/// `input_mt_get_slot_by_key`: the active slot whose key is `key`, else the first slot that is free and was not used in
/// this frame; None if there is neither. Only the first `n` slots exist.
pub fn slot_by_key(
    active: &[bool; SLOTS],
    keys: &[i32; SLOTS],
    used: &[bool; SLOTS],
    n: usize,
    key: i32,
) -> Option<usize> //@ret
//@ requires n <= SLOTS,
//@ ensures match ret {
//@     Some(i) => i < n && ((active[i as int] && keys[i as int] == key) || (!active[i as int] && !used[i as int])),
//@     None => forall|j: int| 0 <= j < n ==> !(active[j] && keys[j] == key) && (active[j] || used[j]),
//@ },
//@     (exists|j: int| 0 <= j < n && active[j] && keys[j] == key) ==> (ret.is_some() && active[ret.unwrap() as int]),
{
    let mut i = 0;
    while i < n
    //@ invariant n <= SLOTS, i <= n, forall|j: int| 0 <= j < i ==> !(active[j] && keys[j] == key),
    //@ decreases n - i,
    {
        if active[i] && keys[i] == key {
            return Some(i);
        }
        i += 1;
    }
    let mut i = 0;
    while i < n
    //@ invariant n <= SLOTS, i <= n, forall|j: int| 0 <= j < n ==> !(active[j] && keys[j] == key),
    //@     forall|j: int| 0 <= j < i ==> (active[j] || used[j]),
    //@ decreases n - i,
    {
        if !active[i] && !used[i] {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// `mt_touch_report`'s frame size for a Win8 Precision Touchpad: the first packet of a frame may carry a contact count of
/// 0 with only a button in it, and is a first packet if the scan time changed; any other non-zero count starts a frame.
pub fn expected_contacts(
    num_received: u32,
    scan_changed: bool,
    count: u32,
    num_expected: u32,
) -> u32 //@ret
//@ ensures count != 0 ==> ret == count,
//@     count == 0 && !(num_received == 0 && scan_changed) ==> ret == num_expected,
//@     count == 0 && num_received == 0 && scan_changed ==> ret == 0,
{
    if (num_received == 0 && scan_changed) || count != 0 {
        count
    } else {
        num_expected
    }
}

/// MT_QUIRK_CONTACT_CNT_ACCURATE: with a contact count, a contact past the count is not taken (`-EAGAIN`).
pub fn takes_contact(have_count: bool, num_received: u32, num_expected: u32) -> bool //@ret
//@ ensures ret == (!have_count || num_received < num_expected),
{
    !have_count || num_received < num_expected
}

/// A frame ends when every expected contact came (`app->num_received >= app->num_expected`).
pub fn frame_ends(num_received: u32, num_expected: u32) -> bool //@ret
//@ ensures ret == (num_received >= num_expected),
{
    num_received >= num_expected
}

/// `enum tp_gesture_state`, the states the port has (no 3-finger drag: it is off by default in libinput 1.28.1).
#[derive(Clone, Copy, PartialEq, Eq)]
#[derive(Debug)] //@omit
pub enum GestureState {
    None,
    Unknown,
    Hold,
    HoldAndMotion,
    PointerMotion,
    ScrollStart,
    Scroll,
    PinchStart,
    Pinch,
    SwipeStart,
    Swipe,
}

/// `enum gesture_event`, the events the port sends. libinput's HOLD_TIMEOUT and TAP_TIMEOUT are one event here: every state
/// takes them alike. `SwipeBegin` and `PinchBegin` are not libinput events: they stand for `tp_gesture_handle_state_swipe_start`
/// and `_pinch_start` setting the state to SWIPE / PINCH at the first motion, so that the step decides that too.
#[derive(Clone, Copy, PartialEq, Eq)]
#[derive(Debug)] //@omit
pub enum GestureEvent {
    Reset,
    End,
    Cancel,
    FingerDetected,
    FingerSwitchTimeout,
    HoldTimeout,
    HoldAndMotionStart,
    PointerMotionStart,
    ScrollStart,
    PinchStart,
    SwipeStart,
    SwipeBegin,
    PinchBegin,
}

/// The gestures libinput notifies clients of (`gesture_notify_hold_begin` and `_end`, `_swipe`, `_pinch`).
#[derive(Clone, Copy, PartialEq, Eq)]
#[derive(Debug)] //@omit
pub enum GestureKind {
    Swipe,
    Pinch,
    Hold,
}

/// What a gesture event does: the next state, whether the two-finger scroll stops here (`tp_gesture_stop_twofinger_scroll`,
/// libinput's zero axis event and GTK's deceleration), whether the scroll buildup is set (`tp_gesture_set_scroll_buildup`, from
/// UNKNOWN as `buildup`, from HOLD as `hold_buildup`), whether the pinch is initialised (`tp_gesture_init_pinch`), whether the
/// hold timer is cancelled, which gesture begins (`gesture_notify_*_begin`) and which one ends, and whether it was cancelled.
#[derive(Clone, Copy, PartialEq, Eq)]
#[derive(Debug)] //@omit
pub struct GestureStep {
    pub next: GestureState,
    pub stop_scroll: bool,
    pub buildup: bool,
    pub hold_buildup: bool,
    pub init_pinch: bool,
    pub cancel_timer: bool,
    pub begin: Option<GestureKind>,
    pub end: Option<(GestureKind, bool)>,
}

/// libinput's `cancelled = event == GESTURE_EVENT_CANCEL`.
#[allow(clippy::match_like_matches_macro)]
fn is_cancel(event: GestureEvent) -> bool //@ret
//@ ensures ret == (event == GestureEvent::Cancel),
{
    match event {
        GestureEvent::Cancel => true,
        _ => false,
    }
}

/// `tp_gesture_handle_event` for the states this port has. An event a state does not take (libinput's `log_gesture_bug`)
/// changes nothing. A cancel that libinput makes inside a handler (`tp_gesture_cancel` on a finger-switch timeout, on a
/// pointer motion, scroll, swipe or pinch start in HOLD, and on pinch start in SCROLL) is folded in: the scroll stops there
/// and the hold, swipe or pinch ends there, cancelled. The step returns the notifications so that the proof covers them.
pub fn gesture_step(state: GestureState, event: GestureEvent) -> GestureStep //@ret
//@ ensures ret.stop_scroll ==> state == GestureState::Scroll,
//@     state == GestureState::Scroll && ret.next != GestureState::Scroll ==> ret.stop_scroll,
//@     ret.next == GestureState::Scroll ==> state == GestureState::Scroll,
//@     ret.next == GestureState::ScrollStart ==> state == GestureState::ScrollStart || event == GestureEvent::ScrollStart,
//@     ret.buildup ==> state == GestureState::Unknown && ret.next == GestureState::ScrollStart,
//@     ret.hold_buildup ==> state == GestureState::Hold && ret.next == GestureState::ScrollStart,
//@     (event == GestureEvent::End || event == GestureEvent::Cancel) && state != GestureState::PinchStart ==> ret.next == GestureState::None,
//@     event == GestureEvent::PinchStart
//@         && (state == GestureState::Unknown || state == GestureState::ScrollStart || state == GestureState::Scroll)
//@         ==> ret.next == GestureState::PinchStart,
//@     ret.init_pinch ==> ret.next == GestureState::PinchStart && state != GestureState::PinchStart,
//@     ret.next == GestureState::PinchStart && state != GestureState::PinchStart ==> ret.init_pinch,
//@     ret.begin.is_some() ==> (ret.next == GestureState::Hold || ret.next == GestureState::HoldAndMotion
//@         || ret.next == GestureState::Swipe || ret.next == GestureState::Pinch),
//@     ret.begin == Some(GestureKind::Hold) ==> event == GestureEvent::HoldTimeout
//@         && (state == GestureState::Unknown || state == GestureState::PointerMotion),
//@     ret.begin == Some(GestureKind::Swipe) ==> state == GestureState::SwipeStart && event == GestureEvent::SwipeBegin
//@         && ret.next == GestureState::Swipe,
//@     ret.begin == Some(GestureKind::Pinch) ==> state == GestureState::PinchStart && event == GestureEvent::PinchBegin
//@         && ret.next == GestureState::Pinch,
//@     ret.next == GestureState::Hold && state != GestureState::Hold ==> ret.begin == Some(GestureKind::Hold),
//@     ret.next == GestureState::HoldAndMotion && state != GestureState::HoldAndMotion
//@         ==> (ret.begin == Some(GestureKind::Hold)) == (state == GestureState::PointerMotion),
//@     ret.next == GestureState::Swipe && state != GestureState::Swipe ==> ret.begin == Some(GestureKind::Swipe),
//@     ret.next == GestureState::Pinch && state != GestureState::Pinch ==> ret.begin == Some(GestureKind::Pinch),
//@     (state == GestureState::Hold || state == GestureState::HoldAndMotion)
//@         && ret.next != GestureState::Hold && ret.next != GestureState::HoldAndMotion
//@         ==> ret.end.is_some() && ret.end.unwrap().0 == GestureKind::Hold,
//@     state == GestureState::Swipe && ret.next != GestureState::Swipe
//@         ==> ret.end.is_some() && ret.end.unwrap().0 == GestureKind::Swipe,
//@     state == GestureState::Pinch && ret.next != GestureState::Pinch
//@         ==> ret.end.is_some() && ret.end.unwrap().0 == GestureKind::Pinch,
//@     ret.end.is_some() ==> ret.end.unwrap().1 == (event != GestureEvent::End),
//@     ret.end.is_some() && ret.end.unwrap().0 == GestureKind::Hold
//@         ==> state == GestureState::Hold || state == GestureState::HoldAndMotion,
//@     ret.end.is_some() && ret.end.unwrap().0 == GestureKind::Swipe ==> state == GestureState::Swipe,
//@     ret.end.is_some() && ret.end.unwrap().0 == GestureKind::Pinch ==> state == GestureState::Pinch,
//@     ret.end.is_some() ==> ret.next != GestureState::Hold && ret.next != GestureState::HoldAndMotion
//@         && ret.next != GestureState::Swipe && ret.next != GestureState::Pinch,
//@     state == GestureState::Unknown && event == GestureEvent::ScrollStart ==> ret.buildup,
//@     state == GestureState::Hold && event == GestureEvent::ScrollStart ==> ret.hold_buildup,
//@     event == GestureEvent::PinchStart && state == GestureState::Hold ==> ret.next == GestureState::PinchStart,
//@     ret.cancel_timer == (event == GestureEvent::End || event == GestureEvent::Cancel
//@         || (event == GestureEvent::Reset && state != GestureState::Hold && state != GestureState::HoldAndMotion
//@             && state != GestureState::Swipe && state != GestureState::Pinch)
//@         || (event == GestureEvent::FingerSwitchTimeout && (state == GestureState::Hold
//@             || state == GestureState::HoldAndMotion || state == GestureState::Scroll
//@             || state == GestureState::Swipe || state == GestureState::Pinch))
//@         || (event == GestureEvent::PointerMotionStart
//@             && (state == GestureState::Hold || state == GestureState::HoldAndMotion))
//@         || ((event == GestureEvent::ScrollStart || event == GestureEvent::SwipeStart)
//@             && (state == GestureState::Unknown || state == GestureState::Hold))
//@         || (event == GestureEvent::PinchStart && (state == GestureState::Unknown || state == GestureState::Hold
//@             || state == GestureState::ScrollStart || state == GestureState::Scroll))),
//@     event != GestureEvent::End && event != GestureEvent::Cancel && !(match state {
//@         GestureState::None => event == GestureEvent::Reset || event == GestureEvent::FingerDetected
//@             || event == GestureEvent::PointerMotionStart || event == GestureEvent::ScrollStart,
//@         GestureState::Unknown => event == GestureEvent::Reset || event == GestureEvent::HoldTimeout
//@             || event == GestureEvent::PointerMotionStart || event == GestureEvent::ScrollStart
//@             || event == GestureEvent::SwipeStart || event == GestureEvent::PinchStart,
//@         GestureState::Hold => event == GestureEvent::FingerSwitchTimeout || event == GestureEvent::HoldAndMotionStart
//@             || event == GestureEvent::PointerMotionStart || event == GestureEvent::ScrollStart
//@             || event == GestureEvent::SwipeStart || event == GestureEvent::PinchStart,
//@         GestureState::HoldAndMotion => event == GestureEvent::FingerSwitchTimeout
//@             || event == GestureEvent::PointerMotionStart,
//@         GestureState::PointerMotion => event == GestureEvent::Reset || event == GestureEvent::HoldTimeout,
//@         GestureState::SwipeStart => event == GestureEvent::Reset || event == GestureEvent::SwipeBegin,
//@         GestureState::ScrollStart => event == GestureEvent::Reset || event == GestureEvent::PinchStart,
//@         GestureState::Scroll => event == GestureEvent::Reset || event == GestureEvent::FingerSwitchTimeout
//@             || event == GestureEvent::PinchStart,
//@         GestureState::PinchStart => event == GestureEvent::Reset || event == GestureEvent::PinchBegin,
//@         GestureState::Pinch => event == GestureEvent::FingerSwitchTimeout,
//@         GestureState::Swipe => event == GestureEvent::FingerSwitchTimeout,
//@     }) ==> ret == (GestureStep { next: state, stop_scroll: false, buildup: false, hold_buildup: false,
//@         init_pinch: false, cancel_timer: false, begin: None, end: None }),
{
    let mut r = GestureStep {
        next: state,
        stop_scroll: false,
        buildup: false,
        hold_buildup: false,
        init_pinch: false,
        cancel_timer: false,
        begin: None,
        end: None,
    };
    match state {
        GestureState::None => match event {
            GestureEvent::Reset | GestureEvent::End | GestureEvent::Cancel => r.cancel_timer = true,
            GestureEvent::FingerDetected => r.next = GestureState::Unknown,
            GestureEvent::PointerMotionStart => r.next = GestureState::PointerMotion,
            GestureEvent::ScrollStart => r.next = GestureState::ScrollStart,
            _ => {}
        },
        GestureState::Unknown => match event {
            GestureEvent::Reset | GestureEvent::End | GestureEvent::Cancel => {
                r.cancel_timer = true;
                r.next = GestureState::None;
            }
            GestureEvent::HoldTimeout => {
                r.next = GestureState::Hold;
                r.begin = Some(GestureKind::Hold);
            }
            // The hold timer keeps running: this pointer motion can end up recognised as hold and motion.
            GestureEvent::PointerMotionStart => r.next = GestureState::PointerMotion,
            GestureEvent::ScrollStart => {
                r.cancel_timer = true;
                r.buildup = true;
                r.next = GestureState::ScrollStart;
            }
            GestureEvent::SwipeStart => {
                r.cancel_timer = true;
                r.next = GestureState::SwipeStart;
            }
            GestureEvent::PinchStart => {
                r.cancel_timer = true;
                r.init_pinch = true;
                r.next = GestureState::PinchStart;
            }
            _ => {}
        },
        GestureState::Hold => match event {
            GestureEvent::End | GestureEvent::Cancel => {
                r.end = Some((GestureKind::Hold, is_cancel(event)));
                r.cancel_timer = true;
                r.next = GestureState::None;
            }
            GestureEvent::FingerSwitchTimeout => {
                r.end = Some((GestureKind::Hold, true));
                r.cancel_timer = true;
                r.next = GestureState::None;
            }
            GestureEvent::HoldAndMotionStart => r.next = GestureState::HoldAndMotion,
            GestureEvent::PointerMotionStart => {
                r.end = Some((GestureKind::Hold, true));
                r.cancel_timer = true;
                r.next = GestureState::PointerMotion;
            }
            GestureEvent::ScrollStart => {
                r.hold_buildup = true;
                r.end = Some((GestureKind::Hold, true));
                r.cancel_timer = true;
                r.next = GestureState::ScrollStart;
            }
            GestureEvent::SwipeStart => {
                r.end = Some((GestureKind::Hold, true));
                r.cancel_timer = true;
                r.next = GestureState::SwipeStart;
            }
            GestureEvent::PinchStart => {
                r.end = Some((GestureKind::Hold, true));
                r.cancel_timer = true;
                r.init_pinch = true;
                r.next = GestureState::PinchStart;
            }
            _ => {}
        },
        GestureState::HoldAndMotion => match event {
            GestureEvent::End | GestureEvent::Cancel => {
                r.end = Some((GestureKind::Hold, is_cancel(event)));
                r.cancel_timer = true;
                r.next = GestureState::None;
            }
            GestureEvent::FingerSwitchTimeout => {
                r.end = Some((GestureKind::Hold, true));
                r.cancel_timer = true;
                r.next = GestureState::None;
            }
            GestureEvent::PointerMotionStart => {
                r.end = Some((GestureKind::Hold, true));
                r.cancel_timer = true;
                r.next = GestureState::PointerMotion;
            }
            _ => {}
        },
        GestureState::PointerMotion => match event {
            GestureEvent::Reset | GestureEvent::End | GestureEvent::Cancel => {
                r.cancel_timer = true;
                r.next = GestureState::None;
            }
            // The caller sends this only for one finger that moved less than HOLD_AND_MOTION_THRESHOLD.
            GestureEvent::HoldTimeout => {
                r.next = GestureState::HoldAndMotion;
                r.begin = Some(GestureKind::Hold);
            }
            _ => {}
        },
        GestureState::SwipeStart => match event {
            GestureEvent::Reset | GestureEvent::End | GestureEvent::Cancel => {
                r.cancel_timer = true;
                r.next = GestureState::None;
            }
            GestureEvent::SwipeBegin => {
                r.next = GestureState::Swipe;
                r.begin = Some(GestureKind::Swipe);
            }
            _ => {}
        },
        GestureState::ScrollStart => match event {
            GestureEvent::Reset | GestureEvent::End | GestureEvent::Cancel => {
                r.cancel_timer = true;
                r.next = GestureState::None;
            }
            GestureEvent::PinchStart => {
                r.cancel_timer = true;
                r.init_pinch = true;
                r.next = GestureState::PinchStart;
            }
            _ => {}
        },
        GestureState::Scroll => match event {
            GestureEvent::Reset
            | GestureEvent::End
            | GestureEvent::Cancel
            | GestureEvent::FingerSwitchTimeout => {
                r.stop_scroll = true;
                r.cancel_timer = true;
                r.next = GestureState::None;
            }
            GestureEvent::PinchStart => {
                r.stop_scroll = true;
                r.cancel_timer = true;
                r.init_pinch = true;
                r.next = GestureState::PinchStart;
            }
            _ => {}
        },
        // libinput's PINCH_START ignores the end events (its state moves on at the next motion).
        GestureState::PinchStart => match event {
            GestureEvent::Reset | GestureEvent::End | GestureEvent::Cancel => r.cancel_timer = true,
            GestureEvent::PinchBegin => {
                r.next = GestureState::Pinch;
                r.begin = Some(GestureKind::Pinch);
            }
            _ => {}
        },
        GestureState::Pinch => match event {
            GestureEvent::End | GestureEvent::Cancel => {
                r.end = Some((GestureKind::Pinch, is_cancel(event)));
                r.cancel_timer = true;
                r.next = GestureState::None;
            }
            GestureEvent::FingerSwitchTimeout => {
                r.end = Some((GestureKind::Pinch, true));
                r.cancel_timer = true;
                r.next = GestureState::None;
            }
            _ => {}
        },
        GestureState::Swipe => match event {
            GestureEvent::End | GestureEvent::Cancel => {
                r.end = Some((GestureKind::Swipe, is_cancel(event)));
                r.cancel_timer = true;
                r.next = GestureState::None;
            }
            GestureEvent::FingerSwitchTimeout => {
                r.end = Some((GestureKind::Swipe, true));
                r.cancel_timer = true;
                r.next = GestureState::None;
            }
            _ => {}
        },
    }
    r
}

/// What `tp_gesture_update_finger_state` decided.
#[derive(Clone, Copy, PartialEq, Eq)]
#[derive(Debug)] //@omit
pub struct FingerUpdate {
    pub count: u32,
    pub pending: u32,
    /// All fingers lifted: the gesture ends.
    pub end: bool,
    /// The count changed at once in UNKNOWN or POINTER_MOTION: the gesture resets.
    pub reset: bool,
    /// The finger-switch timer starts.
    pub arm: bool,
}

/// `tp_gesture_update_finger_state`: `active` touches now, `count` and `pending` before; `debounce` is
/// `tp_gesture_debounce_finger_changes` (a scroll, pinch or swipe going on); `resettable` is the state being UNKNOWN or
/// POINTER_MOTION.
pub fn finger_update(
    active: u32,
    count: u32,
    pending: u32,
    debounce: bool,
    resettable: bool,
) -> FingerUpdate //@ret
//@ ensures active == 0 ==> ret.count == 0 && ret.pending == 0 && (count != 0 ==> ret.end),
//@     active == count ==> ret.count == count && ret.pending == 0 && !ret.end && !ret.arm && !ret.reset,
//@     active != 0 && active != count && !debounce ==> ret.count == active && ret.pending == 0,
//@     ret.pending != 0 ==> debounce && ret.pending == active && ret.count == count,
//@     ret.arm ==> ret.pending != 0 && ret.pending != pending,
//@     ret.reset ==> resettable,
{
    let none = FingerUpdate {
        count,
        pending: 0,
        end: false,
        reset: false,
        arm: false,
    };
    if active == count {
        none
    } else if active == 0 {
        FingerUpdate {
            count: 0,
            pending: 0,
            end: true,
            reset: false,
            arm: false,
        }
    } else if !debounce {
        FingerUpdate {
            count: active,
            pending: 0,
            end: false,
            reset: resettable,
            arm: false,
        }
    } else if active != pending {
        FingerUpdate {
            count,
            pending: active,
            end: false,
            reset: false,
            arm: true,
        }
    } else {
        FingerUpdate {
            count,
            pending,
            end: false,
            reset: false,
            arm: false,
        }
    }
}

/// Largest hi-res value one event may carry (far above what a pad sends in 10 ms).
pub const MAX_HIRES: i32 = 1_048_576;

/// `hidinput_handle_scroll`: `value` hi-res units on top of `acc`; returns the notches to send and what is left. C's
/// division truncates toward zero, so what is left keeps the sign of the sum and stays under one notch.
pub fn wheel_notches(acc: i32, value: i32) -> (i32, i32) //@ret
//@ requires -120 < acc < 120, -MAX_HIRES <= value <= MAX_HIRES,
//@ ensures ret.0 * 120 + ret.1 == acc + value, -120 < ret.1 < 120,
//@     (acc + value >= 0 ==> ret.1 >= 0) && (acc + value <= 0 ==> ret.1 <= 0),
{
    let sum = acc + value;
    let lo = sum / 120;
    (lo, sum - lo * 120)
}
