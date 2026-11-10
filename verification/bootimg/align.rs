// Verus proof: page alignment used when laying out Android boot images (header v2) for LucinaBoot.
//
// Proved: for page size p > 0 and x + (p - 1) <= u64::MAX, align_up(x, p) is the smallest multiple of p
// that is >= x, and no step overflows; pages(len, p) is the page count that covers len bytes.
// Trusted: Verus, Z3, vstd arithmetic lemmas. Not yet connected to executable LucinaBoot code (T-VER-03).

use vstd::arithmetic::div_mod::*;
use vstd::arithmetic::mul::*;
use vstd::prelude::*;

verus! {

pub fn align_up(x: u64, p: u64) -> (r: u64)
    requires
        p > 0,
        x + (p - 1) <= u64::MAX,
    ensures
        x <= r,
        r % p == 0,
        r - x < p,
{
    let s = x + (p - 1);
    let q = s / p;
    proof {
        lemma_fundamental_div_mod(s as int, p as int);
        lemma_mod_pos_bound(s as int, p as int);
        lemma_mul_is_commutative(p as int, q as int);
        lemma_mod_multiples_basic(q as int, p as int);
    }
    q * p
}

pub fn pages(len: u64, p: u64) -> (n: u64)
    requires
        p > 0,
        len + (p - 1) <= u64::MAX,
    ensures
        n * p >= len,
        n * p - len < p,
{
    let a = align_up(len, p);
    let n = a / p;
    proof {
        lemma_fundamental_div_mod(a as int, p as int);
        lemma_mul_is_commutative(p as int, n as int);
    }
    n
}

fn main() {}

} // verus!
