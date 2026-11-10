// Verus proof of the SHIPPED code, not a model (project rule D-011): this file names the source file, and Verus checks it as the
// shell compiles it. What is proved of each function is in its `ensures`; what follows adds runs of presses on top.
#![feature(proc_macro_hygiene)]
#[path = "../../apps/lucina-shell/src/fnkeys.rs"]
pub mod fnkeys;

use fnkeys::*;
use vstd::prelude::*;

verus! {

/// Any number of volume presses, in any order, from any level read back: the level stays in 0..=VOLUME_MAX, and its
/// decibels stay between silence's and the program's own level.
fn presses(start: u8, ups: &Vec<bool>) -> (r: u8)
    ensures
        r <= VOLUME_MAX,
{
    let mut level = volume_step(start, false);
    let mut i = 0;
    while i < ups.len()
        invariant
            level <= VOLUME_MAX,
        decreases ups.len() - i,
    {
        level = volume_step(level, ups[i]);
        let db = volume_db(level);
        assert(db <= 0);
        i += 1;
    }
    level
}

/// Twenty presses up from silence reach the top; one more stays there.
fn full_range() {
    let mut level: u8 = 0;
    let mut i: u8 = 0;
    while i < VOLUME_MAX
        invariant
            i <= VOLUME_MAX,
            level == i,
        decreases VOLUME_MAX - i,
    {
        level = volume_step(level, true);
        i += 1;
    }
    assert(level == VOLUME_MAX);
    let top = volume_step(level, true);
    assert(top == VOLUME_MAX);
}

} // verus!

fn main() {}
