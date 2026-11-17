//! The third pass of `scripts/sweep-caches.sh`, the `debug/` of a MAIN checkout's target: the bash test that runs the real script over
//! a fixture HOME with a projects root, a unit folder and a bin folder of its own. It checks the promise the pass exists for — that one
//! directory of one checkout goes, for its age and with four guards on it, and that nothing else of that target is touched — with no
//! real checkout named and no build of any kind. The test lives in `scripts/tests/`, where the tests of the scripts live, and is called
//! here so that `cargo test` runs it, as `sweep_caches.rs` runs the script itself.

use std::path::Path;
use std::process::Command;

#[test]
fn only_the_debug_of_a_main_checkout_target_goes_and_only_when_it_may() {
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../scripts/tests/sweep-main-debug-test.sh");
    let o = Command::new("bash")
        .arg(&script)
        // Its few KiB of fixtures next to the other target temp files, not in the /tmp every session shares.
        .env("SWEEP_MAIN_DEBUG_TEST_TMP", env!("CARGO_TARGET_TMPDIR"))
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
        msg.contains("sweep-main-debug-test: pass"),
        "the last line says it ran to the end: {msg}"
    );
}
