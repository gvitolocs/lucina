//! lucina-install-disk: puts a kernel and an initramfs on the Galaxy Book Go's EFI system partition, from the network.
//!
//! ```text
//! ssh buildhost cat <repo>/out/juno/reload-stream |
//!     ssh root@host "lucina-install-disk --base 0xa0000000 --cmdline '<kernel command line>' --pause 0"
//! (with --quiet 1, BOOT.CFG also says quiet = 1; refused unless the loader on the disk knows that key)
//! ssh root@host lucina-install-disk status
//! ssh root@host lucina-install-disk rollback
//! ssh buildhost cat <repo>/out/juno/apps-set | ssh root@host lucina-install-disk apps
//! ssh root@host lucina-install-disk apps-status
//! ssh root@host lucina-install-disk apps-rollback
//! lucina-install-disk netinstall --server http://<host>:<port> --keys <file> --board <id> --base <hex> [--go-back]
//! ```
//!
//! `netinstall` is `--root` with the stream from the network instead of stdin: the board's signed manifest, then the image in chunks
//! each checked against it (`lucina_remote::netinst`, docs/design/lucina-net-installer.md). It refuses a manifest that writes
//! beyond the board's compiled targets or has a lower sequence than `\lucinaos\netinst.seq` (unless `--go-back`), and a disk without
//! root-a and root-b gets a table only after a `yes` typed on the console.
//!
//! It takes the stream `lucina-reload` takes, and needs `ufs=on ufs.write=main` on the running kernel; the UFS driver
//! refuses every write to any other unit whatever this program does.
//!
//! The order, so that a power cut at any moment leaves a loader that boots a whole set:
//! 1. The stream arrives in RAM and its sums are checked. Nothing on the disk has been touched.
//! 2. The target is checked: LUN 0's GPT, its one EFI system partition at the expected place, FAT32 with 4096-byte
//!    sectors, and no BOOT.CFG that the loader would prefer to `\asterinas\BOOT.CFG`.
//! 3. The new kernel and initramfs go to new names in `\asterinas\`, are flushed, and after a remount (the file system's
//!    cache is gone) are read back from the device and checked. Any failure stops here: BOOT.CFG still names the old set.
//! 4. The old configuration is saved as PREV.CFG. Its files stay: they are the way back (`rollback`). The generation PREV.CFG named
//!    before the run had its root on the slot just written, so it and its files go (from the check of room on).
//! 5. BOOT.CFG is made one 4096-byte block if it is not yet: old text first, comment lines after, so that it means the
//!    same at either size.
//! 6. The commit: that block is overwritten in place, whole, and flushed. Old set before, new set after.
//! 7. Read back after another remount. Generations nothing names any more are removed; other files never are.
//!
//! `\EFI\BOOT\BOOTAA64.EFI` and the other loaders are not touched.
//!
//! The operating system on its own partitions (docs/research/juno-disk-root.md):
//!
//! ```text
//! lucina-install-disk gpt plan                     # both copies checked, the table it would write, its sum; writes nothing
//! lucina-install-disk gpt write --sha <sum>        # backup kept on the ESP (\lucinaos\gpt-before.bin), then the writes
//! lucina-install-disk gpt repair                   # one copy damaged: written again from the other
//! ... stream ... | lucina-install-disk --root --base 0xa0000000 --cmdline '<line>'
//! ... image stream ... | lucina-install-disk data --format
//! ```
//!
//! With `--root` the kernel's stream is followed by an image stream (`lucina-rootimg pack`). Before step 3 the image goes to the root
//! partition BOOT.CFG does not name: block 0 overwritten and flushed, then the blocks, then block 0, flushed, then read back from the
//! device and its sum checked. BOOT.CFG's command line then names that partition and the image's UUID, so the commit in step 6 switches
//! kernel, initramfs and operating system in one write. The previous partition stays as it is until the next install.
//!
//! `rollback` refuses if the partition PREV.CFG names no longer holds the image it names: that happens when an install was cut
//! while writing over it.
//! `apps` puts Juno's apps on the same partition (`lucina_remote::appset`), and touches no file of the boot set: the stream
//! arrives in RAM with its sums checked, generations other than the one in use are removed for room, the new one goes to a
//! folder of its own and is read back after a remount, and one rename of CURRENT commits it. The one in use before
//! stays, named by PREVIOUS: `apps-rollback`.

// Juno with no C library: Eyra is the libc and the program start, in Rust (docs/research/juno-pure-rust.md).
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
extern crate eyra;

use std::{
    ffi::CString,
    fs::{self, File, OpenOptions},
    io::{self, BufRead, BufReader, Read, Write},
    os::unix::fs::FileExt,
    path::Path,
    process::ExitCode,
    time::Duration,
};

use lucina_remote::{
    appset,
    disk::{
        FOLDER, LBA, LOADER, MOUNT, Mounted, RawTable, check_target, errno, ext2_on, in_folder,
        make_one_block, on_esp, overwrite_block, partition_named, sum_of, sync_all,
        write_lun0_blocks,
    },
    disk_rules::{self, Slot},
    esp::release_apps_mount,
    gptw,
    image::{self, Extent, read_extent, sum_extent_head, uuid_hex},
    install::{
        LAPTOP_ESP, config_block, generation, initrd_name, is_generated, kernel_name,
        loader_knows_quiet, named_files, new_config, old_boot_values, root_of, with_generation,
        with_root,
    },
    netinst::{self, VerifiedChunks},
    netinst_rules,
    stream::{Header, Part, arm64_image, hex},
};
use sha2::{Digest, Sha256};

/// BOOT.CFGs the loader reads before `\asterinas\BOOT.CFG` (boot/lucina-uefi, `candidates`).
const SHADOWS: [&str; 2] = ["EFI/BOOT/BOOT.CFG", "LUCINA/BOOT.CFG"];
const STAGING_DIR: &str = "/tmp/lucina-install-disk";
/// The size of each root partition on the laptop: room for the operating system and the browser several times over.
const ROOT_MIB: u64 = 8192;
/// Room left over beyond the new files, for directories and the two configurations.
const MARGIN: u64 = 1 << 20;

#[derive(Default)]
struct Options {
    base: Option<u64>,
    cmdline: Option<String>,
    /// BOOT.CFG's `pause`, the seconds LucinaUEFI counts down before starting the kernel.
    pause_s: Option<u32>,
    quiet: Option<bool>,
    /// No trial: this generation is good from the start, for a loader that knows no trials or a test.
    no_trial: bool,
    esp: Option<(u64, u64)>,
    /// A test's power cut: stop for good at this stage, without syncing anything.
    stall_at: Option<String>,
    /// The kernel's stream is followed by a root image.
    root: bool,
    /// Size of each root partition for `gpt`, for a test's smaller disk.
    root_mib: Option<u64>,
    /// `gpt write`: the sum `gpt plan` printed.
    sha: Option<String>,
    /// `data`: the image goes on the partition even if it holds a file system.
    format: bool,
    /// The `lucina.root.uuid` of the system that ran before a reload into the installer (finding AB-2).
    running: Option<String>,
}

