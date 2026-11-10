#![cfg_attr(verus_keep_ghost, feature(proc_macro_hygiene))]
// Verus proof of the SHIPPED code, not a model (project rule D-011): this file only names the source file, and Verus checks it
// as lucina-structmap compiles it. What is proved is in each function's `ensures`.
#[path = "../../kernel/lucina-structmap/src/rules.rs"]
pub mod rules;

fn main() {}
