// SPDX-License-Identifier: MPL-2.0

//! The gate between a device's interrupt handler and the one thread that waits for the device (owner's rule of 2026-11-08:
//! every Juno driver is interrupt-driven; `docs/research/juno-driver-coverage.md`, gap 10).
//!
//! # The protocol
//!
//! It is Linux's split of a threaded interrupt (`request_threaded_irq`, `IRQF_ONESHOT`), with the masking done at the device:
//!
//! - **arm** (thread): with nothing pending, the thread enables the device's interrupt and sleeps until something is pending.
//!   With something pending it takes it at once and does not sleep: the line is never enabled with an event already waiting
//!   to be taken.
//! - **fire** (handler): the handler has read and acknowledged the device's status (`venus_isr`, `ufshcd_intr`, `sdhci_irq`,
//!   `a6xx_irq`). If the status holds anything, it is added to what is pending, the device's interrupt is disabled, and the
//!   thread is woken. A level line therefore never interrupts again before the thread has drained (no storm; the lesson of
//!   juno-charging's patch 1131, where a level line left enabled interrupted 22 000 times). A status of 0 is not this
//!   device's (`IRQ_NONE`): nothing changes, so a sleeping thread keeps its enabled line.
//! - **take** (thread): after a wake or a timeout, the thread takes what is pending; a gate still armed (a timeout) disables
//!   the device's interrupt, so that nothing interrupts while nobody waits.
//!
//! Both sides run under one lock that disables local interrupts (the kernel's `ostd::sync::IrqGate`), so each step is one
//! transition of [`Gate`], and the device's enable is written inside it.
//!
//! # What is proved
//!
//! Verus proves this very file (`verification/juno-irqgate/rules.rs`, D-011); what each function promises is in its
//! `ensures`. With [`inv`] (the device's interrupt is enabled exactly while the gate is armed, and an armed gate has
//! nothing pending) holding before a step, it holds after it, for every step. Besides: after a handler that saw anything,
//! the device's interrupt is disabled, whatever it was before; no status bit is ever dropped (each one the handler adds is
//! handed to the thread by the next `arm` or `take`, and only there); the thread sleeps only with its line enabled and
//! nothing pending; and a handler that sees anything wakes it.
#![no_std]
#![allow(unexpected_cfgs)]

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

/// What a step does to the device's interrupt enable.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Line {
    /// Write the enable (the device's interrupt mask register, or its signal enable).
    Enable,
    /// Clear it.
    Disable,
    /// Leave it as it is.
    Keep,
}

/// The gate's state, shared by the handler and the thread.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gate {
    /// The thread enabled the device's interrupt and sleeps (or is about to); no interrupt has come since.
    pub armed: bool,
    /// Status bits the handler took from the device that the thread has not taken yet.
    pub pending: u32,
}

/// What the handler does after it read `status`.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fired {
    pub gate: Gate,
    pub line: Line,
    /// Wake the thread.
    pub wake: bool,
}

/// What the thread does before it sleeps.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Armed {
    pub gate: Gate,
    pub line: Line,
    /// Status bits the thread takes now.
    pub take: u32,
    /// Sleep until [`woken`] (or the timeout).
    pub sleep: bool,
}

/// What the thread does after it woke, or its timeout passed.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Taken {
    pub gate: Gate,
    pub line: Line,
    /// Status bits the thread takes now.
    pub take: u32,
}

#[cfg(verus_keep_ghost)]
verus! {

/// The device's interrupt enable after `line` was applied to `enabled`.
pub open spec fn after(enabled: bool, line: Line) -> bool {
    match line {
        Line::Enable => true,
        Line::Disable => false,
        Line::Keep => enabled,
    }
}

/// The gate and the device agree: the device's interrupt is enabled exactly while the gate is armed, and an armed gate has
/// nothing pending.
pub open spec fn inv(g: Gate, enabled: bool) -> bool {
    &&& enabled == g.armed
    &&& g.armed ==> g.pending == 0
}

/// The empty gate.
pub open spec fn idle_spec() -> Gate {
    Gate { armed: false, pending: 0 }
}

} // verus!

/// The gate at start: not armed, nothing pending, and the device's interrupt disabled ([`inv`] with `enabled` false).
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == idle_spec(),
        inv(r, false),
))]
pub const fn idle() -> Gate {
    Gate { armed: false, pending: 0 }
}

