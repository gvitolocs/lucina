// Verus proof of the SHIPPED code, not a model (project rule D-011): this file names the source file, and Verus checks it as the
// kernel compiles it. On top of each function's `ensures`, the lemmas state the rules over the numbers themselves, so a bug planted
// in a function and its spec alike still fails here (verification/juno-wifi-wake/mutants.sh).
#![feature(proc_macro_hygiene)]
#[path = "../../drivers/lucina-wifi-wake/src/lib.rs"]
pub mod wake;

use vstd::prelude::*;
use wake::*;

verus! {

/// The sources as the device tree and the kernel number them: twelve copy engines in bits 0-11, then GLINK, SMP2P, the send
/// queue and the control device; the old pass is one millisecond.
proof fn the_sources_and_the_old_pass()
    ensures
        CE_COUNT == 12,
        CE_BITS == 0xfffu32,
        GLINK == 0x1000u32,
        SMP2P == 0x2000u32,
        TX == 0x4000u32,
        CONTROL == 0x8000u32,
        ALL == 0xffffu32,
        POLL_MS == 1,
{
}

/// Each copy engine's bit is its own, lies inside the engines' bits, and is none of the other four sources'.
proof fn every_engine_has_its_own_bit(id: u32, other: u32)
    by (bit_vector)
    requires
        id < 12,
        other < 12,
        id != other,
    ensures
        (1u32 << id) & 0xfffu32 == (1u32 << id),
        (1u32 << id) != 0,
        (1u32 << id) & 0xf000u32 == 0,
        (1u32 << id) & (1u32 << other) == 0,
{
}

/// A raise keeps what was pending and adds its own bit.
proof fn a_raise_keeps_every_bit(pending: u32, source: u32)
    by (bit_vector)
    ensures
        (pending | (source & 0xffffu32)) & pending == pending,
        (pending | (source & 0xffffu32)) & (source & 0xffffu32) == (source & 0xffffu32),
{
}

/// A pass as `bring_up` runs it: drain first, then whatever a handler raises while the pass reads its sources. A source raised
/// after the drain always keeps the thread from sleeping, so no interrupt is lost between the pass and the wait.
pub fn a_raise_during_the_pass_is_seen(pending: u32, source: u32) -> (sleeps: bool)
    requires
        source & 0xffffu32 != 0,
    ensures
        !sleeps,
{
    let (_taken, left) = drain(pending);
    let raised = raise(left, source);
    assert(raised != 0) by (bit_vector)
        requires
            raised == left | (source & 0xffffu32),
            source & 0xffffu32 != 0;
    may_sleep(raised, false)
}

/// With interrupts the thread never sleeps with no end while a timer is set, wakes at the timer and never after it, and does not
/// sleep at all with work left behind.
proof fn interrupts_wake_at_the_deadline(left_behind: bool, busy: bool, d: u64, now: u64)
    ensures
        plan_spec(Mode::Interrupts, left_behind, busy, Some(d), now) != Sleep::Forever,
        !left_behind && !busy && d > now ==> plan_spec(Mode::Interrupts, left_behind, busy, Some(d), now) == Sleep::For((d - now) as u64),
        !left_behind && !busy && d <= now ==> plan_spec(Mode::Interrupts, left_behind, busy, Some(d), now) == Sleep::No,
        !left_behind && busy ==> plan_spec(Mode::Interrupts, left_behind, busy, Some(d), now) == Sleep::Yield,
        plan_spec(Mode::Interrupts, true, busy, Some(d), now) == Sleep::No,
        plan_spec(Mode::Interrupts, true, busy, None, now) == Sleep::No,
{
}

/// A sleep with no timer only with interrupts, nothing left behind, no traffic moving and nothing due.
proof fn no_end_only_when_nothing_is_due(mode: Mode, left_behind: bool, busy: bool, deadline: Option<u64>, now: u64)
    ensures
        (plan_spec(mode, left_behind, busy, deadline, now) == Sleep::Forever)
            == (mode == Mode::Interrupts && !left_behind && !busy && deadline.is_none()),
{
}

/// The old modes stay as they were: the poll never sleeps longer than its millisecond, and yields while traffic moves; the yield
/// mode never sleeps.
proof fn the_old_modes_stay(left_behind: bool, busy: bool, deadline: Option<u64>, now: u64)
    ensures
        busy ==> plan_spec(Mode::Poll, left_behind, busy, deadline, now) == Sleep::Yield,
        !busy ==> plan_spec(Mode::Poll, left_behind, busy, deadline, now) == Sleep::For(1),
        plan_spec(Mode::Yield, left_behind, busy, deadline, now) == Sleep::Yield,
{
}

/// The earlier of two deadlines is one of them, and no later than either.
proof fn earliest_is_the_minimum(a: Option<u64>, b: Option<u64>)
    ensures
        min_due(a, b).is_some() == (a.is_some() || b.is_some()),
        a.is_some() ==> min_due(a, b).unwrap() <= a.unwrap(),
        b.is_some() ==> min_due(a, b).unwrap() <= b.unwrap(),
        min_due(a, b).is_some() ==> (min_due(a, b) == a || min_due(a, b) == b),
{
}

} // verus!

fn main() {}
