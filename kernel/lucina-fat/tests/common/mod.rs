//! Helpers for the host tests: file-backed disks, mkfs.fat / fsck.fat / mtools wrappers, a model file system.
#![allow(dead_code)]

use std::{
    fs::{File, OpenOptions},
    os::unix::fs::FileExt,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

pub mod crash;
pub mod edk2;
pub mod tree;

use lucina_fat::{BlockIo, Error, MountOptions, Result, Time, Volume};

/// A disk image in a file, with a counter of device writes.
pub struct FileDisk {
    file: File,
    size: u64,
    pub writes: Arc<AtomicUsize>,
    pub flushes: Arc<AtomicUsize>,
}

impl FileDisk {
    pub fn open(path: &Path) -> FileDisk {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .unwrap();
        let size = file.metadata().unwrap().len();
        FileDisk {
            file,
            size,
            writes: Arc::new(AtomicUsize::new(0)),
            flushes: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl BlockIo for FileDisk {
    fn size(&self) -> u64 {
        self.size
    }
    fn read(&mut self, offset: u64, buf: &mut [u8]) -> Result<()> {
        assert_eq!(offset % 4096, 0, "unaligned device read");
        assert!(buf.len() <= 4096);
        self.file.read_exact_at(buf, offset).map_err(|_| Error::Io)
    }
    fn write(&mut self, offset: u64, buf: &[u8]) -> Result<()> {
        assert_eq!(offset % 4096, 0, "unaligned device write");
        assert!(buf.len() <= 4096);
        self.writes.fetch_add(1, Ordering::SeqCst);
        self.file.write_all_at(buf, offset).map_err(|_| Error::Io)
    }
    fn flush(&mut self) -> Result<()> {
        self.flushes.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

pub fn clock() -> Time {
    // 2026-11-01 12:00:00 UTC
    Time::new(1_793_534_400, 0)
}

pub fn opts() -> MountOptions {
    MountOptions {
        clock,
        ..MountOptions::default()
    }
}

pub fn ro_opts() -> MountOptions {
    MountOptions {
        readonly: true,
        clock,
        ..MountOptions::default()
    }
}

pub fn tmp_dir() -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("lucina-fat");
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Runs mkfs.fat with `args` on a fresh image of `kib` KiB called `name` and returns its path.
pub fn mkfs(name: &str, kib: u64, args: &[&str]) -> PathBuf {
    let path = tmp_dir().join(format!("{name}.img"));
    let _ = std::fs::remove_file(&path);
    let out = Command::new("mkfs.fat")
        .args(args)
        .arg("--invariant")
        .arg("-C")
        .arg(&path)
        .arg(kib.to_string())
        .output()
        .expect("mkfs.fat is installed");
    assert!(
        out.status.success(),
        "mkfs.fat {args:?} failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    path
}

pub fn mount(path: &Path) -> Volume<FileDisk> {
    Volume::mount(FileDisk::open(path), opts()).expect("mount")
}

/// fsck.fat -n on an image; returns (exit status 0, output).
pub fn fsck(path: &Path) -> (bool, String) {
    let out = Command::new("fsck.fat")
        .arg("-n")
        .arg("-v")
        .arg(path)
        .output()
        .expect("fsck.fat is installed");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), text)
}

pub fn assert_fsck_clean(path: &Path) {
    let (ok, text) = fsck(path);
    assert!(ok, "fsck.fat found problems:\n{text}");
}

/// The image configurations of the matrix: (label, mkfs args, size in KiB).
pub fn configs() -> Vec<(&'static str, Vec<&'static str>, u64)> {
    vec![
        ("f12-512", vec!["-F", "12"], 2880),
        ("f12-4096", vec!["-F", "12", "-S", "4096", "-s", "1"], 8192),
        ("f16-512", vec!["-F", "16"], 32 * 1024),
        (
            "f16-4096",
            vec!["-F", "16", "-S", "4096", "-s", "1"],
            48 * 1024,
        ),
        ("f32-512", vec!["-F", "32", "-s", "1"], 40 * 1024),
        (
            "f32-4096",
            vec!["-F", "32", "-S", "4096", "-s", "1"],
            300 * 1024,
        ),
    ]
}

/// A tiny deterministic generator.
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// mtools in the lucinaos-grub image, if docker and the image exist.
pub fn mtools_available() -> bool {
    Command::new("docker")
        .args(["image", "inspect", "lucinaos-grub:trixie"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Runs `sh -c script` in the mtools container with the image directory mounted at /w.
pub fn mtools(dir: &Path, script: &str) -> (bool, String) {
    let out = Command::new("docker")
        .args(["run", "--rm", "--network=none", "-v"])
        .arg(format!("{}:/w", dir.display()))
        .args(["lucinaos-grub:trixie", "sh", "-c", script])
        .output()
        .expect("docker");
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}
