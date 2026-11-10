// SPDX-License-Identifier: MPL-2.0

//! Samsung's EC events on the Galaxy Book Go (`ECTC`, SAM060B, I2C8 address 0x62, interrupt GPIO 68 through PDC pin 112, edge
//! low), as `EC2.sys` reads them (decompiled, `docs/research/juno-fnkeys.md` 1.2): on the interrupt, one plain read of
//! [`EVENT_LEN`] bytes; byte 0 is the type (1: an event), byte 2 the event's code, byte 3 its value, which EC2.sys stores as the
//! new state (`SAMSUNG\OSD\FnLock`, `CapsLock`, `ScreenOff`, value `State`).
//!
//! And the function row's swap with the keyboard's own Fn Lock (kernel patch 1200): the keyboard sends the printed hot keys
//! for a plain press while its Fn Lock is on, so the kernel swaps exactly when the hot keys are wanted first and the keyboard's
//! Fn Lock is off, or F1-F12 are wanted first and it is on.
//!
//! Verus proves this very file (`verification/juno-ec/rules.rs`, D-011).
#![allow(unexpected_cfgs)]

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

/// The EC's I2C address on I2C8.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const EC_ADDR: u8 = 0x62;
/// What EC2.sys reads on each interrupt.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const EVENT_LEN: usize = 12;
/// Byte 0 of an event.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const TYPE_EVENT: u8 = 1;
/// The codes EC2.sys knows (`(address not published)`): 0x20 Fn Lock (registry index 1), 0x21 Caps Lock (2), 9 screen on/off (3).
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const CODE_FN_LOCK: u8 = 0x20;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const CODE_CAPS_LOCK: u8 = 0x21;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const CODE_SCREEN: u8 = 9;

/// An EC event.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EcEvent {
    /// Fn Lock: on or off, or a value that is neither (the state then flips, as EC2.sys flips its own copy).
    FnLock(Option<bool>),
    CapsLock(Option<bool>),
    /// The screen on/off key.
    Screen(Option<bool>),
    /// A code EC2.sys toggles without a name here yet (5, 0x0b, 0x22), or one it does not know.
    Other { code: u8, value: u8 },
}

#[cfg(verus_keep_ghost)]
verus! {

pub open spec fn state_spec(v: u8) -> Option<bool> {
    if v == 0 { Some(false) } else if v == 1 { Some(true) } else { None }
}

} // verus!

/// Byte 3 as a state: 0 off, 1 on, anything else unknown.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        v == 0 ==> r == Some(false),
        v == 1 ==> r == Some(true),
        v > 1 ==> r.is_none(),
))]
pub fn state(v: u8) -> Option<bool> {
    match v {
        0 => Some(false),
        1 => Some(true),
        _ => None,
    }
}

/// The event in a read, or None when byte 0 says it is not one.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        buf[0] != TYPE_EVENT ==> r.is_none(),
        buf[0] == TYPE_EVENT && buf[2] == CODE_FN_LOCK ==> r == Some(EcEvent::FnLock(state_spec(buf[3]))),
        buf[0] == TYPE_EVENT && buf[2] == CODE_CAPS_LOCK ==> r == Some(EcEvent::CapsLock(state_spec(buf[3]))),
        buf[0] == TYPE_EVENT && buf[2] == CODE_SCREEN ==> r == Some(EcEvent::Screen(state_spec(buf[3]))),
))]
pub fn parse(buf: &[u8; EVENT_LEN]) -> Option<EcEvent> {
    if buf[0] != TYPE_EVENT {
        return None;
    }
    let value = buf[3];
    Some(match buf[2] {
        CODE_FN_LOCK => EcEvent::FnLock(state(value)),
        CODE_CAPS_LOCK => EcEvent::CapsLock(state(value)),
        CODE_SCREEN => EcEvent::Screen(state(value)),
        code => EcEvent::Other { code, value },
    })
}

/// A state after an event: the one it says, or the previous one flipped when it says neither.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        said.is_some() ==> r == said.unwrap(),
        said.is_none() ==> r == !previous,
))]
pub fn next_state(previous: bool, said: Option<bool>) -> bool {
    match said {
        Some(on) => on,
        None => !previous,
    }
}

/// Whether the kernel swaps the function row (`fnrow::swap`): the hot keys are wanted first (no `lucina.hid.fnkeys=f`) and the
/// keyboard sends F1-F12 for a plain press (its Fn Lock off), or the other way round.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == (hot_keys_wanted != keyboard_fn_lock),
))]
pub fn swap_active(hot_keys_wanted: bool, keyboard_fn_lock: bool) -> bool {
    hot_keys_wanted != keyboard_fn_lock
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(code: u8, value: u8) -> [u8; EVENT_LEN] {
        let mut b = [0u8; EVENT_LEN];
        b[0] = 1;
        b[2] = code;
        b[3] = value;
        b
    }

    #[test]
    fn events_parse_as_ec2_reads_them() {
        assert_eq!(parse(&ev(0x20, 1)), Some(EcEvent::FnLock(Some(true))));
        assert_eq!(parse(&ev(0x20, 0)), Some(EcEvent::FnLock(Some(false))));
        assert_eq!(parse(&ev(0x21, 1)), Some(EcEvent::CapsLock(Some(true))));
        assert_eq!(parse(&ev(9, 7)), Some(EcEvent::Screen(None)));
        assert_eq!(parse(&ev(0x22, 3)), Some(EcEvent::Other { code: 0x22, value: 3 }));
        let mut not = ev(0x20, 1);
        not[0] = 0;
        assert_eq!(parse(&not), None);
    }

    #[test]
    fn the_row_follows_the_keyboards_fn_lock() {
        assert!(swap_active(true, false), "hot keys wanted, keyboard sends F-keys: swap");
        assert!(!swap_active(true, true), "the keyboard sends hot keys already");
        assert!(swap_active(false, true), "F-keys wanted, keyboard sends hot keys: swap");
        assert!(!swap_active(false, false));
        assert!(next_state(false, None));
        assert!(!next_state(true, Some(false)));
    }
}
