// SPDX-License-Identifier: MPL-2.0

//! What one periodic source did while the machine slept (patch 1342).
//!
//! Linux has no per-driver sleep poll: `try_to_freeze_tasks` (`kernel/power/process.c:28`) stops the user tasks and every
//! kernel thread that asked to be freezable — the charger poller `tps65217_charger_poll_task` is one, with `set_freezable()`
//! at `drivers/power/supply/tps65217_charger.c:165` and its `try_to_freeze()` at `:169` — and the rest are gone because
//! their device is suspended. A sleeping machine has no timer of its own beyond the wake sources.
//!
//! This kernel has no freezer yet, so a thread that must be quiet parks on `qcom_power::wait_while_asleep` or asks
//! `qcom_power::asleep` itself. [`Quiet`] is what each of those sources keeps about itself while it does: how often it ran
//! across the last sleep, and how often it let a pass go because the machine slept. The target of the whole exercise is
//! `ran` = 0 for every source — "asleep: no periodic source at all", `docs/design/juno-power-architecture.md` §5 — and the
//! health line prints them all on wake.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// What one source counted across one sleep.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tally {
    /// Times it ran while the machine slept. The target is 0.
    pub ran: u64,
    /// Times it did not run because the machine slept.
    pub skipped: u64,
    /// Whether it saw a sleep at all: before the first one there is nothing to say, and a `ran` of 0 would read as a
    /// quiet sleep rather than as no sleep.
    pub slept: bool,
}

/// One periodic source's count of the sleep in progress, or of the last one it saw.
///
/// A source with no work at all across a sleep (it parked) keeps a `ran` of 0, which is the point.
pub struct Quiet {
    /// The machine's sleep flag as this source last saw it.
    asleep: AtomicBool,
    /// Whether a sleep edge was ever seen.
    slept: AtomicBool,
    /// Times this source ran during the sleep in progress or the last one.
    ran: AtomicU64,
    /// Times this source stayed away during that same sleep.
    skipped: AtomicU64,
}

impl Quiet {
    /// Nothing counted yet.
    pub const fn new() -> Quiet {
        Quiet {
            asleep: AtomicBool::new(false),
            slept: AtomicBool::new(false),
            ran: AtomicU64::new(0),
            skipped: AtomicU64::new(0),
        }
    }

    /// The source looked at the machine and found it in state `asleep`; call it on every pass, whether or not the pass
    /// then does its work. A new sleep starts its count from nothing, so what a reader gets afterwards is always one
    /// sleep's own and never the sum of several.
    ///
    /// Says whether this is the pass that first sees the machine awake again: the last sleep's tally is complete then,
    /// and this is when a source prints it.
    pub fn seen(&self, asleep: bool) -> bool {
        let was = self.asleep.swap(asleep, Ordering::Relaxed);
        if asleep {
            if !was {
                self.ran.store(0, Ordering::Relaxed);
                self.skipped.store(0, Ordering::Relaxed);
                self.slept.store(true, Ordering::Relaxed);
            }
            false
        } else {
            was
        }
    }

    /// This pass ran. Counted only while the machine sleeps: an awake pass is not a sleep's business.
    pub fn ran(&self) {
        if self.asleep.load(Ordering::Relaxed) {
            self.ran.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// This pass did not run because the machine sleeps.
    pub fn skipped(&self) {
        if self.asleep.load(Ordering::Relaxed) {
            self.skipped.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// The counts of the sleep in progress or of the last one seen.
    pub fn tally(&self) -> Tally {
        Tally {
            ran: self.ran.load(Ordering::Relaxed),
            skipped: self.skipped.load(Ordering::Relaxed),
            slept: self.slept.load(Ordering::Relaxed),
        }
    }
}

impl Default for Quiet {
    fn default() -> Quiet {
        Quiet::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_is_counted_before_a_sleep() {
        let q = Quiet::new();
        // An awake source still calls `seen` on every pass, and its runs are no sleep's business.
        assert!(!q.seen(false));
        q.ran();
        q.skipped();
        assert_eq!(
            q.tally(),
            Tally {
                ran: 0,
                skipped: 0,
                slept: false
            }
        );
    }

    #[test]
    fn the_wake_edge_is_reported_once() {
        let q = Quiet::new();
        assert!(!q.seen(true));
        assert!(q.seen(false));
        assert!(!q.seen(false));
        assert!(!q.seen(true));
        assert!(q.seen(false));
    }

    #[test]
    fn a_sleep_counts_what_ran_and_what_stayed_away() {
        let q = Quiet::new();
        q.seen(true);
        q.skipped();
        q.skipped();
        q.ran();
        q.seen(false);
        assert_eq!(
            q.tally(),
            Tally {
                ran: 1,
                skipped: 2,
                slept: true
            }
        );
        // Awake passes add to neither, so the tally a reader gets after the sleep is that sleep's.
        q.ran();
        q.skipped();
        assert_eq!(
            q.tally(),
            Tally {
                ran: 1,
                skipped: 2,
                slept: true
            }
        );
    }

    #[test]
    fn each_sleep_counts_from_nothing() {
        let q = Quiet::new();
        q.seen(true);
        for _ in 0..5 {
            q.skipped();
        }
        q.seen(false);
        q.seen(true);
        assert_eq!(
            q.tally(),
            Tally {
                ran: 0,
                skipped: 0,
                slept: true
            }
        );
        q.skipped();
        q.seen(false);
        assert_eq!(
            q.tally(),
            Tally {
                ran: 0,
                skipped: 1,
                slept: true
            }
        );
    }

    #[test]
    fn a_source_that_missed_the_sleep_sees_no_edge() {
        // A source that only runs with hardware work outstanding can miss a sleep whole: it is then told neither the
        // start nor the end, so it neither prints the old tally as this sleep's nor starts a new count.
        let q = Quiet::new();
        q.seen(true);
        q.skipped();
        q.seen(false);
        assert_eq!(
            q.tally(),
            Tally {
                ran: 0,
                skipped: 1,
                slept: true
            }
        );
        assert!(!q.seen(false));
        assert_eq!(
            q.tally(),
            Tally {
                ran: 0,
                skipped: 1,
                slept: true
            }
        );
    }
}
