// SPDX-License-Identifier: MPL-2.0

//! Juno's idle standby (`docs/research/juno-power.md`, "Idle standby"): when the screen goes off by itself, when the machine
//! sleeps by itself, when a sleeping machine goes on to the deeper standby, and what holds each off. The shell keeps one
//! [`Standby`], tells it what happened ([`input`], [`take`], [`give`], [`sleep`], [`wake`]), asks [`decide`] when [`deadline`]
//! comes, and carries out the decision; it keeps no timer of its own.
//!
//! The times are the owner's settings ([`Times`], 2026-11-08: "sleep after 15 minutes by default and standby after 1 hour and
//! those are customizable"), kept in bounds by [`settle`]:
//! * the screen off after `off` minutes (Windows' "Turn off the display after": 5 on battery, 10 on the charger by default);
//! * the sleep after `sleep` minutes (default [`SLEEP_DEFAULT_MIN`]): the screen off and stage-1 sleep, the lid's path;
//! * the standby after `standby` minutes (default [`STANDBY_DEFAULT_MIN`]): from the sleep on to the deepest state the kernel
//!   can safely hold.
//!
//! All three count from idle: the later of the last input and the end of the last hold. So the standby comes `standby` minutes
//! after the last activity, also after a sleep by the lid or the power key, and a setting of 60 after 15 means 45 minutes asleep
//! first. 0 is Never.
//!
//! The holds are the originals' (Windows `PowerSetRequest`, ChromeOS's inactivity rules in `power_manager/docs/inactivity_delays.md`):
//! a [`Kind::Display`] hold (a playing video: Windows' display request, ChromeOS's video activity) keeps the screen on and the
//! machine awake; a [`Kind::System`] hold (audio playback: ChromeOS's "the screen will still ... be turned off ... but the system
//! will not suspend"; an agent's inhibitor: Windows' system request, logind's `Inhibit`) lets the screen go off but keeps the
//! machine from sleeping, and a sleeping one from going on to standby. The end of a hold counts like input ("once activity
//! ceases, the inactivity timer starts again"). The power key and the lid sleep at once whatever is held: Windows ends power
//! requests on "user-initiated system sleep entry (power button, lid close ...)".
//!
//! Verus proves this very file (`verification/juno-standby/rules.rs`, D-011). Proved there, for every state, time and setting:
//! 1. the machine never sleeps by itself while a hold is live, nor before `sleep` minutes of idle; it goes to standby only from a
//!    sleep, with nothing held, after `standby` minutes of idle;
//! 2. the screen never goes off by itself while a display hold is live, nor before `off` minutes after the last input and the end
//!    of the last display hold; a screen-off is only chosen when the sleep is not due;
//! 3. Never is never: `sleep` 0 never sleeps, `standby` 0 never goes to standby;
//! 4. the timers count from the latest input or hold end: after input, or the last give of a kind, nothing happens for a while;
//! 5. input lights from any state; a display hold lights a dark screen, and no hold wakes a sleeping machine;
//! 6. [`deadline`] is exact: before it [`decide`] says stay, from it on it says act, and with none it always says stay;
//! 7. holds count: a take and its give leave the counts as they were, and only the last give ends a kind;
//! 8. the settings: every value at most [`MAX_MINUTES`], a standby never set before the sleep, the defaults kept as they are.
//!
//! Not proved: that the shell calls these at the right moments, and that the sleep and standby it asks for happen.
#![no_std]
#![forbid(unsafe_code)]
#![allow(unexpected_cfgs)]

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

/// A minute on CLOCK_MONOTONIC, in microseconds.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const MINUTE_US: i64 = 60_000_000;
/// The owner's defaults (2026-11-08): sleep after 15 minutes, standby after 1 hour.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const SLEEP_DEFAULT_MIN: u32 = 15;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const STANDBY_DEFAULT_MIN: u32 = 60;
/// Windows' longest "Turn off the display after" choice, 5 hours: no setting goes past it.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const MAX_MINUTES: u32 = 300;
/// Times are below 2^62 µs (146,000 years of uptime): sums of them never overflow.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const MAX_TIME_US: i64 = 0x4000_0000_0000_0000;
/// At most this many holds of one kind: one per program at most, in practice a handful.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const MAX_HOLDS: u32 = 0x10000;

/// The machine as standby sees it.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Screen {
    /// On.
    Lit,
    /// The light went off by itself; the machine runs, and input brings the light back.
    Dark,
    /// Asleep, as after the lid (stage 1: `FBIOBLANK`).
    Asleep,
    /// Asleep and gone on to the deeper standby.
    Standby,
}

