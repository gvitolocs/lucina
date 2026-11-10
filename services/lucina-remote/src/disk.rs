//! LUN 0 as `lucina-install-disk`, `lucina-recover` and `lucina-data` use it: its table, its EFI system partition, raw blocks.
//! The UFS driver refuses every write to any other unit whatever these programs do.

use std::{
    ffi::CString,
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    os::unix::fs::FileExt,
    path::Path,
};

use lucina_boot::trial;
use lucina_ufs::gpt;
use sha2::{Digest, Sha256};

use crate::{
    disk_rules,
    gptw::{self, OnDisk},
    image::{self, Ext2Info},
    install::{CONFIG_BLOCK, Esp, check_fat, config_block, find_esp},
};

/// The main user LUN. There is no option for this: nothing else is ever a target.
pub const LUN0: &str = "/dev/ufs0lun0";
pub const LBA: u64 = CONFIG_BLOCK as u64;
pub const MOUNT: &str = "/mnt/lucina-esp";
pub const FOLDER: &str = "asterinas";
/// The loader the firmware runs (`started from` in LOG.TXT).
pub const LOADER: &str = "EFI/BOOT/BOOTAA64.EFI";
/// A commit is then one data block and a flush (kernel/lucina-fat).
const MOUNT_OPTIONS: &str = "nodirtybit,nomtime";

pub fn errno(what: &str) -> String {
    format!("{what}: {}", io::Error::last_os_error())
}

pub fn sync_all() {
    // SAFETY: Takes no arguments, always succeeds.
    unsafe { libc::sync() };
}

/// The ESP mounted at [`MOUNT`], unmounted on the way out of any path.
pub struct Mounted {
    mounted: bool,
}

impl Mounted {
    /// Mounts without touching block 0 (the dirty bit) or directory entries for a same-size write, where the file system
    /// has those options; otherwise as it is, and says so.
    pub fn new(device: &str, read_only: bool) -> Result<Mounted, String> {
        fs::create_dir_all(MOUNT).map_err(|e| format!("{MOUNT}: {e}"))?;
        let cstr = |s: &str| CString::new(s).map_err(|_| format!("{s}: has a NUL"));
        let (dev, dir, fstype) = (cstr(device)?, cstr(MOUNT)?, cstr("vfat")?);
        let options = cstr(MOUNT_OPTIONS)?;
        let flags = if read_only { libc::MS_RDONLY } else { 0 };
        let mount = |data: *const libc::c_void| {
            // SAFETY: The strings outlive the call; `data` is a NUL-terminated string or null.
            unsafe { libc::mount(dev.as_ptr(), dir.as_ptr(), fstype.as_ptr(), flags, data) }
        };
        if mount(options.as_ptr().cast()) == 0 {
            return Ok(Mounted { mounted: true });
        }
        if mount(core::ptr::null()) != 0 {
            return Err(errno(&format!("mount {device} on {MOUNT}")));
        }
        println!("mounted without {MOUNT_OPTIONS}: this kernel does not have them");
        Ok(Mounted { mounted: true })
    }

    pub fn unmount(&mut self) -> Result<(), String> {
        if !self.mounted {
            return Ok(());
        }
        sync_all();
        let dir = CString::new(MOUNT).expect("no NUL");
        // SAFETY: The string outlives the call.
        if unsafe { libc::umount(dir.as_ptr()) } != 0 {
            return Err(errno(&format!("umount {MOUNT}")));
        }
        self.mounted = false;
        Ok(())
    }
}

impl Drop for Mounted {
    fn drop(&mut self) {
        let _ = self.unmount();
    }
}

pub fn on_esp(path: &str) -> String {
    format!("{MOUNT}/{path}")
}

pub fn in_folder(name: &str) -> String {
    on_esp(&format!("{FOLDER}/{name}"))
}

/// Reads whole logical blocks of a block device.
pub fn read_blocks(device: &str, lba: u64, count: usize) -> Result<Vec<u8>, String> {
    let mut f = File::open(device).map_err(|e| format!("{device}: {e}"))?;
    f.seek(SeekFrom::Start(lba * LBA))
        .map_err(|e| format!("{device}: {e}"))?;
    let mut b = vec![0u8; count * CONFIG_BLOCK];
    f.read_exact(&mut b)
        .map_err(|e| format!("reading {device}: {e}"))?;
    Ok(b)
}

