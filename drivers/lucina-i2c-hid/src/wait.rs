// SPDX-License-Identifier: MPL-2.0

//! What an I2C-HID device's thread waits for between two passes (patch 1193), as a plain rule. Verus proves this very file
//! (`verification/juno-input/rules.rs`, D-011).
//!
//! The original is Linux's `i2c_hid_irq` (v6.12 `i2c-hid-core.c`), requested with `IRQF_ONESHOT` by `i2c_hid_init_irq`: the
//! device is read when its interrupt line says it has something, and the line stays masked until the thread has read it. Here
//! the thread sleeps until its line fires, or until a timer of the device's own is due (the touchpad's: `mt_expired_timeout`'s
//! sticky-finger release, and the tap, button and kinetic-scroll timers), whichever comes first. With no timer due it sleeps
//! with no timeout at all.
//!
//! It keeps the old 10 ms tick ([`crate::ReadGate::TICK_MS`]) in three cases, each with a reason the hardware or the test gives:
//! with no interrupt to wait for (the line was not mapped, or `lucina.wakeirq=off`); while the line has not yet been seen to
//! move (a pad the device tree describes wrongly would never interrupt, so [`crate::ReadGate`] reads blind every 100 ms until it
//! has); and with `lucina.hid.test=on`, whose reports arrive through `/dev/lucina-hid-test` with no interrupt at all.
#![allow(unexpected_cfgs)]

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

/// How the thread waits before its next pass.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Wait {
    /// One tick of [`crate::ReadGate::TICK_MS`], as before the interrupt.
    Tick,
    /// Unmask the line and sleep until it fires, for at most this many microseconds (0: do not sleep).
    LineFor(u64),
    /// Unmask the line and sleep until it fires: nothing else is due.
    Line,
}

/// The wait for a pass. `line_irq`: the device's line is mapped and the wake interrupts are on; `line_moved`: the line has been
/// seen to change ([`crate::Device::line_moved`]); `test_on`: `lucina.hid.test=on`; `deadline_us`: the earliest timer of the
/// device's own, if any; `now_us`: now, on the same clock.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == wait_spec(line_irq, line_moved, test_on, deadline_us, now_us),
))]
pub fn wait_plan(line_irq: bool, line_moved: bool, test_on: bool, deadline_us: Option<u64>, now_us: u64) -> Wait {
    if !line_irq || !line_moved || test_on {
        Wait::Tick
    } else {
        match deadline_us {
            None => Wait::Line,
            Some(d) => Wait::LineFor(if d > now_us { d - now_us } else { 0 }),
        }
    }
}

/// The earlier of two timers, `None` being none.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == earliest_spec(a, b),
))]
pub fn earliest(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(if x < y { x } else { y }),
        (Some(x), None) => Some(x),
        (None, y) => y,
    }
}

#[cfg(verus_keep_ghost)]
verus! {

pub open spec fn wait_spec(line_irq: bool, line_moved: bool, test_on: bool, deadline_us: Option<u64>, now_us: u64) -> Wait {
    if !line_irq || !line_moved || test_on {
        Wait::Tick
    } else {
        match deadline_us {
            None => Wait::Line,
            Some(d) => Wait::LineFor(if d > now_us { (d - now_us) as u64 } else { 0 }),
        }
    }
}

pub open spec fn earliest_spec(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(if x < y { x } else { y }),
        (Some(x), None) => Some(x),
        (None, y) => y,
    }
}

} // verus!

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticks_without_a_working_interrupt() {
        assert_eq!(wait_plan(false, true, false, None, 0), Wait::Tick);
        assert_eq!(wait_plan(true, false, false, None, 0), Wait::Tick);
        assert_eq!(wait_plan(true, true, true, None, 0), Wait::Tick);
        assert_eq!(wait_plan(false, true, false, Some(5), 0), Wait::Tick);
    }

    #[test]
    fn waits_for_the_line_until_the_next_timer() {
        assert_eq!(wait_plan(true, true, false, None, 100), Wait::Line);
        assert_eq!(wait_plan(true, true, false, Some(250), 100), Wait::LineFor(150));
        assert_eq!(wait_plan(true, true, false, Some(100), 100), Wait::LineFor(0));
        assert_eq!(wait_plan(true, true, false, Some(40), 100), Wait::LineFor(0));
    }

    #[test]
    fn the_earlier_timer() {
        assert_eq!(earliest(None, None), None);
        assert_eq!(earliest(Some(3), None), Some(3));
        assert_eq!(earliest(None, Some(4)), Some(4));
        assert_eq!(earliest(Some(9), Some(4)), Some(4));
        assert_eq!(earliest(Some(4), Some(9)), Some(4));
    }
}
