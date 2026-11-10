// The touchpad's rules that must hold whatever the pad sends: which slot a contact goes to, when a frame of a hybrid-mode
// pad is whole, how the gesture state moves (a scroll always ends with its stop, and only a scroll has one), how the finger
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
pub fn slot_by_key(active: &[bool; SLOTS], keys: &[i32; SLOTS], used: &[bool; SLOTS], n: usize, key: i32) -> Option<usize> //@ret
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
pub fn expected_contacts(num_received: u32, scan_changed: bool, count: u32, num_expected: u32) -> u32 //@ret
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

/// `enum tp_gesture_state`, the states the port has.
#[derive(Clone, Copy, PartialEq, Eq)]
#[derive(Debug)] //@omit
pub enum GestureState {
    None,
    Unknown,
    PointerMotion,
    ScrollStart,
    Scroll,
    PinchStart,
    Pinch,
    SwipeStart,
    Swipe,
}

/// `enum gesture_event`, the events the port sends.
#[derive(Clone, Copy, PartialEq, Eq)]
#[derive(Debug)] //@omit
pub enum GestureEvent {
    Reset,
    End,
    Cancel,
    FingerDetected,
    FingerSwitchTimeout,
    PointerMotionStart,
    ScrollStart,
    PinchStart,
    SwipeStart,
}

/// What a gesture event does: the next state, whether the two-finger scroll stops here (`tp_gesture_stop_twofinger_scroll`,
/// libinput's zero axis event and GTK's deceleration), and whether the scroll buildup is set (`tp_gesture_set_scroll_buildup`).
#[derive(Clone, Copy, PartialEq, Eq)]
#[derive(Debug)] //@omit
pub struct GestureStep {
    pub next: GestureState,
    pub stop_scroll: bool,
    pub buildup: bool,
}

/// `tp_gesture_handle_event` for the states this port has. An event a state does not take (libinput's `log_gesture_bug`)
/// changes nothing. A cancel that libinput makes inside a handler (`tp_gesture_cancel` on SCROLL's finger-switch timeout
/// and pinch start) is folded in: the scroll stops there too.
pub fn gesture_step(state: GestureState, event: GestureEvent) -> GestureStep //@ret
//@ ensures ret.stop_scroll ==> state == GestureState::Scroll,
//@     state == GestureState::Scroll && ret.next != GestureState::Scroll ==> ret.stop_scroll,
//@     ret.next == GestureState::Scroll ==> state == GestureState::Scroll,
//@     ret.next == GestureState::ScrollStart ==> state == GestureState::ScrollStart || event == GestureEvent::ScrollStart,
//@     ret.buildup ==> state == GestureState::Unknown && ret.next == GestureState::ScrollStart,
//@     (event == GestureEvent::End || event == GestureEvent::Cancel) && state != GestureState::PinchStart ==> ret.next == GestureState::None,
//@     event == GestureEvent::PinchStart
//@         && (state == GestureState::Unknown || state == GestureState::ScrollStart || state == GestureState::Scroll)
//@         ==> ret.next == GestureState::PinchStart,
{
    let stay = GestureStep { next: state, stop_scroll: false, buildup: false };
    match state {
        GestureState::None => match event {
            GestureEvent::FingerDetected => GestureStep { next: GestureState::Unknown, stop_scroll: false, buildup: false },
            GestureEvent::PointerMotionStart => GestureStep { next: GestureState::PointerMotion, stop_scroll: false, buildup: false },
            GestureEvent::ScrollStart => GestureStep { next: GestureState::ScrollStart, stop_scroll: false, buildup: false },
            _ => stay,
        },
        GestureState::Unknown => match event {
            GestureEvent::Reset | GestureEvent::End | GestureEvent::Cancel => GestureStep { next: GestureState::None, stop_scroll: false, buildup: false },
            GestureEvent::PointerMotionStart => GestureStep { next: GestureState::PointerMotion, stop_scroll: false, buildup: false },
            GestureEvent::ScrollStart => GestureStep { next: GestureState::ScrollStart, stop_scroll: false, buildup: true },
            GestureEvent::SwipeStart => GestureStep { next: GestureState::SwipeStart, stop_scroll: false, buildup: false },
            GestureEvent::PinchStart => GestureStep { next: GestureState::PinchStart, stop_scroll: false, buildup: false },
            _ => stay,
        },
        GestureState::PointerMotion | GestureState::SwipeStart => match event {
            GestureEvent::Reset | GestureEvent::End | GestureEvent::Cancel => GestureStep { next: GestureState::None, stop_scroll: false, buildup: false },
            _ => stay,
        },
        GestureState::ScrollStart => match event {
            GestureEvent::Reset | GestureEvent::End | GestureEvent::Cancel => GestureStep { next: GestureState::None, stop_scroll: false, buildup: false },
            GestureEvent::PinchStart => GestureStep { next: GestureState::PinchStart, stop_scroll: false, buildup: false },
            _ => stay,
        },
        GestureState::Scroll => match event {
            GestureEvent::Reset | GestureEvent::End | GestureEvent::Cancel | GestureEvent::FingerSwitchTimeout => {
                GestureStep { next: GestureState::None, stop_scroll: true, buildup: false }
            }
            GestureEvent::PinchStart => GestureStep { next: GestureState::PinchStart, stop_scroll: true, buildup: false },
            _ => stay,
        },
        // libinput's PINCH_START ignores the end events (its state moves on at the next motion).
        GestureState::PinchStart => stay,
        GestureState::Pinch | GestureState::Swipe => match event {
            GestureEvent::End | GestureEvent::Cancel | GestureEvent::FingerSwitchTimeout => GestureStep { next: GestureState::None, stop_scroll: false, buildup: false },
            _ => stay,
        },
    }
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
pub fn finger_update(active: u32, count: u32, pending: u32, debounce: bool, resettable: bool) -> FingerUpdate //@ret
//@ ensures active == 0 ==> ret.count == 0 && ret.pending == 0 && (count != 0 ==> ret.end),
//@     active == count ==> ret.count == count && ret.pending == 0 && !ret.end && !ret.arm && !ret.reset,
//@     active != 0 && active != count && !debounce ==> ret.count == active && ret.pending == 0,
//@     ret.pending != 0 ==> debounce && ret.pending == active && ret.count == count,
//@     ret.arm ==> ret.pending != 0 && ret.pending != pending,
//@     ret.reset ==> resettable,
{
    let none = FingerUpdate { count, pending: 0, end: false, reset: false, arm: false };
    if active == count {
        none
    } else if active == 0 {
        FingerUpdate { count: 0, pending: 0, end: true, reset: false, arm: false }
    } else if !debounce {
        FingerUpdate { count: active, pending: 0, end: false, reset: resettable, arm: false }
    } else if active != pending {
        FingerUpdate { count, pending: active, end: false, reset: false, arm: true }
    } else {
        FingerUpdate { count, pending, end: false, reset: false, arm: false }
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
