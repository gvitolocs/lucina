// SPDX-License-Identifier: MPL-2.0

//! The wake-up timer for CPUs whose own timer stops while they sleep.
//!
//! A CPU in a power-down state loses its generic timer (the device tree says `local-timer-stop`), so one timer outside the
//! CPUs wakes them: the memory-mapped `arm,armv7-timer-mem` frame. Behaviour of Linux's one-shot tick broadcast
//! (`kernel/time/tick-broadcast.c`, 7.1), reimplemented: a CPU that goes into such a state hands its next timer event to the
//! broadcast; the broadcast timer is set for the earliest of them and its interrupt routed to that CPU (the device is
//! `CLOCK_EVT_FEAT_DYNIRQ`); when it fires, every CPU whose event is due is woken and the timer is set for the earliest of the
//! rest. A CPU that wakes for another reason takes its entry back; the timer may then fire for nobody, which costs one
//! interrupt and changes nothing.
//!
//! Counter values are `u64` CPU counter ticks; [`ASLEEP_NONE`] marks a CPU that is not waiting on the broadcast. The kernel
//! holds one lock around these calls.

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

use super::domain::MAX_CPUS;

/// Not waiting on the broadcast.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const ASLEEP_NONE: u64 = u64::MAX;

/// Where the broadcast timer must be set: for `deadline`, its interrupt routed to `cpu`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub struct Program {
    pub cpu: usize,
    pub deadline: u64,
}

/// Each CPU's event while it waits on the broadcast.
#[derive(Clone, Copy, Debug)]
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub struct Broadcast {
    pub deadline: [u64; MAX_CPUS],
}

#[cfg_attr(verus_keep_ghost, verus_verify)]
impl Broadcast {
    pub const fn new() -> Self {
        Self { deadline: [ASLEEP_NONE; MAX_CPUS] }
    }

