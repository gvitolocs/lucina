//! lucina-data: the persistent /data partition. `/etc/lucina/init.sh` starts it in the background, so nothing waits for it but
//! what reads /data: the shell's first frame does not.
//!
//! ```text
//! lucina-data mount    # checks if needed, mounts /data, then stays and syncs every few seconds
//! lucina-data clean [--cap SECONDS]   # flush (syncfs), unmount or remount read-only, flush the device, then mark clean: for an
//!                                     # orderly shutdown or restart; with --cap it gives up after that long
//! lucina-data reopen   # after a clean whose restart did not happen: state 0, /data read-write again
//! lucina-data status
//! ```
//!
//! ext2 has no journal. What a power cut can do, and what this does about it (docs/research/juno-disk-root.md):
//! - The superblock's state says "not clean" on the medium before the first mount that may write, and only `clean` sets it back.
//!   A not-clean file system goes through e2fsck before it is mounted. A cut therefore costs a check at the next boot, and what
//!   e2fsck cannot save goes to lost+found.
//! - This kernel writes back only on sync and fsync, so the loop below is what bounds how much a cut loses: [`SYNC_EVERY`].
//!   Programs that must not lose a file use write, fsync, rename, fsync of the folder.
//!
//! `clean` is a chain, and the mark is its last write: the loop below is parked; the seed is saved; `syncfs(2)` on /data (which
//! reports errors, unlike `sync(2)`); `umount`, or if /data is busy a remount read-only, checked in /proc/mounts; the raw device is
//! flushed and the result checked; only then is the state set to VALID, and the device flushed again. A failure anywhere leaves
//! the state "not clean" (and one at the last flush puts it back), so the next boot checks. After an unclean end the check's
//! findings go to the event log and what e2fsck moved or cut is copied to /data/lucina/repaired (`lucina_remote::repair`).
//!
//! The kernel's random seed lives here too: [`lucina_remote::seed`]. It costs no time before the shell's first frame, since
//! /data only comes up after it.
//!
//! `/run/lucina/data-ready` appears when /data can be used, and `/run/lucina/data-failed` holds the reason if it cannot.
//!
//! On a test boot with the stream's own user space (`lucina,reload-data` in `/chosen`, `lucina_remote::testroot`): `none` leaves
//! /data unmounted (data-failed says so); `ro` mounts it read-only with no check, no state write, no seed and no sync loop; `rw`
//! checks and mounts it as usual and writes a test-boot event, but `clean` never writes the clean mark: the state stays 0 (mounted,
//! not clean), so the next boot of the installed generation checks it. `reopen` is refused unless the mode is `rw`.

// Juno with no C library: Eyra is the libc and the program start, in Rust (docs/research/juno-pure-rust.md).
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
extern crate eyra;

