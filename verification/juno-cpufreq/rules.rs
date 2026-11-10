// Verus proof of the SHIPPED code, not a model (project rule D-011): this file only names the source file, and Verus checks it as the
// kernel compiles it. What is proved is in each function's `ensures`.
#![feature(proc_macro_hygiene)]
#[path = "../../kernel/lucina-cpufreq/src/rules.rs"]
pub mod rules;

fn main() {}
