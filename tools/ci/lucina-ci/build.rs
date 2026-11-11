//! Embeds the source id of this crate (`src/srcid.rs`) as `LUCINA_CI_SRC_ID`; plain std, no dependencies.
#[path = "src/srcid.rs"]
mod srcid;

fn main() {
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=Cargo.lock");
    let id = srcid::compute(std::path::Path::new(".")).unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=LUCINA_CI_SRC_ID={id}");
}