use std::{
    ffi::CString,
    fs::{self, File, OpenOptions},
    os::unix::{
        fs::{DirBuilderExt, FileExt, PermissionsExt},
        io::AsRawFd,
    },
    path::Path,
    process::{Command, ExitCode, Stdio},
    sync::mpsc,
    thread::sleep,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use lucina_evlog::{
    event::{Channel, Event, Level},
    providers,
    store::{Retention, Store},
};
use lucina_remote::{
    dataclean::{may_mark_clean, mount_is_read_only},
    disk::{self, LBA, errno, partition_named, read_blocks, sync_all},
    fsck, gptw,
    image::{self, STATE_ERROR, STATE_VALID, needs_check},
    repair,
    seed::{self, Loaded, SeedFile},
    testroot::{self, Data, TestBoot},
};

const DATA: &str = "/data";
const RUN: &str = "/run/lucina";
const FSCK: &str = "/sbin/e2fsck";
const SYNC_EVERY: Duration = Duration::from_secs(10);
const DEVICE_WAIT: Duration = Duration::from_secs(20);
const SEED_FILE: &str = "/data/lucina/random-seed";
const EVENTS: &str = "/data/lucina/events";
/// `clean` creates this to stop the loop in `run_mount`; the loop answers with [`PARKED`] and waits while this exists.
const CLEANING: &str = "/run/lucina/data-cleaning";
const PARKED: &str = "/run/lucina/data-loop-parked";
const PARK_WAIT: Duration = Duration::from_secs(3);
/// The folders everything else expects, with their modes. `lucina` holds secrets (Wi-Fi, the account's token).
const FOLDERS: [(&str, u32); 4] = [
    ("lucina", 0o700),
    ("home", 0o755),
    ("cache", 0o755),
    ("files", 0o755),
];

fn device() -> Result<String, String> {
    let started = Instant::now();
    loop {
        match partition_named(gptw::DATA) {
            Ok(Some((device, _))) if Path::new(&device).exists() => return Ok(device),
            Ok(Some(_)) | Err(_) if started.elapsed() < DEVICE_WAIT => {
                sleep(Duration::from_millis(100));
            }
            Ok(None) if started.elapsed() < DEVICE_WAIT => sleep(Duration::from_millis(100)),
            Ok(Some((device, _))) => return Err(format!("{device} is not there")),
            Ok(None) => return Err(format!("LUN 0 has no partition named {}", gptw::DATA)),
            Err(e) => return Err(e),
        }
    }
}

fn set_state(device: &str, state: u16) -> Result<(), String> {
    let block = read_blocks(device, 0, 1)?;
    let f = OpenOptions::new()
        .write(true)
        .open(device)
        .map_err(|e| format!("{device}: {e}"))?;
    f.write_all_at(&image::with_state(&block, state), 0)
        .map_err(|e| format!("writing {device}: {e}"))?;
    f.sync_all().map_err(|e| format!("flushing {device}: {e}"))
}

fn mount(device: &str) -> Result<(), String> {
    let (dev, dir, fstype) = (
        CString::new(device).map_err(|e| e.to_string())?,
        CString::new(DATA).map_err(|e| e.to_string())?,
        c"ext2",
    );
    // SAFETY: The strings outlive the call; no data.
    if unsafe {
        libc::mount(
            dev.as_ptr(),
            dir.as_ptr(),
            fstype.as_ptr(),
            libc::MS_NOATIME,
            core::ptr::null(),
        )
    } != 0
    {
        return Err(errno(&format!("mount {device} on {DATA}")));
    }
    Ok(())
}

/// /data read-only for a test boot: nothing is written to the device, not even the state.
fn mount_read_only(device: &str) -> Result<(), String> {
    let (dev, dir) = (
        CString::new(device).map_err(|e| e.to_string())?,
        CString::new(DATA).map_err(|e| e.to_string())?,
    );
    // SAFETY: The strings outlive the call; no data.
    if unsafe {
        libc::mount(
            dev.as_ptr(),
            dir.as_ptr(),
            c"ext2".as_ptr(),
            libc::MS_RDONLY | libc::MS_NOATIME,
            core::ptr::null(),
        )
    } != 0
    {
        return Err(errno(&format!("mount {device} on {DATA} read-only")));
    }
    Ok(())
}

/// `mount` on a test boot whose /data mode is not `rw`.
fn test_mount(test: &TestBoot) -> Result<(), String> {
    fs::create_dir_all(RUN).map_err(|e| format!("{RUN}: {e}"))?;
    match test.data {
        Data::None => {
            let why = "a test boot with the stream's own user space: /data is not mounted (lucina,reload-data none)";
            say_in_kernel_log(why);
            fs::write(format!("{RUN}/data-failed"), format!("{why}\n"))
                .map_err(|e| format!("{RUN}/data-failed: {e}"))
        }
        Data::ReadOnly => {
            let result = device().and_then(|d| mount_read_only(&d));
            match result {
                Ok(()) => {
                    say_in_kernel_log(
                        "a test boot: /data mounted read-only, not checked, no sync loop",
                    );
                    fs::write(format!("{RUN}/data-ready"), "")
                        .map_err(|e| format!("{RUN}/data-ready: {e}"))
                }
                Err(e) => {
                    let _ = fs::write(format!("{RUN}/data-failed"), format!("{e}\n"));
                    Err(e)
                }
            }
        }
        Data::ReadWrite => run_mount(),
    }
}

/// With /data read-write on a test boot: one event, with the marker line, so that the event log says which boot this was.
fn test_boot_event(test: &TestBoot) {
    let line = fs::read_to_string(testroot::MARKER_FILE).unwrap_or_default();
    let line = line.trim();
    let now = unix_seconds();
    let mut event = Event::new(
        providers::SETUP,
        providers::id::TEST_BOOT,
        Level::Warning,
        Channel::Setup,
        if line.is_empty() {
            format!(
                "A test boot with the stream's own user space (mode {}, data rw)",
                test.root
            )
        } else {
            format!("A test boot with the stream's own user space: {line}")
        },
    )
    .field("mode", test.root.to_string())
    .field("data", test.data.name().to_string());
    event.next_step = Some("Nothing: any later boot is the installed generation again; /data is checked at the next one.".into());
    event.time_ms = Some(now as i64 * 1000);
    let written = Store::open(Path::new(EVENTS), Retention::default())
        .and_then(|mut s| s.append(event, now as i64 * 1000));
    say_in_kernel_log(match written {
        Ok(_) => "a test boot: the event is in the event log",
        Err(_) => "a test boot: the event could not be written",
    });
}

/// What a check found, for after /data is up.
struct Checked {
    report: fsck::Report,
    ms: u128,
    /// Why the check ran: the state the superblock had.
    why: String,
}

/// Runs e2fsck, answering yes to everything: nobody is there to ask, and a /data that mounts is the point.
fn check(device: &str, state: u16, log: &mut String) -> Result<Checked, String> {
    let started = Instant::now();
    let out = Command::new(FSCK)
        .args(["-f", "-y", device])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("{FSCK}: {e}"))?;
    let code = out.status.code().unwrap_or(-1);
    let all = [out.stdout.as_slice(), out.stderr.as_slice()].concat();
    let _ = fs::write(format!("{RUN}/e2fsck.log"), &all);
    let ms = started.elapsed().as_millis();
    log.push_str(&format!("e2fsck: exit {code} in {ms} ms\n"));
    let report = fsck::parse(&String::from_utf8_lossy(&all), code);
    let summary = report.summary(ms);
    log.push_str(&format!("e2fsck summary: {summary}\n"));
    let _ = fs::write(format!("{RUN}/e2fsck.summary"), format!("{summary}\n"));
    say_in_kernel_log(&format!("/data checked: {summary}"));
    // 0 clean, 1 and 2 repaired, 4 and above: errors remain.
    if !(0..=2).contains(&code) {
        return Err(format!("e2fsck exit {code}, see {RUN}/e2fsck.log"));
    }
    let why = if state & STATE_ERROR != 0 {
        format!("the file system's error flag was set (state {state})")
    } else if state == 0 {
        "the last boot did not end with lucina-data clean (state 0: mounted, not marked clean)"
            .to_string()
    } else {
        format!("the superblock state was {state}, not clean")
    };
    Ok(Checked { report, ms, why })
}