impl Options {
    fn parse(args: &[String]) -> Result<Options, String> {
        let mut o = Options::default();
        let mut args = args.iter();
        while let Some(a) = args.next() {
            let mut value = || args.next().ok_or_else(|| format!("{a} needs a value"));
            match a.as_str() {
                "--base" => {
                    let v = value()?;
                    o.base = Some(
                        u64::from_str_radix(v.trim_start_matches("0x"), 16)
                            .map_err(|_| format!("--base {v}: not a hexadecimal address"))?,
                    );
                }
                "--cmdline" => o.cmdline = Some(value()?.clone()),
                "--running" => o.running = Some(value()?.clone()),
                "--pause" => {
                    let v = value()?;
                    o.pause_s = Some(
                        v.parse()
                            .map_err(|_| format!("--pause {v}: not a number of seconds"))?,
                    );
                }
                "--no-trial" => o.no_trial = true,
                "--quiet" => {
                    o.quiet = Some(match value()?.as_str() {
                        "0" => false,
                        "1" => true,
                        v => return Err(format!("--quiet {v}: 0 or 1")),
                    });
                }
                "--esp-lba" => {
                    let v = value()?;
                    let (a, b) = v
                        .split_once('-')
                        .and_then(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?)))
                        .ok_or_else(|| format!("--esp-lba {v}: expected <first>-<last>"))?;
                    o.esp = Some((a, b));
                }
                "--test-stall-at" => o.stall_at = Some(value()?.clone()),
                "--root-mib" => {
                    let v = value()?;
                    o.root_mib = Some(
                        v.parse()
                            .map_err(|_| format!("--root-mib {v}: not a number"))?,
                    );
                }
                "--sha" => o.sha = Some(value()?.clone()),
                "--root" => o.root = true,
                "--format" => o.format = true,
                _ => return Err(format!("unknown argument {a:?}")),
            }
        }
        Ok(o)
    }

    fn expected_esp(&self) -> (u64, u64) {
        self.esp.unwrap_or(LAPTOP_ESP)
    }

    fn stage(&self, name: &str) {
        println!("install-stage: {name}");
        let _ = io::stdout().flush();
        if self.stall_at.as_deref() == Some(name) {
            loop {
                std::thread::sleep(Duration::from_secs(3600));
            }
        }
    }
}

/// What is on the ESP that this tool relies on: the current configuration, and that the loader will read it.
fn read_config() -> Result<String, String> {
    for shadow in SHADOWS {
        if Path::new(&on_esp(shadow)).exists() {
            return Err(format!(
                "\\{} exists: the loader would read it instead of \\{FOLDER}\\BOOT.CFG",
                shadow.replace('/', "\\")
            ));
        }
    }
    let path = in_folder("BOOT.CFG");
    fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))
}

/// The ESP's allocation unit, from the same `statvfs` as `free_bytes`.
fn cluster_bytes() -> Result<u64, String> {
    let dir = CString::new(MOUNT).expect("no NUL");
    // SAFETY: `statvfs` is plain data, written by the call.
    let mut st: libc::statvfs = unsafe { core::mem::zeroed() };
    // SAFETY: The string and the buffer outlive the call.
    if unsafe { libc::statvfs(dir.as_ptr(), &mut st) } != 0 {
        return Err(errno("statvfs"));
    }
    Ok(st.f_frsize as u64)
}

fn free_bytes() -> Result<u64, String> {
    let dir = CString::new(MOUNT).expect("no NUL");
    // SAFETY: `statvfs` is plain data, written by the call.
    let mut st: libc::statvfs = unsafe { core::mem::zeroed() };
    // SAFETY: The string and the buffer outlive the call.
    if unsafe { libc::statvfs(dir.as_ptr(), &mut st) } != 0 {
        return Err(errno("statvfs"));
    }
    Ok(st.f_bavail as u64 * st.f_frsize as u64)
}

/// Copies a checked file from RAM to a new name on the ESP, flushed. `half` is a test's stage in the middle.
fn write_new(from: &Path, to: &str, opts: &Options, half: &str) -> Result<(), String> {
    let mut src = File::open(from).map_err(|e| format!("{}: {e}", from.display()))?;
    let size = src
        .metadata()
        .map_err(|e| format!("{}: {e}", from.display()))?
        .len();
    let mut out = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(to)
        .map_err(|e| format!("{to}: {e}"))?;
    let mut buf = vec![0u8; 1 << 20];
    let (mut done, mut half_seen) = (0u64, false);
    loop {
        let n = src
            .read(&mut buf)
            .map_err(|e| format!("{}: {e}", from.display()))?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])
            .map_err(|e| format!("writing {to}: {e}"))?;
        done += n as u64;
        if !half_seen && done >= size / 2 {
            half_seen = true;
            opts.stage(half);
        }
    }
    out.sync_all().map_err(|e| format!("flushing {to}: {e}"))
}

/// What `remove_unnamed` would free for these configurations, each file rounded up to whole clusters, without removing anything.
fn reclaimable_bytes(configs: &[&str]) -> Result<u64, String> {
    let mut keep = Vec::new();
    for c in configs {
        keep.extend(named_files(c, FOLDER)?);
    }
    let cluster = cluster_bytes()?.max(1);
    let dir = on_esp(FOLDER);
    let mut total = 0u64;
    for entry in fs::read_dir(&dir).map_err(|e| format!("{dir}: {e}"))? {
        let entry = entry.map_err(|e| format!("{dir}: {e}"))?;
        let name = entry.file_name().to_string_lossy().to_string();
        let named = keep
            .iter()
            .any(|k| k.eq_ignore_ascii_case(&format!("{FOLDER}/{name}")));
        if is_generated(&name) && !named {
            let size = entry.metadata().map_err(|e| format!("{name}: {e}"))?.len();
            total = total.saturating_add(size.div_ceil(cluster).saturating_mul(cluster));
        }
    }
    Ok(total)
}

/// Removes generations that neither configuration names. Other files are never removed.
fn remove_unnamed(configs: &[&str]) -> Result<(), String> {
    let mut keep = Vec::new();
    for c in configs {
        keep.extend(named_files(c, FOLDER)?);
    }
    let dir = on_esp(FOLDER);
    for entry in fs::read_dir(&dir).map_err(|e| format!("{dir}: {e}"))? {
        let entry = entry.map_err(|e| format!("{dir}: {e}"))?;
        let name = entry.file_name().to_string_lossy().to_string();
        let named = keep
            .iter()
            .any(|k| k.eq_ignore_ascii_case(&format!("{FOLDER}/{name}")));
        if is_generated(&name) && !named {
            fs::remove_file(entry.path()).map_err(|e| format!("removing {name}: {e}"))?;
            println!("removed {name}");
        }
    }
    Ok(())
}

fn receive_part(input: &mut impl Read, path: &Path, part: &Part) -> Result<(), String> {
    let mut out = File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut left = part.size;
    let mut buf = vec![0u8; 1 << 16];
    while left > 0 {
        let want = left.min(buf.len() as u64) as usize;
        let n = match input.read(&mut buf[..want]) {
            Ok(0) => return Err(format!("the stream ended {left} bytes early")),
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(format!("stdin: {e}")),
        };
        hasher.update(&buf[..n]);
        out.write_all(&buf[..n])
            .map_err(|e| format!("{}: {e}", path.display()))?;
        left -= n as u64;
    }
    let sum: [u8; 32] = hasher.finalize().into();
    if sum != part.sha256 {
        return Err(format!(
            "{}: sha256 is {}, the header says {}",
            path.display(),
            hex(&sum),
            hex(&part.sha256)
        ));
    }
    Ok(())
}

