// Verus proof of the SHIPPED code, not a model (project rule D-011): drivers/lucina-ir3/src/pack.rs, the step that puts every
// field of an encoded ir3 instruction into its word. Proved in its ensures: the field gets exactly the value, no other bit
// changes, and a value too wide or disagreeing with bits already placed is refused.
#![feature(proc_macro_hygiene)]
#[path = "../../drivers/lucina-ir3/src/pack.rs"]
pub mod pack;

fn main() {}
