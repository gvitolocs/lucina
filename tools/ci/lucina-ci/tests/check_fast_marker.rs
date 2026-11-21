//! The cache-marker step of `scripts/check-fast`, run alone on a fake HOME by `scripts/tests/check-fast-marker-test.sh` and called
//! here so that `cargo test` runs it, as `check_fast_oos.rs` does for the out-of-space pass.

use std::path::Path;
use std::process::Command;

#[test]
fn the_launcher_writes_its_own_marker_file_and_leaves_cargos_tag_alone() {
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../scripts/tests/check-fast-marker-test.sh");
    let o = Command::new("bash")
        .arg(&script)
        .env("CHECK_FAST_MARKER_TEST_TMP", env!("CARGO_TARGET_TMPDIR"))
        .output()
        .unwrap_or_else(|e| panic!("running {} failed: {e}", script.display()));
    let msg = format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(
        o.status.success(),
        "{} did not pass:\n{msg}",
        script.display()
    );
    assert!(msg.contains("check-fast-marker-test: pass"), "{msg}");
}
