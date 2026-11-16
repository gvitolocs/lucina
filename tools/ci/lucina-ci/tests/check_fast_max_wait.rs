//! `scripts/check-fast` passes `--max-wait 900` to a queue whose usage offers it (see `scripts/tests/check-fast-max-wait-test.sh`,
//! which runs the real launcher over a stub `bq`); called here so `cargo test` covers it, as `check_fast_oos.rs` does.

use std::path::Path;
use std::process::Command;

#[test]
fn check_fast_asks_the_queue_for_a_bounded_wait() {
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../scripts/tests/check-fast-max-wait-test.sh");
    let o = Command::new("bash")
        .arg(&script)
        .env("CHECK_FAST_MAXWAIT_TEST_TMP", env!("CARGO_TARGET_TMPDIR"))
        .output()
        .unwrap_or_else(|e| panic!("running {} failed: {e}", script.display()));
    let msg = format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(o.status.success(), "exit {:?}:\n{msg}", o.status.code());
    assert!(msg.contains("check-fast-max-wait-test: pass"), "{msg}");
}
