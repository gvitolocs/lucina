// SPDX-License-Identifier: MPL-2.0

//! The Galaxy Book Go's function row with its hot keys first (owner, 2026-11-08: "the F-row keys must do their media/system
//! action by default, without holding Fn").
//!
//! Samsung's manual for this laptop (Galaxy Book Go user manual, "Using hot keys" and "Hot key functions", pages 16-17): a
//! hot key is Fn plus the F-key, "When the Fn Lock key is on, hot keys are activated without pressing the Fn key". The
//! keyboard (0cf2:9028) holds that state itself; Samsung's EC driver only records it for the OSD (EC event 0x20 →
//! `SAMSUNG\OSD\FnLock`, EC2.sys) and nothing sets it at boot. Here the swap is done by the kernel instead, as Linux's
//! `hid-apple` `fnmode` does: a plain F-key gives its printed hot key, and a hot key (what the keyboard sends with Fn held)
//! gives the plain F-key. The keyboard's own Fn Lock key still flips both, so it then gives F1-F12 first.
//!
//! The printed functions, from the manual: F1 Windows settings, F2/F3 brightness, F4 display switch, F5 touchpad on/off,
//! F6 mute, F7/F8 volume, F9 display on/off, F10 CONNECT, F11 Consulting Mode, F12 Airplane mode. F10 and F11 have no Juno
//! function yet and stay F10 and F11.
//!
//! Verus proves this very file (`verification/juno-fnrow/rules.rs`, D-011): [`swap`] gives each key at most one other key,
//! applying it twice gives the key back, and a key outside the row's table is left alone.
#![allow(unexpected_cfgs)]

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

/// Linux key codes (`include/uapi/linux/input-event-codes.h`).
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const KEY_F1: u16 = 59;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const KEY_F2: u16 = 60;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const KEY_F3: u16 = 61;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const KEY_F4: u16 = 62;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const KEY_F5: u16 = 63;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const KEY_F6: u16 = 64;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const KEY_F7: u16 = 65;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const KEY_F8: u16 = 66;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const KEY_F9: u16 = 67;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const KEY_F12: u16 = 88;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const KEY_MUTE: u16 = 113;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const KEY_VOLUMEDOWN: u16 = 114;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const KEY_VOLUMEUP: u16 = 115;
/// `KEY_CONFIG`: Linux's code for the Consumer page's "AL Consumer Control Configuration", the settings key.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const KEY_CONFIG: u16 = 171;
/// `KEY_F21`: what Linux's keyboards send for a touchpad on/off key (systemd's hwdb, desktops' `XF86TouchpadToggle`);
/// `KEY_TOUCHPAD_TOGGLE` (530) is past this kernel's key codes.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const KEY_F21: u16 = 191;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const KEY_BRIGHTNESSDOWN: u16 = 224;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const KEY_BRIGHTNESSUP: u16 = 225;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const KEY_SWITCHVIDEOMODE: u16 = 227;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const KEY_DISPLAY_OFF: u16 = 245;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const KEY_RFKILL: u16 = 247;

#[cfg(verus_keep_ghost)]
verus! {

/// The row's pairs: (F-key, its hot key).
pub open spec fn pair(f: u16, h: u16) -> bool {
    (f == KEY_F1 && h == KEY_CONFIG)
    || (f == KEY_F2 && h == KEY_BRIGHTNESSDOWN)
    || (f == KEY_F3 && h == KEY_BRIGHTNESSUP)
    || (f == KEY_F4 && h == KEY_SWITCHVIDEOMODE)
    || (f == KEY_F5 && h == KEY_F21)
    || (f == KEY_F6 && h == KEY_MUTE)
    || (f == KEY_F7 && h == KEY_VOLUMEDOWN)
    || (f == KEY_F8 && h == KEY_VOLUMEUP)
    || (f == KEY_F9 && h == KEY_DISPLAY_OFF)
    || (f == KEY_F12 && h == KEY_RFKILL)
}

/// Whether `k` is in the table, as an F-key or as a hot key.
pub open spec fn in_row(k: u16) -> bool {
    exists|o: u16| pair(k, o) || pair(o, k)
}

} // verus!

