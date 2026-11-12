//! The out-of-space pass of `scripts/check-fast`, end to end: the bash test that runs the real launcher over a stub of
//! `lucina-ci check-fast` (CHECK_FAST_BIN) and reads the raw log it kept (CHECK_FAST_LOG). It checks the condition the pass is
//! allowed to exist under — a full disk is ONE failure class, and a failure may be renamed but never turned into a pass, a skip
//! or a warning — with no build, no queue and no disk filled. The test lives in `scripts/tests/`, where the tests of the scripts
//! live, and is called here so that `cargo test` runs it, as `sweep_caches.rs` runs `scripts/sweep-caches.sh`.

use std::path::Path;
use std::process::Command;

#[test]
fn out_of_space_is_one_class_and_never_a_pass() {
    let script =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../scripts/tests/check-fast-oos-test.sh");
    let o = Command::new("bash")
        .arg(&script)
        // Its few KiB of fixtures next to the other target temp files, not in the /tmp every session shares.
        .env("CHECK_FAST_OOS_TEST_TMP", env!("CARGO_TARGET_TMPDIR"))
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
        msg.contains("check-fast-oos-test: pass"),
        "the last line says it ran to the end: {msg}"
    );
}
