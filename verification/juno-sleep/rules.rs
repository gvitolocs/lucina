// Verus proof of the SHIPPED code, not a model of it (project rule D-011): this file includes kernel/lucina-sleep/src/lib.rs by
// #[path], so Verus checks `next`, `after` and the rest as the kernel compiles them; what each promises is in its `ensures`.
// On top of that, `step` below is one turn of the kernel's loop (ask `next`, do the action, report it with `after`), over any
// inputs and any device outcome, and its `ensures` are the properties the owner asked for (numbered as in lib.rs):
//   1. reverse order: the suspended steps are a stack. A Suspend that worked pushes its step, a Resume pops the top one, and
//      awake means the stack is empty: so every step suspended is resumed once, last first;
//   2. armed before idle: CpuIdle is only suspended while WakeArmed is, and WakeArmed only while a wake source is armed;
//   3. a wake during the suspend stops it: no Suspend is issued then or for the rest of that turn back (Resuming issues
//      none), and Resuming only ever ends in Awake;
//   4. the prefix: what is suspended is always order(0..done);
//   5. the end: each non-Idle action from Suspending or Resuming lowers `rank` (at most 2 * STEPS), and an Idle there means
//      the sequence reached Asleep or Awake;
//   6. the PDC words written are a wake pin's IRQ_i_CFG or, before version 3.2, its IRQ_ENABLE_BANK word.
#![feature(proc_macro_hygiene)]
#[path = "../../kernel/lucina-sleep/src/lib.rs"]
pub mod sleep;

use sleep::*;
use vstd::prelude::*;

verus! {

/// What is suspended: the first `done` steps of the order, in the order they were suspended.
pub open spec fn stack(s: Sleep) -> Seq<Step> {
    Seq::new(s.done as nat, |i: int| order(i as u8))
}

/// How much is left: down from 2 * STEPS while suspending (and turning back), down to 0 while resuming.
pub open spec fn rank(s: Sleep) -> int {
    match s.phase {
        Phase::Suspending => 2 * STEPS - s.done,
        Phase::Resuming { .. } => s.done as int,
        _ => 0,
    }
}

/// The order is a permutation: each step at one place.
proof fn order_index(i: u8)
    requires
        i < STEPS,
    ensures
        index_of_spec(order(i)) == i,
{
}

proof fn index_order(x: Step)
    ensures
        order(index_of_spec(x)) == x,
        index_of_spec(x) < STEPS,
{
}

/// One turn of the kernel's loop.
pub fn step(s: Sleep, inp: Inputs, ok: bool) -> (r: (Sleep, Action))
    requires
        valid(s),
    ensures
        valid(r.0),
        // 4: the prefix (by construction of `stack`), and awake means nothing is suspended.
        r.0.phase == Phase::Awake ==> stack(r.0).len() == 0,
        // 1: a Suspend that worked pushes its step; one that failed changes nothing suspended.
        forall|x: Step| #![auto] ok && r.1 == Action::Suspend(x) ==> stack(r.0) == stack(s).push(x),
        !ok && r.1 is Suspend ==> stack(r.0) == stack(s),
        // 1: a Resume pops the top.
        r.1 is Resume ==> stack(s).len() >= 1
            && r.1 == Action::Resume(stack(s).last())
            && stack(r.0) == stack(s).drop_last(),
        // An Idle changes nothing suspended.
        r.1 == Action::Idle ==> stack(r.0) == stack(s),
        // 2: CpuIdle only on top of an armed WakeArmed.
        r.1 == Action::Suspend(Step::CpuIdle) ==> suspended_spec(s, Step::WakeArmed),
        r.1 == Action::Suspend(Step::WakeArmed) ==> inp.armed,
        // 3: a wake while suspending, or any turn back, issues no Suspend; a turn back ends only in Awake.
        s.phase == Phase::Suspending && inp.wake ==> !(r.1 is Suspend)
            && (r.0.phase is Resuming || r.0.phase == Phase::Awake),
        s.phase is Resuming ==> !(r.1 is Suspend)
            && (r.0.phase is Resuming || r.0.phase == Phase::Awake),
        // 5: progress.
        0 <= rank(r.0) <= 2 * STEPS,
        (s.phase == Phase::Suspending || s.phase is Resuming) && r.1 != Action::Idle
            ==> rank(r.0) < rank(s),
        s.phase == Phase::Suspending && r.1 == Action::Idle ==> r.0.phase == Phase::Asleep || r.0.phase == Phase::Awake,
        s.phase is Resuming && r.1 == Action::Idle ==> r.0.phase == Phase::Awake,
{
    let (s1, a) = next(s, inp);
    let s2 = match a {
        Action::Idle => s1,
        _ => after(s1, a, ok),
    };
    proof {
        if s.done >= 1 {
            order_index((s.done - 1) as u8);
        }
        if s.done < STEPS {
            order_index(s.done);
        }
        match a {
            Action::Suspend(x) => {
                index_order(x);
                if ok {
                    assert(stack(s2) =~= stack(s).push(x));
                } else {
                    assert(stack(s2) =~= stack(s));
                }
            }
            Action::Resume(x) => {
                index_order(x);
                assert(stack(s2) =~= stack(s).drop_last());
            }
            Action::Idle => {
                if s2.done == s.done {
                    assert(stack(s2) =~= stack(s));
                }
            }
        }
    }
    (s2, a)
}

/// The spec of `suspended`, for the ensures above.
pub open spec fn suspended_spec(s: Sleep, step: Step) -> bool {
    index_of_spec(step) < s.done
}

/// 6: the PDC words. Read from the spec of `pdc_write_allowed`: a wake pin's IRQ_i_CFG, or before version 3.2 the bank word.
proof fn pdc_only_wake_pins(version: u32, offset: u32)
    requires
        pdc_allowed_spec(version, offset),
    ensures
        offset == IRQ_I_CFG + 4 * SPMI_PIN || offset == IRQ_I_CFG + 4 * LID_PIN
            || offset == IRQ_I_CFG + 4 * KEYBOARD_PIN || offset == IRQ_I_CFG + 4 * TOUCHPAD_PIN
            || offset == IRQ_I_CFG + 4 * PDIC1_PIN || offset == IRQ_I_CFG + 4 * PDIC0_PIN
            || (version < PDC_VERSION_3_2 && (offset == IRQ_ENABLE_BANK + 4 * (SPMI_PIN / 32)
                || offset == IRQ_ENABLE_BANK + 4 * (LID_PIN / 32)
                || offset == IRQ_ENABLE_BANK + 4 * (KEYBOARD_PIN / 32)
                || offset == IRQ_ENABLE_BANK + 4 * (TOUCHPAD_PIN / 32))),
        SPMI_PIN == 1 && LID_PIN == 49 && KEYBOARD_PIN == 67 && TOUCHPAD_PIN == 33 && PDIC1_PIN == 42 && PDIC0_PIN == 50,
{
}

} // verus!

fn main() {}
