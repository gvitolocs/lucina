// Verus proof of the SHIPPED code, not a model (project rule D-011): this file only names the source file, and Verus checks it as the
// kernel compiles it. What is proved is in each function's `ensures`.
#![feature(proc_macro_hygiene)]
#[path = "../../drivers/lucina-charge/src/rules.rs"]
pub mod rules;
#[path = "../../drivers/lucina-charge/src/adc.rs"]
pub mod adc;
#[path = "../../drivers/lucina-charge/src/pdic_rules.rs"]
pub mod pdic_rules;
#[path = "../../drivers/lucina-charge/src/sleep_drain.rs"]
pub mod sleep_drain;
#[path = "../../drivers/lucina-charge/src/supply_state.rs"]
pub mod supply_state;

fn main() {}
