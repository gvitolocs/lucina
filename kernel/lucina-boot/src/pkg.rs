//! The payload package LucinaBoot finds in the ramdisk: a kernel `Image`, an optional initramfs, named DTBs.
//!
//! All integers are little endian. Data is 4 KiB-aligned so a package can be inspected with `dd`.
//!
//! ```text
//! offset  size  field
//!      0     8  magic "LUCBPKG\0"
//!      8     4  version (1)
//!     12     4  entry count (1..=MAX_ENTRIES)
//!     16     8  total size: header + entry table + data, the whole package
//!     24    40  reserved, zero
//!     64  64*n  entries:
//!                 u32 kind (1 kernel Image, 2 initramfs, 3 device tree)
//!                 u32 flags (0)
//!                 u64 offset from the start of the package
//!                 u64 size
//!                 u64 reserved, zero
//!                 [u8; 32] name, UTF-8, NUL-padded (device trees: the panel or "default")
//! ```
//!
//! A valid package has exactly one kernel, at most one initramfs, and device trees with distinct non-empty names.
//! Entries lie inside the package, after the entry table, and do not overlap.

/// `magic`.
pub const MAGIC: [u8; 8] = *b"LUCBPKG\0";
pub const VERSION: u32 = 1;
pub const HEADER_SIZE: usize = 64;
pub const ENTRY_SIZE: usize = 64;
pub const MAX_ENTRIES: usize = 16;
pub const NAME_LEN: usize = 32;
/// Alignment of each entry's data.
pub const DATA_ALIGN: u64 = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum Kind {
    Kernel = 1,
    Initramfs = 2,
    Dtb = 3,
}

impl Kind {
    fn from_u32(v: u32) -> Option<Kind> {
        match v {
            1 => Some(Kind::Kernel),
            2 => Some(Kind::Initramfs),
            3 => Some(Kind::Dtb),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Truncated,
    BadMagic,
    UnsupportedVersion(u32),
    /// Entry count is 0 or above [`MAX_ENTRIES`].
    BadCount,
    /// `total size` exceeds the buffer or is smaller than the entry table.
    BadTotalSize,
    /// Non-zero reserved bytes in the header.
    BadHeader,
    /// Unknown kind, non-zero flags or reserved field.
    BadEntry(usize),
    /// Entry name is not NUL-padded UTF-8, or a device tree has an empty or duplicate name.
    BadName(usize),
    /// Entry data lies outside the package, inside the entry table, or overlaps another entry.
    BadRange(usize),
    /// Not exactly one kernel.
    KernelCount,
    /// More than one initramfs.
    InitramfsCount,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::Truncated => f.write_str("package truncated"),
            Error::BadMagic => f.write_str("not a LucinaBoot package (bad magic)"),
            Error::UnsupportedVersion(v) => write!(f, "package version {v}, expected {VERSION}"),
            Error::BadCount => write!(f, "entry count must be 1..={MAX_ENTRIES}"),
            Error::BadTotalSize => f.write_str("package total size is wrong"),
            Error::BadHeader => f.write_str("package header has non-zero reserved bytes"),
            Error::BadEntry(i) => write!(f, "entry {i}: bad kind, flags or reserved field"),
            Error::BadName(i) => write!(f, "entry {i}: bad, empty or duplicate name"),
            Error::BadRange(i) => write!(f, "entry {i}: data out of bounds or overlapping"),
            Error::KernelCount => f.write_str("package needs exactly one kernel"),
            Error::InitramfsCount => f.write_str("package has more than one initramfs"),
        }
    }
}

/// One validated entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry<'a> {
    pub kind: Kind,
    pub name: &'a str,
    /// Offset of the data from the start of the package.
    pub offset: u64,
    pub data: &'a [u8],
}

/// A validated package.
#[derive(Debug, Clone, Copy)]
pub struct Package<'a> {
    bytes: &'a [u8],
    count: usize,
}

