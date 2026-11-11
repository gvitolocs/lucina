// Verus proof of the SHIPPED code, not a model (project rule D-011): this file only names the source file, and Verus checks that
// file as the net installer compiles it. What is proved is in each function's `ensures`.
#![feature(proc_macro_hygiene)]
#[path = "../../services/lucina-release/src/rules.rs"]
pub mod rules;
#[path = "../../kernel/lucina-boot/src/board.rs"]
pub mod board;

fn main() {}
