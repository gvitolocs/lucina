// Verus proof of the SHIPPED code, not a model (project rule D-011): this file only names the source file, and Verus checks it as the
// kernel compiles it. What is proved is in each function's `ensures` and in the proof functions `round_ends` and `no_loss`.
#![feature(proc_macro_hygiene)]
extern crate alloc;

#[path = "../../kernel/lucina-rcu/src/lib.rs"]
pub mod rcu;

fn main() {}
