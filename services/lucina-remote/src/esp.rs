//! The saved Wi-Fi credentials, so that Wi-Fi comes up with no cable. When the machine boots from its root partition they live in
//! `/data/lucina/wifi.cfg`, mode 600 in a folder of mode 700 (everything here runs as root and the disk is not encrypted, so the
//! modes keep out nothing yet; docs/research/juno-disk-root.md). A machine running from RAM keeps them on the laptop's EFI system
//! partition (LUN 0, nothing else is ever a target): `lucina-wifi save` writes `\lucinaos\wifi.cfg`.
//! `lucina-wifi migrate` moves them from the second place to the first.
//!
//! The owner decided this (dc-23df0450680f). It is plaintext: FAT has no permissions, and anyone holding the disk can read it.
//! Writing needs `ufs=on ufs.write=main` on the running kernel, whose UFS driver refuses any other unit whatever this does.
//! Reading needs the driver on, which `lucina.wifi=on` does, read-only.
//!
//! A save goes to a new name, is flushed, and renamed over the old one, so a power cut leaves the old file or the new one. The
//! file is then read back after a remount: the cache is gone, and what comes back is what the disk holds.

use std::{
    ffi::CString,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};

use crate::install::{CONFIG_BLOCK, LAPTOP_ESP, check_fat, find_esp};

const LUN0: &str = "/dev/ufs0lun0";
const MOUNT: &str = "/mnt/lucina-esp-wifi";
const FOLDER: &str = "lucinaos";
const FILE: &str = "wifi.cfg";
const NEW: &str = "wifi.new";
/// See `lucina-install-disk`: what the file system's commit needs.
const MOUNT_OPTIONS: &str = "nodirtybit,nomtime";

/// `/etc/lucina/init.sh` makes this when `lucina-data` is on its way, and `lucina-data` makes the next two.
const DATA_EXPECTED: &str = "/run/lucina/data-expected";
const DATA_READY: &str = "/run/lucina/data-ready";
const DATA_FAILED: &str = "/run/lucina/data-failed";
const DATA_FILE: &str = "/data/lucina/wifi.cfg";
const DATA_NEW: &str = "/data/lucina/wifi.new";

/// Whether /data is where the credentials live on this boot. `Err` while it is still coming up.
fn data_in_use() -> Result<bool, String> {
    if !Path::new(DATA_EXPECTED).exists() {
        return Ok(false);
    }
    if Path::new(DATA_READY).exists() {
        return Ok(true);
    }
    if Path::new(DATA_FAILED).exists() {
        return Ok(false);
    }
    Err("/data is not ready yet".into())
}

fn save_to_data(text: &[u8]) -> Result<(), String> {
    use std::os::unix::fs::OpenOptionsExt;
    let _ = fs::remove_file(DATA_NEW);
    let mut out = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(DATA_NEW)
        .map_err(|e| format!("{DATA_NEW}: {e}"))?;
    out.write_all(text)
        .and_then(|()| out.sync_all())
        .map_err(|e| format!("writing {DATA_NEW}: {e}"))?;
    drop(out);
    fs::rename(DATA_NEW, DATA_FILE).map_err(|e| format!("{DATA_NEW} to {DATA_FILE}: {e}"))?;
    if let Ok(dir) = File::open("/data/lucina") {
        dir.sync_all()
            .map_err(|e| format!("flushing /data/lucina: {e}"))?;
    }
    if fs::read(DATA_FILE).ok().as_deref() != Some(text) {
        return Err(format!("{DATA_FILE} does not read back as it was written"));
    }
    Ok(())
}

/// Moves the credentials from the EFI partition to /data: the copy there is written and read back before the other goes. Prints
/// nothing of them. True if there was something to move.
pub fn migrate() -> Result<bool, String> {
    if !data_in_use()? {
        return Err("/data is not in use on this boot".into());
    }
    let device = partition().map_err(hint)?;
    let saved = {
        let _mounted = Mounted::new(&device, true).map_err(hint)?;
        read_from(MOUNT)
    };
    let Some(saved) = saved else {
        return Ok(false);
    };
    save_to_data(&saved)?;
    let _mounted = Mounted::new(&device, false).map_err(hint)?;
    let file = in_folder(MOUNT, FILE);
    fs::remove_file(&file).map_err(|e| hint(format!("{file}: {e}")))?;
    Ok(true)
}

fn errno(what: &str) -> String {
    format!("{what}: {}", std::io::Error::last_os_error())
}

