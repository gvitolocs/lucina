// SPDX-License-Identifier: MPL-2.0

//! Idle rules for Juno's CPUs that can be tested without the hardware (D-022). `docs/research/juno-power.md`.
//!
//! # One wake for each timer, not one per CPU
//!
//! Juno's kernel keeps its timers in system-wide queues. An idle CPU used to stop its tick until the earliest of them, so
//! every idle CPU woke at every expiry, found that another CPU had run it, and went back to sleep: about 6000 wakes a second
//! on the idle laptop (`idle_wakes` in `/proc/lucina-power`, 2026-11-04).
//!
//! Linux queues a timer on one CPU's own timer base (`kernel/time/timer.c`; `get_nohz_timer_target` moves it to a busy CPU
//! when the arming one is idle) and since 6.9 hands the global timers of idle CPUs to one migrator per group
//! (`kernel/time/timer_migration.c`), so an expiry wakes one CPU. The rule here gets the same with shared queues: every CPU
//! publishes when it will next wake (`TICKING` while its tick runs), and a CPU stopping its tick takes the earliest expiry
//! only when no other CPU already wakes at or before it. The kernel makes that choice under one lock, so two CPUs cannot each
//! count on the other; a CPU that wakes early starts ticking, which still covers what it was covering.
//!
//! # Which idle state a CPU takes
//!
//! [`teo`] chooses among a CPU's PSCI idle states, [`admit`] holds every choice to the latency budget, the next timer and
//! the broadcast hand-over, [`domain`] decides when the last idle CPU takes the cluster down,
//! [`broadcast`] wakes CPUs whose timer stops, and [`states`] holds the PSCI parameter rules and cpuidle's counters.
//! `docs/research/juno-cpuidle.md`.
#![no_std]
#![allow(unexpected_cfgs)]

pub mod admit;
pub mod broadcast;
pub mod domain;
pub mod states;
#[cfg_attr(verus_keep_ghost, verifier::external)]
pub mod teo;

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

/// A CPU whose periodic tick runs: it sees every expiry within a tick.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const TICKING: u64 = 0;
/// A CPU that has not started, or does not take part: it covers nothing.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const ABSENT: u64 = u64::MAX;

/// Whether a CPU other than `me` wakes at or before `deadline` (counter values): one whose tick runs does, one asleep until
/// `w` does when `w <= deadline`, an absent one never.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures r == exists|i: int| 0 <= i < wake_at.len() && i != me as int
        && wake_at[i] != ABSENT && wake_at[i] <= deadline
))]
pub fn covered(wake_at: &[u64], me: usize, deadline: u64) -> bool {
    let mut i = 0;
    #[cfg_attr(verus_keep_ghost, verus_spec(
        invariant
            0 <= i <= wake_at.len(),
            forall|j: int| 0 <= j < i && j != me as int ==> !(wake_at[j] != ABSENT && wake_at[j] <= deadline),
        decreases wake_at.len() - i
    ))]
    while i < wake_at.len() {
        if i != me && wake_at[i] != ABSENT && wake_at[i] <= deadline {
            return true;
        }
        i += 1;
    }
    false
}

/// When an idle CPU should wake: at the system's next expiry `event` if it must cover it itself, else at `cap`, the longest
/// it may sleep (the clock source must be refreshed). Both are counter values on the tick grid.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r <= cap,
        event.is_none() || covered_by_other ==> r == cap,
        event.is_some() && !covered_by_other ==> r == if event.unwrap() < cap { event.unwrap() } else { cap },
))]
pub fn wake_at(event: Option<u64>, covered_by_other: bool, cap: u64) -> u64 {
    match event {
        Some(e) if !covered_by_other => {
            if e < cap {
                e
            } else {
                cap
            }
        }
        _ => cap,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ticking_cpu_covers_everything() {
        assert!(covered(&[TICKING, 500], 1, 100));
        assert!(!covered(&[TICKING, 500], 0, 100));
    }

    #[test]
    fn a_sleeper_covers_what_is_due_at_or_after_its_wake() {
        let w = [ABSENT, 200, 900];
        assert!(covered(&w, 2, 200));
        assert!(covered(&w, 2, 300));
        assert!(!covered(&w, 2, 199));
        // Its own entry never counts.
        assert!(!covered(&w, 1, 300));
    }

    #[test]
    fn absent_cpus_cover_nothing() {
        assert!(!covered(&[ABSENT, ABSENT], 0, u64::MAX - 1));
        assert!(!covered(&[], 0, 5));
    }

    #[test]
    fn wake_takes_the_event_only_when_nobody_else_does() {
        assert_eq!(wake_at(Some(100), false, 1000), 100);
        assert_eq!(wake_at(Some(100), true, 1000), 1000);
        assert_eq!(wake_at(None, false, 1000), 1000);
        assert_eq!(wake_at(Some(5000), false, 1000), 1000);
    }

    /// Eight CPUs going idle one after another for the same expiry: exactly one takes it.
    #[test]
    fn one_wake_for_one_expiry() {
        let event = 10_000;
        let cap = 1_000_000;
        let mut wake = [TICKING; 8];
        for me in 0..8 {
            let c = covered(&wake, me, event);
            wake[me] = wake_at(Some(event), c, cap);
        }
        assert_eq!(wake.iter().filter(|&&w| w == event).count(), 1);
        assert_eq!(wake[7], event, "the last to sleep found nobody ticking");
    }
}
