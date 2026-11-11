// SPDX-License-Identifier: MPL-2.0

//! What a sleep cost the battery, from the fuel gauge at its two ends (patch 1197), as plain arithmetic. Verus proves this very
//! file (`verification/juno-charge/rules.rs`, D-011). The kernel reads the gauge when the machine goes to sleep and when it
//! wakes, and every real sleep records its own drain in the event log, so no test window is needed to know what sleeping costs.
//!
//! The figures are the gauge's SBS RemainingCapacity (mAh) and RelativeStateOfCharge (%), and its Voltage (mV) at both ends:
//! average power is the charge used times the mean of the two voltages, over the time asleep. With a charger in at either end
//! or during the sleep there is no drain to report, and the caller does not ask for one.
#![allow(unexpected_cfgs)]

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

/// The charge used over the sleep, in mAh: positive when the battery lost charge.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == (start as int) - (end as int),
))]
pub fn drain_mah(start: u16, end: u16) -> i32 {
    start as i32 - end as i32
}

/// The state of charge used over the sleep, in percentage points.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == (start as int) - (end as int),
))]
pub fn drain_pct(start: u8, end: u8) -> i16 {
    start as i16 - end as i16
}

/// The average power over the sleep in mW: the charge used times the mean voltage, per time (mAh x mV x 3600 / ms). None when
/// nothing was used or no time passed.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == average_spec(drained_mah, start_mv, end_mv, asleep_ms),
))]
pub fn average_mw(drained_mah: u16, start_mv: u16, end_mv: u16, asleep_ms: u64) -> Option<u64> {
    if drained_mah == 0 || asleep_ms == 0 {
        return None;
    }
    let mean = (start_mv as u64 + end_mv as u64) / 2;
    #[cfg(verus_keep_ghost)]
    proof! {
        assert((drained_mah as int) * (mean as int) <= 65535 * 65535) by (nonlinear_arith)
            requires
                0 <= drained_mah <= 65535,
                0 <= mean <= 65535;
    }
    Some((drained_mah as u64 * mean) * 3600 / asleep_ms)
}

#[cfg(verus_keep_ghost)]
verus! {

pub open spec fn average_spec(drained_mah: u16, start_mv: u16, end_mv: u16, asleep_ms: u64) -> Option<u64> {
    if drained_mah == 0 || asleep_ms == 0 {
        None
    } else {
        Some((((drained_mah as int) * (((start_mv as int) + (end_mv as int)) / 2) * 3600) / (asleep_ms as int)) as u64)
    }
}

/// The figures on known numbers: 100 mAh at 3.8 V over one hour is 380 mW, and the mean of the two voltages counts.
proof fn the_average_on_known_numbers()
    ensures
        average_spec(100, 3800, 3800, 3_600_000) == Some(380u64),
        average_spec(100, 3700, 3900, 3_600_000) == Some(380u64),
        average_spec(100, 3800, 3800, 1_800_000) == Some(760u64),
        average_spec(0, 3800, 3800, 3_600_000).is_none(),
        average_spec(100, 3800, 3800, 0).is_none(),
{
    assert(((100 * ((3800 + 3800) / 2) * 3600) / 3_600_000) == 380) by (compute);
    assert(((100 * ((3700 + 3900) / 2) * 3600) / 3_600_000) == 380) by (compute);
    assert(((100 * ((3800 + 3800) / 2) * 3600) / 1_800_000) == 760) by (compute);
}

} // verus!

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sleep_on_battery() {
        assert_eq!(drain_mah(3120, 2978), 142);
        assert_eq!(drain_pct(74, 71), 3);
        // 4.33 h, 142 mAh around 3.9 V: about 128 mW.
        assert_eq!(average_mw(142, 3920, 3880, 15_593_452), Some(127));
    }

    #[test]
    fn the_figures_on_known_numbers() {
        assert_eq!(average_mw(100, 3800, 3800, 3_600_000), Some(380));
        assert_eq!(average_mw(100, 3700, 3900, 3_600_000), Some(380));
        assert_eq!(average_mw(100, 3800, 3800, 1_800_000), Some(760));
    }

    #[test]
    fn no_drain_no_figure() {
        assert_eq!(average_mw(0, 3800, 3800, 3_600_000), None);
        assert_eq!(average_mw(100, 3800, 3800, 0), None);
        // A battery that gained charge is a negative drain: the caller then asks for no average.
        assert_eq!(drain_mah(2000, 2100), -100);
        assert_eq!(drain_pct(50, 52), -2);
    }

    #[test]
    fn the_largest_numbers_do_not_overflow() {
        assert_eq!(
            average_mw(65535, 65535, 65535, 1),
            Some(65535u64 * 65535 * 3600)
        );
    }
}
