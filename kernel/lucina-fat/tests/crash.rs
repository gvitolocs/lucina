//! Power cuts at every device write, on a volume with the geometry of the Galaxy Book Go's EFI system partition.
//!
//! See `tests/common/crash.rs` for how the states are made and checked. The scenarios are what `lucina-install-disk` does
//! (temporary file, fsync, rename over the target, fsync) and the ways that can go wrong around it.
//!
//! `install_full_sizes` is the slow one: real file sizes, about 6000 cuts. It runs with `LUCINA_FAT_CRASH_FULL=1`.

mod common;

use std::{
    os::unix::fs::FileExt,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
};

use common::{
    FileDisk,
    crash::{Content, Fault, LogDisk, Mark, Op, Replay, Stats, replay},
    edk2::{Fw, FwType},
    fsck, mkfs, mount, opts,
    tree::Tree,
};
use lucina_fat::{Error, MountOptions, Volume};

/// LBA 256-66815 of 4096 bytes.
const ESP_KIB: u64 = 66560 * 4;
const BOOT: &str = "/EFI/BOOT/BOOTAA64.EFI";
const WINDOWS: &str = "/EFI/Microsoft/Boot/bootmgfw.efi";
const KERNEL: &str = "/asterinas/KERNEL-A0.IMG";
const CONFIG: &str = "/asterinas/BOOT.CFG";

fn esp(name: &str) -> PathBuf {
    mkfs(name, ESP_KIB, &["-F", "32", "-S", "4096", "-s", "1"])
}

fn content(seed: u64, len: usize) -> Content {
    let mut r = common::Rng(seed | 1);
    let mut v = vec![0u8; len];
    for c in v.chunks_mut(8) {
        let x = r.next().to_le_bytes();
        c.copy_from_slice(&x[..c.len()]);
    }
    Arc::new(v)
}

fn write_all(t: &mut Tree<impl lucina_fat::BlockIo>, path: &str, data: &[u8]) {
    let mut n = match t.resolve(path) {
        Ok(n) => n,
        Err(Error::NotFound) => t.create(path).unwrap(),
        Err(e) => panic!("{path}: {e:?}"),
    };
    for (i, c) in data.chunks(64 * 1024).enumerate() {
        assert_eq!(
            t.vol.write(&mut n, i as u64 * 64 * 1024, c).unwrap(),
            c.len()
        );
    }
    t.vol.sync_node(&mut n).unwrap();
}

/// The directories and the files of a volume as it was before the installer.
fn populate(base: &Path, files: &[(&str, Content)]) {
    let mut t = Tree::new(mount(base));
    for d in [
        "/EFI",
        "/EFI/BOOT",
        "/EFI/Microsoft",
        "/EFI/Microsoft/Boot",
        "/asterinas",
    ] {
        if t.resolve(d).is_err() {
            t.mkdir(d).unwrap();
        }
    }
    for (p, c) in files {
        write_all(&mut t, p, c);
    }
    t.sync().unwrap();
    let (ok, text) = fsck(base);
    assert!(ok, "the base image is not clean:\n{text}");
}

/// The scenario's side: a volume on a logging disk.
struct Sim {
    t: Tree<LogDisk>,
    log: Arc<Mutex<Vec<Op>>>,
}

impl Sim {
    fn new(base: &Path) -> Sim {
        Self::with_opts(base, opts())
    }

    fn with_opts(base: &Path, o: MountOptions) -> Sim {
        let log = Arc::new(Mutex::new(Vec::new()));
        let disk = LogDisk::open(base, log.clone());
        Sim {
            t: Tree::new(Volume::mount(disk, o).unwrap()),
            log,
        }
    }

    fn mark(&self, m: Mark) {
        self.log.lock().unwrap().push(Op::Mark(m));
    }

    fn allow(&self, p: &str, c: Option<Content>) {
        self.mark(Mark::Allow(p.into(), c));
    }

    fn promise(&self, p: &str, c: Option<Content>) {
        self.mark(Mark::Promise(p.into(), c));
    }

    /// fsync(2) of a file: its entry, then everything to the device, but the volume stays marked in use.
    fn fsync(&mut self) {
        self.t.vol.checkpoint().unwrap();
    }

    /// The installer's step: write a temporary file next to the target, make it durable, rename it over the target and
    /// make that durable.
    fn replace(&mut self, path: &str, new: &Content) {
        let tmp = format!("{path}.new");
        write_all(&mut self.t, &tmp, new);
        self.fsync();
        self.allow(path, Some(new.clone()));
        self.t.rename(&tmp, path).unwrap();
        self.fsync();
        self.promise(path, Some(new.clone()));
    }

    fn finish(mut self) -> Vec<Op> {
        self.t.sync().unwrap();
        self.log.lock().unwrap().clone()
    }
}

