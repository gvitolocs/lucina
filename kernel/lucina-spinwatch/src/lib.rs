//! The spin-lock wait watchdog's logic, without the machine (patch 1214): ostd's `sync/spinwatch.rs` has the per-CPU slots, the
//! counter and the crash.
//!
//! - [`rules::decide`]: when a waiter marks its wait, reports it or ends the system (proved by Verus).
//! - [`rules::holder_tier`]: which CPUs can hold a lock that others wait for, derived afterwards from the waits (proved by Verus).
//! - [`mode_of`]: what `lucina.spinwatch=` says.
//! - [`Thresholds`]: the times, in counter ticks.
//!
//! Behaviour from Linux: its old `__spin_lock_debug` ("lockup suspected", removed in 2017 for the unfairness of a trylock loop) counted spins up to about a
//! second and then printed the lock and its owner; its hard-lockup detector (`kernel/watchdog.c`, `watchdog_buddy.c`) is what
//! Linux relies on now, which needs an interrupt that a pile-up with interrupts off on every CPU never takes; BPF's
//! `rqspinlock` checks the clock from inside the spin loop, every so many spins, as this does.
#![no_std]
#![forbid(unsafe_code)]

pub mod rules;

pub use rules::{ACT_SECONDS, Action, Holder, MARK_SECONDS, Mode, Stage, Thresholds};

/// The CPUs that have a slot: more are not watched.
pub const SLOTS: usize = 64;
/// The counter is read once in this many spins.
pub const CHECK_EVERY: u32 = 1024;

/// What `lucina.spinwatch=<value>` asks for: `off`, `report` or `crash`. No argument, or one it does not know, is `crash`.
pub fn mode_of(value: Option<&str>) -> Mode {
    match value {
        Some("off") => Mode::Off,
        Some("report") => Mode::Report,
        _ => Mode::Crash,
    }
}

/// The mode the kernel command line asks for: the first `lucina.spinwatch=`, before `--`.
pub fn mode_of_cmdline(cmdline: &str) -> Mode {
    mode_of(
        cmdline
            .split_ascii_whitespace()
            .take_while(|arg| *arg != "--")
            .find_map(|arg| arg.strip_prefix("lucina.spinwatch=")),
    )
}

