//! `scripts/check-fast` runs as one job of the build queue's `long` class, unless it is already inside one; see
//! `scripts/tests/check-fast-queue-test.sh`, which runs the real launcher over a stub `bq`. Called here so `cargo test` covers it.

use std::path::Path;
use std::process::Command;

#[test]
fn check_fast_queues_once_as_a_long_job_and_never_waits_on_itself() {
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../scripts/tests/check-fast-queue-test.sh");
    let o = Command::new("bash")
        .arg(&script)
        .env("CHECK_FAST_QUEUE_TEST_TMP", env!("CARGO_TARGET_TMPDIR"))
        .output()
        .unwrap_or_else(|e| panic!("running {} failed: {e}", script.display()));
    let msg = format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(o.status.success(), "exit {:?}:\n{msg}", o.status.code());
    assert!(msg.contains("check-fast-queue-test: pass"), "{msg}");
}