/// The size of a block device or partition, in logical blocks.
pub fn device_blocks(device: &str) -> Result<u64, String> {
    let mut f = File::open(device).map_err(|e| format!("{device}: {e}"))?;
    let bytes = f
        .seek(SeekFrom::End(0))
        .map_err(|e| format!("{device}: {e}"))?;
    Ok(bytes / LBA)
}

/// Step 2 before anything is mounted: returns the partition's device.
pub fn check_target(expected: (u64, u64)) -> Result<(String, Esp), String> {
    let (lba1, array) = read_table()?;
    let esp = find_esp(&lba1, &array, expected)?;
    let device = format!("{LUN0}p{}", esp.index);
    check_fat(&read_blocks(&device, 0, 1)?).map_err(|e| format!("{device}: {e}"))?;
    if let Ok(mounts) = fs::read_to_string("/proc/mounts")
        && mounts
            .lines()
            .any(|l| l.split(' ').next() == Some(device.as_str()))
    {
        return Err(format!("{device} is mounted: unmount it first"));
    }
    println!(
        "target: {device}, EFI system partition, LBA {}-{}, FAT32 with {CONFIG_BLOCK}-byte sectors",
        esp.first_lba, esp.last_lba
    );
    Ok((device, esp))
}

/// The primary table: the header block and the partition array.
pub fn read_table() -> Result<(Vec<u8>, Vec<u8>), String> {
    let lba1 = read_blocks(LUN0, 1, 1)
        .map_err(|e| format!("{e} (is the driver on? boot with ufs=on ufs.write=main)"))?;
    let header = gpt::parse_header(&lba1).ok_or("LUN 0 has no GPT at LBA 1")?;
    let bytes = header.entries as usize * header.entry_size as usize;
    if bytes == 0 || bytes > 64 * CONFIG_BLOCK {
        return Err(format!("the GPT's partition array is {bytes} bytes"));
    }
    let array = read_blocks(LUN0, header.entries_lba, bytes.div_ceil(CONFIG_BLOCK))?;
    Ok((lba1, array))
}

/// The partition with this GPT name: its device and its size in blocks.
pub fn partition_named(name: &str) -> Result<Option<(String, u64)>, String> {
    let (lba1, array) = read_table()?;
    let header = gpt::parse_header(&lba1).ok_or("LUN 0 has no GPT at LBA 1")?;
    let entries = gpt::parse_entries(&header, &array);
    let esp_last = entries
        .iter()
        .filter(|e| gpt::guid(&e.type_guid) == gptw::EFI_SYSTEM)
        .map(|e| e.last_lba)
        .max()
        .ok_or("LUN 0 has no EFI system partition")?;
    let Some(e) = entries.iter().find(|e| e.name == name) else {
        return Ok(None);
    };
    let blocks = disk_rules::writable_partition(e.first_lba, e.last_lba, esp_last, header.last_usable).ok_or_else(|| {
        format!(
            "{name} at LBA {}-{} is not behind the EFI system partition and inside the usable area, which ends at {}",
            e.first_lba, e.last_lba, header.last_usable
        )
    })?;
    Ok(Some((format!("{LUN0}p{}", e.index), blocks)))
}

/// Both copies of the table, as `gptw` wants them.
pub struct RawTable {
    pub disk_blocks: u64,
    primary_header: Vec<u8>,
    primary_array: Vec<u8>,
    backup_header: Vec<u8>,
    backup_array: Vec<u8>,
}

