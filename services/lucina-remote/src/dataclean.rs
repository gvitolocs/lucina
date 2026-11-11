//! Marks /data clean before an orderly restart, with a time limit, for the callers that restart the machine themselves
//! (`lucina-reload`, `lucina-trial`, `lucina-recover`). /data has no journal: without this, every restart that skips the shell's
//! power menu costs the next boot a full e2fsck. The mark is `lucina-data clean`'s last write, after the data is on the disk; if
//! anything before it fails, it is not set and the next boot checks (docs/research/juno-disk-root.md).

use std::{
    path::Path,
    process::{Command, Stdio},
    thread::sleep,
    time::{Duration, Instant},
};

pub const DATA_READY: &str = "/run/lucina/data-ready";
pub const LUCINA_DATA: &str = "/sbin/lucina-data";
/// How long a restart waits for /data. `lucina-data clean --cap` gives up by itself after the same time.
pub const CAP: Duration = Duration::from_secs(5);

/// The one decision that sets the clean mark: the file system's own flush (`syncfs`) reported no error, /data is unmounted or
/// mounted read-only (nothing can write to it any more), and the raw device flush reported no error. Any "no" means the mark is
/// not set and the next boot checks. `sync(2)` is no basis for it: it ignores errors by design.
pub fn may_mark_clean(
    syncfs_ok: bool,
    unmounted_or_read_only: bool,
    device_flush_ok: bool,
) -> bool {
    syncfs_ok && unmounted_or_read_only && device_flush_ok
}

/// Whether `/proc/mounts` lists `target` read-only. `None` if it is not listed.
pub fn mount_is_read_only(mounts: &str, target: &str) -> Option<bool> {
    mounts
        .lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            let (_dev, dir, _fs, opts) = (f.next()?, f.next()?, f.next()?, f.next()?);
            (dir == target).then(|| opts.split(',').any(|o| o == "ro"))
        })
        .next_back()
}

/// What happened, in words for a log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// This boot has no /data.
    NoData,
    Clean,
    /// Not marked: the next boot checks it.
    NotClean(String),
}

impl Outcome {
    pub fn describe(&self) -> String {
        match self {
            Outcome::NoData => "no /data on this boot".to_string(),
            Outcome::Clean => "/data marked clean".to_string(),
            Outcome::NotClean(why) => {
                format!("/data not marked clean ({why}); the next boot checks it")
            }
        }
    }
}

/// Runs `program args` and waits at most `cap`; kills it after that. `Ok(())` if it exited 0.
pub fn run_capped(program: &str, args: &[&str], cap: Duration) -> Result<(), String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("{program}: {e}"))?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => return Err(format!("{program} {status}")),
            Ok(None) if started.elapsed() >= cap => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{program} took longer than {} s", cap.as_secs()));
            }
            Ok(None) => sleep(Duration::from_millis(25)),
            Err(e) => return Err(format!("{program}: {e}")),
        }
    }
}

/// After a restart that was meant to follow `clean_before_restart` and did not happen: `lucina-data reopen` writes state 0
/// ("not clean") first, then mounts or remounts /data read-write, so the machine that runs on is as after a normal mount.
pub fn reopen_after_failed_restart() -> Outcome {
    if !Path::new(DATA_READY).exists() {
        return Outcome::NoData;
    }
    match run_capped(LUCINA_DATA, &["reopen"], CAP) {
        Ok(()) => Outcome::Clean,
        Err(e) => Outcome::NotClean(format!("reopen: {e}")),
    }
}

/// `lucina-data clean`, if this boot has /data, capped. Never blocks the restart for long, and never fails it.
pub fn clean_before_restart() -> Outcome {
    if !Path::new(DATA_READY).exists() {
        return Outcome::NoData;
    }
    // The cap is also given to the program, which then gives up by itself and says why on its stderr.
    let secs = CAP.as_secs().to_string();
    match run_capped(
        LUCINA_DATA,
        &["clean", "--cap", &secs],
        CAP + Duration::from_secs(1),
    ) {
        Ok(()) => Outcome::Clean,
        Err(e) => Outcome::NotClean(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_program_that_exits_zero_is_ok() {
        assert_eq!(
            run_capped("/bin/sh", &["-c", "exit 0"], Duration::from_secs(5)),
            Ok(())
        );
    }

    #[test]
    fn a_failing_program_is_an_error() {
        assert!(run_capped("/bin/sh", &["-c", "exit 3"], Duration::from_secs(5)).is_err());
    }

    #[test]
    fn a_program_that_hangs_is_killed_at_the_cap() {
        let started = Instant::now();
        let e = run_capped("/bin/sh", &["-c", "sleep 30"], Duration::from_millis(200)).unwrap_err();
        assert!(e.contains("took longer"), "{e}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_missing_program_is_an_error() {
        assert!(
            run_capped(
                "/nonexistent/lucina-data",
                &["clean"],
                Duration::from_secs(1)
            )
            .is_err()
        );
    }

    #[test]
    fn a_failed_syncfs_or_flush_or_a_busy_writable_mount_leaves_the_mark_unset() {
        assert!(may_mark_clean(true, true, true));
        assert!(!may_mark_clean(false, true, true), "syncfs failed");
        assert!(
            !may_mark_clean(true, false, true),
            "still mounted read-write"
        );
        assert!(
            !may_mark_clean(true, true, false),
            "the device flush failed"
        );
        assert!(!may_mark_clean(false, false, false));
    }

    #[test]
    fn the_mount_table_says_read_only_or_not() {
        let t =
            "/dev/ufs0lun0p2 / ext2 ro,noatime 0 0\n/dev/ufs0lun0p4 /data ext2 rw,noatime 0 0\n";
        assert_eq!(mount_is_read_only(t, "/data"), Some(false));
        assert_eq!(mount_is_read_only(t, "/"), Some(true));
        assert_eq!(mount_is_read_only(t, "/mnt"), None);
        let ro = "/dev/ufs0lun0p4 /data ext2 rw,noatime 0 0\n/dev/ufs0lun0p4 /data ext2 ro,noatime 0 0\n";
        assert_eq!(mount_is_read_only(ro, "/data"), Some(true));
        // "errors=remount-ro" is not "ro".
        assert_eq!(
            mount_is_read_only("d /data ext2 rw,errors=remount-ro 0 0", "/data"),
            Some(false)
        );
    }

    #[test]
    fn the_words_say_what_the_next_boot_does() {
        assert!(
            Outcome::NotClean("busy".into())
                .describe()
                .contains("the next boot checks it")
        );
        assert_eq!(Outcome::Clean.describe(), "/data marked clean");
    }
}