fn le32(b: &[u8], off: usize) -> Result<u32, Error> {
    let s = b.get(off..off + 4).ok_or(Error::Truncated)?;
    Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn le64(b: &[u8], off: usize) -> Result<u64, Error> {
    let s = b.get(off..off + 8).ok_or(Error::Truncated)?;
    let mut v = [0u8; 8];
    v.copy_from_slice(s);
    Ok(u64::from_le_bytes(v))
}

/// The name field: UTF-8 up to the first NUL, and only NULs after it.
fn name_of(field: &[u8]) -> Option<&str> {
    let n = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    if field[n..].iter().any(|&b| b != 0) {
        return None;
    }
    core::str::from_utf8(&field[..n]).ok()
}

impl<'a> Package<'a> {
    /// Validates the header and every entry. `bytes` may be longer than the package (a ramdisk rounded up).
    pub fn parse(bytes: &'a [u8]) -> Result<Package<'a>, Error> {
        if bytes.len() < HEADER_SIZE {
            return Err(Error::Truncated);
        }
        if bytes[..8] != MAGIC {
            return Err(Error::BadMagic);
        }
        let version = le32(bytes, 8)?;
        if version != VERSION {
            return Err(Error::UnsupportedVersion(version));
        }
        let count = le32(bytes, 12)? as usize;
        if count == 0 || count > MAX_ENTRIES {
            return Err(Error::BadCount);
        }
        let table_end = (HEADER_SIZE + count * ENTRY_SIZE) as u64;
        let total = le64(bytes, 16)?;
        if total > bytes.len() as u64 || total < table_end {
            return Err(Error::BadTotalSize);
        }
        if bytes[24..HEADER_SIZE].iter().any(|&b| b != 0) {
            return Err(Error::BadHeader);
        }
        let pkg = Package {
            bytes: &bytes[..total as usize],
            count,
        };

        let (mut kernels, mut initramfs) = (0, 0);
        let mut ranges = [(0u64, 0u64); MAX_ENTRIES];
        for i in 0..count {
            let e = pkg.raw_entry(i)?;
            let end = e.offset.checked_add(e.size).ok_or(Error::BadRange(i))?;
            if e.offset < table_end || end > total {
                return Err(Error::BadRange(i));
            }
            if ranges[..i]
                .iter()
                .any(|&(s, t)| e.size > 0 && s < t && e.offset < t && s < end)
            {
                return Err(Error::BadRange(i));
            }
            ranges[i] = (e.offset, end);
            match e.kind {
                Kind::Kernel => kernels += 1,
                Kind::Initramfs => initramfs += 1,
                Kind::Dtb => {
                    if e.name.is_empty() {
                        return Err(Error::BadName(i));
                    }
                    for j in 0..i {
                        let o = pkg.raw_entry(j)?;
                        if o.kind == Kind::Dtb && o.name == e.name {
                            return Err(Error::BadName(i));
                        }
                    }
                }
            }
        }
        if kernels != 1 {
            return Err(Error::KernelCount);
        }
        if initramfs > 1 {
            return Err(Error::InitramfsCount);
        }
        Ok(pkg)
    }

    fn raw_entry(&self, i: usize) -> Result<RawEntry<'a>, Error> {
        let b = self.bytes;
        let at = HEADER_SIZE + i * ENTRY_SIZE;
        let kind = Kind::from_u32(le32(b, at)?).ok_or(Error::BadEntry(i))?;
        if le32(b, at + 4)? != 0 || le64(b, at + 24)? != 0 {
            return Err(Error::BadEntry(i));
        }
        let field = b.get(at + 32..at + 32 + NAME_LEN).ok_or(Error::Truncated)?;
        let name = name_of(field).ok_or(Error::BadName(i))?;
        Ok(RawEntry {
            kind,
            name,
            offset: le64(b, at + 8)?,
            size: le64(b, at + 16)?,
        })
    }

    /// The whole package (`total size` bytes).
    pub fn bytes(&self) -> &'a [u8] {
        self.bytes
    }

    pub fn len(&self) -> usize {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Entry `i` (validated in [`Package::parse`]).
    pub fn entry(&self, i: usize) -> Option<Entry<'a>> {
        if i >= self.count {
            return None;
        }
        let e = self.raw_entry(i).ok()?;
        let data = self
            .bytes
            .get(e.offset as usize..(e.offset + e.size) as usize)?;
        Some(Entry {
            kind: e.kind,
            name: e.name,
            offset: e.offset,
            data,
        })
    }

    pub fn entries(&self) -> impl Iterator<Item = Entry<'a>> + '_ {
        (0..self.count).filter_map(|i| self.entry(i))
    }

    pub fn kernel(&self) -> Entry<'a> {
        // parse() guarantees exactly one.
        self.entries()
            .find(|e| e.kind == Kind::Kernel)
            .unwrap_or(Entry {
                kind: Kind::Kernel,
                name: "",
                offset: 0,
                data: &[],
            })
    }

    pub fn initramfs(&self) -> Option<Entry<'a>> {
        self.entries().find(|e| e.kind == Kind::Initramfs)
    }

    /// The device tree named `name`.
    pub fn dtb(&self, name: &str) -> Option<Entry<'a>> {
        self.entries()
            .find(|e| e.kind == Kind::Dtb && e.name == name)
    }

    pub fn dtbs(&self) -> impl Iterator<Item = Entry<'a>> + '_ {
        self.entries().filter(|e| e.kind == Kind::Dtb)
    }
}

