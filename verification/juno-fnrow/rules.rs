// Verus proof of the SHIPPED code, not a model (project rule D-011): this file names the source file, and Verus checks it as the
// kernel compiles it. On top of each function's `ensures`: the swap is its own inverse for every key code.
#![feature(proc_macro_hygiene)]
#[path = "../../drivers/lucina-i2c-hid/src/fnrow.rs"]
pub mod fnrow;

use fnrow::*;
use vstd::prelude::*;

verus! {

/// Pressing through the swap twice gives the key back, for every code: no key is lost, none reaches two places.
fn twice(code: u16) -> (r: u16)
    ensures
        r == code,
{
    let a = swap(code);
    swap(a)
}

/// The hot keys the table names are distinct from every F-key of the table: a plain F-key can never be its own hot key.
fn no_fixed_point(code: u16)
    requires
        in_row(code),
{
    let a = swap(code);
    assert(a != code);
}

} // verus!

fn main() {}
