// Must verify: a CI self-test (docs/ci/VERUS.md).
use vstd::prelude::*;
verus! {
fn add_one(x: u8) -> (r: u8)
    requires x < 255,
    ensures r == x + 1,
{
    x + 1
}
}
fn main() {}
