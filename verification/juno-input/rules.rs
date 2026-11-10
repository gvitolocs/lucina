// Verus proof of the SHIPPED code, not a model (project rule D-011): this file names the source file, and Verus checks it as the
// kernel compiles it. On top of its `ensures`, the lemmas state the rule over the numbers themselves, so a bug planted in the
// function and its spec alike still fails here (verification/juno-input/mutants.sh).
#![feature(proc_macro_hygiene)]
#[path = "../../drivers/lucina-i2c-hid/src/setup.rs"]
pub mod setup;
#[path = "../../drivers/lucina-i2c-hid/src/wait.rs"]
pub mod wait;

use setup::*;
use wait::*;
use vstd::prelude::*;

verus! {

/// The thread's loop as `run` in qcom_hid.rs drives it: count the failures, wait while the rule says so, stop when it says
/// none. It ends: at most SETUP_TRIES setups, each retry a second after the failure before it, and never a retry before one.
proof fn tries_are_bounded_and_paced(failed: u32)
    ensures
        failed == 0 ==> setup_retry_spec(failed).is_none(),
        1 <= failed < 4 ==> setup_retry_spec(failed) == Some(1000u32),
        failed >= 4 ==> setup_retry_spec(failed).is_none(),
        SETUP_TRIES == 4 && SETUP_RETRY_MS == 1000,
{
}

/// The number of setups `run` makes when every one fails: the first, then one per retry the rule allows.
pub fn setups_when_all_fail() -> (n: u32)
    ensures
        n == 4,
{
    let mut failed: u32 = 0;
    let mut n: u32 = 0;
    loop
        invariant
            failed == n,
            n < 4,
        decreases 4 - n,
    {
        n = n + 1;
        failed = failed + 1;
        if setup_retry_ms(failed).is_none() {
            return n;
        }
    }
}

/// Patch 1193: the HID thread keeps its 10 ms tick whenever the interrupt cannot be trusted: no line, a line not yet seen to move,
/// or the test device's reports, which come with no interrupt.
proof fn no_working_line_keeps_the_tick(moved: bool, test_on: bool, d: Option<u64>, now: u64)
    ensures
        wait_spec(false, moved, test_on, d, now) == Wait::Tick,
        wait_spec(true, false, test_on, d, now) == Wait::Tick,
        wait_spec(true, moved, true, d, now) == Wait::Tick,
{
}

/// A timer of the device's own bounds the wait: the thread wakes at it, never after it, and never sleeps with no end while it is
/// set.
proof fn a_due_timer_bounds_the_wait(d: u64, now: u64)
    ensures
        wait_spec(true, true, false, Some(d), now) != Wait::Line,
        d > now ==> wait_spec(true, true, false, Some(d), now) == Wait::LineFor((d - now) as u64),
        d <= now ==> wait_spec(true, true, false, Some(d), now) == Wait::LineFor(0),
{
}

/// A wait with no end only with a working line and nothing due.
proof fn no_end_only_when_nothing_is_due(irq: bool, moved: bool, test_on: bool, d: Option<u64>, now: u64)
    ensures
        (wait_spec(irq, moved, test_on, d, now) == Wait::Line) == (irq && moved && !test_on && d.is_none()),
{
}

/// The earlier of two timers is one of them, and no later than either.
proof fn earliest_is_the_minimum(a: Option<u64>, b: Option<u64>)
    ensures
        earliest_spec(a, b).is_some() == (a.is_some() || b.is_some()),
        a.is_some() ==> earliest_spec(a, b).unwrap() <= a.unwrap(),
        b.is_some() ==> earliest_spec(a, b).unwrap() <= b.unwrap(),
        earliest_spec(a, b).is_some() ==> (earliest_spec(a, b) == a || earliest_spec(a, b) == b),
{
}

} // verus!

fn main() {}
