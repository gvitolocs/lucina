// SPDX-License-Identifier: MPL-2.0

//! The laptop's function keys: which key does what, and the levels they move (owner, 2026-11-08: "I press the F buttons for
//! volume and screen brightness and nothing happens").
//!
//! The Galaxy Book Go's keyboard (0cf2:9028) sends its function row through its own HID report descriptor: report 3 is a
//! Consumer Control usage (Brightness 0x6F/0x70, Volume 0xE9/0xEA, Mute 0xE2, the media keys), which the kernel turns into
//! Linux key codes (`hid-input.c`), and report 6 is the Wireless Radio Button (`KEY_RFKILL`). Samsung's EC events (EC2.sys)
//! are toggles, none of them a level (`docs/research/juno-fnkeys.md`).
//!
//! This file is pure: key code to action, and each level's next value. Verus proves this very file
//! (`verification/juno-fnkeys/rules.rs`, D-011): every level stays in its range, a step moves it by at most one step and
//! towards the key's direction, and every key has exactly one action.
#![allow(unexpected_cfgs)]

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

/// Linux key codes (`include/uapi/linux/input-event-codes.h`).
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const KEY_MUTE: u16 = 113;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const KEY_VOLUMEDOWN: u16 = 114;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const KEY_VOLUMEUP: u16 = 115;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const KEY_BRIGHTNESSDOWN: u16 = 224;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const KEY_BRIGHTNESSUP: u16 = 225;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const KEY_RFKILL: u16 = 247;
/// The function row's other hot keys (kernel patch 1169, lucina-i2c-hid `fnrow`): F1 settings, F5 touchpad, F4 display
/// switch, F9 display off.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const KEY_CONFIG: u16 = 171;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const KEY_F21: u16 = 191;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const KEY_SWITCHVIDEOMODE: u16 = 227;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const KEY_DISPLAY_OFF: u16 = 245;

/// The system volume's levels: 0 is silence, [`VOLUME_MAX`] is the program's own level unchanged (the kernel's ceiling still
/// applies on top). Each level is [`VOLUME_STEP_DB`] apart.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const VOLUME_MAX: u8 = 20;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const VOLUME_STEP_DB: i32 = 2;
/// The backlight's levels, as `/sys/class/backlight` gives them (`max_brightness` 100), and one key press.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const BRIGHTNESS_MAX: u32 = 100;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const BRIGHTNESS_STEP: u32 = 10;

/// What a key does.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Brightness(bool),
    Volume(bool),
    Mute,
    Radio,
    Settings,
    Touchpad,
    DisplaySwitch,
    DisplayOff,
    /// Not a function key: the views handle it.
    Other,
}

#[cfg(verus_keep_ghost)]
verus! {

/// The action each key code has.
pub open spec fn action_spec(code: u16) -> Action {
    if code == KEY_BRIGHTNESSUP { Action::Brightness(true) }
    else if code == KEY_BRIGHTNESSDOWN { Action::Brightness(false) }
    else if code == KEY_VOLUMEUP { Action::Volume(true) }
    else if code == KEY_VOLUMEDOWN { Action::Volume(false) }
    else if code == KEY_MUTE { Action::Mute }
    else if code == KEY_RFKILL { Action::Radio }
    else if code == KEY_CONFIG { Action::Settings }
    else if code == KEY_F21 { Action::Touchpad }
    else if code == KEY_SWITCHVIDEOMODE { Action::DisplaySwitch }
    else if code == KEY_DISPLAY_OFF { Action::DisplayOff }
    else { Action::Other }
}

} // verus!

/// Whether holding the key repeats its action (Windows repeats the volume and brightness keys at the keyboard rate; a
/// toggle repeated would flip back and forth).
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == (a is Brightness || a is Volume),
))]
pub fn repeats(a: Action) -> bool {
    matches!(a, Action::Brightness(_) | Action::Volume(_))
}

/// The action of key `code`.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == action_spec(code),
))]
pub fn action(code: u16) -> Action {
    match code {
        KEY_BRIGHTNESSUP => Action::Brightness(true),
        KEY_BRIGHTNESSDOWN => Action::Brightness(false),
        KEY_VOLUMEUP => Action::Volume(true),
        KEY_VOLUMEDOWN => Action::Volume(false),
        KEY_MUTE => Action::Mute,
        KEY_RFKILL => Action::Radio,
        KEY_CONFIG => Action::Settings,
        KEY_F21 => Action::Touchpad,
        KEY_SWITCHVIDEOMODE => Action::DisplaySwitch,
        KEY_DISPLAY_OFF => Action::DisplayOff,
        _ => Action::Other,
    }
}