fn run(name: &str, base: &Path, log: &[Op], initial: &[Mark], how: Replay) -> Stats {
    let s = replay(name, base, log, initial, how);
    eprintln!("{name}: {s:?}");
    s
}

fn have(p: &str, c: &Content) -> Mark {
    Mark::Promise(p.into(), Some(c.clone()))
}

fn absent(p: &str) -> Mark {
    Mark::Promise(p.into(), None)
}

/// Rebuilds the disk from the log up to (not including) op `cut` in a new image.
fn image_at(base: &Path, log: &[Op], cut: usize, name: &str) -> PathBuf {
    let path = common::tmp_dir().join("crash").join(format!("{name}.img"));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    assert!(
        Command::new("cp")
            .args(["--sparse=always", "--reflink=auto"])
            .arg(base)
            .arg(&path)
            .status()
            .unwrap()
            .success()
    );
    let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    for op in &log[..cut] {
        if let Op::Write(off, d) = op {
            f.write_all_at(d, *off).unwrap();
        }
    }
    path
}

struct Sizes {
    boot_old: usize,
    boot_new: usize,
    kernel_old: usize,
    kernel_new: usize,
}

const QUICK: Sizes = Sizes {
    boot_old: 40_000,
    boot_new: 60_000,
    kernel_old: 200_000,
    kernel_new: 300_000,
};

/// Replaces the boot loader, the kernel and the configuration of an installed volume.
fn install_over_existing(name: &str, base: PathBuf, sizes: &Sizes, how: Replay) {
    let (boot_old, boot_new) = (content(1, sizes.boot_old), content(2, sizes.boot_new));
    let (kernel_old, kernel_new) = (content(3, sizes.kernel_old), content(4, sizes.kernel_new));
    let (cfg_old, cfg_new) = (content(5, 300), content(6, 350));
    let windows = content(7, 5000);
    populate(
        &base,
        &[
            (BOOT, boot_old.clone()),
            (WINDOWS, windows.clone()),
            (KERNEL, kernel_old.clone()),
            (CONFIG, cfg_old.clone()),
        ],
    );
    let mut sim = Sim::new(&base);
    sim.replace(BOOT, &boot_new);
    sim.replace(KERNEL, &kernel_new);
    sim.replace(CONFIG, &cfg_new);
    let log = sim.finish();
    let initial = [
        have(BOOT, &boot_old),
        have(WINDOWS, &windows),
        have(KERNEL, &kernel_old),
        have(CONFIG, &cfg_old),
    ];
    run(name, &base, &log, &initial, how);

    // The volume as the last write left it: nothing to find at all.
    let end = image_at(&base, &log, log.len(), &format!("{name}.end"));
    let (ok, text) = fsck(&end);
    assert!(ok, "{name}: not clean at the end:\n{text}");
    let fw = Fw::open(&end).unwrap();
    assert_eq!(fw.read_file(KERNEL).unwrap().unwrap(), *kernel_new);
    assert_eq!(fw.read_file(BOOT).unwrap().unwrap(), *boot_new);
    assert_eq!(fw.read_file(CONFIG).unwrap().unwrap(), *cfg_new);
    assert_eq!(fw.read_file(&format!("{KERNEL}.new")).unwrap(), None);
    std::fs::remove_file(&end).unwrap();

    // A second install after a cut, as the installer would run after the power came back.
    let cuts: Vec<usize> = (1..8).map(|k| k * log.len() / 8).collect();
    for (k, &cut) in cuts.iter().enumerate() {
        let img = image_at(&base, &log, cut, &format!("{name}.again{k}"));
        let mut t = Tree::new(mount(&img));
        for p in [BOOT, KERNEL, CONFIG] {
            let tmp = format!("{p}.new");
            if t.resolve(&tmp).is_ok() {
                t.unlink(&tmp).unwrap();
            }
        }
        for (p, c) in [(BOOT, &boot_new), (KERNEL, &kernel_new), (CONFIG, &cfg_new)] {
            let tmp = format!("{p}.new");
            write_all(&mut t, &tmp, c);
            t.vol.checkpoint().unwrap();
            t.rename(&tmp, p).unwrap();
            t.vol.checkpoint().unwrap();
        }
        t.sync().unwrap();
        drop(t);
        let (_, text) = fsck(&img);
        for l in text.lines() {
            assert!(
                !l.contains("FATs differ") && !l.contains("File size"),
                "{name}: after installing again from cut {cut}: {l}\n{text}"
            );
        }
        let fw = Fw::open(&img).unwrap();
        fw.check_free_hint().unwrap();
        assert_eq!(
            fw.read_file(KERNEL).unwrap().unwrap(),
            *kernel_new,
            "cut {cut}"
        );
        std::fs::remove_file(&img).unwrap();
    }
}