impl RawTable {
    /// Reads both copies. The array's place and size come from each header if it has the signature, else from the other's.
    pub fn read() -> Result<RawTable, String> {
        let disk_blocks = device_blocks(LUN0)?;
        let last = disk_blocks.checked_sub(1).ok_or("LUN 0 has no blocks")?;
        let primary_header = read_blocks(LUN0, 1, 1)?;
        let backup_header = read_blocks(LUN0, last, 1)?;
        // 128 entries of 128 bytes is what every copy this tool knows has; a header that says otherwise is refused by gptw.
        let array_blocks = 4;
        let primary_array = read_blocks(LUN0, 2, array_blocks)?;
        let backup_array = read_blocks(LUN0, last - array_blocks as u64, array_blocks)?;
        Ok(RawTable {
            disk_blocks,
            primary_header,
            primary_array,
            backup_header,
            backup_array,
        })
    }

    pub fn view(&self) -> OnDisk<'_> {
        OnDisk {
            disk_blocks: self.disk_blocks,
            primary_header: &self.primary_header,
            primary_array: &self.primary_array,
            backup_header: &self.backup_header,
            backup_array: &self.backup_array,
        }
    }

    /// Everything a table write could change: both headers and both arrays, for the backup file.
    pub fn backup_bytes(&self) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&self.primary_header);
        b.extend_from_slice(&self.primary_array);
        b.extend_from_slice(&self.backup_array);
        b.extend_from_slice(&self.backup_header);
        b
    }
}

/// Writes one block on LUN 0 and nothing else. Only `gpt` and its repair write here: everything else goes through a partition.
pub fn write_lun0_blocks(writes: &[gptw::BlockWrite]) -> Result<(), String> {
    let f = OpenOptions::new()
        .write(true)
        .open(LUN0)
        .map_err(|e| format!("{LUN0}: {e}"))?;
    for w in writes {
        f.write_all_at(&w.data, w.lba * LBA)
            .map_err(|e| format!("writing LBA {}: {e}", w.lba))?;
    }
    f.sync_all()
        .map_err(|e| format!("flushing {LUN0}: {e} (the writes may not be on the medium)"))
}

/// A copy of the trial's record, as a path on the mounted ESP.
fn trial_path(copy: trial::Copy) -> String {
    let path = match copy {
        trial::Copy::A => trial::COPY_A,
        trial::Copy::B => trial::COPY_B,
    };
    on_esp(&path.trim_start_matches('\\').replace('\\', "/"))
}

/// The trial's record on the mounted ESP, and the copy the next write goes to.
pub fn read_trial() -> trial::Newest {
    let read = |copy| fs::read(trial_path(copy)).ok().and_then(|b| trial::Record::decode(&b));
    trial::newest(read(trial::Copy::A), read(trial::Copy::B))
}

/// Writes a copy of the record in place, so that its folder's directory block changes as little as a file's date.
pub fn write_trial(copy: trial::Copy, record: &trial::Record) -> Result<(), String> {
    let path = trial_path(copy);
    overwrite_block(&path, &record.encode())?;
    if fs::read(&path).ok().and_then(|b| trial::Record::decode(&b)) != Some(*record) {
        return Err(format!("{path} does not read back as written"));
    }
    Ok(())
}

