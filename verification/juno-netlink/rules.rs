// Verus proof of the SHIPPED code, not a model (project rule D-011): this file only names the source file, and Verus checks it as
// lucina-netlink compiles it (Eyra's getifaddrs on Juno). What is proved is in each function's `ensures`.
#![feature(proc_macro_hygiene)]
#[path = "../../kernel/lucina-netlink/src/walk.rs"]
pub mod walk;

fn main() {}
