//! docs/spec/fs-durability.md: the replace protocol and sync(2), with their witnesses.

use lucina_models::fs::{Kernel, Replace, Step, SyncAll};
use lucina_models::{check, explain};

const LOST: &str = "a cut never loses the file";
const DONE: &str = "after the folder's fsync the new content survives a cut";

fn run(m: Replace) -> Vec<&'static str> {
    let out = check(m.clone());
    println!(
        "{m:?}: {} states, {} unique, found {:?}",
        out.states, out.unique, out.discovered
    );
    out.discovered
}

fn fails(m: Replace, property: &'static str) {
    let found = run(m.clone());
    assert!(
        found.contains(&property),
        "witness {m:?}: `{property}` held, the model cannot see this bug"
    );
    println!(
        "counterexample for `{property}`:\n{}",
        explain(m, property).unwrap_or_default()
    );
}

#[test]
fn safe_replace_holds() {
    let m = Replace::safe();
    let found = run(m.clone());
    for p in [LOST, DONE] {
        assert!(
            !found.contains(&p),
            "`{p}` fails:\n{}",
            explain(m.clone(), p).unwrap_or_default()
        );
    }
    for p in [
        "the program finishes",
        "a cut can leave the old content after the rename",
    ] {
        assert!(found.contains(&p), "never reached `{p}`");
    }
}

/// Without the file's fsync before the rename, writeback can put the new name on the disk before the data: the classic
/// zero-length file after a crash.
#[test]
fn witness_rename_before_fsync() {
    fails(
        Replace {
            program: vec![
                Step::Create,
                Step::Write,
                Step::Rename,
                Step::FsyncDir,
                Step::Done,
            ],
            ..Replace::safe()
        },
        LOST,
    );
}

/// Without the folder's fsync, the program reports success and a cut brings the old file back (LucinaNet's store,
/// 2026-11-04; memory juno-ext4-dir-fsync).
#[test]
fn witness_no_folder_fsync() {
    fails(
        Replace {
            program: vec![
                Step::Create,
                Step::Write,
                Step::FsyncFile,
                Step::Rename,
                Step::Done,
            ],
            ..Replace::safe()
        },
        DONE,
    );
}

/// An fsync that does not flush the device's cache promises nothing: the cache may write the name before the data.
#[test]
fn witness_fsync_without_device_flush() {
    fails(
        Replace {
            kernel: Kernel {
                flush_on_fsync: false,
                fsync_writes_new_name: true,
            },
            ..Replace::safe()
        },
        LOST,
    );
}

#[test]
fn sync_all_holds() {
    let out = check(SyncAll {
        mounts: 3,
        stop_at_first_error: false,
    });
    assert!(
        !out.discovered
            .contains(&"sync(2) leaves every healthy file system on the disk")
    );
    assert!(out.discovered.contains(&"sync(2) returns"));
}

/// Before 1116: sync(2) stopped at the first failing file system, and the ESP behind it was never written (2026-11-08).
#[test]
fn witness_sync_stops_at_first_error() {
    let out = check(SyncAll {
        mounts: 3,
        stop_at_first_error: true,
    });
    assert!(
        out.discovered
            .contains(&"sync(2) leaves every healthy file system on the disk")
    );
}
