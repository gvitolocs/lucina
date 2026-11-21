//! scripts/lib/in-lane-worker.sh and the eight proof entry points that source it: the bash test
//! `scripts/tests/in-lane-worker-test.sh` runs the helper over a fake /proc, stub `lane` programs and entry points started
//! with a worker marker, and never reaches Verus or TLC. It lives in `scripts/tests/`, where the tests of the scripts live,
//! and is called here so that `cargo test` runs it, as `check_fast_oos.rs` runs `check-fast-oos-test.sh`.

use std::path::Path;
use std::process::Command;

#[test]
fn proof_entry_points_refuse_inside_a_lane_worker() {
    let script =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../scripts/tests/in-lane-worker-test.sh");
    let o = Command::new("bash")
        .arg(&script)
        .env("IN_LANE_WORKER_TEST_TMP", env!("CARGO_TARGET_TMPDIR"))
        .output()
        .unwrap_or_else(|e| panic!("running {} failed: {e}", script.display()));
    let msg = format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(
        o.status.success(),
        "{} did not pass, exit {:?}:\n{msg}",
        script.display(),
        o.status.code()
    );
    assert!(
        msg.contains("in-lane-worker-test: pass"),
        "the last line says it ran to the end: {msg}"
    );
}