/// Writes an image stream to a partition. It holds no file system this tool would recognise until the last extent lands.
fn apply_image(
    input: &mut impl Read,
    device: &str,
    opts: &Options,
) -> Result<image::Header, String> {
    let header = image::Header::read(input)?;
    let room = lucina_remote::disk::device_blocks(device)?;
    if header.blocks > room {
        return Err(format!(
            "the image has {} blocks, {device} has {room}",
            header.blocks
        ));
    }
    let f = OpenOptions::new()
        .write(true)
        .open(device)
        .map_err(|e| format!("{device}: {e}"))?;
    let zeros = vec![0u8; 1 << 20];
    f.write_all_at(&zeros[..LBA as usize], 0)
        .and_then(|()| f.sync_all())
        .map_err(|e| format!("clearing the superblock of {device}: {e}"))?;
    opts.stage("root-cleared");
    let mut sum = Sha256::new();
    let mut table: Vec<Extent> = Vec::new();
    let mut buf = vec![0u8; 1 << 20];
    let started = std::time::Instant::now();
    let mut bytes = 0u64;
    for n in 0..header.extents {
        let e = read_extent(input)?;
        if !disk_rules::extent_fits(e.first, e.count, header.blocks, room) {
            return Err(format!("an extent past the end: {e:?}"));
        }
        sum_extent_head(&mut sum, &e);
        let mut left = e.count * LBA;
        let mut at = e.first * LBA;
        while left > 0 {
            let chunk = left.min(buf.len() as u64) as usize;
            if e.zero {
                f.write_all_at(&zeros[..chunk], at)
            } else {
                input
                    .read_exact(&mut buf[..chunk])
                    .map_err(|x| format!("the stream ended: {x}"))?;
                f.write_all_at(&buf[..chunk], at)
            }
            .map_err(|x| format!("writing {device} at {at}: {x}"))?;
            sum.update(if e.zero {
                &zeros[..chunk]
            } else {
                &buf[..chunk]
            });
            at += chunk as u64;
            left -= chunk as u64;
            bytes += chunk as u64;
        }
        table.push(e);
        if n == header.extents / 2 {
            opts.stage("root-half");
        }
        // The last extent is block 0: everything before it is on the medium first.
        if n + 2 == header.extents {
            f.sync_all()
                .map_err(|x| format!("flushing {device}: {x}"))?;
        }
    }
    let got: [u8; 32] = sum.finalize().into();
    if got != header.sha256 {
        // Block 0 is on the medium with a good superblock, so take it back: nothing may mistake this for the image.
        let _ = f
            .write_all_at(&zeros[..LBA as usize], 0)
            .and_then(|()| f.sync_all());
        return Err(format!(
            "the image's sha256 is {}, its header says {}",
            hex(&got),
            hex(&header.sha256)
        ));
    }
    f.sync_all()
        .map_err(|x| format!("flushing {device}: {x}"))?;
    println!(
        "{device}: {} MiB written in {} ms, {} MiB/s",
        bytes >> 20,
        started.elapsed().as_millis(),
        bytes / 1000 / started.elapsed().as_millis().max(1) as u64
    );
    opts.stage("root-written");
    verify_image(device, &table, &header)?;
    opts.stage("root-verified");
    Ok(header)
}

/// Reads the blocks of an image back from the device and checks them against the header.
fn verify_image(device: &str, table: &[Extent], header: &image::Header) -> Result<(), String> {
    let f = File::open(device).map_err(|e| format!("{device}: {e}"))?;
    let mut sum = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let started = std::time::Instant::now();
    let mut bytes = 0u64;
    for e in table {
        sum_extent_head(&mut sum, e);
        let mut left = e.count * LBA;
        let mut at = e.first * LBA;
        while left > 0 {
            let chunk = left.min(buf.len() as u64) as usize;
            f.read_exact_at(&mut buf[..chunk], at)
                .map_err(|x| format!("reading {device} back: {x}"))?;
            sum.update(&buf[..chunk]);
            at += chunk as u64;
            left -= chunk as u64;
            bytes += chunk as u64;
        }
    }
    let got: [u8; 32] = sum.finalize().into();
    if got != header.sha256 {
        return Err(format!(
            "{device} reads back as {}, it should be {}",
            hex(&got),
            hex(&header.sha256)
        ));
    }
    println!(
        "verified: {device} reads back, sums match, uuid {}, {} MiB in {} ms, {} MiB/s",
        uuid_hex(&header.uuid),
        bytes >> 20,
        started.elapsed().as_millis(),
        bytes / 1000 / started.elapsed().as_millis().max(1) as u64
    );
    Ok(())
}

/// The root partition the current configuration does not name, in the order A, B.
/// The two root partitions' devices, from the table.
fn root_devices() -> Result<(String, String), String> {
    let device_of = |name: &str| {
        partition_named(name)?
            .map(|(device, _)| device)
            .ok_or_else(|| format!("LUN 0 has no partition named {name}: run gpt write first"))
    };
    Ok((device_of(gptw::ROOT_A)?, device_of(gptw::ROOT_B)?))
}

/// The slot a configuration boots from, if it names one of the two.
fn slot_of(config: &str, a: &str, b: &str) -> Option<Slot> {
    match root_of(config).map(|(device, _)| device).as_deref() {
        Some(d) if d == a => Some(Slot::A),
        Some(d) if d == b => Some(Slot::B),
        _ => None,
    }
}

/// The partition nothing boots from: never the one BOOT.CFG names.
fn free_slot(config: &str, a: &str, b: &str) -> Slot {
    disk_rules::free_slot(slot_of(config, a, b))
}

fn root_blocks(opts: &Options) -> u64 {
    opts.root_mib.unwrap_or(ROOT_MIB).saturating_mul(1 << 20) / LBA
}