#[test]
fn install_over_existing_files() {
    install_over_existing("over", esp("crash-over"), &QUICK, Replay::default());
}

/// `mkfs.fat -S 4096` alone picks FAT16 on this size: the other way the ESP could be formatted, and what firmware reads
/// with the same driver. Windows does FAT32, which is the test above.
#[test]
fn install_on_fat16_with_4096_byte_sectors() {
    let base = mkfs("crash-f16", ESP_KIB, &["-S", "4096"]);
    assert_eq!(Fw::open(&base).unwrap().fat_type, FwType::Fat16);
    install_over_existing(
        "f16",
        base,
        &QUICK,
        Replay {
            mtools_samples: 0,
            ..Replay::default()
        },
    );
}

/// A stick: FAT32 with 512-byte sectors and 4096-byte clusters, whose clusters do not start on a 4096-byte block.
#[test]
fn install_on_a_stick_with_512_byte_sectors() {
    let base = mkfs(
        "crash-stick",
        ESP_KIB,
        &["-F", "32", "-S", "512", "-s", "8"],
    );
    install_over_existing(
        "stick",
        base,
        &QUICK,
        Replay {
            mtools_samples: 0,
            ..Replay::default()
        },
    );
}

#[test]
fn install_full_sizes() {
    if std::env::var_os("LUCINA_FAT_CRASH_FULL").is_none() {
        eprintln!(
            "install_full_sizes: set LUCINA_FAT_CRASH_FULL=1 to run it (about 6000 cuts, 5 minutes)"
        );
        return;
    }
    let sizes = Sizes {
        boot_old: 5_500_000,
        boot_new: 6_200_000,
        kernel_old: 13_000_000,
        kernel_new: 14_500_000,
    };
    install_over_existing(
        "full",
        esp("crash-full"),
        &sizes,
        Replay {
            subsets: 4,
            exhaustive_up_to: 16,
            mtools_samples: 40,
        },
    );
}

/// The first install: no directory, no file of ours; the firmware's own files stay.
#[test]
fn first_install_makes_directories() {
    let base = esp("crash-first");
    let windows = content(7, 5000);
    let mut t = Tree::new(mount(&base));
    for d in ["/EFI", "/EFI/Microsoft", "/EFI/Microsoft/Boot"] {
        t.mkdir(d).unwrap();
    }
    write_all(&mut t, WINDOWS, &windows);
    t.sync().unwrap();
    drop(t);
    let (boot, kernel, cfg) = (content(2, 60_000), content(4, 300_000), content(6, 350));
    let mut sim = Sim::new(&base);
    sim.t.mkdir("/EFI/BOOT").unwrap();
    sim.t.mkdir("/asterinas").unwrap();
    sim.replace(BOOT, &boot);
    sim.replace(KERNEL, &kernel);
    sim.replace(CONFIG, &cfg);
    let log = sim.finish();
    let initial = [
        have(WINDOWS, &windows),
        absent(BOOT),
        absent(KERNEL),
        absent(CONFIG),
    ];
    run("first", &base, &log, &initial, Replay::default());
}

/// The old file is deleted and its clusters are free for the next file, without a sync in between: the new file's data
/// must not overwrite clusters that the entry on disk still names.
#[test]
fn deleted_clusters_are_not_overwritten_before_the_delete_is_durable() {
    let base = esp("crash-reuse");
    let (a, b) = (content(1, 500_000), content(2, 500_000));
    let windows = content(7, 5000);
    populate(
        &base,
        &[("/asterinas/A.BIN", a.clone()), (WINDOWS, windows.clone())],
    );
    let mut sim = Sim::new(&base);
    // Make the allocator look at A's clusters first.
    let _ = sim.t.vol.statfs();
    sim.allow("/asterinas/A.BIN", None);
    sim.t.unlink("/asterinas/A.BIN").unwrap();
    sim.t.vol.rewind_allocator();
    write_all(&mut sim.t, "/asterinas/B.BIN", &b);
    sim.allow("/asterinas/B.BIN", Some(b.clone()));
    sim.fsync();
    sim.promise("/asterinas/A.BIN", None);
    sim.promise("/asterinas/B.BIN", Some(b.clone()));
    let log = sim.finish();
    let initial = [
        have("/asterinas/A.BIN", &a),
        have(WINDOWS, &windows),
        absent("/asterinas/B.BIN"),
    ];
    run("reuse", &base, &log, &initial, Replay::default());
}