/// What a hold keeps.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// The screen on and the machine awake: a playing video.
    Display,
    /// The machine awake (or asleep but not in standby), the screen may go off: audio, an agent's inhibitor.
    System,
}

/// What to do now.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Decision {
    Stay,
    /// The light off (the shell's idle dark: level kept, nothing blanked).
    ScreenOff,
    /// Sleep, the same way as the lid.
    Sleep,
    /// From the sleep on to standby.
    Standby,
}

/// The settings, in minutes; 0 is Never.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Times {
    pub off: u32,
    pub sleep: u32,
    pub standby: u32,
}

/// The standby state the shell keeps.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Standby {
    pub screen: Screen,
    pub display_holds: u32,
    pub system_holds: u32,
    /// The last input of any kind, CLOCK_MONOTONIC µs.
    pub last_input_us: i64,
    /// When the last display hold ended (or the start).
    pub display_ended_us: i64,
    /// When the last system hold ended (or the start).
    pub system_ended_us: i64,
}

/// The settings in bounds: each at most [`MAX_MINUTES`], and a standby never before the sleep (one set earlier is moved to it).
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == settle_spec(t),
))]
pub fn settle(t: Times) -> Times {
    let off = if t.off > MAX_MINUTES { MAX_MINUTES } else { t.off };
    let sleep = if t.sleep > MAX_MINUTES { MAX_MINUTES } else { t.sleep };
    let standby = if t.standby > MAX_MINUTES { MAX_MINUTES } else { t.standby };
    let standby = if standby > 0 && sleep > 0 && standby < sleep { sleep } else { standby };
    Times { off, sleep, standby }
}

/// At the start: lit, nothing held, every timer counting from `now`.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == start_spec(now),
))]
pub fn start(now: i64) -> Standby {
    Standby {
        screen: Screen::Lit,
        display_holds: 0,
        system_holds: 0,
        last_input_us: now,
        display_ended_us: now,
        system_ended_us: now,
    }
}

#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == max_spec(a as int, b as int),
))]
fn later(a: i64, b: i64) -> i64 {
    if a >= b { a } else { b }
}

/// When the screen goes off by itself: `off` minutes after the later of the last input and the end of the last display hold;
/// none with `off` 0.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires
        bounded(s),
        t.off <= MAX_MINUTES,
    ensures
        t.off == 0 ==> r.is_none(),
        t.off > 0 ==> r == Some(off_at_spec(s, t) as i64),
))]
pub fn screen_off_at(s: Standby, t: Times) -> Option<i64> {
    if t.off == 0 {
        return None;
    }
    Some(later(s.last_input_us, s.display_ended_us) + (t.off as i64) * MINUTE_US)
}

/// The later of the last input and the end of the last hold of either kind: what the sleep and the standby count from.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires
        bounded(s),
    ensures
        r == idle_since_spec(s),
))]
fn idle_since(s: Standby) -> i64 {
    later(later(s.last_input_us, s.display_ended_us), s.system_ended_us)
}

/// When the machine sleeps by itself: `sleep` minutes after [`idle_since`]; none with `sleep` 0.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires
        bounded(s),
        t.sleep <= MAX_MINUTES,
    ensures
        t.sleep == 0 ==> r.is_none(),
        t.sleep > 0 ==> r == Some(sleep_at_spec(s, t) as i64),
))]
pub fn sleep_at(s: Standby, t: Times) -> Option<i64> {
    if t.sleep == 0 {
        return None;
    }
    Some(idle_since(s) + (t.sleep as i64) * MINUTE_US)
}

/// When a sleeping machine goes on to standby: `standby` minutes after [`idle_since`]; none with `standby` 0.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires
        bounded(s),
        t.standby <= MAX_MINUTES,
    ensures
        t.standby == 0 ==> r.is_none(),
        t.standby > 0 ==> r == Some(standby_at_spec(s, t) as i64),
))]
pub fn standby_at(s: Standby, t: Times) -> Option<i64> {
    if t.standby == 0 {
        return None;
    }
    Some(idle_since(s) + (t.standby as i64) * MINUTE_US)
}

/// Whether the sleep is due at `now` with nothing holding it.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires
        bounded(s),
        t.sleep <= MAX_MINUTES,
    ensures
        r == sleep_due_spec(s, now, t),
))]
fn sleep_due(s: Standby, now: i64, t: Times) -> bool {
    if s.system_holds > 0 {
        return false;
    }
    match sleep_at(s, t) {
        Some(at) => now >= at,
        None => false,
    }
}

