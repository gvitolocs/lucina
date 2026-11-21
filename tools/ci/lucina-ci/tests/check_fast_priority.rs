//! `scripts/check-fast` queues itself as one `long` job with `--priority gate` when it runs as a merge gate and `--priority normal`
//! otherwise, byte for byte, and a queue that cannot take that call makes the check fail with exit 2; see
//! `scripts/tests/check-fast-priority-test.sh`, which runs a copy of the launcher (its passwd home replaced by a temp one holding a
//! stub `bq`). Called here so `cargo test` covers it, as `check_fast_max_wait.rs` does.

use std::path::Path;
use std::process::Command;

#[test]
fn check_fast_asks_the_queue_for_the_gate_priority() {
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../scripts/tests/check-fast-priority-test.sh");
    let o = Command::new("bash")
        .arg(&script)
        .env("CHECK_FAST_PRIORITY_TEST_TMP", env!("CARGO_TARGET_TMPDIR"))
        .output()
        .unwrap_or_else(|e| panic!("running {} failed: {e}", script.display()));
    let msg = format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(o.status.success(), "exit {:?}:\n{msg}", o.status.code());
    assert!(msg.contains("check-fast-priority-test: pass"), "{msg}");
}
