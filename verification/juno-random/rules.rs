// Verus proof of the SHIPPED code, not a model (project rule D-011): this file only names the source files, and Verus checks
// those files as the kernel and lucina-data compile them. What is proved is in each function's `ensures`.
#[path = "../../kernel/lucina-random/src/rules.rs"]
pub mod rules;
#[path = "../../kernel/lucina-random/src/seed_rules.rs"]
pub mod seed_rules;
#[path = "../../kernel/lucina-boot/src/rng_marker.rs"]
pub mod rng_marker;
// rng_marker's bytes use trial's CRC.
#[path = "../../kernel/lucina-boot/src/trial.rs"]
pub mod trial;

fn main() {}