fn gpt(opts: &Options, what: &str) -> Result<(), String> {
    let esp = opts.expected_esp();
    let table = RawTable::read()?;
    match what {
        "plan" => {
            let plan = gptw::plan(&table.view(), esp, root_blocks(opts))?;
            println!(
                "both copies of the table are sound; LUN 0 has {} blocks",
                table.disk_blocks
            );
            println!("  p1 EFI system partition, LBA {}-{}: stays", esp.0, esp.1);
            for p in &plan.partitions {
                println!(
                    "  p{} {}, LBA {}-{}, {} MiB",
                    p.index,
                    p.name,
                    p.first,
                    p.last,
                    ((p.last - p.first + 1) * LBA) >> 20
                );
            }
            if plan.already {
                println!("this table is on the disk already: nothing to write");
                return Ok(());
            }
            for w in plan.backup.iter().chain(&plan.primary) {
                println!("  write LBA {}", w.lba);
            }
            println!("sha {}", plan.sha256);
            Ok(())
        }
        "write" => {
            let plan = gptw::plan(&table.view(), esp, root_blocks(opts))?;
            if plan.already {
                println!("this table is on the disk already: nothing written");
                return Ok(());
            }
            let sha = opts
                .sha
                .as_deref()
                .ok_or("gpt write needs --sha <the sum gpt plan printed>")?;
            if sha != plan.sha256 {
                return Err(format!(
                    "--sha {sha} is not this plan's sum, {}: nothing written",
                    plan.sha256
                ));
            }
            save_gpt_backup(&table, opts)?;
            opts.stage("gpt-backup-saved");
            for (i, w) in plan.backup.iter().enumerate() {
                write_lun0_blocks(core::slice::from_ref(w))?;
                opts.stage(&format!("gpt-backup-{i}"));
            }
            for (i, w) in plan.primary.iter().enumerate() {
                write_lun0_blocks(core::slice::from_ref(w))?;
                opts.stage(&format!("gpt-primary-{i}"));
            }
            let after = RawTable::read()?;
            gptw::check_both(&after.view())?;
            if !gptw::plan(&after.view(), esp, root_blocks(opts))?.already {
                return Err("the table does not read back as written".into());
            }
            println!(
                "WRITTEN: both copies read back sound. The kernel lists the new partitions after a restart."
            );
            Ok(())
        }
        "repair" => match gptw::repair(&table.view())? {
            None => {
                println!("both copies are sound: nothing written");
                Ok(())
            }
            Some(writes) => {
                write_lun0_blocks(&writes)?;
                gptw::check_both(&RawTable::read()?.view())?;
                println!("REPAIRED: both copies read back sound");
                Ok(())
            }
        },
        other => Err(format!("gpt {other}: plan, write or repair")),
    }
}

/// The table as it was, on the ESP, so that a person with a shell can put it back. Never replaced: the first one is the original.
fn save_gpt_backup(table: &RawTable, opts: &Options) -> Result<(), String> {
    release_apps_mount();
    let (device, _) = check_target(opts.expected_esp())?;
    let esp = Mounted::new(&device, false)?;
    let folder = on_esp("lucinaos");
    if !Path::new(&folder).exists() {
        fs::create_dir(&folder).map_err(|e| format!("{folder}: {e}"))?;
    }
    let path = on_esp("lucinaos/gpt-before.bin");
    if !Path::new(&path).exists() {
        let mut out = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| format!("{path}: {e}"))?;
        out.write_all(&table.backup_bytes())
            .and_then(|()| out.sync_all())
            .map_err(|e| format!("writing {path}: {e}"))?;
    }
    println!(
        "the table as it was: \\lucinaos\\gpt-before.bin, sha256 {}",
        hex(&sum_of(&path)?)
    );
    drop(esp);
    Ok(())
}

/// The /data partition's file system. Refuses to replace one unless told to.
fn data(opts: &Options) -> Result<(), String> {
    let (device, _) =
        partition_named(gptw::DATA)?.ok_or("LUN 0 has no data partition: run gpt write first")?;
    if let Some(info) = ext2_on(&device)?
        && !opts.format
    {
        println!(
            "{device} already holds a file system, label {:?}: nothing written (--format replaces it)",
            info.label
        );
        return Ok(());
    }
    let mut input = BufReader::with_capacity(1 << 16, io::stdin().lock());
    let header = apply_image(&mut input, &device, opts)?;
    println!("INSTALLED {device}: uuid {}", uuid_hex(&header.uuid));
    Ok(())
}

/// The root the running system was started from: `lucina.root.uuid` on this kernel's command line, or, in the installer a
/// reload started (whose command line has none), what `--running` says ran before it. Both given must agree.
fn running_root(opts: &Options) -> Result<Option<String>, String> {
    let line = fs::read_to_string("/proc/cmdline").unwrap_or_default();
    let here = line
        .split_ascii_whitespace()
        .take_while(|a| *a != "--")
        .find_map(|a| a.strip_prefix("lucina.root.uuid="))
        .map(str::to_string);
    match (here, opts.running.clone()) {
        (Some(h), Some(r)) if h != r => {
            Err(format!("this system runs root {h}, --running says {r}"))
        }
        (h, r) => Ok(h.or(r)),
    }
}

fn install(opts: &Options) -> Result<(), String> {
    install_from(opts, BufReader::with_capacity(1 << 16, io::stdin().lock()))
}

