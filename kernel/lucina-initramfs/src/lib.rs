//! Reads and writes the "new ASCII" (newc, `070701`) cpio format that initramfs uses, and overlays a directory
//! tree onto an archive: an entry of the tree replaces an archive entry of the same path, everything is owned by
//! root, and the archive ends with one trailer.

use std::{
    fs,
    io::{self, Read, Write},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::Path,
};

const MAGIC: &[u8; 6] = b"070701";
const TRAILER: &str = "TRAILER!!!";

/// One archive member. `mode` includes the file type bits (`S_IFREG`, `S_IFDIR`, `S_IFLNK`, ...).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub nlink: u32,
    pub mtime: u32,
    pub rdevmajor: u32,
    pub rdevminor: u32,
    /// File contents, or the target of a symlink.
    pub data: Vec<u8>,
}

pub const S_IFDIR: u32 = 0o040000;
pub const S_IFREG: u32 = 0o100000;
pub const S_IFLNK: u32 = 0o120000;

fn bad(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

fn pad4(n: usize) -> usize {
    (4 - n % 4) % 4
}

/// Parses a newc archive up to its trailer (anything after it is ignored).
pub fn parse(mut data: &[u8]) -> io::Result<Vec<Entry>> {
    let mut entries = Vec::new();
    let start_len = data.len();
    loop {
        if data.len() < 110 {
            return Err(bad("archive ends without a trailer"));
        }
        if &data[..6] != MAGIC {
            return Err(bad(format!(
                "no newc header at offset {}",
                start_len - data.len()
            )));
        }
        let field = |i: usize| -> io::Result<u32> {
            let s = std::str::from_utf8(&data[6 + 8 * i..14 + 8 * i])
                .map_err(|_| bad("header field"))?;
            u32::from_str_radix(s, 16).map_err(|_| bad("header field"))
        };
        let (mode, uid, gid, nlink, mtime, filesize) = (
            field(1)?,
            field(2)?,
            field(3)?,
            field(4)?,
            field(5)?,
            field(6)?,
        );
        let (rdevmajor, rdevminor, namesize) = (field(9)?, field(10)?, field(11)? as usize);
        let name_end = 110 + namesize;
        let data_start = name_end + pad4(name_end);
        let data_end = data_start + filesize as usize;
        if namesize == 0 || data.len() < data_end {
            return Err(bad("truncated entry"));
        }
        let name = std::str::from_utf8(&data[110..name_end - 1])
            .map_err(|_| bad("entry name"))?
            .to_string();
        if name == TRAILER {
            return Ok(entries);
        }
        entries.push(Entry {
            name,
            mode,
            uid,
            gid,
            nlink,
            mtime,
            rdevmajor,
            rdevminor,
            data: data[data_start..data_end].to_vec(),
        });
        let next = data_end + pad4(data_end);
        data = &data[next.min(data.len())..];
    }
}

/// Writes entries and a trailer. Inode numbers are assigned in order (hard links are not kept).
pub fn write(entries: &[Entry], out: &mut impl Write) -> io::Result<()> {
    let trailer = Entry {
        name: TRAILER.into(),
        mode: 0,
        uid: 0,
        gid: 0,
        nlink: 1,
        mtime: 0,
        rdevmajor: 0,
        rdevminor: 0,
        data: Vec::new(),
    };
    for (ino, e) in entries.iter().chain(std::iter::once(&trailer)).enumerate() {
        let namesize = e.name.len() + 1;
        let header = format!(
            "070701{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}",
            if e.name == TRAILER { 0 } else { ino as u32 + 1 },
            e.mode,
            e.uid,
            e.gid,
            e.nlink,
            e.mtime,
            e.data.len(),
            0,
            0,
            e.rdevmajor,
            e.rdevminor,
            namesize,
            0
        );
        out.write_all(header.as_bytes())?;
        out.write_all(e.name.as_bytes())?;
        out.write_all(&[0])?;
        out.write_all(&[0; 3][..pad4(110 + namesize)])?;
        out.write_all(&e.data)?;
        out.write_all(&[0; 3][..pad4(e.data.len())])?;
    }
    Ok(())
}

/// The entries for the tree under `dir`, as archive paths relative to it, parents before children, owned by root.
pub fn tree_entries(dir: &Path) -> io::Result<Vec<Entry>> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<Entry>) -> io::Result<()> {
        let mut names: Vec<_> = fs::read_dir(dir)?.collect::<Result<_, _>>()?;
        names.sort_by_key(|e| e.file_name());
        for de in names {
            let path = de.path();
            let meta = fs::symlink_metadata(&path)?;
            let name = path
                .strip_prefix(root)
                .unwrap()
                .to_str()
                .ok_or_else(|| bad("non-UTF-8 path"))?
                .to_string();
            let perm = meta.permissions().mode() & 0o7777;
            let (mode, data) = if meta.file_type().is_symlink() {
                (
                    S_IFLNK | 0o777,
                    fs::read_link(&path)?.into_os_string().into_encoded_bytes(),
                )
            } else if meta.is_dir() {
                (S_IFDIR | perm, Vec::new())
            } else if meta.is_file() {
                (S_IFREG | perm, fs::read(&path)?)
            } else {
                return Err(bad(format!(
                    "{name}: only files, directories and symlinks are supported"
                )));
            };
            out.push(Entry {
                name,
                mode,
                uid: 0,
                gid: 0,
                nlink: if meta.is_dir() { 2 } else { 1 },
                mtime: meta.mtime().clamp(0, u32::MAX as i64) as u32,
                rdevmajor: 0,
                rdevminor: 0,
                data,
            });
            if meta.is_dir() {
                walk(root, &path, out)?;
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out)?;
    Ok(out)
}

/// `base` with `overlay` on top: an overlay entry replaces a base entry of the same path, in place, or is
/// appended.
pub fn overlay(mut base: Vec<Entry>, overlay: Vec<Entry>) -> Vec<Entry> {
    let norm = |n: &str| {
        n.trim_start_matches("./")
            .trim_start_matches('/')
            .to_string()
    };
    for e in overlay {
        match base.iter_mut().find(|b| norm(&b.name) == norm(&e.name)) {
            Some(b) => {
                *b = Entry {
                    name: b.name.clone(),
                    ..e
                }
            }
            None => base.push(e),
        }
    }
    base
}

/// Reads a gzip'd or plain archive.
pub fn read_archive(bytes: &[u8]) -> io::Result<Vec<Entry>> {
    if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut raw = Vec::new();
        flate2::read::MultiGzDecoder::new(bytes).read_to_end(&mut raw)?;
        parse(&raw)
    } else {
        parse(bytes)
    }
}