/// `ticks` of a `frequency` Hz counter in milliseconds; 0 Hz gives 0.
pub fn millis(ticks: u64, frequency: u64) -> u64 {
    if frequency == 0 {
        return 0;
    }
    ((ticks as u128 * 1000) / frequency as u128).min(u64::MAX as u128) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use rules::{decide, holder_tier, next_stage, waiters};

    const HZ: u64 = 19_200_000;

    fn at(seconds_x10: u64, mode: Mode, stage: Stage) -> Action {
        let t = Thresholds::for_frequency(HZ);
        let start = 1_000_000;
        decide(
            start + HZ * seconds_x10 / 10,
            start,
            t.mark_after,
            t.act_after,
            mode,
            stage,
        )
    }

    #[test]
    fn modes() {
        assert_eq!(mode_of(None), Mode::Crash);
        assert_eq!(mode_of(Some("crash")), Mode::Crash);
        assert_eq!(mode_of(Some("report")), Mode::Report);
        assert_eq!(mode_of(Some("off")), Mode::Off);
        assert_eq!(mode_of(Some("bogus")), Mode::Crash);
        assert_eq!(
            mode_of_cmdline("console=ttyAMA0 lucina.spinwatch=report quiet"),
            Mode::Report
        );
        assert_eq!(
            mode_of_cmdline("lucina.spinwatch=off lucina.spinwatch=report"),
            Mode::Off
        );
        assert_eq!(
            mode_of_cmdline("quiet -- lucina.spinwatch=off"),
            Mode::Crash
        );
        assert_eq!(mode_of_cmdline(""), Mode::Crash);
    }

    #[test]
    fn a_short_wait_does_nothing() {
        for mode in [Mode::Off, Mode::Report, Mode::Crash] {
            assert_eq!(at(0, mode, Stage::Quiet), Action::Nothing);
            assert_eq!(at(9, mode, Stage::Quiet), Action::Nothing);
        }
    }

    #[test]
    fn a_second_marks_once() {
        assert_eq!(at(10, Mode::Crash, Stage::Quiet), Action::Mark);
        assert_eq!(at(30, Mode::Report, Stage::Quiet), Action::Mark);
        assert_eq!(at(30, Mode::Crash, Stage::Marked), Action::Nothing);
        assert_eq!(at(30, Mode::Off, Stage::Quiet), Action::Nothing);
    }

    #[test]
    fn five_seconds_act() {
        assert_eq!(at(50, Mode::Crash, Stage::Marked), Action::Crash);
        assert_eq!(at(50, Mode::Crash, Stage::Quiet), Action::Crash);
        assert_eq!(at(50, Mode::Report, Stage::Marked), Action::Report);
        assert_eq!(at(80, Mode::Report, Stage::Reported), Action::Nothing);
        assert_eq!(at(500, Mode::Off, Stage::Quiet), Action::Nothing);
    }

    #[test]
    fn a_counter_behind_the_start_is_no_wait() {
        let t = Thresholds::for_frequency(HZ);
        assert_eq!(
            decide(
                5,
                10 * HZ,
                t.mark_after,
                t.act_after,
                Mode::Crash,
                Stage::Quiet
            ),
            Action::Nothing
        );
    }

    #[test]
    fn a_counter_without_a_frequency_never_acts() {
        let t = Thresholds::for_frequency(0);
        assert_eq!(
            decide(
                u64::MAX - 1,
                0,
                t.mark_after,
                t.act_after,
                Mode::Crash,
                Stage::Quiet
            ),
            Action::Nothing
        );
        assert_eq!(millis(1000, 0), 0);
    }

    #[test]
    fn stages() {
        assert_eq!(next_stage(Stage::Quiet, Action::Nothing), Stage::Quiet);
        assert_eq!(next_stage(Stage::Quiet, Action::Mark), Stage::Marked);
        assert_eq!(next_stage(Stage::Marked, Action::Report), Stage::Reported);
        assert_eq!(next_stage(Stage::Quiet, Action::Crash), Stage::Reported);
    }

    #[test]
    fn a_report_is_said_once_over_a_long_wait() {
        let t = Thresholds::for_frequency(HZ);
        let mut stage = Stage::Quiet;
        let (mut marks, mut reports) = (0, 0);
        for tick in (0..20 * HZ).step_by((HZ / 100) as usize) {
            let action = decide(tick, 0, t.mark_after, t.act_after, Mode::Report, stage);
            marks += (action == Action::Mark) as u32;
            reports += (action == Action::Report) as u32;
            stage = next_stage(stage, action);
        }
        assert_eq!((marks, reports), (1, 1));
    }

    #[test]
    fn the_holder_is_derived_from_the_waits() {
        // CPU 0 holds A, 1..=3 wait for it, 4 waits for B (and may hold A while it does), 5 waits for nothing.
        let (a, b) = (0xffff_8000_1000, 0xffff_8000_2000);
        let waits = [0, a, a, a, b, 0];
        assert_eq!(holder_tier(&waits, a, 0), Holder::Free);
        assert_eq!(holder_tier(&waits, a, 1), Holder::No);
        assert_eq!(holder_tier(&waits, a, 4), Holder::Nested);
        assert_eq!(holder_tier(&waits, a, 5), Holder::Free);
        assert_eq!(holder_tier(&waits, a, 6), Holder::No);
        assert_eq!(holder_tier(&waits, b, 1), Holder::Nested);
        assert_eq!(holder_tier(&waits, b, 4), Holder::No);
        assert_eq!(holder_tier(&waits, 0, 0), Holder::No);
        assert_eq!(waiters(&waits, a), 3);
        assert_eq!(waiters(&waits, b), 1);
        assert_eq!(waiters(&waits, 0), 0);
    }

    #[test]
    fn milliseconds() {
        assert_eq!(millis(HZ * 5 + HZ / 2, HZ), 5500);
        assert_eq!(millis(u64::MAX, 1), u64::MAX);
    }
}
