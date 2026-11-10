//! The random seed kept on `/data`, for `lucina-data`. The kernel's first seed comes from the hardware; this one adds what no
//! attacker of the hardware can know, and is what a machine with no hardware source has at all.
//!
//! At boot: read the seed, replace it on the disk by one worked out from it, and only then give the kernel what it yields, in
//! the order of `lucina_random::seed_rules`. So a seed is never used twice, whatever moment a power cut picks. It goes in through
//! a write to `/dev/urandom`, which counts for nothing: only the hardware's bits make the generator count as seeded. Later, on a
//! seeded kernel, the file is written again with new output: every [`SAVE_EVERY`] and at an orderly shutdown.
//!
//! Without a seed file, or with a damaged one, the boot goes on without it and says so. Nothing here prints a seed.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    time::Duration,
};

use lucina_random::{
    rules::{GRND_INSECURE, GRND_NONBLOCK},
    seed_file::{self, SEED_BYTES},
    seed_rules::{self, Step},
};
use zeroize::Zeroizing;

/// How long a power cut can leave the file behind what the kernel has seen.
pub const SAVE_EVERY: Duration = Duration::from_secs(3600);

/// `RNDRESEEDCRNG`: `_IO('R', 0x07)`.
const RNDRESEEDCRNG: libc::c_ulong = 0x5207;

pub struct SeedFile {
    path: PathBuf,
    /// `/dev/urandom`; another file in a test.
    pool: PathBuf,
}

/// What a boot did, for the log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Loaded {
    /// Mixed in, and the file on the disk is a new one.
    Mixed,
    Missing,
    Damaged,
    /// The old seed could not be replaced, so it was not used.
    NotReplaced,
}

