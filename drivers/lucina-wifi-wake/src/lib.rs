// SPDX-License-Identifier: MPL-2.0

//! When Juno's Wi-Fi thread runs (patch 1122).
//!
//! The thread used to make a pass every millisecond, and as often as the scheduler let it while traffic moved: more than 1000
//! wakes a second with the machine idle, the largest wake source on Juno. Linux does not poll here. `ath10k_snoc` requests one
//! line per copy engine (`ath10k_snoc_request_irq`), and its handler (`ath10k_snoc_per_engine_handler`) masks the engine, marks
//! it pending and schedules NAPI, whose poll (`ath10k_snoc_napi_poll`) services the pending engines and unmasks them. The modem's
//! QMI comes over GLINK, woken by the GLINK doorbell (`qcom_glink_smem_intr`), and its state bits over SMP2P
//! (`qcom_smp2p_intr`). The lines are the device tree's and agree with Windows' `\_SB.QWLN` and `\_SB.GLNK`: copy engines 0-11 on
//! SPI 414-425, level high; the modem's GLINK edge SPI 449 and its SMP2P SPI 451, edge rising.
//!
//! # Raise, drain, sleep
//!
//! Each source has a bit ([`ce`], [`GLINK`], [`SMP2P`], [`TX`] for a frame the network stack queued, [`CONTROL`] for a
//! command on `/dev/lucina-wifi`). A handler [`raise`]s its bit and wakes the thread. A pass begins with [`drain`], which takes
//! the word and leaves it clear before any source is read, so whatever comes during the pass is seen by the next one. After the
//! pass the thread waits only when [`may_sleep`] allows it: nothing raised since the drain, and nothing left behind.
//!
//! # Deadlines
//!
//! What still happens at a time is the drivers' own timers: the bring-up's, a scan's, a join's, a key's, the WMI head's
//! (`ath10k`), the station's authentication, association, handshake and lost-beacon probes (`mac80211`), GLINK's intent and
//! answer timeouts, a QMI transaction's, and the HTT ring refill's retry. The thread sleeps until the [`earliest`] of them, or
//! with no timer when there is none ([`plan`]).
//!
//! # Modes
//!
//! [`Mode::Interrupts`] is the default. While traffic moves (`busy`: the last pass moved a frame, with `lucina.wifi.fast`) it
//! keeps passing without sleeping, as NAPI keeps polling while its budget is used (`ath10k_snoc_napi_poll` completes only when
//! `done < budget`), and goes back to its lines once traffic stops. [`Mode::Poll`] is the old 1 ms pass, for `lucina.wifi.irq=off` or when a line could not
//! be taken: it never sleeps longer than [`POLL_MS`]. [`Mode::Yield`] is `lucina.wifi.pass=yield`: no sleep at all.
//!
//! Verus proves this very file (`verification/juno-wifi-wake/rules.rs`). What each function promises is in its `ensures`.
#![no_std]
#![allow(unexpected_cfgs)]

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

/// The copy engines: `CE_COUNT` in `ath10k`, one line each (`ath10k_snoc_resource_init`).
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const CE_COUNT: u32 = 12;
/// Bits 0 to 11: one per copy engine.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const CE_BITS: u32 = 0xfff;
/// The modem's GLINK doorbell: the `glink-edge` of `remoteproc_mpss`, SPI 449.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const GLINK: u32 = 0x1000;
/// The modem's SMP2P: `smp2p-mpss`, SPI 451.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const SMP2P: u32 = 0x2000;
/// A frame the network stack queued for sending.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const TX: u32 = 0x4000;
/// A command written to `/dev/lucina-wifi`.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const CONTROL: u32 = 0x8000;
/// Every source.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const ALL: u32 = 0xffff;
/// The old pass, kept for [`Mode::Poll`].
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const POLL_MS: u64 = 1;

/// The bit of copy engine `id`, or none past the last.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        id < CE_COUNT ==> r == (1u32 << id),
        id >= CE_COUNT ==> r == 0,
))]
pub fn ce(id: u32) -> u32 {
    if id < CE_COUNT { 1u32 << id } else { 0 }
}

/// A handler's raise: its bit joins what is pending. Anything that is not a source changes nothing.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == pending | (source & ALL),
))]
pub fn raise(pending: u32, source: u32) -> u32 {
    pending | (source & ALL)
}

/// The start of a pass: what was pending, taken, and the word left clear.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r.0 == pending && r.1 == 0,
))]
pub fn drain(pending: u32) -> (u32, u32) {
    (pending, 0)
}

/// Whether the thread may wait: nothing raised since the drain, and the pass left nothing it could do now.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == (pending == 0 && !left_behind),
))]
pub fn may_sleep(pending: u32, left_behind: bool) -> bool {
    pending == 0 && !left_behind
}

/// The earlier of two deadlines, `None` being none.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == min_due(a, b),
))]
pub fn earliest(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(if x < y { x } else { y }),
        (Some(x), None) => Some(x),
        (None, y) => y,
    }
}

/// How the thread is woken.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// By its lines, and by its deadlines.
    Interrupts,
    /// The old 1 ms pass (`lucina.wifi.irq=off`, or a line that could not be taken).
    Poll,
    /// `lucina.wifi.pass=yield`: never sleeps.
    Yield,
}

/// How the thread waits after a pass.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Sleep {
    /// Until something is raised: no timer.
    Forever,
    /// Until something is raised, or this many milliseconds.
    For(u64),
    /// Not at all: something is due now, or was left behind.
    No,
    /// Gives the CPU to whoever else wants it, then goes on.
    Yield,
}

/// The wait after a pass. `left_behind`: the pass left work it can do now; `busy`: traffic moves and `lucina.wifi.fast` is
/// on (with interrupts: the last pass moved a frame; in the poll: a frame in the last few milliseconds), and both modes then
/// yield instead of sleeping; `deadline`: the earliest timer, on
/// the same clock as `now`.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == plan_spec(mode, left_behind, busy, deadline, now),
))]
pub fn plan(mode: Mode, left_behind: bool, busy: bool, deadline: Option<u64>, now: u64) -> Sleep {
    match mode {
        Mode::Yield => Sleep::Yield,
        Mode::Poll => {
            if busy {
                Sleep::Yield
            } else {
                Sleep::For(POLL_MS)
            }
        }
        Mode::Interrupts => {
            if left_behind {
                Sleep::No
            } else if busy {
                Sleep::Yield
            } else {
                match deadline {
                    None => Sleep::Forever,
                    Some(d) => {
                        if d > now {
                            Sleep::For(d - now)
                        } else {
                            Sleep::No
                        }
                    }
                }
            }
        }
    }
}

#[cfg(verus_keep_ghost)]
verus! {

pub open spec fn min_due(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(if x < y { x } else { y }),
        (Some(x), None) => Some(x),
        (None, y) => y,
    }
}

pub open spec fn plan_spec(mode: Mode, left_behind: bool, busy: bool, deadline: Option<u64>, now: u64) -> Sleep {
    match mode {
        Mode::Yield => Sleep::Yield,
        Mode::Poll => if busy { Sleep::Yield } else { Sleep::For(POLL_MS) },
        Mode::Interrupts => if left_behind {
            Sleep::No
        } else if busy {
            Sleep::Yield
        } else {
            match deadline {
                None => Sleep::Forever,
                Some(d) => if d > now { Sleep::For((d - now) as u64) } else { Sleep::No },
            }
        },
    }
}

} // verus!

#[cfg(test)]
mod tests;
