// Must NOT verify: a CI self-test (docs/ci/VERUS.md). If CI accepts this file, the gate is broken.
use vstd::prelude::*;
verus! {
fn add_one(x: u8) -> (r: u8)
    requires x < 255,
    ensures r == x + 2,
{
    x + 1
}
}
fn main() {}