/// The handler, once it has read and acknowledged `status` at the device.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        forall|enabled: bool| #[trigger] inv(g, enabled) ==> inv(r.gate, after(enabled, r.line)),
        status != 0 ==> forall|enabled: bool| !(#[trigger] after(enabled, r.line)),
        status != 0 ==> r.gate.pending == (g.pending | status),
        status != 0 ==> r.wake,
        status == 0 ==> r.gate == g && r.line == Line::Keep && !r.wake,
))]
pub fn fire(g: Gate, status: u32) -> Fired {
    if status == 0 {
        return Fired { gate: g, line: Line::Keep, wake: false };
    }
    let held = g.pending;
    let pending = held | status;
    #[cfg(verus_keep_ghost)]
    proof! { assert(pending != 0) by (bit_vector) requires pending == (held | status), status != 0; }
    Fired { gate: Gate { armed: false, pending }, line: Line::Disable, wake: pending != 0 }
}

/// The thread, before it sleeps.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        forall|enabled: bool| #[trigger] inv(g, enabled) ==> inv(r.gate, after(enabled, r.line)),
        r.take == g.pending,
        r.gate.pending == 0,
        r.sleep == (g.pending == 0),
        r.sleep ==> r.gate.armed && r.line == Line::Enable,
        !r.sleep ==> !r.gate.armed,
))]
pub fn arm(g: Gate) -> Armed {
    if g.pending != 0 {
        // The invariant says the line is off already: it stays off, and what is pending is taken now.
        Armed { gate: Gate { armed: false, pending: 0 }, line: Line::Keep, take: g.pending, sleep: false }
    } else {
        Armed { gate: Gate { armed: true, pending: 0 }, line: Line::Enable, take: 0, sleep: true }
    }
}

/// The condition the thread sleeps on.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == (g.pending != 0),
))]
pub fn woken(g: Gate) -> bool {
    g.pending != 0
}

/// The thread, after it woke or its timeout passed.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        forall|enabled: bool| #[trigger] inv(g, enabled) ==> inv(r.gate, after(enabled, r.line)),
        forall|enabled: bool| #[trigger] inv(g, enabled) ==> !after(enabled, r.line),
        r.take == g.pending,
        r.gate == idle_spec(),
))]
pub fn take(g: Gate) -> Taken {
    Taken { gate: Gate { armed: false, pending: 0 }, line: if g.armed { Line::Disable } else { Line::Keep }, take: g.pending }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Applies `line` to the model's enable.
    fn apply(enabled: &mut bool, line: Line) {
        match line {
            Line::Enable => *enabled = true,
            Line::Disable => *enabled = false,
            Line::Keep => {}
        }
    }

    fn ok(g: Gate, enabled: bool) -> bool {
        enabled == g.armed && (!g.armed || g.pending == 0)
    }

    #[test]
    fn a_completion_while_asleep_wakes_and_masks() {
        let (mut g, mut e) = (idle(), false);
        let a = arm(g);
        assert!(a.sleep);
        g = a.gate;
        apply(&mut e, a.line);
        assert!(e && ok(g, e));
        let f = fire(g, 0b1);
        g = f.gate;
        apply(&mut e, f.line);
        assert!(f.wake && !e && woken(g) && ok(g, e));
        let t = take(g);
        apply(&mut e, t.line);
        assert_eq!(t.take, 0b1);
        assert!(!e && ok(t.gate, e));
    }

    #[test]
    fn an_event_before_the_arm_is_taken_without_sleeping() {
        let mut e = false;
        let f = fire(idle(), 0b100);
        apply(&mut e, f.line);
        let a = arm(f.gate);
        apply(&mut e, a.line);
        assert!(!a.sleep && !e);
        assert_eq!(a.take, 0b100);
        assert!(ok(a.gate, e));
    }

    #[test]
    fn a_timeout_disables_the_line() {
        let mut e = false;
        let a = arm(idle());
        apply(&mut e, a.line);
        let t = take(a.gate);
        apply(&mut e, t.line);
        assert!(!e && t.take == 0 && ok(t.gate, e));
    }

    #[test]
    fn a_status_of_zero_is_not_ours() {
        let mut e = false;
        let a = arm(idle());
        apply(&mut e, a.line);
        let f = fire(a.gate, 0);
        apply(&mut e, f.line);
        assert!(e && !f.wake && f.gate == a.gate);
    }

    #[test]
    fn two_interrupts_before_a_take_keep_both_bits() {
        let mut e = false;
        let a = arm(idle());
        apply(&mut e, a.line);
        let f1 = fire(a.gate, 0b01);
        apply(&mut e, f1.line);
        let f2 = fire(f1.gate, 0b10);
        apply(&mut e, f2.line);
        assert_eq!(take(f2.gate).take, 0b11);
    }
}