/// A directory of more than one cluster, with the target and the temporary file in different blocks, and a temporary
/// file in another directory.
#[test]
fn rename_over_a_target_in_another_block_or_directory() {
    let base = esp("crash-span");
    let (old, new, new2) = (content(1, 90_000), content(2, 120_000), content(3, 80_000));
    let keep = content(4, 3000);
    let mut files: Vec<(String, Content)> = vec![
        ("/asterinas/TARGET.BIN".into(), old.clone()),
        ("/asterinas/KEEP.BIN".into(), keep.clone()),
    ];
    for i in 0..200 {
        files.push((
            format!("/asterinas/filler number {i:03}.bin"),
            content(100 + i, 10),
        ));
    }
    let refs: Vec<(&str, Content)> = files.iter().map(|(p, c)| (p.as_str(), c.clone())).collect();
    populate(&base, &refs);
    let mut sim = Sim::new(&base);
    // The temporary file lands behind the fillers, in another block than TARGET.BIN.
    write_all(&mut sim.t, "/asterinas/TARGET.BIN.new", &new);
    sim.fsync();
    sim.allow("/asterinas/TARGET.BIN", Some(new.clone()));
    sim.t
        .rename("/asterinas/TARGET.BIN.new", "/asterinas/TARGET.BIN")
        .unwrap();
    sim.fsync();
    sim.promise("/asterinas/TARGET.BIN", Some(new.clone()));
    // Now from another directory.
    write_all(&mut sim.t, "/EFI/moved.new", &new2);
    sim.fsync();
    sim.allow("/asterinas/TARGET.BIN", Some(new2.clone()));
    sim.t
        .rename("/EFI/moved.new", "/asterinas/TARGET.BIN")
        .unwrap();
    sim.fsync();
    sim.promise("/asterinas/TARGET.BIN", Some(new2.clone()));
    let log = sim.finish();
    let initial = [
        have("/asterinas/TARGET.BIN", &old),
        have("/asterinas/KEEP.BIN", &keep),
    ];
    run("span", &base, &log, &initial, Replay::default());
}

/// The directory grows by a cluster for the temporary file: the new cluster must be empty when the chain reaches it.
#[test]
fn directory_growth_never_shows_a_stale_cluster() {
    let base = esp("crash-grow");
    let junk = content(9, 8192);
    // Free clusters full of old directory-looking data: what a deleted file leaves behind.
    let keep = content(4, 3000);
    populate(
        &base,
        &[
            ("/asterinas/KEEP.BIN", keep.clone()),
            ("/asterinas/JUNK.BIN", junk),
        ],
    );
    {
        let mut t = Tree::new(mount(&base));
        t.unlink("/asterinas/JUNK.BIN").unwrap();
        t.sync().unwrap();
    }
    let mut sim = Sim::new(&base);
    sim.t.vol.rewind_allocator();
    let names: Vec<String> = (0..60)
        .map(|i| format!("/asterinas/a rather long file name {i:03}.bin"))
        .collect();
    for n in &names {
        sim.t.create(n).unwrap();
    }
    sim.fsync();
    let log = sim.finish();
    run(
        "grow",
        &base,
        &log,
        &[have("/asterinas/KEEP.BIN", &keep)],
        Replay::default(),
    );
}

/// Nearly full: a replacement that does not fit fails, the old file stays, the temporary file goes and its clusters
/// come back; one that fits succeeds even though old and new together only just fit.
#[test]
fn low_free_space() {
    let base = esp("crash-full-disk");
    let old = content(1, 200_000);
    populate(&base, &[("/asterinas/K.IMG", old.clone())]);
    let free_kib;
    {
        let mut t = Tree::new(mount(&base));
        let free = t.vol.statfs().unwrap().free_clusters as usize;
        free_kib = free * 4;
        // Leave 300 KiB.
        let fill = (free - 75) * 4096;
        let mut n = t.create("/filler.bin").unwrap();
        let chunk = vec![0x5au8; 1 << 20];
        let mut done = 0;
        while done < fill {
            let k = chunk.len().min(fill - done);
            assert_eq!(t.vol.write(&mut n, done as u64, &chunk[..k]).unwrap(), k);
            done += k;
        }
        t.vol.sync_node(&mut n).unwrap();
        t.sync().unwrap();
        assert_eq!(t.vol.statfs().unwrap().free_clusters, 75);
    }
    assert!(free_kib > 200_000);
    let (too_big, fits) = (content(2, 400_000), content(3, 290_000));
    let mut sim = Sim::new(&base);
    // Too big: a short write, then ENOSPC. The installer gives up and removes the temporary file.
    let mut n = sim.t.create("/asterinas/K.IMG.new").unwrap();
    let w = sim.t.vol.write(&mut n, 0, &too_big).unwrap();
    assert_eq!(w, 300 * 1024, "a short write up to the free space");
    assert_eq!(
        sim.t.vol.write(&mut n, w as u64, &too_big[w..]),
        Err(Error::NoSpace)
    );
    sim.t.vol.sync_node(&mut n).unwrap();
    sim.fsync();
    sim.t.unlink("/asterinas/K.IMG.new").unwrap();
    sim.fsync();
    assert_eq!(sim.t.vol.statfs().unwrap().free_clusters, 75);
    // Fits.
    sim.replace("/asterinas/K.IMG", &fits);
    // The old file's clusters came back.
    assert_eq!(sim.t.vol.statfs().unwrap().free_clusters, 75 - 71 + 49);
    // And the next replacement reuses them.
    let again = content(4, 200_000);
    sim.replace("/asterinas/K.IMG", &again);
    let log = sim.finish();
    run(
        "lowfree",
        &base,
        &log,
        &[have("/asterinas/K.IMG", &old)],
        Replay::default(),
    );
}