fn bring_up(log: &mut String) -> Result<Option<Checked>, String> {
    let device = device()?;
    let t0 = Instant::now();
    let info =
        disk::ext2_on(&device)?.ok_or_else(|| format!("{device} holds no ext2 file system"))?;
    let mut checked = None;
    if needs_check(info.state) {
        log.push_str(&format!(
            "not clean: checking (superblock state {})\n",
            info.state
        ));
        checked = Some(check(&device, info.state, log)?);
    }
    set_state(&device, 0)?;
    let t1 = Instant::now();
    mount(&device)?;
    log.push_str(&format!(
        "{device}: check {} ms, mount {} ms\n",
        (t1 - t0).as_millis(),
        t1.elapsed().as_millis()
    ));
    for (name, mode) in FOLDERS {
        let path = format!("{DATA}/{name}");
        if !Path::new(&path).exists() {
            fs::DirBuilder::new()
                .mode(mode)
                .create(&path)
                .map_err(|e| format!("{path}: {e}"))?;
        }
        fs::set_permissions(&path, fs::Permissions::from_mode(mode))
            .map_err(|e| format!("{path}: {e}"))?;
    }
    sync_all();
    Ok(checked)
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// After an unclean end, once /data is up: copy what e2fsck moved or cut, and write the event. Never fails the boot.
fn after_check(c: &Checked, log: &mut String) {
    let now = unix_seconds();
    let outcome = if c.report.exit > 0 || c.report.touched_files() {
        match repair::copy_repairs(
            Path::new(DATA),
            &c.report,
            &repair::timestamp(now),
            repair::CAP_BYTES,
        ) {
            Ok(o) => o,
            Err(e) => {
                log.push_str(&format!("repaired copies: failed: {e}\n"));
                repair::Outcome::default()
            }
        }
    } else {
        repair::Outcome::default()
    };
    log.push_str(&format!("repaired copies: {}\n", outcome.describe()));
    for s in &outcome.skipped {
        log.push_str(&format!("repaired copies: skipped {s}\n"));
    }
    let mut summary = format!(
        "/data was checked at boot because {}: {}",
        c.why,
        c.report.summary(c.ms)
    );
    if let Some(dir) = &outcome.dir {
        summary.push_str(&format!(
            ". Copies of what e2fsck moved or cut: {} (MANIFEST lists them)",
            dir.display()
        ));
    }
    let level = match c.report.exit {
        0 => Level::Info,
        1 | 2 => Level::Warning,
        _ => Level::Error,
    };
    let k = &c.report.counts;
    let mut event = Event::new(
        providers::STORAGE,
        providers::id::DATA_CHECKED,
        level,
        Channel::System,
        summary,
    )
    .field("e2fsck_exit", c.report.exit)
    .field("check_ms", c.ms as u64)
    .field("reason", c.why.clone())
    .field("fixed", k.total())
    .field("inodes", k.inodes)
    .field("blocks", k.blocks)
    .field("directory_entries", k.entries)
    .field("lost_found", k.lost_found)
    .field("bitmaps_and_counts", k.bitmaps)
    .field("other", k.other)
    .field("copied", outcome.copied as u64)
    .field("skipped", outcome.skipped.len() as u64);
    if let Some(dir) = &outcome.dir {
        event = event.field("repaired_dir", dir.display().to_string());
        event.next_step = Some(format!(
            "Look at {}/MANIFEST: each line names a file e2fsck moved or cut, with the copy.",
            dir.display()
        ));
    }
    event.time_ms = Some(now as i64 * 1000);
    match Store::open(Path::new(EVENTS), Retention::default())
        .and_then(|mut s| s.append(event, now as i64 * 1000))
    {
        Ok(_) => log.push_str("event log: written\n"),
        Err(e) => log.push_str(&format!("event log: not written: {e}\n")),
    }
}

/// Stops while `clean` works: it has asked by creating [`CLEANING`]. Answers with [`PARKED`], and goes on if the request is withdrawn.
fn park_if_asked() {
    if !Path::new(CLEANING).exists() {
        return;
    }
    let _ = fs::write(PARKED, "");
    while Path::new(CLEANING).exists() {
        sleep(Duration::from_millis(100));
    }
    let _ = fs::remove_file(PARKED);
}

fn run_mount() -> Result<(), String> {
    fs::create_dir_all(RUN).map_err(|e| format!("{RUN}: {e}"))?;
    let mut log = String::new();
    let result = bring_up(&mut log);
    let _ = fs::write(format!("{RUN}/data.log"), &log);
    let checked = match result {
        Ok(c) => c,
        Err(e) => {
            let _ = fs::write(format!("{RUN}/data-failed"), format!("{e}\n"));
            return Err(e);
        }
    };
    fs::write(format!("{RUN}/data-ready"), "").map_err(|e| format!("{RUN}/data-ready: {e}"))?;
    if let Some(test) = testroot::test_boot() {
        test_boot_event(&test);
    }
    // After "ready", so that what reads /data does not wait for the copies.
    if let Some(c) = &checked {
        after_check(c, &mut log);
        let _ = fs::write(format!("{RUN}/data.log"), &log);
    }
    let seed = SeedFile::new(SEED_FILE, "/dev/urandom");
    say_in_kernel_log(match seed.load() {
        Loaded::Mixed => "random seed on /data: mixed in, and replaced",
        Loaded::Missing => "random seed on /data: none yet",
        Loaded::Damaged => "random seed on /data: damaged, not used",
        Loaded::NotReplaced => "random seed on /data: could not be replaced, so not used",
    });
    // The first save comes as soon as the kernel is seeded, which on a machine with a hardware source is at once.
    let mut next_save = Instant::now();
    loop {
        park_if_asked();
        if Instant::now() >= next_save {
            match seed.save() {
                Ok(true) => next_save = Instant::now() + seed::SAVE_EVERY,
                Ok(false) => {}
                Err(e) => {
                    say_in_kernel_log(&format!("random seed on /data: not saved: {e}"));
                    next_save = Instant::now() + seed::SAVE_EVERY;
                }
            }
        }
        let slices = SYNC_EVERY.as_millis() / 100;
        for _ in 0..slices {
            sleep(Duration::from_millis(100));
            if Path::new(CLEANING).exists() {
                break;
            }
        }
        park_if_asked();
        sync_all();
    }
}

fn say_in_kernel_log(line: &str) {
    let _ = OpenOptions::new()
        .write(true)
        .open("/dev/kmsg")
        .and_then(|mut kmsg| {
            use std::io::Write;
            kmsg.write_all(format!("lucina-data: {line}\n").as_bytes())
        });
}

/// `syncfs(2)` on the file system holding `dir`: unlike `sync(2)` it reports a write or flush that failed. Called as a system call
/// because the C library of this system (Eyra) need not have the wrapper.
fn syncfs_at(dir: &str) -> Result<(), String> {
    let f = File::open(dir).map_err(|e| format!("open {dir}: {e}"))?;
    // SAFETY: syncfs takes one open descriptor, which `f` keeps open through the call.
    if unsafe { libc::syscall(libc::SYS_syncfs, f.as_raw_fd() as libc::c_long) } != 0 {
        return Err(errno(&format!("syncfs {dir}")));
    }
    Ok(())
}

/// Flushes the raw device and checks the result: a real flush, not `sync(2)`.
fn flush_device(device: &str) -> Result<(), String> {
    let f = OpenOptions::new()
        .write(true)
        .open(device)
        .map_err(|e| format!("{device}: {e}"))?;
    f.sync_all().map_err(|e| format!("flushing {device}: {e}"))
}

fn mounted_read_only() -> Option<bool> {
    mount_is_read_only(
        &fs::read_to_string("/proc/mounts").unwrap_or_default(),
        DATA,
    )
}

/// Takes /data out of use: unmounted, or if something holds it open, mounted read-only. `Ok(true)` if it was unmounted,
/// `Ok(false)` if read-only; an error if neither happened.
fn release_data(device: &str) -> Result<bool, String> {
    let dir = CString::new(DATA).map_err(|e| e.to_string())?;
    // SAFETY: The string outlives the call.
    if unsafe { libc::umount(dir.as_ptr()) } == 0 {
        return Ok(true);
    }
    let busy = std::io::Error::last_os_error();
    let dev = CString::new(device).map_err(|e| e.to_string())?;
    // SAFETY: The strings outlive the call; no data.
    let r = unsafe {
        libc::mount(
            dev.as_ptr(),
            dir.as_ptr(),
            core::ptr::null(),
            libc::MS_REMOUNT | libc::MS_RDONLY,
            core::ptr::null(),
        )
    };
    if r != 0 {
        return Err(format!(
            "umount {DATA}: {busy}; and remounting it read-only: {}",
            std::io::Error::last_os_error()
        ));
    }
    match mounted_read_only() {
        Some(true) => Ok(false),
        other => Err(format!(
            "umount {DATA}: {busy}; remount read-only returned 0 but /proc/mounts says {other:?}"
        )),
    }
}

fn clean_chain(device: &str) -> Result<(), String> {
    // What the kernel knows at the end of this boot, for the next one. A failed write is an I/O error only if it says so.
    if let Err(e) = SeedFile::new(SEED_FILE, "/dev/urandom").save() {
        eprintln!("random seed on /data: not saved: {e}");
        if e.raw_os_error() == Some(libc::EIO) {
            return Err(format!("{e}: not marked clean"));
        }
    }
    let mut syncfs_ok = true;
    let mut why = Vec::new();
    if let Err(e) = syncfs_at(DATA) {
        syncfs_ok = false;
        why.push(e);
    }
    let mut unmounted_or_ro = false;
    if syncfs_ok {
        match release_data(device) {
            Ok(unmounted) => {
                unmounted_or_ro = true;
                // Read-only but mounted: what the remount itself wrote goes out too, and an error there counts.
                if !unmounted && let Err(e) = syncfs_at(DATA) {
                    syncfs_ok = false;
                    why.push(e);
                }
            }
            Err(e) => why.push(e),
        }
    }
    // The last flush before the mark: after nothing can write to /data any more.
    let mut flush_ok = false;
    if syncfs_ok && unmounted_or_ro {
        match flush_device(device) {
            Ok(()) => flush_ok = true,
            Err(e) => why.push(e),
        }
    }
    if !may_mark_clean(syncfs_ok, unmounted_or_ro, flush_ok) {
        return Err(format!(
            "{}: not marked clean, the next boot checks it",
            why.join("; ")
        ));
    }
    // The mark is the last write.
    if let Err(e) = set_state(device, STATE_VALID) {
        let _ = set_state(device, 0);
        return Err(format!("{e}: not marked clean"));
    }
    if let Err(e) = flush_device(device) {
        let _ = set_state(device, 0);
        return Err(format!(
            "{e}: the mark was taken back, the next boot checks it"
        ));
    }
    Ok(())
}

fn clean_inner() -> Result<(), String> {
    // A caller's own working directory in /data would hold it busy.
    let _ = std::env::set_current_dir("/");
    if let Some(test) = testroot::test_boot() {
        // Never the clean mark on a test boot: what it wrote is flushed, and the next installed boot checks /data.
        if test.data == Data::ReadWrite && Path::new(&format!("{RUN}/data-ready")).exists() {
            syncfs_at(DATA)?;
        }
        return Err(format!(
            "a test boot (data {}): /data is not marked clean, the next boot of the installed generation checks it",
            test.data.name()
        ));
    }
    let device = device()?;
    if !Path::new(&format!("{RUN}/data-ready")).exists() {
        return Err("this boot did not bring /data up: nothing marked".to_string());
    }
    // Stop the loop first: it must not write while this flushes, and it is the other writer of the seed.
    let _ = fs::write(CLEANING, "");
    let started = Instant::now();
    while !Path::new(PARKED).exists() && started.elapsed() < PARK_WAIT {
        sleep(Duration::from_millis(50));
    }
    let result = clean_chain(&device);
    if result.is_err() {
        // /data stays in use: the loop goes on.
        let _ = fs::remove_file(CLEANING);
    }
    result
}

/// `clean`, and with `--cap` it gives up after that many seconds so that a restart is never held for long.
fn clean(args: &[String]) -> Result<(), String> {
    let cap = match args {
        [] => None,
        [flag, secs] if flag == "--cap" => Some(
            secs.parse::<u64>()
                .map_err(|_| format!("--cap {secs:?} is not a number of seconds"))?,
        ),
        _ => return Err("usage: lucina-data clean [--cap SECONDS]".to_string()),
    };
    let Some(cap) = cap else { return clean_inner() };
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(clean_inner());
    });
    match rx.recv_timeout(Duration::from_secs(cap)) {
        Ok(r) => r,
        Err(_) => {
            let _ = fs::remove_file(CLEANING);
            Err(format!(
                "did not finish in {cap} s: not marked clean, the next boot checks it"
            ))
        }
    }
}

