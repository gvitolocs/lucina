// SPDX-License-Identifier: MPL-2.0

//! The kernel's own symbol table, so that the crash screen can name the frames of a backtrace.
//!
//! Linux links a `kallsyms` table into the kernel in a second pass. Juno's kernel instead carries a fixed placeholder,
//! [`Placeholder`], exported as `LUCINA_KSYMS`; right after linking, the build ([`build::fill_file`], run by OSDK
//! before it makes the `Image`) writes the table of that very ELF into it. The addresses do not move, the table always
//! belongs to the kernel that carries it, and a kernel loaded with kexec brings its own. A placeholder left empty (a
//! build without the step, a table that did not fit) reads as no table, and the screen shows bare addresses as before.
//!
//! Layout, little-endian, from the start of the placeholder:
//!
//! | at | what |
//! |---|---|
//! | 0 | [`MAGIC`], which the builder looks for |
//! | 16 | capacity: the placeholder's size in bytes |
//! | 20 | state: [`FILLED`] once written, else 0 |
//! | 24 | count of entries |
//! | 28 | where the names start |
//! | 32 | base: the address the entries count from |
//! | 40 | entries, 12 bytes each, by address: start − base (u32), size (u32), name (offset into the names << 8, length) |
//!
//! Lookup ([`Table::parse`], [`Table::lookup`]) reads only inside the placeholder and checks every offset, so a corrupt or
//! half-written table gives no name rather than a fault: it runs while the kernel crashes.
#![no_std]

/// What the builder looks for. The kernel never compares it, so it is in the image once, in the placeholder.
pub const MAGIC: [u8; 16] = *b"LUCINA KSYMS v1\0";
/// The state of a written table.
pub const FILLED: u32 = 0x4b53_5946;
/// The placeholder's size. The Juno kernel's 15 600 functions take 574 KB with short names cut at 96 bytes (2026-11-08);
/// a table that outgrows it is written with shorter names.
pub const CAPACITY: usize = 1 << 20;
/// Bytes before the entries.
pub const HEADER: usize = 40;
/// Bytes per entry.
pub const ENTRY: usize = 12;

/// The space in the image that the build fills. The kernel declares one, as `LUCINA_KSYMS`, with [`placeholder`].
#[repr(C, align(8))]
pub struct Placeholder {
    magic: [u8; 16],
    capacity: u32,
    rest: [u8; CAPACITY - 20],
}

/// An empty placeholder: the magic, the capacity, then zeros.
pub const fn placeholder() -> Placeholder {
    Placeholder {
        magic: MAGIC,
        capacity: CAPACITY as u32,
        rest: [0; CAPACITY - 20],
    }
}

impl Placeholder {
    /// The bytes as they are in memory now, read through a pointer the compiler cannot see through: it would otherwise
    /// assume the zeros it was given and fold every lookup away.
    pub fn bytes(&'static self) -> &'static [u8] {
        let at = core::hint::black_box(self as *const Placeholder as *const u8);
        // SAFETY: `at` is this placeholder's address, valid for its size for 'static, and nothing writes it at run time
        // (the build wrote it into the image).
        unsafe { core::slice::from_raw_parts(at, core::mem::size_of::<Placeholder>()) }
    }
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at.checked_add(4)?)?.try_into().ok()?))
}

fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at.checked_add(8)?)?.try_into().ok()?))
}

/// A written table.
#[derive(Clone, Copy, Debug)]
pub struct Table<'a> {
    blob: &'a [u8],
    count: usize,
    names_at: usize,
    base: u64,
}

impl<'a> Table<'a> {
    /// The table in `blob`, if one was written and its header holds together.
    pub fn parse(blob: &'a [u8]) -> Option<Self> {
        let capacity = u32_at(blob, 16)? as usize;
        let blob = blob.get(..capacity)?;
        if u32_at(blob, 20)? != FILLED {
            return None;
        }
        let count = u32_at(blob, 24)? as usize;
        let names_at = u32_at(blob, 28)? as usize;
        let entries_end = count.checked_mul(ENTRY)?.checked_add(HEADER)?;
        if entries_end > names_at || names_at > blob.len() {
            return None;
        }
        Some(Self {
            blob,
            count,
            names_at,
            base: u64_at(blob, 32)?,
        })
    }

    /// How many functions it names.
    pub fn len(&self) -> usize {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Entry `i`: start, size, name.
    fn entry(&self, i: usize) -> Option<(u64, u64, &'a str)> {
        let at = HEADER + i * ENTRY;
        let start = self.base.checked_add(u64::from(u32_at(self.blob, at)?))?;
        let size = u64::from(u32_at(self.blob, at + 4)?);
        let name = u32_at(self.blob, at + 8)?;
        let (offset, len) = ((name >> 8) as usize, (name & 0xff) as usize);
        let from = self.names_at.checked_add(offset)?;
        let bytes = self.blob.get(from..from.checked_add(len)?)?;
        Some((start, size, core::str::from_utf8(bytes).ok()?))
    }

    /// The function `address` is in, and how far into it: the last one starting at or before it, if it ends after it.
    /// A size of 0 (an assembly label) reaches to the next function.
    pub fn lookup(&self, address: u64) -> Option<(&'a str, u64)> {
        if address < self.base {
            return None;
        }
        // The first entry starting after `address`.
        let (mut low, mut high) = (0, self.count);
        while low < high {
            let mid = low + (high - low) / 2;
            if self.entry(mid)?.0 <= address {
                low = mid + 1;
            } else {
                high = mid;
            }
        }
        let (start, size, name) = self.entry(low.checked_sub(1)?)?;
        let offset = address - start;
        (size == 0 || offset < size).then_some((name, offset))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The kernel's placeholder is as large as it says, and an empty one is no table.
    #[test]
    fn the_placeholder_says_its_size() {
        assert_eq!(core::mem::size_of::<Placeholder>(), CAPACITY);
        static P: Placeholder = placeholder();
        let bytes = P.bytes();
        assert_eq!(&bytes[..16], &MAGIC);
        assert_eq!(u32::from_le_bytes(bytes[16..20].try_into().unwrap()) as usize, CAPACITY);
        assert!(Table::parse(bytes).is_none());
    }
}