/// `install` with the stream from `input`: stdin, or the net installer's verified chunks.
fn install_from(opts: &Options, mut input: impl BufRead) -> Result<(), String> {
    release_apps_mount();
    let base = opts
        .base
        .ok_or("--base <the kernel's DRAM base> is required")?;

    // 1. The stream, in RAM.
    fs::create_dir_all(STAGING_DIR).map_err(|e| format!("{STAGING_DIR}: {e}"))?;
    let (kernel_tmp, initrd_tmp) = (
        Path::new(STAGING_DIR).join("kernel"),
        Path::new(STAGING_DIR).join("initrd"),
    );
    let header = Header::read(&mut input).map_err(|e| format!("the stream's header: {e}"))?;
    receive_part(&mut input, &kernel_tmp, &header.kernel)?;
    receive_part(&mut input, &initrd_tmp, &header.initrd)?;
    let mut head = [0u8; 64];
    File::open(&kernel_tmp)
        .and_then(|mut f| f.read_exact(&mut head))
        .map_err(|e| format!("{}: {e}", kernel_tmp.display()))?;
    arm64_image(&head).ok_or("the stream's kernel is not an arm64 image")?;
    println!(
        "received: kernel {} bytes, initramfs {} bytes, sums match",
        header.kernel.size, header.initrd.size
    );
    opts.stage("received");

    // 2. The target. Everything that can refuse does so before the first write.
    let (device, _) = check_target(opts.expected_esp())?;
    opts.stage("target-checked");
    let mut esp = Mounted::new(&device, false)?;
    let old = read_config()?;
    let prev = fs::read_to_string(in_folder("PREV.CFG")).ok();
    // Above what PREV.CFG names too: after a rollback, the next generation must not reuse the name of a kept set.
    let current = generation(&old).max(prev.as_deref().map_or(0, generation));
    let next = current + 1;
    let (kernel_name, initrd_name) = (kernel_name(next), initrd_name(next));
    if opts.quiet == Some(true) {
        let path = on_esp(LOADER);
        let loader = fs::read(&path).map_err(|e| format!("{path}: {e}"))?;
        if !loader_knows_quiet(&loader) {
            return Err(format!(
                "{LOADER} does not know quiet = 1 and would refuse this BOOT.CFG: replace the loader first"
            ));
        }
    }
    // Every check that can refuse runs here, before the first write: the root image, a removal or a new file. The new BOOT.CFG
    // is built once with a stand-in image id, which names the same files and has the same length class as the real one.
    let root = if opts.root {
        let (a, b) = root_devices()?;
        // AB-2 (docs/spec/juno-ab-boot.md): BOOT.CFG must name the root that runs. After a settle rollback, or a second install
        // in one boot, it names another, and its free slot would be the running one.
        if let Some((_, boot_uuid)) = root_of(&old) {
            match running_root(opts)? {
                Some(r) if r == boot_uuid => {}
                Some(r) => {
                    return Err(format!(
                        "BOOT.CFG names root {boot_uuid}, but the system that runs came from {r} (a rollback or an install since it \
                         started): restart into BOOT.CFG's generation first; nothing was written"
                    ));
                }
                None => {
                    return Err("which root ran before this installer is not known: give --running <its lucina.root.uuid>; \
                                nothing was written"
                        .into());
                }
            }
        }
        let slot = free_slot(&old, &a, &b);
        let line = opts.cmdline.clone().unwrap_or_else(|| command_line(&old));
        Some((slot, a, b, line))
    } else {
        None
    };
    let device_of = |s: Slot, a: &str, b: &str| match s {
        Slot::A => a.to_string(),
        Slot::B => b.to_string(),
    };
    let probe_line = match &root {
        Some((slot, a, b, line)) => Some(with_root(line, &device_of(*slot, a, b), &"0".repeat(32))),
        None => opts.cmdline.clone(),
    };
    // The values this install replaces, for the log: with --pause 0 the boot has no choice menu any more.
    println!("{}", old_boot_values(&old));
    let probe = new_config(
        &old,
        next,
        base,
        probe_line.as_deref(),
        opts.pause_s,
        opts.quiet,
    )?;
    config_block(&probe)?;
    for file in named_files(&probe, FOLDER)? {
        let made = file.ends_with(&kernel_name) || file.ends_with(&initrd_name);
        if !made && !Path::new(&on_esp(&file)).exists() {
            return Err(format!(
                "the new BOOT.CFG names {file}, which is not on the ESP; nothing was written"
            ));
        }
    }
    // The root slot written is, with two slots, the one PREV.CFG boots: writing it ends that generation, so its kernel and
    // initramfs are freed with the rest (2026-11-08: two kept sets and a new one no longer fit beside the capsules and the apps).
    let prev_slot = match &root {
        Some((_, a, b, _)) => prev.as_deref().and_then(|p| slot_of(p, a, b)),
        None => None,
    };
    let drop_prev = match (&root, prev_slot) {
        (Some((slot, ..)), Some(p)) => disk_rules::same_slot(*slot, p),
        _ => false,
    };
    let keep: Vec<&str> = core::iter::once(old.as_str())
        .chain(prev.as_deref().filter(|_| !drop_prev))
        .collect();
    let (free, reclaimable) = (free_bytes()?, reclaimable_bytes(&keep)?);
    let needed = header.kernel.size + header.initrd.size + MARGIN;
    let refusal = match &root {
        Some((slot, a, b, _)) => disk_rules::may_write_root(
            *slot,
            slot_of(&old, a, b),
            // Checked above to be BOOT.CFG's.
            slot_of(&old, a, b),
            prev_slot,
            // An install ends by writing PREV.CFG anew from the old BOOT.CFG, so the set PREV.CFG names now is replaced.
            true,
            free,
            reclaimable,
            header.kernel.size,
            header.initrd.size,
            MARGIN,
        )
        .err(),
        None => (!disk_rules::esp_has_room(
            free,
            reclaimable,
            header.kernel.size,
            header.initrd.size,
            MARGIN,
        ))
        .then_some(disk_rules::RootRefusal::NoRoom),
    };
    match refusal {
        None => {}
        Some(disk_rules::RootRefusal::NoRoom) => {
            return Err(format!(
                "{free} bytes free on the ESP and {reclaimable} to be freed, {needed} needed; nothing was written"
            ));
        }
        Some(why) => {
            return Err(format!(
                "the root slot is refused ({why:?}); nothing was written"
            ));
        }
    }
    let cmdline = match root {
        Some((slot, a, b, line)) => {
            let slot = device_of(slot, &a, &b);
            println!("root image: to {slot}");
            if drop_prev {
                // AB-1 (docs/spec/juno-ab-boot.md): PREV.CFG goes, and is on the disk, before its root is overwritten, so a cut
                // inside the image never leaves a PREV.CFG naming a root that is gone.
                let prev_path = in_folder("PREV.CFG");
                fs::remove_file(&prev_path).map_err(|e| format!("removing {prev_path}: {e}"))?;
                sync_all();
                esp.unmount()?;
                esp = Mounted::new(&device, false)?;
                if Path::new(&prev_path).exists() {
                    return Err(format!(
                        "{prev_path} is still there after its removal; nothing else was written"
                    ));
                }
                println!("PREV.CFG removed: its root on {slot} is overwritten next");
            }
            let image = apply_image(&mut input, &slot, opts)?;
            Some(with_root(&line, &slot, &uuid_hex(&image.uuid)))
        }
        None => opts.cmdline.clone(),
    };
    // The home screen's "Generation N" (lucina.generation=).
    let cmdline = Some(with_generation(
        &cmdline.unwrap_or_else(|| command_line(&old)),
        next,
    ));
    let new = new_config(
        &old,
        next,
        base,
        cmdline.as_deref(),
        opts.pause_s,
        opts.quiet,
    )?;
    let block = config_block(&new)?;
    remove_unnamed(&keep)?;
    // Freed clusters may be reused below, so the removals are on the disk first. On the laptop's FAT driver a removal reaches the
    // disk, and the free count, only when the file system is unmounted (2026-11-08: PREV.CFG and a whole set were still there
    // after `sync`, and the room read the same as before them), so the ESP is mounted afresh before its room is read.
    sync_all();
    esp.unmount()?;
    esp = Mounted::new(&device, false)?;
    let free = free_bytes()?;
    if free < needed {
        return Err(format!("{free} bytes free on the ESP, {needed} needed"));
    }
    println!("generation {current} -> {next}; {free} bytes free, {needed} needed");
    opts.stage("mounted");

    // 3. The new set, under names nothing uses yet. It is read back from a new, read-only mount: the file system's own
    // cache of what was written is gone.
    let (kernel_path, initrd_path) = (in_folder(&kernel_name), in_folder(&initrd_name));
    write_new(&kernel_tmp, &kernel_path, opts, "kernel-half")?;
    opts.stage("kernel-written");
    write_new(&initrd_tmp, &initrd_path, opts, "initrd-half")?;
    opts.stage("initrd-written");
    sync_all();
    esp.unmount()?;
    let mut esp = Mounted::new(&device, true)?;
    for (path, part) in [
        (&kernel_path, &header.kernel),
        (&initrd_path, &header.initrd),
    ] {
        let sum = sum_of(path)?;
        if sum != part.sha256 {
            return Err(format!(
                "{path} reads back as {}, it should be {}; BOOT.CFG was not changed",
                hex(&sum),
                hex(&part.sha256)
            ));
        }
    }
    println!("verified: {kernel_name} and {initrd_name} read back from the device, sums match");
    opts.stage("verified");
    esp.unmount()?;
    let mut esp = Mounted::new(&device, false)?;

    // 4. The way back.
    let prev_path = in_folder("PREV.CFG");
    let _ = fs::remove_file(&prev_path);
    sync_all();
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&prev_path)
        .map_err(|e| format!("{prev_path}: {e}"))?;
    f.write_all(old.as_bytes())
        .and_then(|()| f.sync_all())
        .map_err(|e| format!("writing {prev_path}: {e}"))?;
    drop(f);
    opts.stage("prev-saved");

    // What the loader marks its call to the firmware's random service with. Nothing about it may stop an install.
    if let Err(e) = lucina_remote::disk::prepare_rng_marker() {
        println!("random seed: no marker record, so the loader will not ask the firmware: {e}");
    }

    // The new generation's trial, before the commit: until BOOT.CFG names it, a record about it means nothing to the loader.
    if !opts.no_trial {
        lucina_remote::disk::prepare_trial()?;
        let newest = lucina_remote::disk::read_trial();
        let record = lucina_boot::trial::Record {
            seq: newest.record.map_or(1, |r| r.seq.saturating_add(1)),
            generation: generation(&new) as u32,
            state: lucina_boot::trial::State::Trial,
            tries: lucina_boot::trial::TRIES,
            settle: 0,
        };
        lucina_remote::disk::write_trial(newest.write_to, &record)?;
        sync_all();
        opts.stage("trial-written");
    }

    // 5. and 6. BOOT.CFG one block long, then the commit.
    let config_path = in_folder("BOOT.CFG");
    make_one_block(&config_path, &old)?;
    sync_all();
    opts.stage("commit-ready");
    overwrite_block(&config_path, &block)?;
    sync_all();
    opts.stage("committed");

    // 7. What the disk says now, from the device.
    esp.unmount()?;
    let mut esp = Mounted::new(&device, true)?;
    if fs::read(&config_path).map_err(|e| format!("{config_path}: {e}"))? != block {
        return Err(format!("{config_path} does not read back as written"));
    }
    for (path, part) in [
        (&kernel_path, &header.kernel),
        (&initrd_path, &header.initrd),
    ] {
        if sum_of(path)? != part.sha256 {
            return Err(format!("{path} does not read back as written"));
        }
    }
    esp.unmount()?;
    let mut esp = Mounted::new(&device, false)?;
    remove_unnamed(&[&new, &old])?;
    sync_all();
    opts.stage("cleaned");
    esp.unmount()?;
    sync_all();
    println!(
        "INSTALLED generation {next}: {FOLDER}/{kernel_name} at {base:#x}, {FOLDER}/{initrd_name}"
    );
    println!("command line: {}", command_line(&new));
    println!(
        "previous set kept (lucina-install-disk rollback); the running kernel is unchanged until a reboot"
    );
    Ok(())
}

