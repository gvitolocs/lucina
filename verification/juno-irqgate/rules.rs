// Verus proof of the SHIPPED code, not a model (project rule D-011): this file names the source file, and Verus checks it as the
// kernel compiles it. What is proved of each step is in its `ensures`; what follows adds the protocol's runs on top of them.
#![feature(proc_macro_hygiene)]
#[path = "../../kernel/lucina-irqgate/src/lib.rs"]
pub mod gate;

use gate::*;
use vstd::prelude::*;

verus! {

/// A thread that sleeps, an interrupt that comes, then the thread's take: the bits come out, the line ends disabled, and the
/// invariant holds at every point.
fn run_sleep_fire_take(status: u32) -> (r: u32)
    requires
        status != 0,
    ensures
        r == status,
{
    let mut e = false;
    let g = idle();
    let a = arm(g);
    assert(a.sleep && a.line == Line::Enable);
    e = true;
    assert(inv(a.gate, e));
    let f = fire(a.gate, status);
    assert(inv(f.gate, after(e, f.line)));
    e = false;
    assert(f.wake);
    let t = take(f.gate);
    assert(inv(t.gate, after(e, t.line)));
    assert(!after(e, t.line));
    assert(t.take == (0u32 | status));
    assert((0u32 | status) == status) by (bit_vector);
    t.take
}

fn run_fire_before_arm(status: u32) -> (r: u32)
    requires
        status != 0,
    ensures
        r == status,
{
    let f = fire(idle(), status);
    assert(inv(f.gate, after(false, f.line)));
    assert((0u32 | status) == status) by (bit_vector);
    let a = arm(f.gate);
    assert(!a.sleep);
    a.take
}

} // verus!

fn main() {}