/// A plain rename with no target, within a block: one write, so old name or new name, never both or neither.
#[test]
fn plain_rename_within_a_block_is_atomic() {
    let base = esp("crash-plain");
    let a = content(1, 50_000);
    populate(
        &base,
        &[("/asterinas/first name of the file.bin", a.clone())],
    );
    let mut sim = Sim::new(&base);
    let before = sim.log.lock().unwrap().len();
    sim.t
        .rename(
            "/asterinas/first name of the file.bin",
            "/asterinas/second, longer name of the file.bin",
        )
        .unwrap();
    sim.fsync();
    {
        let log = sim.log.lock().unwrap();
        let dir_writes = log[before..]
            .iter()
            .filter(
                |o| matches!(o, Op::Write(_, d) if d.windows(10).any(|w| w == b"s\0e\0c\0o\0n\0")),
            )
            .count();
        assert_eq!(dir_writes, 1, "the entry moves in one block write");
    }
    let log = sim.finish();
    run("plain", &base, &log, &[], Replay::default());
}

/// A plain rename across blocks or directories cannot be one write. The new name comes first: for a moment both names
/// lead to the same clusters, which fsck.fat reports, and the file is never lost.
#[test]
fn plain_rename_across_directories_never_loses_the_file() {
    let base = esp("crash-move");
    let a = content(1, 50_000);
    populate(&base, &[("/asterinas/MOVE.BIN", a.clone())]);
    let mut sim = Sim::new(&base);
    sim.mark(Mark::AllowFsck("share clusters"));
    sim.mark(Mark::AllowFsck("Truncating"));
    sim.mark(Mark::AllowFsck("Duplicate"));
    sim.mark(Mark::AllowFsck("/EFI/MOVE.BIN"));
    sim.mark(Mark::AllowFsck("/asterinas/MOVE.BIN"));
    sim.mark(Mark::AllowFsck("Cluster"));
    sim.mark(Mark::AllowFsck("File size"));
    sim.allow("/EFI/MOVE.BIN", Some(a.clone()));
    sim.t
        .rename("/asterinas/MOVE.BIN", "/EFI/MOVE.BIN")
        .unwrap();
    sim.fsync();
    sim.promise("/EFI/MOVE.BIN", Some(a.clone()));
    let log = sim.finish();
    run(
        "move",
        &base,
        &log,
        &[absent("/EFI/MOVE.BIN")],
        Replay::default(),
    );
}

/// In-place changes of a file (truncate, append) are not what the installer does; a cut leaves a file whose size and
/// chain disagree in the harmless direction, and everything else alone.
#[test]
fn truncate_and_append_leave_other_files_alone() {
    let base = esp("crash-trunc");
    let (a, keep) = (content(1, 100_000), content(2, 7000));
    populate(
        &base,
        &[
            ("/asterinas/A.BIN", a),
            ("/asterinas/KEEP.BIN", keep.clone()),
        ],
    );
    let mut sim = Sim::new(&base);
    sim.mark(Mark::AllowFsck("/asterinas/A.BIN"));
    sim.mark(Mark::AllowFsck("File size"));
    sim.mark(Mark::AllowFsck("Truncating"));
    sim.t.truncate("/asterinas/A.BIN", 30_000).unwrap();
    sim.fsync();
    sim.t.truncate("/asterinas/A.BIN", 0).unwrap();
    sim.fsync();
    sim.t
        .write_file("/asterinas/A.BIN", 0, &[1u8; 20_000])
        .unwrap();
    sim.fsync();
    sim.t
        .write_file("/asterinas/A.BIN", 20_000, &[2u8; 20_000])
        .unwrap();
    sim.fsync();
    let log = sim.finish();
    run(
        "trunc",
        &base,
        &log,
        &[have("/asterinas/KEEP.BIN", &keep)],
        Replay::default(),
    );
}