/// The volume after one press: one level up or down, never past 0 or [`VOLUME_MAX`]. A level read from elsewhere that is
/// above the top comes back to the top.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r <= VOLUME_MAX,
        up ==> r >= (if level > VOLUME_MAX { VOLUME_MAX } else { level }),
        !up ==> r <= level,
        level <= VOLUME_MAX ==> (up ==> r <= level + 1) && (!up ==> r + 1 >= level),
        level < VOLUME_MAX && up ==> r == level + 1,
        0 < level && level <= VOLUME_MAX && !up ==> r == level - 1,
))]
pub fn volume_step(level: u8, up: bool) -> u8 {
    let level = if level > VOLUME_MAX {
        VOLUME_MAX
    } else {
        level
    };
    if up {
        if level < VOLUME_MAX {
            level + 1
        } else {
            VOLUME_MAX
        }
    } else if level > 0 {
        level - 1
    } else {
        0
    }
}

/// The decibels a volume level takes off the program's own level: 0 at the top, [`VOLUME_STEP_DB`] more for each level
/// below it. Level 0 is silence, which the caller sends as mute.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires
        level <= VOLUME_MAX,
    ensures
        r <= 0,
        r >= -(VOLUME_MAX as i32) * VOLUME_STEP_DB,
        r == -((VOLUME_MAX - level) as i32) * VOLUME_STEP_DB,
))]
pub fn volume_db(level: u8) -> i32 {
    -((VOLUME_MAX - level) as i32) * VOLUME_STEP_DB
}

/// The backlight after one press: [`BRIGHTNESS_STEP`] up or down, within 0..=[`BRIGHTNESS_MAX`]. Down never reaches 0 from
/// above 0: a press can dim to the lowest light, not switch the screen off (the idle timer does that).
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r <= BRIGHTNESS_MAX,
        up ==> r >= (if level > BRIGHTNESS_MAX { BRIGHTNESS_MAX } else { level }),
        !up ==> r <= level,
        level > 0 && !up ==> r >= 1,
        level <= BRIGHTNESS_MAX && up ==> r <= level + BRIGHTNESS_STEP,
        level <= BRIGHTNESS_MAX && !up ==> r + BRIGHTNESS_STEP >= level,
))]
pub fn brightness_step(level: u32, up: bool) -> u32 {
    let level = if level > BRIGHTNESS_MAX {
        BRIGHTNESS_MAX
    } else {
        level
    };
    if up {
        if level > BRIGHTNESS_MAX - BRIGHTNESS_STEP {
            BRIGHTNESS_MAX
        } else {
            level + BRIGHTNESS_STEP
        }
    } else if level > BRIGHTNESS_STEP {
        level - BRIGHTNESS_STEP
    } else if level > 0 {
        1
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_have_their_actions() {
        assert_eq!(action(225), Action::Brightness(true));
        assert_eq!(action(224), Action::Brightness(false));
        assert_eq!(action(115), Action::Volume(true));
        assert_eq!(action(114), Action::Volume(false));
        assert_eq!(action(113), Action::Mute);
        assert_eq!(action(247), Action::Radio);
        assert_eq!(action(171), Action::Settings);
        assert_eq!(action(191), Action::Touchpad);
        assert_eq!(action(227), Action::DisplaySwitch);
        assert_eq!(action(245), Action::DisplayOff);
        assert_eq!(action(59), Action::Other);
        assert!(repeats(action(115)) && repeats(action(224)));
        assert!(
            !repeats(action(113))
                && !repeats(action(191))
                && !repeats(action(245))
                && !repeats(action(59))
        );
    }

    #[test]
    fn levels_stay_in_range() {
        assert_eq!(volume_step(VOLUME_MAX, true), VOLUME_MAX);
        assert_eq!(volume_step(0, false), 0);
        assert_eq!(volume_step(200, false), VOLUME_MAX - 1);
        assert_eq!(volume_db(VOLUME_MAX), 0);
        assert_eq!(volume_db(0), -40);
        assert_eq!(brightness_step(95, true), 100);
        assert_eq!(brightness_step(5, false), 1);
        assert_eq!(brightness_step(1, false), 1);
        assert_eq!(brightness_step(0, false), 0);
        assert_eq!(brightness_step(50, false), 40);
    }
}
