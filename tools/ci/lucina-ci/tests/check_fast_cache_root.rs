//! A cache root that cannot be created warns and does not stop `scripts/check-fast`: the bash test
//! `scripts/tests/check-fast-cache-root-test.sh` runs the real launcher over a stub, and is called here so `cargo test` covers it.

use std::path::Path;
use std::process::Command;

#[test]
fn an_uncreatable_cache_root_warns_and_does_not_stop_the_launcher() {
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../scripts/tests/check-fast-cache-root-test.sh");
    let o = Command::new("bash")
        .arg(&script)
        .env(
            "CHECK_FAST_CACHE_ROOT_TEST_TMP",
            env!("CARGO_TARGET_TMPDIR"),
        )
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
    assert!(msg.contains("check-fast-cache-root-test: pass"), "{msg}");
}