fn command_line(config: &str) -> String {
    config
        .lines()
        .find_map(|l| {
            l.trim()
                .strip_prefix("cmdline")?
                .trim_start()
                .strip_prefix('=')
        })
        .unwrap_or("")
        .trim()
        .to_string()
}

/// Reads only: mounts the ESP read-only and says what BOOT.CFG names.
fn status(opts: &Options) -> Result<(), String> {
    release_apps_mount();
    let (device, _) = check_target(opts.expected_esp())?;
    let _esp = Mounted::new(&device, true)?;
    let config = read_config()?;
    println!("generation {}", generation(&config));
    println!("command line: {}", command_line(&config));
    for file in named_files(&config, FOLDER)? {
        match fs::metadata(on_esp(&file)) {
            Ok(m) => println!(
                "{file}: {} bytes, sha256 {}",
                m.len(),
                hex(&sum_of(&on_esp(&file))?)
            ),
            Err(e) => println!("{file}: {e}"),
        }
    }
    match lucina_remote::disk::read_rng_marker() {
        Some(r) => println!("firmware seed marker: {:?}, seq {}", r.state, r.seq),
        None => println!("firmware seed marker: none: the loader does not ask the firmware"),
    }
    match root_of(&config) {
        Some((device, uuid)) => println!("root: {device}, uuid {uuid}"),
        None => println!("root: none, the initramfs is the system"),
    }
    for name in [gptw::ROOT_A, gptw::ROOT_B, gptw::DATA] {
        match partition_named(name)? {
            Some((device, blocks)) => match ext2_on(&device)? {
                Some(i) => println!(
                    "{name}: {device}, {} MiB, ext2 uuid {} label {:?}, {} MiB file system",
                    (blocks * LBA) >> 20,
                    uuid_hex(&i.uuid),
                    i.label,
                    (i.blocks * LBA) >> 20
                ),
                None => println!(
                    "{name}: {device}, {} MiB, no file system",
                    (blocks * LBA) >> 20
                ),
            },
            None => println!("{name}: no such partition"),
        }
    }
    if let Ok(prev) = fs::read_to_string(in_folder("PREV.CFG")) {
        println!(
            "previous: generation {}, root {:?}",
            generation(&prev),
            root_of(&prev)
        );
    }
    Ok(())
}

/// Back to the configuration PREV.CFG holds, by the same one-block write.
fn rollback(opts: &Options) -> Result<(), String> {
    release_apps_mount();
    let (device, _) = check_target(opts.expected_esp())?;
    let mut esp = Mounted::new(&device, false)?;
    let current = read_config()?;
    let prev_path = in_folder("PREV.CFG");
    let prev = match fs::read_to_string(&prev_path) {
        Ok(p) => p,
        // An install removes PREV.CFG before it overwrites the root PREV.CFG booted (AB-1): that generation is gone.
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err("there is no PREV.CFG: nothing to go back to (an install removes it before overwriting its root)".into());
        }
        Err(e) => return Err(format!("{prev_path}: {e}")),
    };
    for file in named_files(&prev, FOLDER)? {
        if !Path::new(&on_esp(&file)).exists() {
            return Err(format!("PREV.CFG names {file}, which is not on the ESP"));
        }
    }
    if let Some((slot, uuid)) = root_of(&prev) {
        let there = ext2_on(&slot)?.map(|i| uuid_hex(&i.uuid));
        if there.as_deref() != Some(uuid.as_str()) {
            return Err(format!(
                "PREV.CFG names image {uuid} on {slot}, which holds {there:?}: an install wrote over it"
            ));
        }
    }
    let config_path = in_folder("BOOT.CFG");
    make_one_block(&config_path, &current)?;
    sync_all();
    overwrite_block(&config_path, &config_block(&prev)?)?;
    esp.unmount()?;
    sync_all();
    println!("ROLLED BACK: BOOT.CFG now names what PREV.CFG names");
    println!("command line: {}", command_line(&prev));
    Ok(())
}

fn sha256_hex(b: &[u8]) -> String {
    hex(&Sha256::digest(b))
}

fn apps_path(name: &str) -> String {
    on_esp(&format!("{}/{name}", appset::FOLDER))
}

/// Writes a one-line file under a new name and renames it over the old one.
fn write_pointer(name: &str, generation: &str) -> Result<(), String> {
    let (new, file) = (apps_path(&format!("{name}.new")), apps_path(name));
    let _ = fs::remove_file(&new);
    let mut out = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&new)
        .map_err(|e| format!("{new}: {e}"))?;
    out.write_all(format!("{generation}\n").as_bytes())
        .map_err(|e| format!("writing {new}: {e}"))?;
    out.sync_all().map_err(|e| format!("flushing {new}: {e}"))?;
    drop(out);
    fs::rename(&new, &file).map_err(|e| format!("{new} to {file}: {e}"))?;
    sync_all();
    Ok(())
}