fn read_blocks(device: &str, lba: u64, count: usize) -> Result<Vec<u8>, String> {
    let mut f = File::open(device).map_err(|e| format!("{device}: {e}"))?;
    f.seek(SeekFrom::Start(lba * CONFIG_BLOCK as u64))
        .map_err(|e| format!("{device}: {e}"))?;
    let mut b = vec![0u8; count * CONFIG_BLOCK];
    f.read_exact(&mut b)
        .map_err(|e| format!("reading {device}: {e}"))?;
    Ok(b)
}

/// LUN 0's one EFI system partition at the expected place, FAT32, as `lucina-install-disk` checks it.
pub fn partition() -> Result<String, String> {
    let lba1 = read_blocks(LUN0, 1, 1)?;
    let header = lucina_ufs::gpt::parse_header(&lba1).ok_or("LUN 0 has no GPT at LBA 1")?;
    let bytes = header.entries as usize * header.entry_size as usize;
    if bytes == 0 || bytes > 64 * CONFIG_BLOCK {
        return Err(format!("the GPT's partition array is {bytes} bytes"));
    }
    let array = read_blocks(LUN0, header.entries_lba, bytes.div_ceil(CONFIG_BLOCK))?;
    let esp = find_esp(&lba1, &array, LAPTOP_ESP)?;
    let device = format!("{LUN0}p{}", esp.index);
    check_fat(&read_blocks(&device, 0, 1)?).map_err(|e| format!("{device}: {e}"))?;
    Ok(device)
}

/// Where this partition is mounted already, if it is.
pub fn mounted_at(device: &str) -> Option<String> {
    fs::read_to_string("/proc/mounts")
        .ok()?
        .lines()
        .find_map(|line| {
            let mut fields = line.split(' ');
            (fields.next()? == device).then(|| fields.next().map(str::to_string))?
        })
}

/// Mounts the partition read-only at `dir`, staying mounted.
pub fn mount_read_only(device: &str, dir: &str) -> Result<(), String> {
    mount_read_only_as(device, dir, "vfat")
}

fn mount_read_only_as(device: &str, dir: &str, fs_name: &str) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|e| format!("{dir}: {e}"))?;
    let cstr = |s: &str| CString::new(s).map_err(|_| format!("{s}: has a NUL"));
    let (dev, target, fstype) = (cstr(device)?, cstr(dir)?, cstr(fs_name)?);
    // SAFETY: The strings outlive the call; there are no mount options.
    let r = unsafe {
        libc::mount(
            dev.as_ptr(),
            target.as_ptr(),
            fstype.as_ptr(),
            libc::MS_RDONLY,
            core::ptr::null(),
        )
    };
    if r != 0 {
        return Err(errno(&format!("mount {device} on {dir}")));
    }
    Ok(())
}

/// The shell keeps the partition mounted read-only for its apps; nothing runs from it, so it can always let go, and mounts
/// it again when it next needs it. Everything that writes to the partition calls this first.
pub fn release_apps_mount() {
    if let Ok(dir) = CString::new(crate::appset::APPS_MOUNT) {
        // SAFETY: The string outlives the call. Not mounted is fine.
        unsafe { libc::umount(dir.as_ptr()) };
    }
}

struct Mounted;

impl Mounted {
    fn new(device: &str, read_only: bool) -> Result<Mounted, String> {
        fs::create_dir_all(MOUNT).map_err(|e| format!("{MOUNT}: {e}"))?;
        let cstr = |s: &str| CString::new(s).map_err(|_| format!("{s}: has a NUL"));
        let (dev, dir, fstype, options) = (
            cstr(device)?,
            cstr(MOUNT)?,
            cstr("vfat")?,
            cstr(MOUNT_OPTIONS)?,
        );
        let flags = if read_only { libc::MS_RDONLY } else { 0 };
        let mount = |data: *const libc::c_void| {
            // SAFETY: The strings outlive the call; `data` is a NUL-terminated string or null.
            unsafe { libc::mount(dev.as_ptr(), dir.as_ptr(), fstype.as_ptr(), flags, data) }
        };
        if mount(options.as_ptr().cast()) != 0 && mount(core::ptr::null()) != 0 {
            return Err(errno(&format!("mount {device} on {MOUNT}")));
        }
        Ok(Mounted)
    }
}