/// With `mark_dirty: false` the block with the boot sector is never written: the one block that has no second copy
/// firmware would use.
#[test]
fn no_dirty_bit_means_block_zero_is_never_written() {
    let base = esp("crash-nodirty");
    let (old, new) = (content(1, 100_000), content(2, 120_000));
    populate(&base, &[(KERNEL, old.clone())]);
    let mut sim = Sim::with_opts(
        &base,
        MountOptions {
            mark_dirty: false,
            ..opts()
        },
    );
    sim.replace(KERNEL, &new);
    let log = sim.finish();
    assert!(
        !log.iter().any(|o| matches!(o, Op::Write(0, _))),
        "block 0 was written"
    );
    run(
        "nodirty",
        &base,
        &log,
        &[have(KERNEL, &old)],
        Replay::default(),
    );
}

/// fsync issues a flush after the writes, and the classes of writes are separated by flushes.
#[test]
fn every_group_of_writes_ends_in_a_flush() {
    let base = esp("crash-flush");
    let (old, new) = (content(1, 100_000), content(2, 120_000));
    populate(&base, &[(KERNEL, old)]);
    let mut sim = Sim::new(&base);
    sim.replace(KERNEL, &new);
    let log = sim.log.lock().unwrap().clone();
    let mut unflushed = 0;
    for op in &log {
        match op {
            Op::Write(..) => unflushed += 1,
            Op::Flush => unflushed = 0,
            Op::Mark(Mark::Promise(..)) => {
                assert_eq!(unflushed, 0, "a promise with writes not flushed")
            }
            Op::Mark(_) => {}
        }
    }
    assert_eq!(unflushed, 0);
    drop(sim);
}

/// A device that starts failing: the error reaches the caller, and every later sync fails too rather than pretend, and
/// what is on the device at that point is a state like any cut.
#[test]
fn device_errors_reach_the_caller_and_stick() {
    let base = esp("crash-fault");
    let (old, new) = (content(1, 60_000), content(2, 70_000));
    populate(&base, &[(KERNEL, old.clone())]);
    let total = {
        let mut sim = Sim::new(&base);
        sim.replace(KERNEL, &new);
        let log = sim.finish();
        log.iter()
            .filter(|o| matches!(o, Op::Write(..) | Op::Flush))
            .count()
    };
    let mut failed_runs = 0;
    for after in (0..total).step_by((total / 12).max(1)) {
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut disk = LogDisk::open(&base, log.clone());
        disk.fault = Fault {
            after_ops: Some(after),
        };
        let mut t = Tree::new(Volume::mount(disk, opts()).unwrap());
        let tmp = format!("{KERNEL}.new");
        let result = (|| -> lucina_fat::Result<()> {
            let mut n = t.create(&tmp)?;
            t.vol.write(&mut n, 0, &new)?;
            t.vol.sync_node(&mut n)?;
            t.vol.checkpoint()?;
            t.rename(&tmp, KERNEL)?;
            t.vol.checkpoint()?;
            t.vol.sync()
        })();
        assert!(
            result.is_err(),
            "failing after op {after} of {total} went unnoticed"
        );
        failed_runs += 1;
        assert_eq!(
            t.vol.checkpoint(),
            Err(Error::Io),
            "a later fsync must fail too"
        );
        assert_eq!(t.vol.sync(), Err(Error::Io), "a later sync must fail too");
        let ops = log.lock().unwrap().clone();
        let img = image_at(&base, &ops, ops.len(), &format!("fault{after}"));
        let fw = Fw::open(&img).unwrap();
        let got = fw.read_file(KERNEL).unwrap().unwrap();
        assert!(
            got == *old || got == *new,
            "after failing at op {after}: {} bytes",
            got.len()
        );
        std::fs::remove_file(&img).unwrap();
    }
    assert!(failed_runs > 3);
}

/// A boot sector like Windows' on a 4096-byte disk: the signature at the end of the sector as well, or there alone, and
/// FSInfo's last signature there too. All of these mount, and the unknown-free-space hint is honoured.
#[test]
fn windows_style_boot_sector_mounts() {
    let base = esp("crash-win");
    populate(&base, &[("/asterinas/A.BIN", content(1, 5000))]);
    let f = std::fs::OpenOptions::new().write(true).open(&base).unwrap();
    // OEM name, as Windows writes it, and the signature only at the end of the sector.
    f.write_all_at(b"MSDOS5.0", 3).unwrap();
    f.write_all_at(&[0, 0], 510).unwrap();
    f.write_all_at(&[0x55, 0xAA], 4094).unwrap();
    // FSInfo: the trailing signature at the end of the sector, free count unknown.
    f.write_all_at(&[0, 0, 0, 0], 4096 + 508).unwrap();
    f.write_all_at(&0xAA55_0000u32.to_le_bytes(), 4096 + 4092)
        .unwrap();
    f.write_all_at(&u32::MAX.to_le_bytes(), 4096 + 488).unwrap();
    drop(f);
    let mut t = Tree::new(mount(&base));
    let free = t.vol.statfs().unwrap().free_clusters;
    assert!(free > 60000, "counted, not taken from the hint: {free}");
    assert_eq!(t.read_file("/asterinas/A.BIN").unwrap().len(), 5000);
    t.write_file("/asterinas/B.BIN", 0, b"new").unwrap();
    t.sync().unwrap();
    assert_eq!(t.vol.statfs().unwrap().free_clusters, free - 1);
}

