// Verus proof of the SHIPPED code, not a model (project rule D-011): drivers/lucina-ir3/src/delay.rs, Mesa ir3_delayslots for an
// a6xx with full registers. Proved in its ensures: the slots or sync flag between each producer and reader, and that the
// slots the legaliser adds are enough.
#![feature(proc_macro_hygiene)]
#[path = "../../drivers/lucina-ir3/src/delay.rs"]
pub mod delay;

use delay::*;
use vstd::prelude::*;

verus! {

/// The a6xx values of Mesa ir3_compiler.c (delay_slots), which the rules above are stated in.
proof fn lemma_mesas_a6xx_values()
    ensures ALU_TO_ALU == 3, NON_ALU == 6, CAT3_SRC2_READ == 2,
{
}

} // verus!

fn main() {}
