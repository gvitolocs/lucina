//! lucina-recover: on the recovery stick. One command, typed on the laptop's own keyboard, that makes the disk boot again.
//!
//! ```text
//! lucina-recover            # puts the loader and BOOT.CFG right, says what it did
//! lucina-recover loader     # only the loader and a BOOT.CFG that does not parse: keeps the generation
//! lucina-recover old        # the same, with the previous loader: \EFI\BOOT\BOOTOLD.EFI, else the older copy the stick carries
//! lucina-recover check      # says what it would do, writes nothing
//! lucina-recover console    # gives the screen back to the text console (KD_TEXT), used by the stick's start script
//! ```
//!
//! 1. LUN 0's table: if one copy is damaged and the other sound, the sound one is written over the damaged one.
//! 2. `\EFI\BOOT\BOOTAA64.EFI` becomes the loader the stick carries, the one known to boot this laptop. Written under a new name,
//!    flushed, read back from a read-only mount, then renamed over the old one (atomic on this file system, docs/research/vfat.md).
//! 3. `\asterinas\BOOT.CFG`: if PREV.CFG is whole and another generation, that one takes over, as a whole BOOT.CFG can still crash at every boot;
//!    if it does not parse, or names a file that is not there, `PREV.CFG` takes its place if that one is whole;
//!    if neither is, the kernel and the rescue initramfs this stick carries go on the disk and BOOT.CFG names them. A `quiet` line
//!    the restored loader would refuse goes. The file is overwritten in place, one block, as `lucina-install-disk` commits.
//!
//! Partitions other than the ESP are not touched, and the UFS driver refuses any other unit whatever this program does.

// Juno with no C library: Eyra is the libc and the program start, in Rust (docs/research/juno-pure-rust.md).
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
extern crate eyra;

use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, ExitCode},
};

use lucina_boot::uefi::Config;
use lucina_remote::{
    disk::{
        self, FOLDER, LOADER, Mounted, RawTable, copy_flushed, in_folder, make_one_block, on_esp,
        overwrite_block, sum_of, sync_all,
    },
    gptw,
    install::{
        LAPTOP_ESP, config_block, drop_quiet, generation, initrd_name, kernel_name,
        loader_knows_quiet, named_files, new_config,
    },
    stream::hex,
};

/// What the stick carries, in its initramfs.
const CARRIED: &str = "/usr/share/lucina-recover";
const OLD_LOADER: &str = "EFI/BOOT/BOOTOLD.EFI";
const DRAM_BASE: u64 = 0xa000_0000;
const RESCUE_LINE: &str = "/usr/share/lucina-recover/rescue-cmdline.txt";

fn say(line: &str) {
    println!("{line}");
    let _ = std::io::stdout().flush();
}

fn plausible_efi(data: &[u8]) -> bool {
    data.len() > 4096 && data.starts_with(b"MZ")
}

/// Step 1. A kernel that sees no table cannot see the ESP either, so this comes first.
fn table(write: bool) -> Result<bool, String> {
    let t = RawTable::read()?;
    match gptw::repair(&t.view())? {
        None => {
            say("table: both copies are sound");
            Ok(false)
        }
        Some(fix) if write => {
            disk::write_lun0_blocks(&fix)?;
            say(
                "table: one copy was damaged, written again from the sound one. RESTART AND RUN THIS AGAIN: the kernel read the old table.",
            );
            Ok(true)
        }
        Some(_) => {
            say("table: one copy is damaged and would be written again from the sound one");
            Ok(false)
        }
    }
}