/// After a `clean` whose restart did not happen: writes state 0 first (nothing is clean any more once /data takes writes), then
/// mounts /data again if it was unmounted or makes it read-write if it was remounted read-only, and lets the sync loop go on.
fn reopen() -> Result<(), String> {
    let device = device()?;
    set_state(&device, 0)?;
    match mounted_read_only() {
        None => mount(&device)?,
        Some(true) => {
            let (dev, dir) = (
                CString::new(device.as_str()).map_err(|e| e.to_string())?,
                CString::new(DATA).map_err(|e| e.to_string())?,
            );
            // SAFETY: The strings outlive the call; no data.
            if unsafe {
                libc::mount(
                    dev.as_ptr(),
                    dir.as_ptr(),
                    core::ptr::null(),
                    libc::MS_REMOUNT | libc::MS_NOATIME,
                    core::ptr::null(),
                )
            } != 0
            {
                return Err(errno(&format!("remount {DATA} read-write")));
            }
        }
        Some(false) => {}
    }
    let _ = fs::remove_file(CLEANING);
    Ok(())
}

fn status() -> Result<(), String> {
    let device = device()?;
    let info =
        disk::ext2_on(&device)?.ok_or_else(|| format!("{device} holds no ext2 file system"))?;
    println!(
        "{device}: label {}, {} MiB, state {}, ready: {}",
        info.label,
        (info.blocks * LBA) >> 20,
        if info.state == STATE_VALID {
            "clean"
        } else {
            "not clean"
        },
        Path::new(&format!("{RUN}/data-ready")).exists()
    );
    if let Ok(summary) = fs::read_to_string(format!("{RUN}/e2fsck.summary")) {
        print!("last check: {summary}");
    }
    if let Ok(log) = fs::read_to_string(format!("{RUN}/data.log")) {
        print!("{log}");
    }
    if let Ok(failed) = fs::read_to_string(format!("{RUN}/data-failed")) {
        print!("failed: {failed}");
    }
    Ok(())
}

fn main() -> ExitCode {
    let arg = std::env::args().nth(1);
    let test = testroot::test_boot();
    let result = match (arg.as_deref(), &test) {
        (Some("mount"), Some(t)) => test_mount(t),
        (Some("mount"), None) => run_mount(),
        (Some("reopen"), Some(t)) if t.data != Data::ReadWrite => Err(format!(
            "a test boot (data {}): /data is not reopened read-write",
            t.data.name()
        )),
        (Some("reopen"), _) => reopen(),
        (Some("clean"), _) => clean(&std::env::args().skip(2).collect::<Vec<_>>()),
        (None | Some("status"), _) => status(),
        (Some(other), _) => Err(format!(
            "unknown command {other:?}: lucina-data [mount|clean [--cap SECONDS]|reopen|status]"
        )),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("lucina-data: {e}");
            ExitCode::FAILURE
        }
    }
}
