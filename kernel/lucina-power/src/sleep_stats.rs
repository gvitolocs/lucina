//! The sleep stats of Linux's `drivers/soc/qcom/qcom_stats.c` as plain arithmetic: no registers, no SMEM, no clock. The
//! kernel (`kernel/core/src/device/qcom_power.rs`, patch 1340) reads the records and prints them; what a reader has to know
//! to say what Linux says is here, where it can be tested.
//!
//! The originals, v6.12 `qcom_stats.c`:
//! - `qcom_print_stats` (lines 75-89): the accumulated time printed for a record that is still running carries the time
//!   since `last_entered_at` as well — line 82-83 compares the two stamps and then adds `arch_timer_read_counter()` minus
//!   `last_entered_at`.
//! - the record is `struct sleep_stats` (lines 62-68): `count` at 0x4, `last_entered_at` at 0x8, `last_exited_at` at 0x10
//!   and `accumulated` at 0x18 (lines 21-24), 32 bytes, records one after another (line 168).
//! - the ticks: Linux prints them raw. They are the rate of the counter whose value Linux adds to a running record, so
//!   the millisecond figures are scaled by that same rate — which the kernel reads from the machine, and which on this
//!   SoC is the 19.2 MHz clock the GENI driver assumes when the firmware leaves the counter's rate unset.

/// The rate to assume when the machine's own is not known: 19.2 MHz, the sleep clock (`lucina_geni::FALLBACK_COUNTER_HZ`
/// is the same crystal). The kernel scales records at the rate the firmware reports and falls back to this.
pub const TICK_HZ: u64 = 19_200_000;

/// Ticks of `TICK_HZ` in milliseconds. Saturating rather than wrapping: a record of a machine that never slept is a
/// count of the counter, not a number to be scaled into an overflow.
pub fn ticks_to_ms(ticks: u64) -> u64 {
    ticks_to_ms_at(ticks, TICK_HZ)
}

/// The same for a counter of the reader's own rate, which is what the kernel uses: the record's stamps and the `now`
/// added to them come from a counter whose rate only the machine knows, so scaling them by a guessed crystal would
/// invent a millisecond figure. `hz` 0 — a firmware that left the counter's rate unset — has no milliseconds.
pub fn ticks_to_ms_at(ticks: u64, hz: u64) -> u64 {
    if hz == 0 {
        return 0;
    }
    ticks.saturating_mul(1000) / hz
}

/// Whether a record was still running as it was read: `qcom_stats.c:82`, `last_entered_at > last_exited_at`. A record
/// whose exit is as late as its entry has come out of sleep.
pub fn running(entered: u64, exited: u64) -> bool {
    entered > exited
}

/// The accumulated time as Linux prints it (`qcom_stats.c:77-83`): the record's own figure, and while the record is still
/// running the time since it was entered too. The stamps are Linux's `last_entered_at` and `last_exited_at`.
///
/// `now` is the same counter the record's stamps come from. A `now` that has not reached `entered` — the record written
/// between the two reads, or a counter that rolled — adds nothing, so a stat that cannot happen reads as the record said
/// rather than as a huge sleep.
pub fn accumulated_shown(accumulated: u64, entered: u64, exited: u64, now: u64) -> u64 {
    if running(entered, exited) {
        accumulated.wrapping_add(now.saturating_sub(entered))
    } else {
        accumulated
    }
}

/// How much a tick figure rose between two readings, `then` at the start of a sleep and `now` at its end. A figure that
/// came back smaller — the firmware restarted its records, or a 32-bit half was read across the other half's roll — is
/// no rise for this sleep, not the 2^64 ticks a subtraction modulo 2^64 would give.
pub fn ticks_delta(now: u64, then: u64) -> u64 {
    now.saturating_sub(then)
}

/// The same for a record's 32-bit count.
pub fn count_delta(now: u32, then: u32) -> u32 {
    now.saturating_sub(then)
}