/// Step 2. Returns the loader's bytes as they are after it.
fn loader(write: bool, old_one: bool) -> Result<Vec<u8>, String> {
    let (target, old, new) = (
        on_esp(LOADER),
        on_esp(OLD_LOADER),
        on_esp("EFI/BOOT/BOOTAA64.NEW"),
    );
    let carried = Path::new(CARRIED).join("BOOTAA64.EFI");
    let carried_old = Path::new(CARRIED).join("BOOTOLD.EFI");
    let read_carried = |path: &Path| {
        fs::read(path)
            .map(|data| (path.display().to_string(), data))
            .map_err(|e| format!("{}: {e}", path.display()))
    };
    let on_disk_old = fs::read(&old).ok().filter(|data| plausible_efi(data));
    let source = if old_one {
        match on_disk_old {
            Some(data) => (old.clone(), data),
            None => read_carried(&carried_old)?,
        }
    } else {
        match read_carried(&carried) {
            Ok(found) => found,
            // Not on the stick: nothing better to restore than a loader that is whole, so that one stays.
            Err(e) => match (
                fs::read(&target).ok().filter(|data| plausible_efi(data)),
                on_disk_old,
            ) {
                (Some(data), _) => (target.clone(), data),
                (None, Some(data)) => (old.clone(), data),
                (None, None) => return Err(e),
            },
        }
    };
    let want = hex(&sha(&source.1));
    let now = fs::read(&target).ok();
    if now.as_deref() == Some(source.1.as_slice()) {
        say(&format!(
            "loader: already the one to restore, sha256 {}",
            &want[..16]
        ));
        return Ok(source.1);
    }
    say(&format!(
        "loader: \\{} is {}, restoring {} (sha256 {})",
        LOADER.replace('/', "\\"),
        now.as_deref().map_or("missing".to_string(), |d| format!(
            "sha256 {}",
            &hex(&sha(d))[..16]
        )),
        source.0,
        &want[..16]
    ));
    if !write {
        return Ok(source.1);
    }
    copy_flushed(Path::new(&source.0), &new)?;
    sync_all();
    if hex(&sum_of(&new)?) != want {
        return Err(format!("{new} does not read back as written"));
    }
    fs::rename(&new, &target).map_err(|e| format!("{new} to {target}: {e}"))?;
    sync_all();
    if hex(&sum_of(&target)?) != want {
        return Err(format!("{target} does not read back as written"));
    }
    say("loader: restored and read back");
    Ok(source.1)
}

fn sha(data: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    Sha256::digest(data).into()
}

/// Whether a configuration parses and every file it names is on the ESP, not empty.
fn whole(text: &str) -> Result<(), String> {
    Config::parse(text).map_err(|e| format!("does not parse: {e}"))?;
    for file in named_files(text, FOLDER)? {
        match fs::metadata(on_esp(&file)) {
            Ok(m) if m.len() > 0 => {}
            Ok(_) => return Err(format!("{file} is empty")),
            Err(e) => return Err(format!("{file}: {e}")),
        }
    }
    Ok(())
}

/// Step 3.
fn config(write: bool, only_loader: bool, loader_bytes: &[u8]) -> Result<(), String> {
    let (path, prev_path) = (in_folder("BOOT.CFG"), in_folder("PREV.CFG"));
    let current = fs::read(&path)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default();
    let prev = fs::read(&prev_path)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .ok();
    let keep_quiet = loader_knows_quiet(loader_bytes);
    let tidy = |t: &str| {
        if keep_quiet {
            t.to_string()
        } else {
            drop_quiet(t)
        }
    };
    let wanted = match whole(&tidy(&current)) {
        // Whole is not the same as working: a generation that crashes at every boot has a whole BOOT.CFG. What is written last
        // goes first, then, unless `lucina-recover loader` was asked for: PREV.CFG, if it is whole and another generation.
        Ok(())
            if !only_loader
                && prev.as_deref().is_some_and(|p| {
                    whole(&tidy(p)).is_ok() && generation(p) != generation(&current)
                }) =>
        {
            let t = tidy(prev.as_deref().unwrap_or(""));
            say(&format!(
                "BOOT.CFG: generation {} goes back to generation {} (PREV.CFG). `lucina-recover loader` leaves it as it is.",
                generation(&current),
                generation(&t)
            ));
            t
        }
        Ok(()) => {
            let t = tidy(&current);
            say(&format!(
                "BOOT.CFG: whole, generation {}{}",
                generation(&t),
                if t == current {
                    ""
                } else {
                    "; its quiet line goes, the restored loader does not know it"
                }
            ));
            t
        }
        Err(why) => {
            say(&format!("BOOT.CFG: {why}"));
            match prev.as_deref().map(|p| (tidy(p), whole(&tidy(p)))) {
                Some((t, Ok(()))) => {
                    say(&format!(
                        "BOOT.CFG: PREV.CFG is whole, generation {}: it takes over",
                        generation(&t)
                    ));
                    t
                }
                other => {
                    if let Some((_, Err(why))) = other {
                        say(&format!("PREV.CFG: {why}"));
                    }
                    rescue_set(write, &current)?
                }
            }
        }
    };
    if wanted == current {
        return Ok(());
    }
    if !write {
        say("BOOT.CFG would be replaced by:");
        say(&wanted);
        return Ok(());
    }
    let block = config_block(&wanted)?;
    if fs::metadata(&path).is_ok_and(|m| m.len() == disk::LBA) {
        overwrite_block(&path, &block)?;
    } else if fs::metadata(&path).is_ok_and(|m| m.len() > 0 && m.len() < disk::LBA) {
        make_one_block(&path, &current)?;
        sync_all();
        overwrite_block(&path, &block)?;
    } else {
        // Missing or torn in size: no safer way is left.
        let _ = fs::remove_file(&path);
        fs::write(&path, &block).map_err(|e| format!("{path}: {e}"))?;
    }
    sync_all();
    if fs::read(&path).map_err(|e| format!("{path}: {e}"))? != block {
        return Err(format!("{path} does not read back as written"));
    }
    say("BOOT.CFG: written and read back");
    Ok(())
}

