// SPDX-License-Identifier: MPL-2.0

//! PSCI idle-state parameters and the per-state statistics cpuidle keeps.
//!
//! Behaviour of Linux's `psci_power_state_is_valid` / `psci_power_state_loses_context` (`include/linux/psci.h`,
//! `drivers/firmware/psci/psci.c`) and of the counters `cpuidle_enter_state` updates (`drivers/cpuidle/cpuidle.c`, 7.1),
//! reimplemented.

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

use super::teo::State;

/// `PSCI_FEATURES(CPU_SUSPEND)` bit 0: OS-initiated mode.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const FEATURE_OSI: u32 = 1 << 0;
/// `PSCI_FEATURES(CPU_SUSPEND)` bit 1: the extended StateID format.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const FEATURE_EXTENDED: u32 = 1 << 1;

/// PSCI 0.2's original format: StateID [15:0], StateType [16], PowerLevel [25:24].
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const ORIGINAL_MASK: u32 = 0x0301_ffff;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const ORIGINAL_TYPE: u32 = 1 << 16;
/// PSCI 1.0's extended format: StateID [27:0], StateType [30].
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const EXTENDED_MASK: u32 = 0x4fff_ffff;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const EXTENDED_TYPE: u32 = 1 << 30;

/// Whether `param` uses only the bits its format defines.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures r == (param & !(if extended { EXTENDED_MASK } else { ORIGINAL_MASK }) == 0),
))]
pub fn param_is_valid(param: u32, extended: bool) -> bool {
    let mask = if extended { EXTENDED_MASK } else { ORIGINAL_MASK };
    param & !mask == 0
}

/// Whether entering `param` powers the CPU down, so that it comes back at the resume entry point with its context lost.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures r == (param & (if extended { EXTENDED_TYPE } else { ORIGINAL_TYPE }) != 0),
))]
pub fn param_loses_context(param: u32, extended: bool) -> bool {
    let bit = if extended { EXTENDED_TYPE } else { ORIGINAL_TYPE };
    param & bit != 0
}

/// One state's counters, as `/sys/devices/system/cpu/cpuN/cpuidle/stateK/` shows them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    /// Entries that went in.
    pub usage: u64,
    /// Time spent, in nanoseconds.
    pub time_ns: u64,
    /// Stays shorter than the target residency while a shallower state was enabled.
    pub above: u64,
    /// Stays long enough for the next deeper enabled state.
    pub below: u64,
    /// Entries the firmware or the driver refused.
    pub rejected: u64,
}

/// Counts an entry into `states[entered]` that lasted `residency_ns` (wake-up included), as `cpuidle_enter_state` does.
pub fn record(usage: &mut [Usage], states: &[State], disabled: &[bool], entered: usize, residency_ns: i64) {
    let u = &mut usage[entered];
    let diff = residency_ns.max(0);
    u.usage += 1;
    u.time_ns = u.time_ns.saturating_add(diff as u64);
    if diff < states[entered].target_residency_ns {
        if (0..entered).rev().any(|i| !disabled[i]) {
            u.above += 1;
        }
    } else {
        let delay = states[entered].exit_latency_ns;
        if diff > delay {
            if let Some(i) = (entered + 1..states.len()).find(|&i| !disabled[i]) {
                if diff - delay >= states[i].target_residency_ns {
                    u.below += 1;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_laptops_params_are_valid_and_power_down() {
        for p in [0x4000_0003, 0x4000_0004, 0x4100_0044, 0x4100_1244, 0x4100_b244] {
            assert!(param_is_valid(p, true), "{p:#x}");
            assert!(param_loses_context(p, true), "{p:#x}");
        }
        assert!(!param_is_valid(0x8000_0003, true));
        assert!(!param_loses_context(0x0000_0001, true));
        assert!(param_loses_context(0x0001_0000, false));
    }

    const S: [State; 3] = [
        State { target_residency_ns: 1_000, exit_latency_ns: 1_000 },
        State { target_residency_ns: 1_774_000, exit_latency_ns: 1_450_000 },
        State { target_residency_ns: 4_001_000, exit_latency_ns: 1_617_000 },
    ];

    #[test]
    fn above_and_below_follow_cpuidle() {
        let mut u = [Usage::default(); 3];
        let on = [false; 3];
        // Too short for state 1, WFI was available.
        record(&mut u, &S, &on, 1, 1_000_000);
        assert_eq!(u[1], Usage { usage: 1, time_ns: 1_000_000, above: 1, below: 0, rejected: 0 });
        // Long enough for state 2 after its own exit latency.
        record(&mut u, &S, &on, 1, 1_450_000 + 4_001_000);
        assert_eq!(u[1].below, 1);
        // Too short for state 2's needs.
        record(&mut u, &S, &on, 1, 1_450_000 + 4_000_999);
        assert_eq!(u[1].below, 1);
        // WFI: nothing shallower.
        record(&mut u, &S, &on, 0, 0);
        assert_eq!(u[0].above, 0);
        // A disabled deeper state does not count.
        record(&mut u, &S, &[false, false, true], 1, 100_000_000);
        assert_eq!(u[1].below, 1);
    }
}