/// An FSInfo that lies (what a crash, or another system, can leave) must not make the volume look full or free.
#[test]
fn a_lying_free_count_is_not_believed_for_writes() {
    let base = esp("crash-lie");
    populate(&base, &[("/asterinas/A.BIN", content(1, 5000))]);
    let f = std::fs::OpenOptions::new().write(true).open(&base).unwrap();
    f.write_all_at(&0u32.to_le_bytes(), 4096 + 488).unwrap();
    drop(f);
    let mut t = Tree::new(mount(&base));
    t.write_file("/asterinas/B.BIN", 0, b"still room").unwrap();
    t.sync().unwrap();
    drop(t);
    let (ok, text) = fsck(&base);
    assert!(ok, "the real count must be back:\n{text}");
}

/// A cut between FAT copies leaves them different. The next mount that writes makes them equal again, from the first.
#[test]
fn different_fat_copies_are_made_equal_by_the_next_change() {
    let base = esp("crash-fats");
    populate(&base, &[("/asterinas/A.BIN", content(1, 5000))]);
    // Change an entry of the second FAT only.
    let g = {
        let t = Tree::new(mount(&base));
        t.vol.geometry().clone()
    };
    let f = std::fs::OpenOptions::new().write(true).open(&base).unwrap();
    f.write_all_at(
        &0x0FFF_FFFFu32.to_le_bytes(),
        g.fat_start + g.fat_bytes + 4 * 5000,
    )
    .unwrap();
    drop(f);
    let (ok, text) = fsck(&base);
    assert!(!ok && text.contains("FATs differ"), "{text}");
    let mut t = Tree::new(mount(&base));
    t.write_file("/asterinas/B.BIN", 0, b"x").unwrap();
    t.sync().unwrap();
    drop(t);
    let (ok, text) = fsck(&base);
    assert!(ok, "{text}");
}

/// The names the firmware looks up, in the case and form it looks them up, and a temporary name next to them.
#[test]
fn firmware_finds_the_files_by_name() {
    let base = esp("crash-names");
    populate(
        &base,
        &[
            (BOOT, content(1, 1000)),
            (KERNEL, content(2, 1000)),
            (CONFIG, content(3, 100)),
            ("/asterinas/KERNEL-A1.IMG", content(4, 1000)),
            ("/asterinas/KERNEL-A0.IMG.new", content(5, 1000)),
        ],
    );
    let fw = Fw::open(&base).unwrap();
    for (p, len) in [
        ("/EFI/BOOT/BOOTAA64.EFI", 1000),
        ("/efi/boot/bootaa64.efi", 1000),
        ("/asterinas/KERNEL-A0.IMG", 1000),
        ("/ASTERINAS/kernel-a0.img", 1000),
        ("/asterinas/KERNEL-A1.IMG", 1000),
        ("/asterinas/BOOT.CFG", 100),
        ("/asterinas/KERNEL-A0.IMG.new", 1000),
    ] {
        assert_eq!(fw.read_file(p).unwrap().map(|d| d.len()), Some(len), "{p}");
    }
    // In the case the name was given, in 8.3 where it fits, with a long name where it does not.
    let root = fw.list(2).unwrap();
    assert!(root.iter().any(|e| e.name == "EFI"));
    assert!(root.iter().any(|e| e.name == "asterinas"));
    let asterinas = root.iter().find(|e| e.name == "asterinas").unwrap().cluster;
    let names: Vec<String> = fw
        .list(asterinas)
        .unwrap()
        .iter()
        .map(|e| e.name.clone())
        .collect();
    for want in [
        "KERNEL-A0.IMG",
        "BOOT.CFG",
        "KERNEL-A1.IMG",
        "KERNEL-A0.IMG.new",
    ] {
        assert!(names.iter().any(|n| n == want), "{want} in {names:?}");
    }
    // Other tools agree.
    drop(fw);
    let _ = FileDisk::open(&base);
    let (ok, text) = fsck(&base);
    assert!(ok, "{text}");
}