fn read_pointer(name: &str) -> Option<String> {
    let text = fs::read_to_string(apps_path(name)).ok()?;
    let text = text.trim();
    appset::is_generation(text).then(|| text.to_string())
}

/// Removes every generation except these. Nothing but folders that look like generations is ever touched.
fn prune_generations(keep: &[&str]) -> Result<(), String> {
    let dir = on_esp(appset::FOLDER);
    for entry in fs::read_dir(&dir).map_err(|e| format!("{dir}: {e}"))? {
        let entry = entry.map_err(|e| format!("{dir}: {e}"))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if appset::is_generation(&name) && !keep.contains(&name.as_str()) {
            fs::remove_dir_all(entry.path()).map_err(|e| format!("removing {name}: {e}"))?;
            println!("removed the old set {name}");
        }
    }
    sync_all();
    Ok(())
}

/// Installs a set of apps: see the top of this file.
/// Where the image keeps its apps' manifests (`lucina_app::manifest::MANIFEST_DIR`).
const IMAGE_MANIFESTS: &str = "/usr/share/lucina/apps";

/// Whether the running image carries apps of its own: then lucina-shell never reads a set on the EFI partition
/// (`start_disk_apps` runs only when the image has none, since 2026-11-02), so a set there would only take room the kernels need.
fn image_has_apps() -> bool {
    fs::read_dir(IMAGE_MANIFESTS)
        .map(|d| {
            d.flatten()
                .any(|e| e.file_name().to_string_lossy().ends_with(".app"))
        })
        .unwrap_or(false)
}

fn install_apps(opts: &Options) -> Result<(), String> {
    if image_has_apps() {
        return Err(format!(
            "this system's image carries its apps ({IMAGE_MANIFESTS}), so the shell never reads a set on the EFI partition: \
             nothing written. Apps outside the image go to /data/lucina/apps"
        ));
    }
    let mut input = BufReader::new(io::stdin().lock());
    let header = appset::read_header(&mut input, sha256_hex)?;
    let _ = fs::remove_dir_all(STAGING_DIR);
    fs::create_dir_all(STAGING_DIR).map_err(|e| format!("{STAGING_DIR}: {e}"))?;
    let mut total = 0;
    for e in &header.entries {
        let path = format!("{STAGING_DIR}/{}", e.name);
        let mut out = File::create(&path).map_err(|err| format!("{path}: {err}"))?;
        let mut sum = Sha256::new();
        let mut left = e.size;
        let mut buf = vec![0u8; 1 << 16];
        while left > 0 {
            let want = left.min(buf.len() as u64) as usize;
            let n = input
                .read(&mut buf[..want])
                .map_err(|err| format!("reading {}: {err}", e.name))?;
            if n == 0 {
                return Err(format!("the stream ends inside {}", e.name));
            }
            sum.update(&buf[..n]);
            out.write_all(&buf[..n])
                .map_err(|err| format!("{path}: {err}"))?;
            left -= n as u64;
        }
        if hex(&sum.finalize()) != e.sum {
            return Err(format!("{}: the sum is wrong", e.name));
        }
        total += e.size;
    }
    let mut extra = [0u8; 1];
    if input.read(&mut extra).map_err(|e| e.to_string())? != 0 {
        return Err("there is more in the stream than its header says".into());
    }
    println!(
        "received {} files, {total} bytes, as {}",
        header.entries.len(),
        header.generation
    );
    opts.stage("apps-received");

    release_apps_mount();
    let (device, _) = check_target(opts.expected_esp())?;
    let mut esp = Mounted::new(&device, false)?;
    fs::create_dir_all(on_esp(appset::FOLDER)).map_err(|e| format!("{}: {e}", appset::FOLDER))?;
    let current = read_pointer("CURRENT");
    let generation = header.generation.as_str();
    if current.as_deref() == Some(generation) {
        println!("APPS: {generation} is already in use");
        return Ok(());
    }
    // Room first: what is in use stays, whatever happens next.
    let keep: Vec<&str> = current.iter().map(String::as_str).collect();
    prune_generations(&keep)?;
    let free = free_bytes()?;
    if total + MARGIN > free {
        return Err(format!(
            "{total} bytes do not fit: {free} free on the partition, with the set in use kept"
        ));
    }
    let folder = apps_path(generation);
    fs::create_dir(&folder).map_err(|e| format!("{folder}: {e}"))?;
    for e in &header.entries {
        let from = format!("{STAGING_DIR}/{}", e.name);
        write_new(
            Path::new(&from),
            &format!("{folder}/{}", e.name),
            opts,
            "apps-half",
        )?;
    }
    sync_all();
    opts.stage("apps-copied");

    // Read back with the file system's cache gone.
    esp.unmount()?;
    let mut esp = Mounted::new(&device, true)?;
    for e in &header.entries {
        let path = format!("{folder}/{}", e.name);
        if hex(&sum_of(&path)?) != e.sum {
            return Err(format!("{path} does not read back as it was written"));
        }
    }
    println!("verified {}", generation);
    esp.unmount()?;
    let mut esp = Mounted::new(&device, false)?;
    if let Some(old) = &current {
        write_pointer("PREVIOUS", old)?;
    }
    opts.stage("apps-before-commit");
    // The commit.
    write_pointer("CURRENT", generation)?;
    opts.stage("apps-committed");
    esp.unmount()?;
    let esp = Mounted::new(&device, true)?;
    if read_pointer("CURRENT").as_deref() != Some(generation) {
        return Err("CURRENT does not read back as it was written".into());
    }
    drop(esp);
    println!("APPS INSTALLED {generation}; the set before it stays: {current:?}");
    let _ = fs::remove_dir_all(STAGING_DIR);
    Ok(())
}

/// Which sets are on the disk.
fn apps_status(opts: &Options) -> Result<(), String> {
    release_apps_mount();
    let (device, _) = check_target(opts.expected_esp())?;
    let _esp = Mounted::new(&device, true)?;
    println!("CURRENT {:?}", read_pointer("CURRENT"));
    println!("PREVIOUS {:?}", read_pointer("PREVIOUS"));
    if let Some(current) = read_pointer("CURRENT") {
        let dir = apps_path(&current);
        for entry in fs::read_dir(&dir).map_err(|e| format!("{dir}: {e}"))? {
            let entry = entry.map_err(|e| format!("{dir}: {e}"))?;
            let size = entry.metadata().map_err(|e| e.to_string())?.len();
            println!("{size} {}", entry.file_name().to_string_lossy());
        }
    }
    println!("free {}", free_bytes()?);
    Ok(())
}

/// Goes back to the set before: CURRENT and PREVIOUS change places.
fn apps_rollback(opts: &Options) -> Result<(), String> {
    release_apps_mount();
    let (device, _) = check_target(opts.expected_esp())?;
    let _esp = Mounted::new(&device, false)?;
    let previous = read_pointer("PREVIOUS").ok_or("there is no set to go back to")?;
    if !Path::new(&apps_path(&previous)).is_dir() {
        return Err(format!("{previous} is gone"));
    }
    let current = read_pointer("CURRENT");
    write_pointer("CURRENT", &previous)?;
    if let Some(c) = current {
        write_pointer("PREVIOUS", &c)?;
    }
    println!("APPS ROLLED BACK to {previous}");
    Ok(())
}