    /// The earliest waiting event and its CPU, the lowest-numbered of equals.
    #[cfg_attr(verus_keep_ghost, verus_spec(r =>
        ensures
            r.is_none() <==> forall|i: int| 0 <= i < MAX_CPUS as int ==> self.deadline@[i] == ASLEEP_NONE,
            r.is_some() ==> r.unwrap().cpu < MAX_CPUS
                && self.deadline@[r.unwrap().cpu as int] == r.unwrap().deadline
                && r.unwrap().deadline != ASLEEP_NONE
                && forall|i: int| 0 <= i < MAX_CPUS as int ==> r.unwrap().deadline <= self.deadline@[i],
    ))]
    pub fn earliest(&self) -> Option<Program> {
        let mut best: Option<Program> = None;
        let mut i = 0;
        #[cfg_attr(verus_keep_ghost, verus_spec(
            invariant
                0 <= i <= MAX_CPUS,
                best.is_none() <==> forall|j: int| 0 <= j < i ==> self.deadline@[j] == ASLEEP_NONE,
                best.is_some() ==> best.unwrap().cpu < i
                    && self.deadline@[best.unwrap().cpu as int] == best.unwrap().deadline
                    && best.unwrap().deadline != ASLEEP_NONE
                    && forall|j: int| 0 <= j < i ==> best.unwrap().deadline <= self.deadline@[j],
            decreases MAX_CPUS - i
        ))]
        while i < MAX_CPUS {
            let d = self.deadline[i];
            if d != ASLEEP_NONE {
                let better = match best {
                    None => true,
                    Some(b) => d < b.deadline,
                };
                if better {
                    best = Some(Program { cpu: i, deadline: d });
                }
            }
            i += 1;
        }
        best
    }

    /// `cpu` sleeps until `deadline` (not [`ASLEEP_NONE`]) without its own timer. Returns the new setting when the
    /// broadcast must be set again: when this event is now strictly the earliest.
    #[cfg_attr(verus_keep_ghost, verus_spec(r =>
        requires cpu < MAX_CPUS, deadline != ASLEEP_NONE,
        ensures
            final(self).deadline@ == old(self).deadline@.update(cpu as int, deadline),
            r.is_some() ==> r.unwrap() == (Program { cpu, deadline }),
            r.is_some() <==> forall|i: int| 0 <= i < MAX_CPUS as int && i != cpu as int ==> deadline < old(self).deadline@[i],
    ))]
    pub fn enter(&mut self, cpu: usize, deadline: u64) -> Option<Program> {
        let mut earliest_other = true;
        let mut i = 0;
        #[cfg_attr(verus_keep_ghost, verus_spec(
            invariant
                0 <= i <= MAX_CPUS,
                cpu < MAX_CPUS,
                self.deadline@ == old(self).deadline@,
                earliest_other == forall|j: int| 0 <= j < i && j != cpu as int ==> deadline < old(self).deadline@[j],
            decreases MAX_CPUS - i
        ))]
        while i < MAX_CPUS {
            if i != cpu && self.deadline[i] <= deadline {
                earliest_other = false;
            }
            i += 1;
        }
        self.deadline[cpu] = deadline;
        if earliest_other { Some(Program { cpu, deadline }) } else { None }
    }

    /// `cpu` woke and no longer waits on the broadcast.
    #[cfg_attr(verus_keep_ghost, verus_spec(
        requires cpu < MAX_CPUS,
        ensures final(self).deadline@ == old(self).deadline@.update(cpu as int, ASLEEP_NONE),
    ))]
    pub fn exit(&mut self, cpu: usize) {
        self.deadline[cpu] = ASLEEP_NONE;
    }

    /// The broadcast timer fired at `now`: every CPU whose event is due (at or before `now`) is taken off and marked in
    /// `wake`, to be woken; the others stay. The kernel then sets the timer by [`Broadcast::earliest`].
    #[cfg_attr(verus_keep_ghost, verus_spec(
        requires now != ASLEEP_NONE,
        ensures
            forall|i: int| 0 <= i < MAX_CPUS as int ==> final(wake)@[i] == (old(self).deadline@[i] <= now),
            forall|i: int| 0 <= i < MAX_CPUS as int ==> final(self).deadline@[i]
                == if old(self).deadline@[i] <= now { ASLEEP_NONE } else { old(self).deadline@[i] },
    ))]
    pub fn fire(&mut self, now: u64, wake: &mut [bool; MAX_CPUS]) {
        let mut i = 0;
        #[cfg_attr(verus_keep_ghost, verus_spec(
            invariant
                0 <= i <= MAX_CPUS,
                now != ASLEEP_NONE,
                forall|j: int| 0 <= j < i ==> wake@[j] == (old(self).deadline@[j] <= now),
                forall|j: int| 0 <= j < i ==> self.deadline@[j]
                    == if old(self).deadline@[j] <= now { ASLEEP_NONE } else { old(self).deadline@[j] },
                forall|j: int| i <= j < MAX_CPUS as int ==> self.deadline@[j] == old(self).deadline@[j],
            decreases MAX_CPUS - i
        ))]
        while i < MAX_CPUS {
            let due = self.deadline[i] <= now;
            wake[i] = due;
            if due {
                self.deadline[i] = ASLEEP_NONE;
            }
            i += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_earliest_sleeper_gets_the_timer() {
        let mut b = Broadcast::new();
        assert_eq!(b.earliest(), None);
        assert_eq!(b.enter(3, 500), Some(Program { cpu: 3, deadline: 500 }));
        assert_eq!(b.enter(5, 900), None);
        assert_eq!(b.enter(1, 200), Some(Program { cpu: 1, deadline: 200 }));
        // A tie does not move the timer.
        assert_eq!(b.enter(2, 200), None);
        assert_eq!(b.earliest(), Some(Program { cpu: 1, deadline: 200 }));
    }

    #[test]
    fn firing_wakes_exactly_the_due_ones() {
        let mut b = Broadcast::new();
        b.enter(0, 100);
        b.enter(1, 200);
        b.enter(2, 300);
        let mut wake = [false; MAX_CPUS];
        b.fire(200, &mut wake);
        assert!(wake[0] && wake[1] && !wake[2]);
        assert_eq!(b.earliest(), Some(Program { cpu: 2, deadline: 300 }));
        b.exit(2);
        assert_eq!(b.earliest(), None);
    }

    #[test]
    fn a_cpu_that_woke_early_is_not_woken_again() {
        let mut b = Broadcast::new();
        b.enter(4, 100);
        b.exit(4);
        let mut wake = [false; MAX_CPUS];
        b.fire(1000, &mut wake);
        assert!(!wake.iter().any(|&w| w));
    }
}
