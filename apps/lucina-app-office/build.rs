//! The sum of `office.pkg`, from what `office/build.sh` left beside it. A build with no package gets none, and then refuses to unpack.

fn main() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let file = root.join("target/aarch64-unknown-linux-musl/juno-app/office.pkg.sha256");
    println!("cargo:rerun-if-changed={}", file.display());
    let sum = std::fs::read_to_string(&file).unwrap_or_default();
    println!("cargo:rustc-env=LUCINA_OFFICE_PKG_SHA256={}", sum.trim());
}