impl Drop for Mounted {
    fn drop(&mut self) {
        // SAFETY: Takes no arguments.
        unsafe { libc::sync() };
        if let Ok(dir) = CString::new(MOUNT) {
            // SAFETY: The string outlives the call.
            unsafe { libc::umount(dir.as_ptr()) };
        }
    }
}

fn in_folder(root: &str, name: &str) -> String {
    format!("{root}/{FOLDER}/{name}")
}

fn read_from(root: &str) -> Option<Vec<u8>> {
    fs::read(in_folder(root, FILE)).ok()
}

/// The credentials on the data partition, for a boot that runs from RAM and so has no /data (the installer a reload starts): the
/// partition is mounted read-only for this one read. `None` when there is no such partition or file.
fn read_from_data_partition() -> Result<Option<Vec<u8>>, String> {
    let Some((device, _)) = crate::disk::partition_named(crate::gptw::DATA)? else {
        return Ok(None);
    };
    const DIR: &str = "/mnt/lucina-data-wifi";
    mount_read_only_as(&device, DIR, "ext2")?;
    let saved = fs::read(format!("{DIR}/lucina/{FILE}")).ok();
    let dir = CString::new(DIR).expect("no NUL");
    // SAFETY: The string outlives the call.
    unsafe { libc::umount(dir.as_ptr()) };
    Ok(saved)
}

/// The saved credentials, if the disk is there and has them. Does not wait for the disk.
pub fn read_saved() -> Result<Option<Vec<u8>>, String> {
    if data_in_use()? {
        match fs::read(DATA_FILE) {
            Ok(saved) => return Ok(Some(saved)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("{DATA_FILE}: {e}")),
        }
    }
    if !Path::new(LUN0).exists() {
        return Err(format!("{LUN0} is not there (is the UFS driver on?)"));
    }
    if !Path::new(DATA_EXPECTED).exists()
        && let Some(saved) = read_from_data_partition()?
    {
        return Ok(Some(saved));
    }
    let device = partition()?;
    if let Some(root) = mounted_at(&device) {
        return Ok(read_from(&root));
    }
    let _mounted = Mounted::new(&device, true)?;
    Ok(read_from(MOUNT))
}

fn refuse_if_mounted(device: &str) -> Result<(), String> {
    release_apps_mount();
    match mounted_at(device) {
        Some(root) => Err(format!("{device} is mounted on {root}: unmount it first")),
        None => Ok(()),
    }
}

fn hint(err: String) -> String {
    format!("{err} (does this kernel run with ufs=on ufs.write=main?)")
}

/// Stores the credentials. Nothing else on the partition changes.
pub fn save(text: &[u8]) -> Result<(), String> {
    if data_in_use()? {
        return save_to_data(text);
    }
    let device = partition().map_err(hint)?;
    refuse_if_mounted(&device)?;
    {
        let _mounted = Mounted::new(&device, false).map_err(hint)?;
        let folder = format!("{MOUNT}/{FOLDER}");
        if !Path::new(&folder).exists() {
            fs::create_dir(&folder).map_err(|e| hint(format!("{folder}: {e}")))?;
        }
        let new = in_folder(MOUNT, NEW);
        let _ = fs::remove_file(&new);
        let mut out = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&new)
            .map_err(|e| hint(format!("{new}: {e}")))?;
        out.write_all(text)
            .map_err(|e| format!("writing {new}: {e}"))?;
        out.sync_all().map_err(|e| format!("flushing {new}: {e}"))?;
        drop(out);
        let file = in_folder(MOUNT, FILE);
        fs::rename(&new, &file).map_err(|e| format!("{new} to {file}: {e}"))?;
    }
    let _mounted = Mounted::new(&device, true)?;
    if read_from(MOUNT).as_deref() != Some(text) {
        return Err(format!(
            "{FOLDER}/{FILE} does not read back as it was written"
        ));
    }
    Ok(())
}

/// Removes the stored credentials. True if there were any.
pub fn forget() -> Result<bool, String> {
    let mut had = false;
    if data_in_use()? && Path::new(DATA_FILE).exists() {
        fs::remove_file(DATA_FILE).map_err(|e| format!("{DATA_FILE}: {e}"))?;
        had = true;
    }
    let device = partition().map_err(hint)?;
    refuse_if_mounted(&device)?;
    let _mounted = Mounted::new(&device, false).map_err(hint)?;
    let file = in_folder(MOUNT, FILE);
    if !Path::new(&file).exists() {
        return Ok(had);
    }
    fs::remove_file(&file).map_err(|e| hint(format!("{file}: {e}")))?;
    Ok(true)
}
