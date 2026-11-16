fn main() {
    println!("cargo:rerun-if-env-changed=LUCINA_BUILD_ID");
}