/// What to do at `now`. Nothing while a display hold is live. Lit or dark: the sleep once it is due and nothing holds it,
/// else (lit) the screen off once that is due. Asleep: standby once that is due and nothing holds it. In standby: nothing; input,
/// the lid and the power key wake it.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires
        bounded(s),
        0 <= now < MAX_TIME_US,
        t.off <= MAX_MINUTES,
        t.sleep <= MAX_MINUTES,
        t.standby <= MAX_MINUTES,
    ensures
        r == decide_spec(s, now, t),
))]
pub fn decide(s: Standby, now: i64, t: Times) -> Decision {
    if s.display_holds > 0 {
        return Decision::Stay;
    }
    match s.screen {
        Screen::Standby => Decision::Stay,
        Screen::Asleep => {
            if s.system_holds > 0 {
                return Decision::Stay;
            }
            match standby_at(s, t) {
                Some(at) => {
                    if now >= at { Decision::Standby } else { Decision::Stay }
                }
                None => Decision::Stay,
            }
        }
        Screen::Dark => {
            if sleep_due(s, now, t) { Decision::Sleep } else { Decision::Stay }
        }
        Screen::Lit => {
            if sleep_due(s, now, t) {
                return Decision::Sleep;
            }
            match screen_off_at(s, t) {
                Some(at) => {
                    if now >= at { Decision::ScreenOff } else { Decision::Stay }
                }
                None => Decision::Stay,
            }
        }
    }
}

/// The earlier of two times, either missing.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == earlier_spec(a, b),
))]
fn earlier(a: Option<i64>, b: Option<i64>) -> Option<i64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(if x <= y { x } else { y }),
        (Some(x), None) => Some(x),
        (None, y) => y,
    }
}

/// When [`decide`] will next say something other than stay, if nothing else happens first: the shell's poll timeout. None
/// when nothing will happen by itself.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires
        bounded(s),
        t.off <= MAX_MINUTES,
        t.sleep <= MAX_MINUTES,
        t.standby <= MAX_MINUTES,
    ensures
        r == deadline_spec(s, t),
))]
pub fn deadline(s: Standby, t: Times) -> Option<i64> {
    if s.display_holds > 0 {
        return None;
    }
    let sleep = if s.system_holds > 0 { None } else { sleep_at(s, t) };
    match s.screen {
        Screen::Standby => None,
        Screen::Asleep => {
            if s.system_holds > 0 { None } else { standby_at(s, t) }
        }
        Screen::Dark => sleep,
        Screen::Lit => earlier(sleep, screen_off_at(s, t)),
    }
}

/// The state after the shell carried out `d`.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == apply_spec(s, d),
))]
pub fn apply(s: Standby, d: Decision) -> Standby {
    match d {
        Decision::Stay => s,
        Decision::ScreenOff => with_screen(s, Screen::Dark),
        Decision::Sleep => with_screen(s, Screen::Asleep),
        Decision::Standby => with_screen(s, Screen::Standby),
    }
}

/// `s` with another screen, everything else kept.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == with_screen_spec(s, screen),
))]
fn with_screen(s: Standby, screen: Screen) -> Standby {
    Standby {
        screen,
        display_holds: s.display_holds,
        system_holds: s.system_holds,
        last_input_us: s.last_input_us,
        display_ended_us: s.display_ended_us,
        system_ended_us: s.system_ended_us,
    }
}

/// `s` with another last input, everything else kept.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == with_input_spec(s, now),
))]
fn with_input(s: Standby, now: i64) -> Standby {
    Standby {
        screen: s.screen,
        display_holds: s.display_holds,
        system_holds: s.system_holds,
        last_input_us: now,
        display_ended_us: s.display_ended_us,
        system_ended_us: s.system_ended_us,
    }
}

/// Input of any kind (a key, the pointer, the lid opening, a wake): the screen on, the timers from now.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == input_spec(s, now),
))]
pub fn input(s: Standby, now: i64) -> Standby {
    with_input(with_screen(s, Screen::Lit), now)
}

/// The power key or the lid put the machine to sleep: at once, whatever is held (Windows ends power requests on a
/// user-initiated sleep; here they are kept, and they keep it from going on to standby).
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == with_screen_spec(s, Screen::Asleep),
))]
pub fn sleep(s: Standby) -> Standby {
    with_screen(s, Screen::Asleep)
}

/// The power key, the lid or a wake interrupt woke the machine: input.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == input_spec(s, now),
))]
pub fn wake(s: Standby, now: i64) -> Standby {
    input(s, now)
}