/// The options of `netinstall` that are not `install`'s.
struct NetOptions {
    server: String,
    keys: String,
    board: String,
    go_back: bool,
}

fn net_options(args: &[String]) -> Result<(NetOptions, Vec<String>), String> {
    let (mut server, mut keys, mut board, mut go_back) = (None, None, None, false);
    let (mut rest, mut it) = (Vec::new(), args.iter());
    while let Some(a) = it.next() {
        let mut value = || {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{a} needs a value"))
        };
        match a.as_str() {
            "--server" => server = Some(value()?),
            "--keys" => keys = Some(value()?),
            "--board" => board = Some(value()?),
            "--go-back" => go_back = true,
            _ => rest.push(a.clone()),
        }
    }
    let need = |v: Option<String>, name: &str| v.ok_or_else(|| format!("netinstall needs {name}"));
    Ok((
        NetOptions {
            server: need(server, "--server http://<host>:<port>")?,
            keys: need(keys, "--keys <file>")?,
            board: need(board, "--board <id>")?,
            go_back,
        },
        rest,
    ))
}

/// The sequence number of the manifest installed on this machine: `\lucinaos\netinst.seq` on the ESP. Absent is `None`; a file that
/// is not a number is an error, not "absent".
fn installed_seq(opts: &Options) -> Result<Option<u64>, String> {
    release_apps_mount();
    let (device, _) = check_target(opts.expected_esp())?;
    let esp = Mounted::new(&device, true)?;
    let path = on_esp(netinst_rules::SEQ_FILE);
    let seq = match fs::read_to_string(&path) {
        Ok(text) => Some(
            netinst_rules::parse_seq(&text)
                .ok_or_else(|| format!("{path} is not a sequence number"))?,
        ),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("{path}: {e}")),
    };
    drop(esp);
    Ok(seq)
}

/// Records the installed manifest's sequence: a new name, flushed, then renamed over the old.
fn write_seq(opts: &Options, seq: u64) -> Result<(), String> {
    let (device, _) = check_target(opts.expected_esp())?;
    let mut esp = Mounted::new(&device, false)?;
    let folder = on_esp("lucinaos");
    if !Path::new(&folder).exists() {
        fs::create_dir(&folder).map_err(|e| format!("{folder}: {e}"))?;
    }
    let (new, path) = (
        on_esp("lucinaos/netinst.seq.new"),
        on_esp(netinst_rules::SEQ_FILE),
    );
    let _ = fs::remove_file(&new);
    let mut out = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&new)
        .map_err(|e| format!("{new}: {e}"))?;
    out.write_all(netinst_rules::seq_text(seq).as_bytes())
        .and_then(|()| out.sync_all())
        .map_err(|e| format!("writing {new}: {e}"))?;
    drop(out);
    sync_all();
    fs::rename(&new, &path).map_err(|e| format!("{new} -> {path}: {e}"))?;
    sync_all();
    esp.unmount()
}

/// One line from the machine's own console.
fn read_tty_line() -> Result<String, String> {
    let tty = File::open("/dev/tty").map_err(|e| format!("/dev/tty: {e}"))?;
    let mut line = String::new();
    BufReader::new(tty)
        .read_line(&mut line)
        .map_err(|e| format!("/dev/tty: {e}"))?;
    Ok(line)
}

/// A disk without root-a and root-b: shows what `gpt plan` makes, and writes it only after a `yes` typed on the console.
fn make_layout(opts: &mut Options) -> Result<(), String> {
    let has = |name| partition_named(name).map(|p| p.is_some());
    if !netinst_rules::needs_layout(has(gptw::ROOT_A)?, has(gptw::ROOT_B)?) {
        return Ok(());
    }
    println!(
        "netinstall: LUN 0 has no Lucina layout (root-a and root-b). This is what gpt write would make:"
    );
    gpt(opts, "plan")?;
    print!("type yes to write this table to LUN 0: ");
    let _ = io::stdout().flush();
    if !netinst_rules::yes_typed(&read_tty_line()?) {
        return Err("no yes was typed on the console: nothing was written".into());
    }
    println!("netinstall: yes was typed on the console; writing the table");
    let plan = gptw::plan(
        &RawTable::read()?.view(),
        opts.expected_esp(),
        root_blocks(opts),
    )?;
    opts.sha = Some(plan.sha256);
    gpt(opts, "write")
}

/// `netinstall --server http://<host>:<port> --keys <file> --board <id> [--go-back] [install's options]`: the signed manifest,
/// then the image it names chunk by chunk through `install` as `--root` runs it.
fn netinstall(args: &[String]) -> Result<(), String> {
    let (net, rest) = net_options(args)?;
    let mut opts = Options::parse(&rest)?;
    opts.root = true;
    let keys = fs::read_to_string(&net.keys).map_err(|e| format!("{}: {e}", net.keys))?;
    let installed = installed_seq(&opts)?;
    match installed {
        Some(n) => println!("netinstall: installed sequence on this machine: {n}"),
        None => println!("netinstall: no installed sequence on this machine"),
    }
    let m = netinst::fetch_manifest(&net.server, &net.board, &keys, installed, net.go_back)?;
    println!(
        "netinstall: manifest verified: board {}, commit {}, date {}, seq {}, {} bytes in {} chunks",
        m.board,
        m.commit,
        m.date,
        m.seq,
        m.size,
        m.chunks.len()
    );
    make_layout(&mut opts)?;
    let seq = m.seq;
    let chunks = VerifiedChunks::new(&net.server, &format!("/{}/disk-stream", net.board), m)?;
    install_from(&opts, BufReader::with_capacity(1 << 16, chunks))?;
    write_seq(&opts, seq)
        .map_err(|e| format!("INSTALLED, but the sequence {seq} was not recorded: {e}"))?;
    println!("netinstall: sequence {seq} recorded");
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (command, rest) = match args.first().map(String::as_str) {
        Some("status") => ("status", &args[1..]),
        Some("rollback") => ("rollback", &args[1..]),
        Some("data") => ("data", &args[1..]),
        Some("gpt") if args.len() >= 2 => ("gpt", &args[2..]),
        Some("apps") => ("apps", &args[1..]),
        Some("apps-status") => ("apps-status", &args[1..]),
        Some("apps-rollback") => ("apps-rollback", &args[1..]),
        Some("netinstall") => return finish(netinstall(&args[1..])),
        _ => ("install", &args[..]),
    };
    let result = Options::parse(rest).and_then(|opts| match command {
        "status" => status(&opts),
        "rollback" => rollback(&opts),
        "data" => data(&opts),
        "gpt" => gpt(&opts, &args[1]),
        "apps" => install_apps(&opts),
        "apps-status" => apps_status(&opts),
        "apps-rollback" => apps_rollback(&opts),
        _ => install(&opts),
    });
    finish(result)
}

fn finish(result: Result<(), String>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            println!("FAILED: {e}");
            eprintln!("lucina-install-disk: {e}");
            ExitCode::FAILURE
        }
    }
}
