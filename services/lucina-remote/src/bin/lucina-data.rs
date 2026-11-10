//! lucina-data: the persistent /data partition. `/etc/lucina/init.sh` starts it in the background, so nothing waits for it but
//! what reads /data: the shell's first frame does not.
//!
//! ```text
//! lucina-data mount    # checks if needed, mounts /data, then stays and syncs every few seconds
//! lucina-data clean    # sync, unmount, mark the file system clean: for an orderly shutdown
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
//! The kernel's random seed lives here too: [`lucina_remote::seed`]. It costs no time before the shell's first frame, since
//! /data only comes up after it.
//!
//! `/run/lucina/data-ready` appears when /data can be used, and `/run/lucina/data-failed` holds the reason if it cannot.

// Juno with no C library: Eyra is the libc and the program start, in Rust (docs/research/juno-pure-rust.md).
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
extern crate eyra;

use std::{
    ffi::CString,
    fs::{self, OpenOptions},
    os::unix::fs::{DirBuilderExt, FileExt, PermissionsExt},
    path::Path,
    process::{Command, ExitCode, Stdio},
    thread::sleep,
    time::{Duration, Instant},
};

use lucina_remote::{
    disk::{self, LBA, errno, partition_named, read_blocks, sync_all},
    gptw,
    image::{self, STATE_VALID},
    seed::{self, Loaded, SeedFile},
};

const DATA: &str = "/data";
const RUN: &str = "/run/lucina";
const FSCK: &str = "/sbin/e2fsck";
const SYNC_EVERY: Duration = Duration::from_secs(10);
const DEVICE_WAIT: Duration = Duration::from_secs(20);
const SEED_FILE: &str = "/data/lucina/random-seed";
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
    if unsafe { libc::mount(dev.as_ptr(), dir.as_ptr(), fstype.as_ptr(), libc::MS_NOATIME, core::ptr::null()) } != 0 {
        return Err(errno(&format!("mount {device} on {DATA}")));
    }
    Ok(())
}

/// Runs e2fsck, answering yes to everything: nobody is there to ask, and a /data that mounts is the point.
fn check(device: &str, log: &mut String) -> Result<(), String> {
    let started = Instant::now();
    let out = Command::new(FSCK)
        .args(["-f", "-y", device])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("{FSCK}: {e}"))?;
    let code = out.status.code().unwrap_or(-1);
    let _ = fs::write(format!("{RUN}/e2fsck.log"), [out.stdout.as_slice(), out.stderr.as_slice()].concat());
    log.push_str(&format!("e2fsck: exit {code} in {} ms\n", started.elapsed().as_millis()));
    // 0 clean, 1 and 2 repaired, 4 and above: errors remain.
    if !(0..=2).contains(&code) {
        return Err(format!("e2fsck exit {code}, see {RUN}/e2fsck.log"));
    }
    Ok(())
}

fn bring_up(log: &mut String) -> Result<(), String> {
    let device = device()?;
    let t0 = Instant::now();
    let info = disk::ext2_on(&device)?.ok_or_else(|| format!("{device} holds no ext2 file system"))?;
    if info.state != STATE_VALID {
        log.push_str("not clean: checking\n");
        check(&device, log)?;
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
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).map_err(|e| format!("{path}: {e}"))?;
    }
    sync_all();
    Ok(())
}

fn run_mount() -> Result<(), String> {
    fs::create_dir_all(RUN).map_err(|e| format!("{RUN}: {e}"))?;
    let mut log = String::new();
    let result = bring_up(&mut log);
    let _ = fs::write(format!("{RUN}/data.log"), &log);
    if let Err(e) = result {
        let _ = fs::write(format!("{RUN}/data-failed"), format!("{e}\n"));
        return Err(e);
    }
    fs::write(format!("{RUN}/data-ready"), "").map_err(|e| format!("{RUN}/data-ready: {e}"))?;
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
        sleep(SYNC_EVERY);
        sync_all();
    }
}

fn say_in_kernel_log(line: &str) {
    let _ = OpenOptions::new().write(true).open("/dev/kmsg").and_then(|mut kmsg| {
        use std::io::Write;
        kmsg.write_all(format!("lucina-data: {line}\n").as_bytes())
    });
}

fn clean() -> Result<(), String> {
    let device = device()?;
    // What the kernel knows at the end of this boot, for the next one.
    if let Err(e) = SeedFile::new(SEED_FILE, "/dev/urandom").save() {
        eprintln!("random seed on /data: not saved: {e}");
    }
    sync_all();
    let dir = CString::new(DATA).map_err(|e| e.to_string())?;
    // SAFETY: The string outlives the call.
    if unsafe { libc::umount(dir.as_ptr()) } != 0 {
        return Err(errno(&format!("umount {DATA}: still not clean")));
    }
    set_state(&device, STATE_VALID)
}

fn status() -> Result<(), String> {
    let device = device()?;
    let info = disk::ext2_on(&device)?.ok_or_else(|| format!("{device} holds no ext2 file system"))?;
    println!(
        "{device}: label {}, {} MiB, state {}, ready: {}",
        info.label,
        info.blocks * LBA >> 20,
        if info.state == STATE_VALID { "clean" } else { "not clean" },
        Path::new(&format!("{RUN}/data-ready")).exists()
    );
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
    let result = match arg.as_deref() {
        Some("mount") => run_mount(),
        Some("clean") => clean(),
        None | Some("status") => status(),
        Some(other) => Err(format!("unknown command {other:?}: lucina-data [mount|clean|status]")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("lucina-data: {e}");
            ExitCode::FAILURE
        }
    }
}
