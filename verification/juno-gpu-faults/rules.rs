// Verus proof of the SHIPPED code, not a model (project rule D-011): drivers/lucina-adreno/src/fault_log.rs, the ring the GPU SMMU's
// fault interrupt (patch 1173) fills and the retire step drains. Proved in each `ensures`: every slot is inside the array, a new
// record never lands on a live one, records leave in the order they came, a full ring drops and counts, and the blamed table is
// TTBR0 without its ASID.
#![feature(proc_macro_hygiene)]
#[path = "../../drivers/lucina-adreno/src/fault_log.rs"]
pub mod fault_log;

fn main() {}
