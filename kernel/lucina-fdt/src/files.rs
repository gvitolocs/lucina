//! Reading and writing DTB files on the host, defensively: never read from or write to a device node.
//! Same pattern as `lucina-bootimg`'s `files.rs`.

use std::{
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

/// Largest DTB we read.
pub const MAX_DTB_BYTES: u64 = 64 << 20;

fn refuse(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, msg.to_string())
}

/// Reads a regular file of at most [`MAX_DTB_BYTES`].
pub fn read_file(path: &Path) -> io::Result<Vec<u8>> {
    let mut f = fs::File::open(path)?;
    let meta = f.metadata()?;
    if !meta.is_file() {
        return Err(refuse("refusing to read a non-regular file"));
    }
    if meta.len() > MAX_DTB_BYTES {
        return Err(refuse("file larger than 64 MiB"));
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    Read::by_ref(&mut f)
        .take(MAX_DTB_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_DTB_BYTES {
        return Err(refuse("file larger than 64 MiB"));
    }
    Ok(bytes)
}

/// Writes `bytes` to `path` as a regular file.
///
/// Refuses paths under `/dev` and existing targets that are symlinks (dangling or not) or not regular files.
/// The data goes to a fresh `O_CREAT | O_EXCL` temporary file in the same directory, which is renamed over `path`;
/// a rename replaces the directory entry and never writes through it.
pub fn write_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let name = path
        .file_name()
        .ok_or_else(|| refuse("output path has no file name"))?;
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let dir = dir.canonicalize()?;
    if dir.starts_with("/dev") {
        return Err(refuse("refusing to write under /dev"));
    }
    match fs::symlink_metadata(dir.join(name)) {
        Ok(m) if m.file_type().is_symlink() => return Err(refuse("refusing to replace a symlink")),
        Ok(m) if !m.is_file() => return Err(refuse("refusing to replace a non-regular file")),
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let tmp: PathBuf = dir.join(format!(
        ".{}.tmp-{}",
        name.to_string_lossy(),
        std::process::id()
    ));
    let result = (|| {
        let mut f = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        fs::rename(&tmp, dir.join(name))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lucina-fdt-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn writes_and_replaces_regular_files() {
        let d = scratch("write");
        let p = d.join("a.dtb");
        write_file(&p, b"one").unwrap();
        write_file(&p, b"two").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"two");
        assert_eq!(
            fs::read_dir(&d).unwrap().count(),
            1,
            "no temporary file left behind"
        );
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn refuses_symlinks_dirs_and_dev() {
        let d = scratch("refuse");
        let target = d.join("target");
        fs::write(&target, b"keep").unwrap();
        symlink(&target, d.join("link")).unwrap();
        symlink("/dev/lucina-test-missing", d.join("dangling")).unwrap();
        fs::create_dir(d.join("dir")).unwrap();
        for p in [
            d.join("link"),
            d.join("dangling"),
            d.join("dir"),
            PathBuf::from("/dev/null"),
            PathBuf::from("/dev/shm/x"),
        ] {
            assert!(write_file(&p, b"x").is_err(), "{} accepted", p.display());
        }
        assert_eq!(fs::read(&target).unwrap(), b"keep");
        assert!(!Path::new("/dev/lucina-test-missing").exists());
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn refuses_reading_devices() {
        assert!(read_file(Path::new("/dev/zero")).is_err());
        assert!(read_file(Path::new("/")).is_err());
    }
}
