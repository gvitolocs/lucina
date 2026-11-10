// Verus proof: Android boot image (header v0-v2) section layout. `pages` and `layout_pages` are the same code as
// in kernel/lucina-bootimg/src/lib.rs (its `proof_model_matches_code` test compares the bodies token by token).
//
// Proved for every page size mkbootimg accepts and every u32 section size:
//   - no arithmetic overflows (the crate relies on this: it uses plain u64 arithmetic);
//   - the kernel starts right after the one-page header;
//   - every section offset and the image end are page-aligned;
//   - each section fits before the next one starts (no overlap) and the gap is less than one page
//     (tight packing, i.e. the same offsets mkbootimg writes);
//   - the image end is below 2^38.
// Trusted: Verus, Z3, vstd arithmetic lemmas, and the textual comparison test (rustc and Verus compile the same
// tokens to the same u64 semantics; Verus additionally proves no overflow). Not covered: slicing in parse().

use vstd::arithmetic::div_mod::*;
use vstd::arithmetic::mul::*;
use vstd::prelude::*;

verus! {

pub open spec fn valid_page(p: u64) -> bool {
    p == 2048 || p == 4096 || p == 8192 || p == 16384
}

proof fn lemma_mul_bound(a: int, b: int)
    requires
        0 <= a < 0x100_0000,
        0 <= b <= 16384,
    ensures
        a * b < 0x40_0000_0000,
{
    assert(a * b < 0x40_0000_0000) by (nonlinear_arith)
        requires
            0 <= a < 0x100_0000,
            0 <= b <= 16384,
    ;
}

pub fn pages(len: u32, p: u64) -> (n: u64)
    requires
        valid_page(p),
    ensures
        n * p >= len,
        n * p - len < p,
        n < 0x20_0008,
{
    let n = (len as u64 + (p - 1)) / p;
    proof {
        let s = len as int + p as int - 1;
        lemma_fundamental_div_mod(s, p as int);
        lemma_mod_pos_bound(s, p as int);
        lemma_mul_is_commutative(p as int, n as int);
        lemma_mul_inequality(2048, p as int, n as int);
    }
    n
}

pub struct Layout {
    pub kernel: u64,
    pub ramdisk: u64,
    pub second: u64,
    pub recovery_dtbo: u64,
    pub dtb: u64,
    pub end: u64,
}

/// A section of `size` bytes at `off`, followed by the next section at `next`.
pub open spec fn packed(off: u64, size: u32, next: u64, p: u64) -> bool {
    &&& off % p == 0
    &&& off + size <= next
    &&& next - (off + size) < p
}

proof fn lemma_step(start: u64, n: u64, size: u32, p: u64)
    requires
        valid_page(p),
        n * p >= size,
        n * p - size < p,
        start + n < 0x100_0000,
    ensures
        start * p + size <= (start + n) * p,
        (start + n) * p - (start * p + size) < p,
        (start * p) % (p as int) == 0,
{
    lemma_mul_is_distributive_add_other_way(p as int, start as int, n as int);
    lemma_mod_multiples_basic(start as int, p as int);
}

pub fn layout_pages(p: u64, k: u32, r: u32, s: u32, o: u32, d: u32) -> (l: Layout)
    requires
        valid_page(p),
    ensures
        l.kernel == p,
        packed(l.kernel, k, l.ramdisk, p),
        packed(l.ramdisk, r, l.second, p),
        packed(l.second, s, l.recovery_dtbo, p),
        packed(l.recovery_dtbo, o, l.dtb, p),
        packed(l.dtb, d, l.end, p),
        l.end % p == 0,
        l.end < 0x40_0000_0000,
{
    let kernel_page: u64 = 1;
    let ramdisk_page = kernel_page + pages(k, p);
    let second_page = ramdisk_page + pages(r, p);
    let recovery_dtbo_page = second_page + pages(s, p);
    let dtb_page = recovery_dtbo_page + pages(o, p);
    let end_page = dtb_page + pages(d, p);
    proof {
        lemma_mul_bound(kernel_page as int, p as int);
        lemma_mul_bound(ramdisk_page as int, p as int);
        lemma_mul_bound(second_page as int, p as int);
        lemma_mul_bound(recovery_dtbo_page as int, p as int);
        lemma_mul_bound(dtb_page as int, p as int);
        lemma_mul_bound(end_page as int, p as int);
        lemma_step(kernel_page, (ramdisk_page - kernel_page) as u64, k, p);
        lemma_step(ramdisk_page, (second_page - ramdisk_page) as u64, r, p);
        lemma_step(second_page, (recovery_dtbo_page - second_page) as u64, s, p);
        lemma_step(recovery_dtbo_page, (dtb_page - recovery_dtbo_page) as u64, o, p);
        lemma_step(dtb_page, (end_page - dtb_page) as u64, d, p);
        lemma_mod_multiples_basic(end_page as int, p as int);
    }
    Layout {
        kernel: kernel_page * p,
        ramdisk: ramdisk_page * p,
        second: second_page * p,
        recovery_dtbo: recovery_dtbo_page * p,
        dtb: dtb_page * p,
        end: end_page * p,
    }
}

fn main() {}

} // verus!