/// Both copies exist, each one block, each in a folder of its own: the loader overwrites them and creates nothing.
pub fn prepare_trial() -> Result<(), String> {
    for copy in [trial::Copy::A, trial::Copy::B] {
        let path = trial_path(copy);
        if fs::metadata(&path).is_ok_and(|m| m.len() == LBA) {
            continue;
        }
        if let Some(dir) = Path::new(&path).parent() {
            fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let mut f = File::create(&path).map_err(|e| format!("{path}: {e}"))?;
        f.write_all(&[0u8; CONFIG_BLOCK])
            .and_then(|()| f.sync_all())
            .map_err(|e| format!("{path}: {e}"))?;
    }
    sync_all();
    Ok(())
}

/// A path of the loader's, on the mounted ESP.
fn esp_path(path: &str) -> String {
    on_esp(&path.trim_start_matches('\\').replace('\\', "/"))
}

/// The record of `rng_marker`, for the loader to find: both copies one block long, and a first record if neither reads. A record
/// that reads stays as it is: a `Skip` is for good, an install does not take it back. To ask the firmware again after one, delete
/// the two files and install.
pub fn prepare_rng_marker() -> Result<(), String> {
    use lucina_boot::rng_marker;

    for name in [rng_marker::COPY_A, rng_marker::COPY_B] {
        let path = esp_path(name);
        if fs::metadata(&path).is_ok_and(|m| m.len() == LBA) {
            continue;
        }
        if let Some(dir) = Path::new(&path).parent() {
            fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let mut f = File::create(&path).map_err(|e| format!("{path}: {e}"))?;
        f.write_all(&[0u8; CONFIG_BLOCK])
            .and_then(|()| f.sync_all())
            .map_err(|e| format!("{path}: {e}"))?;
    }
    let read = |name: &str| fs::read(esp_path(name)).ok().and_then(|b| rng_marker::Record::decode(&b));
    let newest = rng_marker::newest(read(rng_marker::COPY_A), read(rng_marker::COPY_B));
    if newest.record.is_none() {
        let path = esp_path(rng_marker::COPY_A);
        overwrite_block(&path, &rng_marker::FIRST.encode())?;
        if read(rng_marker::COPY_A) != Some(rng_marker::FIRST) {
            return Err(format!("{path} does not read back as written"));
        }
    }
    sync_all();
    Ok(())
}

/// What the loader's marker says now.
pub fn read_rng_marker() -> Option<lucina_boot::rng_marker::Record> {
    use lucina_boot::rng_marker;
    let read = |name: &str| fs::read(esp_path(name)).ok().and_then(|b| rng_marker::Record::decode(&b));
    rng_marker::newest(read(rng_marker::COPY_A), read(rng_marker::COPY_B)).record
}

/// The superblock of the file system on a partition, if it has an ext2 one.
pub fn ext2_on(device: &str) -> Result<Option<Ext2Info>, String> {
    Ok(image::parse_superblock(&read_blocks(device, 0, 1)?))
}

pub fn sum_of(path: &str) -> Result<[u8; 32], String> {
    let mut f = File::open(path).map_err(|e| format!("{path}: {e}"))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f
            .read(&mut buf)
            .map_err(|e| format!("reading {path}: {e}"))?;
        if n == 0 {
            return Ok(hasher.finalize().into());
        }
        hasher.update(&buf[..n]);
    }
}

/// Replaces a file's only block in place. The size must already be one block, or the directory entry changes too.
pub fn overwrite_block(path: &str, block: &[u8]) -> Result<(), String> {
    debug_assert_eq!(block.len(), CONFIG_BLOCK);
    let size = fs::metadata(path)
        .map_err(|e| format!("{path}: {e}"))?
        .len();
    if size != LBA {
        return Err(format!(
            "{path}: {size} bytes, not one block: not committing"
        ));
    }
    let f = OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|e| format!("{path}: {e}"))?;
    f.write_all_at(block, 0)
        .map_err(|e| format!("writing {path}: {e}"))?;
    f.sync_all().map_err(|e| format!("flushing {path}: {e}"))
}

/// Makes BOOT.CFG one block long without changing what it says, for the in-place commit.
pub fn make_one_block(path: &str, text: &str) -> Result<(), String> {
    let size = fs::metadata(path)
        .map_err(|e| format!("{path}: {e}"))?
        .len();
    if size == LBA {
        return Ok(());
    }
    // An empty file has no cluster: its first write would allocate one, and that is not this safe path.
    if size == 0 || size > LBA {
        return Err(format!("{path}: {size} bytes, expected 1 to {LBA}"));
    }
    let f = OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|e| format!("{path}: {e}"))?;
    f.write_all_at(&config_block(text)?, 0)
        .map_err(|e| format!("writing {path}: {e}"))?;
    f.sync_all().map_err(|e| format!("flushing {path}: {e}"))
}

/// Copies a file to a new name on a mounted file system, flushed.
pub fn copy_flushed(from: &Path, to: &str) -> Result<(), String> {
    let data = fs::read(from).map_err(|e| format!("{}: {e}", from.display()))?;
    let _ = fs::remove_file(to);
    let mut out = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(to)
        .map_err(|e| format!("{to}: {e}"))?;
    out.write_all(&data)
        .map_err(|e| format!("writing {to}: {e}"))?;
    out.sync_all().map_err(|e| format!("flushing {to}: {e}"))
}
