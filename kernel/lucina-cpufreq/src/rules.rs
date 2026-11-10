// SPDX-License-Identifier: MPL-2.0

//! The rules that pick a CPU's clock from its load and what cooling allows: the frequency [`wanted_khz`] asks for, and
//! the row of the domain's table [`step_for`] runs. Verus proves [`step_for`] and [`wanted_khz`] in this very file
//! (`verification/juno-cpufreq/rules.rs`); [`CpuLoad::tick`] has host tests only.

#![allow(unexpected_cfgs)]

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;
#[cfg(verus_keep_ghost)]
use vstd::math::min;

/// One usable row of a domain's table.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Step {
    /// What to write to the `REG_PERF_STATE` register for this frequency.
    pub index: u8,
    pub khz: u32,
    pub millivolts: u32,
    /// Always false: boost rows are filtered out, as Linux's cpufreq core keeps boost off.
    pub boost: bool,
}

/// The lowest of `steps` (ascending) that gives `khz`, no higher than `cap`; a position in `steps`.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires steps.len() > 0,
    ensures
        r <= min(cap as int, steps.len() - 1),
        steps[r as int].khz >= khz || r == min(cap as int, steps.len() - 1),
        forall|i: int| 0 <= i < r ==> steps[i].khz < khz,
))]
pub fn step_for(steps: &[Step], khz: u32, cap: usize) -> usize {
    let top = cap.min(steps.len().saturating_sub(1));
    let mut i = 0;
    #[cfg_attr(verus_keep_ghost, verus_spec(
        invariant
            0 <= i <= top,
            top < steps.len(),
            forall|j: int| 0 <= j < i ==> steps[j].khz < khz,
        decreases top - i
    ))]
    while i < top && steps[i].khz < khz {
        i += 1;
    }
    i
}

/// How busy a CPU has been, kept from its timer tick: a running average over about 16 ticks, 0 to 1024.
///
/// An idle CPU stops its tick, so the time it slept arrives as ticks that passed all at once.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CpuLoad {
    pub util: u32,
    /// Busy ticks in a row, up to now.
    pub busy_run: u32,
}

impl CpuLoad {
    /// Takes one tick that was `busy` or not, after `skipped` idle ticks slept through. Not proved: Verus skips it (host tests only).
    #[cfg_attr(verus_keep_ghost, verifier::external_body)]
    pub fn tick(&mut self, skipped: u64, busy: bool) {
        if skipped > 0 {
            self.busy_run = 0;
            // Decays to nothing in 128 idle ticks; no need to loop further than that.
            for _ in 0..skipped.min(128) {
                self.util -= self.util / 16 + u32::from(!self.util.is_multiple_of(16));
            }
        }
        if busy {
            self.busy_run = self.busy_run.saturating_add(1);
            self.util = (self.util * 15 + 1024).div_ceil(16);
        } else {
            self.busy_run = 0;
            self.util -= self.util / 16 + u32::from(!self.util.is_multiple_of(16));
        }
    }
}

/// Busy ticks in a row after which a CPU gets its highest clock: a thing that takes this long may take long, and what
/// someone waits for should end soon. The next frame, a launch, a page: nothing here runs for long at a time when idle.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const BUSY_RUN_FOR_MAX: u32 = 2;

/// The clock a domain's CPU wants, as Linux's schedutil without frequency invariance: 25% above what it is using of the
/// clock it runs at. Everything from 80% on is the top row; that is what the 25% is for.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r <= max_khz,
        load.busy_run >= BUSY_RUN_FOR_MAX ==> r == max_khz,
        load.busy_run < BUSY_RUN_FOR_MAX ==> r as int == min(
            (current_khz as int * load.util as int / 1024) * 5 / 4,
            max_khz as int,
        ),
))]
pub fn wanted_khz(load: CpuLoad, current_khz: u32, max_khz: u32) -> u32 {
    if load.busy_run >= BUSY_RUN_FOR_MAX {
        return max_khz;
    }
    #[cfg(verus_keep_ghost)]
    proof! {
        assert(current_khz as u64 * load.util as u64 <= u64::MAX)
            by (nonlinear_arith)
            requires current_khz as int <= u32::MAX as int,
                load.util as int <= u32::MAX as int;
    }
    let used = u64::from(current_khz) * u64::from(load.util) / 1024;
    #[cfg(verus_keep_ghost)]
    proof! {
        assert(used * 5 <= u64::MAX)
            by (nonlinear_arith)
            requires used as int <= u64::MAX as int / 1024;
    }
    (used * 5 / 4).min(u64::from(max_khz)) as u32
}