/// The light, the sleep or the standby was refused: try again a full timeout later, as if there had been input, without
/// lighting anything.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == with_input_spec(s, now),
))]
pub fn refused(s: Standby, now: i64) -> Standby {
    with_input(s, now)
}

/// A hold of `kind` begins. A display hold lights a dark screen (Windows' display request does); no hold wakes a sleeping
/// machine.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires
        s.display_holds < MAX_HOLDS,
        s.system_holds < MAX_HOLDS,
    ensures
        r == take_spec(s, kind),
))]
pub fn take(s: Standby, kind: Kind) -> Standby {
    match kind {
        Kind::Display => Standby {
            screen: match s.screen {
                Screen::Dark => Screen::Lit,
                Screen::Lit => Screen::Lit,
                Screen::Asleep => Screen::Asleep,
                Screen::Standby => Screen::Standby,
            },
            display_holds: s.display_holds + 1,
            system_holds: s.system_holds,
            last_input_us: s.last_input_us,
            display_ended_us: s.display_ended_us,
            system_ended_us: s.system_ended_us,
        },
        Kind::System => Standby {
            screen: s.screen,
            display_holds: s.display_holds,
            system_holds: s.system_holds + 1,
            last_input_us: s.last_input_us,
            display_ended_us: s.display_ended_us,
            system_ended_us: s.system_ended_us,
        },
    }
}

/// A hold of `kind` ends; the last one of its kind restarts the timers at `now`. A give with none held changes nothing (the
/// shell gives each hold back once, but a slip must not underflow).
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == give_spec(s, kind, now),
))]
pub fn give(s: Standby, kind: Kind, now: i64) -> Standby {
    match kind {
        Kind::Display => {
            if s.display_holds == 0 {
                return s;
            }
            let n = s.display_holds - 1;
            Standby {
                screen: s.screen,
                display_holds: n,
                system_holds: s.system_holds,
                last_input_us: s.last_input_us,
                display_ended_us: if n == 0 { now } else { s.display_ended_us },
                system_ended_us: s.system_ended_us,
            }
        }
        Kind::System => {
            if s.system_holds == 0 {
                return s;
            }
            let n = s.system_holds - 1;
            Standby {
                screen: s.screen,
                display_holds: s.display_holds,
                system_holds: n,
                last_input_us: s.last_input_us,
                display_ended_us: s.display_ended_us,
                system_ended_us: if n == 0 { now } else { s.system_ended_us },
            }
        }
    }
}

#[cfg(verus_keep_ghost)]
verus! {

pub open spec fn max_spec(a: int, b: int) -> int {
    if a >= b { a } else { b }
}

/// Every time in the state is a time: not negative and below the bound.
pub open spec fn bounded(s: Standby) -> bool {
    0 <= s.last_input_us < MAX_TIME_US && 0 <= s.display_ended_us < MAX_TIME_US && 0 <= s.system_ended_us < MAX_TIME_US
}

pub open spec fn settle_spec(t: Times) -> Times {
    let off = if t.off > 300 { 300u32 } else { t.off };
    let sleep = if t.sleep > 300 { 300u32 } else { t.sleep };
    let standby0 = if t.standby > 300 { 300u32 } else { t.standby };
    let standby = if standby0 > 0 && sleep > 0 && standby0 < sleep { sleep } else { standby0 };
    Times { off, sleep, standby }
}

pub open spec fn start_spec(now: i64) -> Standby {
    Standby {
        screen: Screen::Lit,
        display_holds: 0,
        system_holds: 0,
        last_input_us: now,
        display_ended_us: now,
        system_ended_us: now,
    }
}

pub open spec fn off_at_spec(s: Standby, t: Times) -> int {
    max_spec(s.last_input_us as int, s.display_ended_us as int) + t.off * MINUTE_US
}

pub open spec fn idle_since_spec(s: Standby) -> int {
    max_spec(max_spec(s.last_input_us as int, s.display_ended_us as int), s.system_ended_us as int)
}

pub open spec fn sleep_at_spec(s: Standby, t: Times) -> int {
    idle_since_spec(s) + t.sleep * MINUTE_US
}

pub open spec fn standby_at_spec(s: Standby, t: Times) -> int {
    idle_since_spec(s) + t.standby * MINUTE_US
}

pub open spec fn sleep_due_spec(s: Standby, now: i64, t: Times) -> bool {
    s.system_holds == 0 && t.sleep > 0 && now >= sleep_at_spec(s, t)
}

pub open spec fn decide_spec(s: Standby, now: i64, t: Times) -> Decision {
    if s.display_holds > 0 {
        Decision::Stay
    } else {
        match s.screen {
            Screen::Standby => Decision::Stay,
            Screen::Asleep => if s.system_holds == 0 && t.standby > 0 && now >= standby_at_spec(s, t) {
                Decision::Standby
            } else {
                Decision::Stay
            },
            Screen::Dark => if sleep_due_spec(s, now, t) { Decision::Sleep } else { Decision::Stay },
            Screen::Lit => if sleep_due_spec(s, now, t) {
                Decision::Sleep
            } else if t.off > 0 && now >= off_at_spec(s, t) {
                Decision::ScreenOff
            } else {
                Decision::Stay
            },
        }
    }
}

pub open spec fn earlier_spec(a: Option<i64>, b: Option<i64>) -> Option<i64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(if x <= y { x } else { y }),
        (Some(x), None) => Some(x),
        (None, y) => y,
    }
}

