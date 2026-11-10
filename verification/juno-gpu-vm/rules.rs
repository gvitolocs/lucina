// Verus proof of the SHIPPED code, not a model (project rule D-011): this file only names the source file, and Verus checks it as the
// kernel compiles it. What is proved is in each function's `ensures` in drivers/lucina-adreno/src/gpu_rules.rs: a program's mapping
// stays inside its window and never reaches the kernel's slots, every descriptor carries exactly its address and its
// access, a command buffer lies inside its object, the budgets hold, a fence never moves past what was queued.
#![feature(proc_macro_hygiene)]
#[path = "../../drivers/lucina-adreno/src/gpu_rules.rs"]
pub mod gpu_rules;

use gpu_rules::*;
use vstd::prelude::*;

verus! {

/// A range `user_range` accepts never touches level 1 slots below 64 (the kernel's physical memory and the stub), nor the
/// fence page.
proof fn lemma_user_range_is_above_the_kernel(iova: u64, len: u64, at: u64)
    requires
        is_user_range(iova, len),
        iova <= at < iova + len,
    ensures
        at >= KERNEL_END,
        at / 0x4000_0000 >= 64,
        at != FENCE_IOVA,
        at < VA_END,
{
}

} // verus!

fn main() {}
