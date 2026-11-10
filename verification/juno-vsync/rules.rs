// Verus proof of the SHIPPED code, not a model (project rule D-011): this file names the source file, and Verus checks it as the
// kernel compiles it. What is proved is in `decide`'s `ensures`; the runs below add that a wait always ends in a start.
#![feature(proc_macro_hygiene)]
#[path = "../../drivers/lucina-adreno/src/scan_rules.rs"]
pub mod scan;

use scan::*;
use vstd::prelude::*;

verus! {

/// After the vertical blank the scan is in the blanking (at or past the last row, or before row 0): the batch goes.
fn after_vblank(height: i32, r: i32, bottom: u32)
    requires
        height > 0,
        r >= height || r < 0,
{
    let d = decide(Some((r, height)), bottom);
    assert(d == Scan::Go);
}

/// A batch that draws nothing (bottom 0) never waits.
fn empty_batch(r: i32, height: i32) {
    let d = decide(Some((r, height)), 0);
    assert(d == Scan::Go);
}

} // verus!

fn main() {}
