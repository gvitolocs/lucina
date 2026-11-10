// SPDX-License-Identifier: MPL-2.0

//! The task-CPU slot of Juno's scheduler, and the rule that keeps an exited task out of every run queue
//! (`docs/research/juno-taskcpu.md`).
//!
//! The original is Asterinas' `ostd/src/task/scheduler/info.rs`: every task has `schedule_info().cpu`, an `AtomicU32` whose
//! `u32::MAX` means "on no CPU". A waker's `unpark_target` claims the slot with a CAS from `NONE` to the selected
//! CPU; if the CAS fails it rechecks under the previous CPU's run-queue lock. A task that exits stores `NONE` again,
//! so a waker that reaches its CAS only after the exit finds `NONE`, claims it and enqueues the dead task; the next
//! `exit_current` on that CPU then hits `unreachable!()` (Juno boot 108, `ostd/src/task/mod.rs:210`).
//!
//! Linux refuses the same wake-up with `TASK_DEAD` (`do_task_dead` stores it, `ttwu_state_match` in
//! `try_to_wake_up` refuses it). This crate is that, as three pure functions over the raw slot value: exit stores
//! [`DEAD`] instead of [`NONE`], a failed claim that sees [`DEAD`] refuses the wake-up, and nothing ever stores over
//! [`DEAD`]. Verus proves this very file (`verification/juno-taskcpu/rules.rs`, D-011): what each function promises
//! is in its `ensures`, and the protocol model there proves that once a task has exited it is never in a run queue
//! nor current anywhere, over all interleavings of any number of wakers.
#![no_std]
#![allow(unexpected_cfgs)]

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

/// The raw slot value of "on no CPU" (Asterinas's `AtomicCpuId` NONE).
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const NONE: u32 = u32::MAX;

/// The raw slot value of "exited, never wake again" (Linux's `TASK_DEAD`). Stored by exit instead of [`NONE`];
/// a waker that sees it refuses the wake-up, and no step ever stores over it.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const DEAD: u32 = u32::MAX - 1;

/// What the raw slot value means.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Slot {
    /// [`NONE`]: on no CPU, not exited.
    Free,
    /// Any other value: on that CPU (in its run queue or running as its current).
    On(u32),
    /// [`DEAD`]: exited.
    Dead,
}

/// What the raw slot value means: [`NONE`] is [`Slot::Free`], [`DEAD`] is [`Slot::Dead`], anything else is
/// [`Slot::On`] of the value itself.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == if raw == NONE { Slot::Free } else if raw == DEAD { Slot::Dead } else { Slot::On(raw) },
))]
pub fn decode(raw: u32) -> Slot {
    if raw == NONE {
        Slot::Free
    } else if raw == DEAD {
        Slot::Dead
    } else {
        Slot::On(raw)
    }
}

/// The raw value of "on CPU `cpu`".
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires
        cpu < DEAD,
    ensures
        r == cpu,
))]
pub fn encode_on(cpu: u32) -> u32 { cpu }

/// A waker's decision after its claim (CAS [`NONE`] -> cpu) failed and saw `prev` (never [`NONE`]): recheck under
/// CPU `prev`'s run-queue lock, or refuse because the task has exited ([`DEAD`]).
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Wake {
    /// The task may still be in CPU `p`'s run queue: recheck the slot under that lock.
    Recheck(u32),
    /// The task has exited: refuse the wake-up.
    Refuse,
}

/// [`DEAD`] refuses the wake-up; any other `prev` (a CPU id) is rechecked under that CPU's run-queue lock.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires
        prev != NONE,
    ensures
        r == if prev == DEAD { Wake::Refuse } else { Wake::Recheck(prev) },
))]
pub fn after_failed_claim(prev: u32) -> Wake {
    if prev == DEAD {
        Wake::Refuse
    } else {
        Wake::Recheck(prev)
    }
}

#[cfg(test)]
mod tests;