struct RawEntry<'a> {
    kind: Kind,
    name: &'a str,
    offset: u64,
    size: u64,
}

/// Writes the header and entry table for entries of the given kinds, names and sizes into `out`, laying the data
/// out in order at [`DATA_ALIGN`]. Returns the data offsets and the total size; the caller copies the data in.
/// Used by the host packer and by tests; LucinaBoot only reads packages.
pub fn layout(
    entries: &[(Kind, &str, u64)],
    out: &mut [u8],
) -> Result<([u64; MAX_ENTRIES], u64), Error> {
    if entries.is_empty() || entries.len() > MAX_ENTRIES {
        return Err(Error::BadCount);
    }
    let table_end = (HEADER_SIZE + entries.len() * ENTRY_SIZE) as u64;
    if (out.len() as u64) < table_end {
        return Err(Error::Truncated);
    }
    let align = |v: u64| v.checked_add(DATA_ALIGN - 1).map(|v| v & !(DATA_ALIGN - 1));
    let mut offsets = [0u64; MAX_ENTRIES];
    let mut pos = align(table_end).ok_or(Error::BadTotalSize)?;
    for (i, &(_, name, size)) in entries.iter().enumerate() {
        if name.len() > NAME_LEN || name.contains('\0') {
            return Err(Error::BadName(i));
        }
        offsets[i] = pos;
        let end = pos.checked_add(size).ok_or(Error::BadRange(i))?;
        pos = if i + 1 < entries.len() {
            align(end).ok_or(Error::BadRange(i))?
        } else {
            end
        };
    }
    let total = pos;
    out[..table_end as usize].fill(0);
    out[..8].copy_from_slice(&MAGIC);
    out[8..12].copy_from_slice(&VERSION.to_le_bytes());
    out[12..16].copy_from_slice(&(entries.len() as u32).to_le_bytes());
    out[16..24].copy_from_slice(&total.to_le_bytes());
    for (i, &(kind, name, size)) in entries.iter().enumerate() {
        let at = HEADER_SIZE + i * ENTRY_SIZE;
        out[at..at + 4].copy_from_slice(&(kind as u32).to_le_bytes());
        out[at + 8..at + 16].copy_from_slice(&offsets[i].to_le_bytes());
        out[at + 16..at + 24].copy_from_slice(&size.to_le_bytes());
        out[at + 32..at + 32 + name.len()].copy_from_slice(name.as_bytes());
    }
    Ok((offsets, total))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A package built in a fixed buffer: entries are (kind, name, data).
    fn build(entries: &[(Kind, &str, &[u8])]) -> Vec<u8> {
        let spec: Vec<(Kind, &str, u64)> = entries
            .iter()
            .map(|&(k, n, d)| (k, n, d.len() as u64))
            .collect();
        let mut out = vec![0u8; 1 << 16];
        let (offsets, total) = layout(&spec, &mut out).unwrap();
        for (i, (_, _, d)) in entries.iter().enumerate() {
            let o = offsets[i] as usize;
            out[o..o + d.len()].copy_from_slice(d);
        }
        out.truncate(total as usize);
        out
    }

    #[test]
    fn round_trip() {
        let b = build(&[
            (Kind::Kernel, "", b"KERNEL"),
            (Kind::Initramfs, "", b"CPIO"),
            (Kind::Dtb, "huaxing", b"H"),
            (Kind::Dtb, "tianma", b"T"),
        ]);
        let p = Package::parse(&b).unwrap();
        assert_eq!(p.len(), 4);
        assert_eq!(p.kernel().data, b"KERNEL");
        assert_eq!(p.kernel().offset, 4096);
        assert_eq!(p.initramfs().unwrap().data, b"CPIO");
        assert_eq!(p.initramfs().unwrap().offset, 8192);
        assert_eq!(p.dtb("tianma").unwrap().data, b"T");
        assert!(p.dtb("other").is_none());
        assert_eq!(p.dtbs().count(), 2);
        // Trailing bytes (a ramdisk padded to a page) are ignored.
        let mut padded = b.clone();
        padded.resize(b.len() + 100, 0xaa);
        assert_eq!(Package::parse(&padded).unwrap().bytes().len(), b.len());
    }

    #[test]
    fn rejects_structural_errors() {
        let good = build(&[(Kind::Kernel, "", b"K"), (Kind::Dtb, "a", b"D")]);
        let mut b = good.clone();
        b[30] = 1;
        assert_eq!(Package::parse(&b).unwrap_err(), Error::BadHeader);
        let mut b = good.clone();
        b[0] = b'X';
        assert_eq!(Package::parse(&b).unwrap_err(), Error::BadMagic);
        let mut b = good.clone();
        b[8] = 2;
        assert_eq!(
            Package::parse(&b).unwrap_err(),
            Error::UnsupportedVersion(2)
        );
        let mut b = good.clone();
        b[12] = 0;
        assert_eq!(Package::parse(&b).unwrap_err(), Error::BadCount);
        let mut b = good.clone();
        b[12] = 17;
        assert_eq!(Package::parse(&b).unwrap_err(), Error::BadCount);
        assert_eq!(
            Package::parse(&good[..good.len() - 1]).unwrap_err(),
            Error::BadTotalSize
        );
        assert_eq!(Package::parse(&good[..10]).unwrap_err(), Error::Truncated);
        // Unknown kind, flags, reserved.
        for at in [64, 68, 88] {
            let mut b = good.clone();
            b[at] = 9;
            assert_eq!(
                Package::parse(&b).unwrap_err(),
                Error::BadEntry(0),
                "byte {at}"
            );
        }
        // Data past the end, or inside the table.
        let mut b = good.clone();
        b[64 + 16 + 2] = 0xff;
        assert_eq!(Package::parse(&b).unwrap_err(), Error::BadRange(0));
        let mut b = good.clone();
        b[64 + 8..64 + 16].copy_from_slice(&64u64.to_le_bytes());
        assert_eq!(Package::parse(&b).unwrap_err(), Error::BadRange(0));
        // Overlap: point the DTB at the kernel's data.
        let mut b = good.clone();
        b[128 + 8..128 + 16].copy_from_slice(&4096u64.to_le_bytes());
        assert_eq!(Package::parse(&b).unwrap_err(), Error::BadRange(1));
        // Garbage after the name's NUL.
        let mut b = good.clone();
        b[128 + 32 + 5] = b'x';
        assert_eq!(Package::parse(&b).unwrap_err(), Error::BadName(1));
    }

    #[test]
    fn rejects_bad_entry_sets() {
        let none = build(&[(Kind::Dtb, "a", b"D")]);
        assert_eq!(Package::parse(&none).unwrap_err(), Error::KernelCount);
        let two = build(&[(Kind::Kernel, "", b"K"), (Kind::Kernel, "", b"K")]);
        assert_eq!(Package::parse(&two).unwrap_err(), Error::KernelCount);
        let rd = build(&[
            (Kind::Kernel, "", b"K"),
            (Kind::Initramfs, "", b"1"),
            (Kind::Initramfs, "", b"2"),
        ]);
        assert_eq!(Package::parse(&rd).unwrap_err(), Error::InitramfsCount);
        let dup = build(&[
            (Kind::Kernel, "", b"K"),
            (Kind::Dtb, "x", b"1"),
            (Kind::Dtb, "x", b"2"),
        ]);
        assert_eq!(Package::parse(&dup).unwrap_err(), Error::BadName(2));
        let empty = build(&[(Kind::Kernel, "", b"K"), (Kind::Dtb, "", b"1")]);
        assert_eq!(Package::parse(&empty).unwrap_err(), Error::BadName(1));
        let mut out = [0u8; 4096];
        let long = "x".repeat(33);
        assert_eq!(
            layout(&[(Kind::Kernel, &long, 1)], &mut out).unwrap_err(),
            Error::BadName(0)
        );
    }

    #[test]
    fn mutations_never_panic() {
        let good = build(&[
            (Kind::Kernel, "", b"KERNEL"),
            (Kind::Initramfs, "", b"CPIO"),
            (Kind::Dtb, "huaxing", b"H"),
        ]);
        let mut x = 0x9e37_79b9_7f4a_7c15u64;
        for _ in 0..20_000 {
            let mut b = good.clone();
            for _ in 0..3 {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                let at = (x % 256) as usize;
                b[at] = (x >> 32) as u8;
            }
            if let Ok(p) = Package::parse(&b) {
                for e in p.entries() {
                    assert!(e.offset as usize + e.data.len() <= p.bytes().len());
                }
                let _ = (p.kernel(), p.initramfs(), p.dtbs().count());
            }
        }
    }
}