/// The F-key's hot key, or None if the key has none (F10, F11, any other key).
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r.is_some() ==> pair(f, r.unwrap()),
        r.is_none() ==> forall|h: u16| !pair(f, h),
))]
pub fn hot_key(f: u16) -> Option<u16> {
    match f {
        KEY_F1 => Some(KEY_CONFIG),
        KEY_F2 => Some(KEY_BRIGHTNESSDOWN),
        KEY_F3 => Some(KEY_BRIGHTNESSUP),
        KEY_F4 => Some(KEY_SWITCHVIDEOMODE),
        KEY_F5 => Some(KEY_F21),
        KEY_F6 => Some(KEY_MUTE),
        KEY_F7 => Some(KEY_VOLUMEDOWN),
        KEY_F8 => Some(KEY_VOLUMEUP),
        KEY_F9 => Some(KEY_DISPLAY_OFF),
        KEY_F12 => Some(KEY_RFKILL),
        _ => None,
    }
}

/// The hot key's F-key, or None.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r.is_some() ==> pair(r.unwrap(), h),
        r.is_none() ==> forall|f: u16| !pair(f, h),
))]
pub fn f_key(h: u16) -> Option<u16> {
    match h {
        KEY_CONFIG => Some(KEY_F1),
        KEY_BRIGHTNESSDOWN => Some(KEY_F2),
        KEY_BRIGHTNESSUP => Some(KEY_F3),
        KEY_SWITCHVIDEOMODE => Some(KEY_F4),
        KEY_F21 => Some(KEY_F5),
        KEY_MUTE => Some(KEY_F6),
        KEY_VOLUMEDOWN => Some(KEY_F7),
        KEY_VOLUMEUP => Some(KEY_F8),
        KEY_DISPLAY_OFF => Some(KEY_F9),
        KEY_RFKILL => Some(KEY_F12),
        _ => None,
    }
}

/// The key a press of `code` gives with the hot keys first: an F-key of the table becomes its hot key, a hot key its
/// F-key, anything else stays.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        pair(code, r) || pair(r, code) || (r == code && !in_row(code)),
))]
pub fn swap(code: u16) -> u16 {
    if let Some(h) = hot_key(code) {
        h
    } else if let Some(f) = f_key(code) {
        f
    } else {
        #[cfg(verus_keep_ghost)]
        proof! {
            assert(forall|o: u16| !pair(code, o));
            assert(forall|o: u16| !pair(o, code));
        }
        code
    }
}

/// Every hot key [`swap`] can give, for the keyboard device to declare.
pub const HOT_KEYS: [u16; 10] = [
    KEY_CONFIG,
    KEY_BRIGHTNESSDOWN,
    KEY_BRIGHTNESSUP,
    KEY_SWITCHVIDEOMODE,
    KEY_F21,
    KEY_MUTE,
    KEY_VOLUMEDOWN,
    KEY_VOLUMEUP,
    KEY_DISPLAY_OFF,
    KEY_RFKILL,
];

/// The keyboard this row belongs to: the Galaxy Book Go's (`ECKB`, HID 0cf2:9028).
pub fn is_galaxy_book_go(vendor: u16, product: u16) -> bool {
    vendor == 0x0cf2 && product == 0x9028
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_row_swaps_both_ways() {
        assert_eq!(swap(KEY_F2), KEY_BRIGHTNESSDOWN);
        assert_eq!(swap(KEY_BRIGHTNESSDOWN), KEY_F2);
        assert_eq!(swap(KEY_F12), KEY_RFKILL);
        assert_eq!(swap(68), 68, "F10 stays");
        assert_eq!(swap(87), 87, "F11 stays");
        assert_eq!(swap(30), 30, "A stays");
        for k in 0..600u16 {
            assert_eq!(swap(swap(k)), k);
        }
        for h in HOT_KEYS {
            assert!(f_key(h).is_some());
        }
    }
}