/// The generation install: new files under new names, then one block of BOOT.CFG is the commit.
#[test]
fn generation_install_commits_with_one_block_write() {
    let base = esp("crash-gen");
    let windows = content(7, 5000);
    let (k1, i1) = (content(1, 200_000), content(2, 100_000));
    let (k2, i2) = (content(3, 300_000), content(4, 120_000));
    let old_cfg: Content =
        Arc::new(b"kernel = K1.IMG 0x80000000\ninitrd = I1.GZ\ncmdline = quiet\n".to_vec());
    let pad = |text: &[u8]| -> Content {
        let mut v = text.to_vec();
        v.resize(4095, b'#');
        v.push(b'\n');
        Arc::new(v)
    };
    let padded = pad(&old_cfg);
    let new_cfg = pad(b"kernel = K2.IMG 0x80000000\ninitrd = I2.GZ\ncmdline = quiet\n");
    populate(
        &base,
        &[
            (WINDOWS, windows.clone()),
            ("/asterinas/K1.IMG", k1.clone()),
            ("/asterinas/I1.GZ", i1.clone()),
            (CONFIG, old_cfg.clone()),
        ],
    );
    let mut sim = Sim::with_opts(
        &base,
        MountOptions {
            mark_dirty: false,
            keep_mtime: true,
            ..opts()
        },
    );
    // 1. The new files. Every name that exists keeps reading as it did.
    sim.allow("/asterinas/K2.IMG", Some(k2.clone()));
    sim.allow("/asterinas/K2.IMG", Some(Arc::new(Vec::new())));
    sim.allow("/asterinas/I2.GZ", Some(i2.clone()));
    sim.allow("/asterinas/I2.GZ", Some(Arc::new(Vec::new())));
    write_all(&mut sim.t, "/asterinas/K2.IMG", &k2);
    write_all(&mut sim.t, "/asterinas/I2.GZ", &i2);
    sim.fsync();
    sim.promise("/asterinas/K2.IMG", Some(k2.clone()));
    sim.promise("/asterinas/I2.GZ", Some(i2.clone()));
    // 3. The old configuration under another name.
    sim.allow("/asterinas/PREV.CFG", Some(old_cfg.clone()));
    sim.allow("/asterinas/PREV.CFG", Some(Arc::new(Vec::new())));
    write_all(&mut sim.t, "/asterinas/PREV.CFG", &old_cfg);
    sim.fsync();
    sim.promise("/asterinas/PREV.CFG", Some(old_cfg.clone()));
    // 4. The padding: what the first bytes say stays the same whether the new size got there or not.
    sim.allow(CONFIG, Some(padded.clone()));
    write_all(&mut sim.t, CONFIG, &padded);
    sim.fsync();
    sim.promise(CONFIG, Some(padded.clone()));
    // 5. The commit.
    let before = sim.log.lock().unwrap().len();
    sim.allow(CONFIG, Some(new_cfg.clone()));
    write_all(&mut sim.t, CONFIG, &new_cfg);
    sim.fsync();
    {
        let log = sim.log.lock().unwrap();
        let writes = log[before..]
            .iter()
            .filter(|o| matches!(o, Op::Write(..)))
            .count();
        let flushes = log[before..]
            .iter()
            .filter(|o| matches!(o, Op::Flush))
            .count();
        assert_eq!(
            (writes, flushes),
            (1, 1),
            "the commit is one block write and one flush"
        );
    }
    sim.promise(CONFIG, Some(new_cfg.clone()));
    let log = sim.finish();
    assert!(
        !log.iter().any(|o| matches!(o, Op::Write(0, _))),
        "block 0 was written"
    );
    run(
        "gen",
        &base,
        &log,
        &[
            have(WINDOWS, &windows),
            have("/asterinas/K1.IMG", &k1),
            have("/asterinas/I1.GZ", &i1),
            have(CONFIG, &old_cfg),
            absent("/asterinas/K2.IMG"),
            absent("/asterinas/I2.GZ"),
            absent("/asterinas/PREV.CFG"),
        ],
        Replay::default(),
    );
}

/// After going read-only every read comes from the device, not from what the volume remembers.
#[test]
fn remount_read_only_forgets_the_cached_blocks() {
    let base = esp("crash-remount");
    let data = content(1, 20_000);
    populate(&base, &[(KERNEL, data.clone())]);
    let mut t = Tree::new(mount(&base));
    assert_eq!(t.read_file(KERNEL).unwrap(), *data);
    // Another writer changes the file behind the volume; the cached copy hides that.
    let g = t.vol.geometry().clone();
    let n = t.resolve(KERNEL).unwrap();
    let off = g.cluster_offset(n.first_cluster);
    let f = std::fs::OpenOptions::new().write(true).open(&base).unwrap();
    f.write_all_at(b"changed on the disk", off).unwrap();
    assert_eq!(&t.read_file(KERNEL).unwrap()[..7], &data[..7]);
    t.vol.set_readonly(true).unwrap();
    assert_eq!(&t.read_file(KERNEL).unwrap()[..7], b"changed");
}