/// How big a part of the sleep's wall time `part_ms` is, in whole percent. A sleep that measured no time has no
/// percentage; a part longer than the whole (a mode that was in sleep before this one and came out after it) says so
/// rather than being clamped. Whole and remainder are scaled apart, so no pair — however silly the record behind it —
/// wraps into a small figure.
pub fn percent_of(part_ms: u64, whole_ms: u64) -> u64 {
    if whole_ms == 0 {
        return 0;
    }
    let whole = part_ms / whole_ms;
    let rest = part_ms % whole_ms;
    whole
        .saturating_mul(100)
        .saturating_add(rest.saturating_mul(100) / whole_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One second of the sleep clock.
    const ONE_S: u64 = TICK_HZ;

    #[test]
    fn ticks_are_the_sleep_clock() {
        assert_eq!(ticks_to_ms(ONE_S), 1000);
        assert_eq!(ticks_to_ms(ONE_S / 2), 500);
        assert_eq!(ticks_to_ms(192), 0);
        assert_eq!(ticks_to_ms(0), 0);
        // At the rate the machine reports: a faster counter means fewer milliseconds for the same ticks, and a firmware
        // that left the rate at 0 gives no milliseconds rather than a division that panics.
        assert_eq!(ticks_to_ms_at(24_000_000, 24_000_000), 1000);
        assert_eq!(ticks_to_ms_at(12_000_000, 24_000_000), 500);
        assert_eq!(ticks_to_ms_at(ONE_S, 0), 0);
    }

    #[test]
    fn a_running_record_carries_the_time_since_it_entered() {
        // Entered at 1 s, no exit yet, now 1.5 s: the record says 10 ticks, and the shown figure adds the 0.5 s.
        assert!(running(ONE_S, 0));
        assert_eq!(
            accumulated_shown(10, ONE_S, 0, ONE_S + ONE_S / 2),
            10 + ONE_S / 2
        );
        assert_eq!(
            ticks_to_ms(accumulated_shown(10, ONE_S, 0, ONE_S + ONE_S / 2)),
            500
        );
    }

    #[test]
    fn an_exited_record_adds_nothing() {
        // Exited after entered, and exited exactly at entry: both are out of sleep, so the figure stays as it is.
        assert!(!running(ONE_S, 2 * ONE_S));
        assert!(!running(ONE_S, ONE_S));
        assert_eq!(accumulated_shown(1234, ONE_S, 2 * ONE_S, 100 * ONE_S), 1234);
        assert_eq!(accumulated_shown(1234, ONE_S, ONE_S, 100 * ONE_S), 1234);
    }

    #[test]
    fn a_counter_behind_the_record_it_still_running_adds_nothing() {
        // `now` before `last_entered_at`: nothing is subtracted backwards, so the shown figure is the record's own.
        assert_eq!(accumulated_shown(500, 10 * ONE_S, ONE_S, 2 * ONE_S), 500);
        assert_eq!(accumulated_shown(500, u64::MAX, 0, 0), 500);
        // A record at the top of the counter keeps counting modulo 2^64, as the hardware does; it never panics.
        assert_eq!(accumulated_shown(0, ONE_S, 0, ONE_S), 0);
        assert_eq!(accumulated_shown(u64::MAX - 10, ONE_S, 0, ONE_S + 20), 9);
    }

    #[test]
    fn a_rolled_record_is_no_rise() {
        assert_eq!(ticks_delta(ONE_S + 100, ONE_S), 100);
        assert_eq!(ticks_delta(0, 0), 0);
        // Came back smaller: a reset record, not 2^64 ticks of sleep.
        assert_eq!(ticks_delta(ONE_S, ONE_S + 100), 0);
        assert_eq!(ticks_delta(3, u64::MAX), 0);
        assert_eq!(count_delta(12, 10), 2);
        assert_eq!(count_delta(10, 12), 0);
        assert_eq!(count_delta(0, u32::MAX), 0);
    }

    #[test]
    fn percentages_of_the_sleep_s_wall_time() {
        assert_eq!(percent_of(900, 1000), 90);
        assert_eq!(percent_of(1000, 1000), 100);
        assert_eq!(percent_of(0, 1000), 0);
        // A sleep that measured nothing has no percentage, and no division by zero.
        assert_eq!(percent_of(10, 0), 0);
        assert_eq!(percent_of(0, 0), 0);
        // Longer than the sleep it is measured against: said as it is, not hidden behind 100.
        assert_eq!(percent_of(1500, 1000), 150);
        // The multiply saturates, so an absurd record cannot wrap into a small percentage.
        assert!(percent_of(u64::MAX, 1) > percent_of(1, 1));
        assert_eq!(percent_of(u64::MAX, u64::MAX), 100);
    }
}