fn getrandom(out: &mut [u8], flags: u32) -> io::Result<()> {
    // SAFETY: fills a live buffer of its own length.
    let n = unsafe { libc::getrandom(out.as_mut_ptr().cast(), out.len(), flags) };
    if n == out.len() as isize {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

impl SeedFile {
    pub fn new(path: impl Into<PathBuf>, pool: impl Into<PathBuf>) -> Self {
        Self { path: path.into(), pool: pool.into() }
    }

    /// Write, fsync, rename, fsync of the folder: the old file or the new one, whole, after any power cut.
    fn write(&self, seed: &[u8; SEED_BYTES]) -> io::Result<()> {
        let tmp = self.path.with_extension("new");
        let mut file = OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
        file.write_all(seed)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp, &self.path)?;
        File::open(self.path.parent().unwrap_or(Path::new("/")))?.sync_all()
    }

    fn read(&self) -> Option<Zeroizing<Vec<u8>>> {
        let mut bytes = Zeroizing::new(Vec::new());
        File::open(&self.path).ok()?.read_to_end(&mut bytes).ok()?;
        Some(bytes)
    }

    /// `/dev/urandom` takes it into the pool, and the reseed puts it to use now, not at the next minute.
    fn mix(&self, value: &[u8; SEED_BYTES]) -> io::Result<()> {
        let mut pool = OpenOptions::new().write(true).open(&self.pool)?;
        pool.write_all(value)?;
        use std::os::fd::AsRawFd;
        // SAFETY: an ioctl without a pointer. It fails on a kernel that is not seeded yet, or on a file in a test, and then the
        // bytes wait in the pool for the next reseed.
        unsafe { libc::ioctl(pool.as_raw_fd(), RNDRESEEDCRNG as _) };
        Ok(())
    }

    /// The boot's steps.
    pub fn load(&self) -> Loaded {
        let old = self.read();
        let mut state = seed_rules::start(old.as_ref().map(|bytes| bytes.len()), SEED_BYTES);
        let old: Option<Zeroizing<[u8; SEED_BYTES]>> = old
            .as_ref()
            .and_then(|bytes| <[u8; SEED_BYTES]>::try_from(bytes.as_slice()).ok())
            .map(Zeroizing::new);
        loop {
            match (seed_rules::next_step(state), &old) {
                (Step::Replace, Some(old)) => {
                    // What the kernel can give right now, seeded or not: the old seed is secret either way.
                    let mut fresh = Zeroizing::new([0u8; 64]);
                    let _ = getrandom(&mut *fresh, GRND_INSECURE);
                    let next = seed_file::next_seed(old, &*fresh);
                    state = match self.write(&next) {
                        Ok(()) => seed_rules::replaced(state),
                        Err(_) => seed_rules::replace_failed(state),
                    };
                }
                (Step::Mix, Some(old)) => {
                    // The seed is gone from the disk now; if this fails it is lost, not used twice.
                    let _ = self.mix(&seed_file::for_kernel(old));
                    state = seed_rules::mixed(state);
                }
                (Step::Done, _) => return Loaded::Mixed,
                (_, _) => {
                    return match state.found {
                        seed_rules::Found::Missing => Loaded::Missing,
                        seed_rules::Found::Damaged => Loaded::Damaged,
                        seed_rules::Found::Whole => Loaded::NotReplaced,
                    };
                }
            }
        }
    }

    /// Writes the file with new output, if the kernel is seeded. Returns whether it did.
    pub fn save(&self) -> io::Result<bool> {
        let mut fresh = Zeroizing::new([0u8; SEED_BYTES]);
        let seeded = getrandom(&mut *fresh, GRND_NONBLOCK).is_ok();
        if !seed_rules::may_save(seeded) {
            return Ok(false);
        }
        self.write(&fresh)?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> (PathBuf, SeedFile, PathBuf) {
        let dir = std::env::temp_dir().join(format!("lucina-seed-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let pool = dir.join("pool");
        fs::write(&pool, b"").unwrap();
        (dir.clone(), SeedFile::new(dir.join("random-seed"), &pool), pool)
    }

    #[test]
    fn a_seed_is_replaced_before_it_is_used_and_never_used_again() {
        let (dir, seed, pool) = scratch("once");
        let old = [7u8; SEED_BYTES];
        fs::write(&seed.path, old).unwrap();

        assert_eq!(seed.load(), Loaded::Mixed);
        let on_disk = fs::read(&seed.path).unwrap();
        assert_eq!(on_disk.len(), SEED_BYTES);
        assert_ne!(on_disk, old);
        assert_eq!(fs::read(&pool).unwrap(), *seed_file::for_kernel(&old));
        // The value given to the kernel is not on the disk, and the seed itself went nowhere.
        assert_ne!(on_disk, fs::read(&pool).unwrap());

        // The next boot starts from the new file, and gives the kernel something else.
        let first = fs::read(&pool).unwrap();
        fs::write(&pool, b"").unwrap();
        assert_eq!(seed.load(), Loaded::Mixed);
        assert_ne!(fs::read(&pool).unwrap(), first);
        assert_ne!(fs::read(&seed.path).unwrap(), on_disk);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn nothing_is_mixed_from_no_file_or_a_damaged_one() {
        let (dir, seed, pool) = scratch("none");
        assert_eq!(seed.load(), Loaded::Missing);
        fs::write(&seed.path, [1u8; 31]).unwrap();
        assert_eq!(seed.load(), Loaded::Damaged);
        assert!(fs::read(&pool).unwrap().is_empty());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_seed_that_cannot_be_replaced_is_not_used() {
        let (dir, seed, pool) = scratch("stuck");
        fs::write(&seed.path, [7u8; SEED_BYTES]).unwrap();
        // The new file cannot be made: a folder is in its place.
        fs::create_dir(seed.path.with_extension("new")).unwrap();
        assert_eq!(seed.load(), Loaded::NotReplaced);
        assert!(fs::read(&pool).unwrap().is_empty());
        assert_eq!(fs::read(&seed.path).unwrap(), [7u8; SEED_BYTES]);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_save_writes_a_new_private_file_of_the_right_size() {
        use std::os::unix::fs::PermissionsExt;

        let (dir, seed, _) = scratch("save");
        assert_eq!(seed.save().unwrap(), true);
        let first = fs::read(&seed.path).unwrap();
        assert_eq!(first.len(), SEED_BYTES);
        assert_eq!(fs::metadata(&seed.path).unwrap().permissions().mode() & 0o777, 0o600);
        seed.save().unwrap();
        assert_ne!(fs::read(&seed.path).unwrap(), first);
        fs::remove_dir_all(dir).unwrap();
    }
}
