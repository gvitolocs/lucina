// Verus proof of the SHIPPED code, not a model (project rule D-011): this file only names the source files, and Verus checks those
// files as the installer and the UFS driver compile them. What is proved is in each function's `ensures`.
#[path = "../../services/lucina-remote/src/disk_rules.rs"]
pub mod disk_rules;
#[path = "../../drivers/lucina-ufs/src/write_rule.rs"]
pub mod write_rule;

#[path = "../../kernel/lucina-boot/src/trial.rs"]
pub mod trial;

fn main() {}
