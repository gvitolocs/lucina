//! Fetched pictures on disk, one file each. `/data` is ext2 with no journal, so a power cut can leave a file with the wrong bytes in
//! it: every file ends in a CRC-32 of the rest, and one that does not add up is removed and fetched again. When the folder grows past
//! its limit the oldest files go, as on iOS (`GalleryDiskCache.swift`), which also touches a file on reading; here reading writes
//! nothing to the disk, so "oldest" means fetched longest ago.

use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

const TABLE: [u32; 256] = {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
};

pub fn crc32(b: &[u8]) -> u32 {
    let mut c = !0u32;
    for &x in b {
        c = TABLE[((c ^ x as u32) & 0xff) as usize] ^ (c >> 8);
    }
    !c
}

/// FNV-1a: a key to a file name. Keys are the server's paths, which hold item ids and versions.
fn name_of(key: &str) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for b in key.bytes() {
        h = (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

pub struct DiskCache {
    root: PathBuf,
    limit: u64,
    /// Bytes written since the folder was last measured. A prune lists every file, so it waits for this.
    written: AtomicU64,
    pruning: Mutex<()>,
}

impl DiskCache {
    /// `limit` in bytes. Nothing is read until it is asked for.
    pub fn new(root: PathBuf, limit: u64) -> DiskCache {
        DiskCache { root, limit, written: AtomicU64::new(0), pruning: Mutex::new(()) }
    }

    fn file(&self, key: &str) -> PathBuf {
        let h = name_of(key);
        self.root.join(format!("{:02x}", (h >> 56) as u8)).join(format!("{h:016x}"))
    }

    /// The bytes stored under `key`. A file that fails its check is removed and counts as absent.
    pub fn read(&self, key: &str) -> Option<Vec<u8>> {
        let path = self.file(key);
        let mut b = std::fs::read(&path).ok()?;
        let good = b.len() >= 4 && {
            let split = b.len() - 4;
            let stored = u32::from_le_bytes([b[split], b[split + 1], b[split + 2], b[split + 3]]);
            crc32(&b[..split]) == stored
        };
        if !good {
            let _ = std::fs::remove_file(&path);
            return None;
        }
        b.truncate(b.len() - 4);
        Some(b)
    }

    pub fn contains(&self, key: &str) -> bool {
        self.file(key).exists()
    }

    /// Forget a file that passed its check but does not decode.
    pub fn remove(&self, key: &str) {
        let _ = std::fs::remove_file(self.file(key));
    }

    /// Store `bytes`, whole or not at all. A full or missing disk costs a fetch later and nothing else.
    pub fn write(&self, key: &str, bytes: &[u8]) {
        use std::io::Write;
        let path = self.file(key);
        let Some(dir) = path.parent() else { return };
        if std::fs::create_dir_all(dir).is_err() {
            return;
        }
        let tmp = path.with_extension("tmp");
        let result = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp).and_then(|mut f| {
            f.write_all(bytes)?;
            f.write_all(&crc32(bytes).to_le_bytes())
        });
        if result.is_err() || std::fs::rename(&tmp, &path).is_err() {
            let _ = std::fs::remove_file(&tmp);
            return;
        }
        let written = self.written.fetch_add(bytes.len() as u64, Ordering::Relaxed) + bytes.len() as u64;
        if written > self.limit / 16 {
            self.prune();
        }
    }

    /// Down to three quarters of the limit if it is over, oldest first.
    pub fn prune(&self) {
        let Ok(_guard) = self.pruning.try_lock() else { return };
        self.written.store(0, Ordering::Relaxed);
        let mut files = Vec::new();
        let mut total = 0u64;
        for shard in read_dir(&self.root) {
            for f in read_dir(&shard) {
                let Ok(m) = std::fs::metadata(&f) else { continue };
                let at = m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs());
                total += m.len();
                files.push((at, m.len(), f));
            }
        }
        if total <= self.limit {
            return;
        }
        files.sort_unstable_by(|a, b| a.0.cmp(&b.0).then_with(|| a.2.cmp(&b.2)));
        let goal = self.limit / 4 * 3;
        for (_, size, f) in files {
            if total <= goal {
                break;
            }
            if std::fs::remove_file(&f).is_ok() {
                total -= size;
            }
        }
    }

    /// Everything goes: the account that fetched it signed out.
    pub fn clear(&self) {
        for shard in read_dir(&self.root) {
            let _ = std::fs::remove_dir_all(shard);
        }
    }
}

fn read_dir(p: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(p).map(|d| d.flatten().map(|e| e.path()).collect()).unwrap_or_default()
}