pub fn write_gzip(entries: &[Entry]) -> io::Result<Vec<u8>> {
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    write(entries, &mut gz)?;
    gz.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, data: &[u8]) -> Entry {
        Entry {
            name: name.into(),
            mode: S_IFREG | 0o644,
            uid: 0,
            gid: 0,
            nlink: 1,
            mtime: 0,
            rdevmajor: 0,
            rdevminor: 0,
            data: data.to_vec(),
        }
    }

    #[test]
    fn round_trip_with_padding() {
        let entries: Vec<Entry> = (0..9)
            .map(|i| file(&"n".repeat(i + 1), &vec![i as u8; i * 3]))
            .collect();
        let mut out = Vec::new();
        write(&entries, &mut out).unwrap();
        assert_eq!(out.len() % 4, 0);
        assert_eq!(parse(&out).unwrap(), entries);
        assert_eq!(
            read_archive(&write_gzip(&entries).unwrap()).unwrap(),
            entries
        );
    }

    #[test]
    fn overlay_replaces_and_appends() {
        let base = vec![file("init", b"old"), file("etc/hosts", b"h")];
        let merged = overlay(base, vec![file("./init", b"new"), file("sbin/x", b"x")]);
        assert_eq!(
            merged
                .iter()
                .map(|e| (e.name.as_str(), e.data.as_slice()))
                .collect::<Vec<_>>(),
            [("init", &b"new"[..]), ("etc/hosts", b"h"), ("sbin/x", b"x")]
        );
    }

    #[test]
    fn refuses_garbage() {
        assert!(parse(b"hello").is_err());
        let mut out = Vec::new();
        write(&[file("a", b"abc")], &mut out).unwrap();
        assert!(parse(&out[..out.len() - 130]).is_err());
    }
}