/// Neither configuration can boot: the set on this stick goes on the disk under the next generation's names.
fn rescue_set(write: bool, current: &str) -> Result<String, String> {
    let next = generation(current).max(1) + 1;
    let (kernel, initrd) = (kernel_name(next), initrd_name(next));
    let line = fs::read_to_string(RESCUE_LINE).map_err(|e| format!("{RESCUE_LINE}: {e}"))?;
    let base = format!(
        "dtb = GBGO.DTB\ncmdline = {}\npause = 3\nlog = LOG.TXT\n",
        line.trim()
    );
    let text = new_config(&base, next, DRAM_BASE, None, None, None)?;
    say(&format!(
        "BOOT.CFG: nothing on the disk can boot; the stick's rescue system goes on it as {kernel} and {initrd}"
    ));
    if write {
        fs::create_dir_all(on_esp(FOLDER)).map_err(|e| format!("{FOLDER}: {e}"))?;
        for (from, to) in [
            ("KERNEL.IMG", &kernel),
            ("RESCUE.GZ", &initrd),
            ("GBGO.DTB", &"GBGO.DTB".to_string()),
        ] {
            copy_flushed(&Path::new(CARRIED).join(from), &in_folder(to))?;
        }
        sync_all();
    }
    Ok(text)
}

fn recover(write: bool, old_one: bool, only_loader: bool) -> Result<(), String> {
    say(if write {
        "lucina-recover: making the disk boot again. Nothing but the EFI partition is changed."
    } else {
        "lucina-recover check: nothing is written."
    });
    if table(write)? {
        return Ok(());
    }
    let (device, _) = disk::check_target(LAPTOP_ESP)?;
    let esp = Mounted::new(&device, !write)?;
    let loader_bytes = loader(write, old_one)?;
    config(write, only_loader, &loader_bytes)?;
    drop(esp);
    sync_all();
    let esp = Mounted::new(&device, true)?;
    say(&format!(
        "loader on the disk: sha256 {}",
        &hex(&sum_of(&on_esp(LOADER))?)[..16]
    ));
    let text = fs::read_to_string(in_folder("BOOT.CFG")).unwrap_or_default();
    say(&format!(
        "BOOT.CFG on the disk: generation {}",
        generation(&text)
    ));
    drop(esp);
    if write {
        say(
            "DONE. Take the stick out, then hold the power button until the screen goes off and press it again.",
        );
    }
    Ok(())
}

fn console() -> Result<(), String> {
    // The same as `lucina-shell --text-console` for what the stick and the rescue system need: the console back on the screen.
    const KDSETMODE: libc::c_ulong = 0x4b3a;
    const KD_TEXT: libc::c_ulong = 0;
    let path = c"/dev/tty0";
    // SAFETY: The string outlives the call.
    let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(disk::errno("/dev/tty0"));
    }
    // SAFETY: A valid descriptor; the argument is a plain number.
    let r = unsafe { libc::ioctl(fd, KDSETMODE as _, KD_TEXT) };
    // SAFETY: Ours to close.
    unsafe { libc::close(fd) };
    if r != 0 {
        return Err(disk::errno("KD_TEXT"));
    }
    Ok(())
}

fn main() -> ExitCode {
    let arg = std::env::args().nth(1);
    let result = match arg.as_deref() {
        None | Some("recover") => recover(true, false, false),
        Some("loader") => recover(true, false, true),
        Some("old") => recover(true, true, false),
        Some("check") => recover(false, false, false),
        Some("console") => console(),
        Some("reboot") => {
            // The rescue system has no /data (then this does nothing); a system that has it marks it clean first.
            println!(
                "{}",
                lucina_remote::dataclean::clean_before_restart().describe()
            );
            let restarted = Command::new("reboot")
                .arg("-f")
                .status()
                .is_ok_and(|s| s.success());
            if !restarted {
                // SAFETY: reboot takes one integer and returns only if it failed.
                unsafe { libc::reboot(libc::RB_AUTOBOOT) };
                println!(
                    "{}",
                    lucina_remote::dataclean::reopen_after_failed_restart().describe()
                );
                return ExitCode::FAILURE;
            }
            Ok(())
        }
        Some(other) => Err(format!(
            "unknown command {other:?}: lucina-recover [loader|old|check|console]"
        )),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            say(&format!("FAILED: {e}"));
            say(
                "Nothing further was changed. This stick also runs an SSH server, if a cable is in: root@<address>.",
            );
            ExitCode::FAILURE
        }
    }
}
