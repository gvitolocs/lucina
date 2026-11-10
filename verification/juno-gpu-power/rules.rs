// Verus proof of the SHIPPED code, not a model (project rule D-011): this file only names the source file, and Verus checks it as the
// kernel compiles it. What is proved is in each function's `ensures`, plus the lemma below about any order of submits and timer looks.
#![feature(proc_macro_hygiene)]
#[path = "../../drivers/lucina-adreno/src/power.rs"]
pub mod power;

fn main() {}