pub open spec fn sleep_opt_spec(s: Standby, t: Times) -> Option<i64> {
    if s.system_holds > 0 || t.sleep == 0 { None } else { Some(sleep_at_spec(s, t) as i64) }
}

pub open spec fn off_opt_spec(s: Standby, t: Times) -> Option<i64> {
    if t.off == 0 { None } else { Some(off_at_spec(s, t) as i64) }
}

pub open spec fn deadline_spec(s: Standby, t: Times) -> Option<i64> {
    if s.display_holds > 0 {
        None
    } else {
        match s.screen {
            Screen::Standby => None,
            Screen::Asleep => if s.system_holds > 0 || t.standby == 0 { None } else { Some(standby_at_spec(s, t) as i64) },
            Screen::Dark => sleep_opt_spec(s, t),
            Screen::Lit => earlier_spec(sleep_opt_spec(s, t), off_opt_spec(s, t)),
        }
    }
}

pub open spec fn apply_spec(s: Standby, d: Decision) -> Standby {
    match d {
        Decision::Stay => s,
        Decision::ScreenOff => with_screen_spec(s, Screen::Dark),
        Decision::Sleep => with_screen_spec(s, Screen::Asleep),
        Decision::Standby => with_screen_spec(s, Screen::Standby),
    }
}

pub open spec fn with_screen_spec(s: Standby, screen: Screen) -> Standby {
    Standby {
        screen,
        display_holds: s.display_holds,
        system_holds: s.system_holds,
        last_input_us: s.last_input_us,
        display_ended_us: s.display_ended_us,
        system_ended_us: s.system_ended_us,
    }
}

pub open spec fn with_input_spec(s: Standby, now: i64) -> Standby {
    Standby {
        screen: s.screen,
        display_holds: s.display_holds,
        system_holds: s.system_holds,
        last_input_us: now,
        display_ended_us: s.display_ended_us,
        system_ended_us: s.system_ended_us,
    }
}

pub open spec fn input_spec(s: Standby, now: i64) -> Standby {
    with_input_spec(with_screen_spec(s, Screen::Lit), now)
}

pub open spec fn take_spec(s: Standby, kind: Kind) -> Standby {
    match kind {
        Kind::Display => Standby {
            screen: if s.screen == Screen::Dark { Screen::Lit } else { s.screen },
            display_holds: (s.display_holds + 1) as u32,
            system_holds: s.system_holds,
            last_input_us: s.last_input_us,
            display_ended_us: s.display_ended_us,
            system_ended_us: s.system_ended_us,
        },
        Kind::System => Standby {
            screen: s.screen,
            display_holds: s.display_holds,
            system_holds: (s.system_holds + 1) as u32,
            last_input_us: s.last_input_us,
            display_ended_us: s.display_ended_us,
            system_ended_us: s.system_ended_us,
        },
    }
}

pub open spec fn give_spec(s: Standby, kind: Kind, now: i64) -> Standby {
    match kind {
        Kind::Display => if s.display_holds == 0 {
            s
        } else {
            Standby {
                screen: s.screen,
                display_holds: (s.display_holds - 1) as u32,
                system_holds: s.system_holds,
                last_input_us: s.last_input_us,
                display_ended_us: if s.display_holds == 1 { now } else { s.display_ended_us },
                system_ended_us: s.system_ended_us,
            }
        },
        Kind::System => if s.system_holds == 0 {
            s
        } else {
            Standby {
                screen: s.screen,
                display_holds: s.display_holds,
                system_holds: (s.system_holds - 1) as u32,
                last_input_us: s.last_input_us,
                display_ended_us: s.display_ended_us,
                system_ended_us: if s.system_holds == 1 { now } else { s.system_ended_us },
            }
        },
    }
}

} // verus!

#[cfg(test)]
mod tests;
