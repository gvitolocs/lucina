//! ext4's extent trees, for reading.
//!
//! An ext4 file with `EXT4_EXTENTS_FL` has a tree in the 60 bytes of `i_block`, not block pointers. This reads the tree of
//! one file into a sorted list, which is all a read-only filesystem needs. Source of truth for the format: Linux
//! `fs/ext4/ext4_extents.h` and `fs/ext4/extents.c`.
//!
//! What is on the disk is not trusted: magic, entry counts, depth, order and every block number are checked, and a tree that
//! fails any of it is an error and not a wrong read. Other programs' disks are not what this is for, but a flash cell going
//! bad can do the same.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::{boxed::Box, vec::Vec};

pub const BLOCK_SIZE: usize = 4096;
/// The size of `i_block`.
pub const ROOT_SIZE: usize = 60;

const MAGIC: u16 = 0xf30a;
const ENTRY_SIZE: usize = 12;
/// `EXT4_MAX_EXTENT_DEPTH`.
const MAX_DEPTH: u16 = 5;
/// `EXT_INIT_MAX_LEN`: a longer `ee_len` is an extent that has been allocated and not written, and reads as zeros.
const INIT_MAX_LEN: u16 = 1 << 15;
/// Blocks of the tree read for one file, so that one bad tree cannot take the machine with it. With 340 extents per block
/// this is over 22 million extents.
const MAX_NODES: usize = 65536;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error<E> {
    /// A node of the tree is not one, and what is wrong with it.
    Bad(&'static str),
    /// Block numbers over 32 bits, which the filesystem around this does not have.
    TooHigh,
    /// Reading a block failed.
    Read(E),
}

/// Blocks `first..first + len` of the file are at `start..start + len` on the disk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Extent {
    pub first: u32,
    pub len: u32,
    pub start: u32,
}

/// What is at a block of a file, from there on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Run {
    /// These many blocks follow each other on the disk, from `start`.
    Mapped { start: u32, len: u32 },
    /// These many blocks are not on the disk, and read as zeros.
    Hole { len: u32 },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExtentMap {
    extents: Vec<Extent>,
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

struct Walk<'a, E, F: FnMut(u32, &mut [u8; BLOCK_SIZE]) -> Result<(), E>> {
    read_block: &'a mut F,
    total_blocks: u32,
    extents: Vec<Extent>,
    /// Where the last extent or index ended, for the order.
    next_first: u64,
    nodes: usize,
}

impl<E, F: FnMut(u32, &mut [u8; BLOCK_SIZE]) -> Result<(), E>> Walk<'_, E, F> {
    /// One node, and what is under it. `depth` is what the parent says this one has.
    fn node(&mut self, node: &[u8], depth: Option<u16>) -> Result<(), Error<E>> {
        if u16_at(node, 0) != MAGIC {
            return Err(Error::Bad("no magic number"));
        }
        let (entries, max, own_depth) = (
            u16_at(node, 2) as usize,
            u16_at(node, 4) as usize,
            u16_at(node, 6),
        );
        // A tree of depth 0 with nothing in it is an empty file, which any node may look like while it is being written.
        if entries > max || max > node.len() / ENTRY_SIZE - 1 {
            return Err(Error::Bad("more entries than fit"));
        }
        if own_depth > MAX_DEPTH || depth.is_some_and(|d| d != own_depth) {
            return Err(Error::Bad("a depth that does not fit its parent"));
        }

        for i in 0..entries {
            let entry = &node[ENTRY_SIZE * (i + 1)..ENTRY_SIZE * (i + 2)];
            let first = u32_at(entry, 0);
            if (first as u64) < self.next_first {
                return Err(Error::Bad("entries out of order"));
            }
            if own_depth == 0 {
                self.leaf(entry, first)?;
            } else {
                let (leaf_lo, leaf_hi) = (u32_at(entry, 4), u16_at(entry, 8));
                if leaf_hi != 0 {
                    return Err(Error::TooHigh);
                }
                if leaf_lo == 0 || leaf_lo >= self.total_blocks {
                    return Err(Error::Bad("a node past the end of the disk"));
                }
                self.nodes += 1;
                if self.nodes > MAX_NODES {
                    return Err(Error::Bad("too many nodes"));
                }
                let mut block = Box::new([0u8; BLOCK_SIZE]);
                (self.read_block)(leaf_lo, &mut block).map_err(Error::Read)?;
                self.node(&block[..], Some(own_depth - 1))?;
            }
        }
        Ok(())
    }

    fn leaf(&mut self, entry: &[u8], first: u32) -> Result<(), Error<E>> {
        let (raw_len, start_hi, start_lo) = (u16_at(entry, 4), u16_at(entry, 6), u32_at(entry, 8));
        let (len, written) = if raw_len > INIT_MAX_LEN {
            (raw_len - INIT_MAX_LEN, false)
        } else {
            (raw_len, true)
        };
        if len == 0 {
            return Err(Error::Bad("an empty extent"));
        }
        if start_hi != 0 {
            return Err(Error::TooHigh);
        }
        if start_lo as u64 + len as u64 > self.total_blocks as u64 || start_lo == 0 {
            return Err(Error::Bad("an extent past the end of the disk"));
        }
        let end = first as u64 + len as u64;
        if end > u32::MAX as u64 {
            return Err(Error::Bad("an extent past the end of a file"));
        }
        self.next_first = end;
        if !written {
            return Ok(());
        }
        // Merging is not needed for the answers to be right, but it makes for longer reads.
        if let Some(last) = self.extents.last_mut()
            && last.first + last.len == first
            && last.start as u64 + last.len as u64 == start_lo as u64
        {
            last.len += len as u32;
        } else {
            self.extents.push(Extent {
                first,
                len: len as u32,
                start: start_lo,
            });
        }
        Ok(())
    }
}

impl ExtentMap {
    /// Reads the tree whose root is `i_block`. `total_blocks` is the size of the filesystem. `read_block` gets a block of the
    /// disk.
    pub fn read<E, F>(
        root: &[u8; ROOT_SIZE],
        total_blocks: u32,
        mut read_block: F,
    ) -> Result<Self, Error<E>>
    where
        F: FnMut(u32, &mut [u8; BLOCK_SIZE]) -> Result<(), E>,
    {
        let mut walk = Walk {
            read_block: &mut read_block,
            total_blocks,
            extents: Vec::new(),
            next_first: 0,
            nodes: 0,
        };
        walk.node(root, None)?;
        Ok(Self {
            extents: walk.extents,
        })
    }

    pub fn extents(&self) -> &[Extent] {
        &self.extents
    }

    /// What is at `block`, for up to `max` blocks from there.
    pub fn lookup(&self, block: u32, max: u32) -> Run {
        let after = self.extents.partition_point(|e| e.first <= block);
        if after > 0 {
            let e = self.extents[after - 1];
            let into = block - e.first;
            if into < e.len {
                return Run::Mapped {
                    start: e.start + into,
                    len: (e.len - into).min(max),
                };
            }
        }
        let hole = self
            .extents
            .get(after)
            .map_or(max, |next| (next.first - block).min(max));
        Run::Hole { len: hole }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::{collections::HashMap, vec, vec::Vec};

    use super::*;

    const TOTAL: u32 = 1000;

    fn header(entries: u16, max: u16, depth: u16) -> Vec<u8> {
        let mut h = Vec::new();
        h.extend_from_slice(&MAGIC.to_le_bytes());
        h.extend_from_slice(&entries.to_le_bytes());
        h.extend_from_slice(&max.to_le_bytes());
        h.extend_from_slice(&depth.to_le_bytes());
        h.extend_from_slice(&0u32.to_le_bytes());
        h
    }

    fn leaf(first: u32, len: u16, start: u32) -> [u8; 12] {
        let mut e = [0u8; 12];
        e[0..4].copy_from_slice(&first.to_le_bytes());
        e[4..6].copy_from_slice(&len.to_le_bytes());
        e[8..12].copy_from_slice(&start.to_le_bytes());
        e
    }

    fn index(first: u32, block: u32) -> [u8; 12] {
        let mut e = [0u8; 12];
        e[0..4].copy_from_slice(&first.to_le_bytes());
        e[4..8].copy_from_slice(&block.to_le_bytes());
        e
    }

    fn root(entries: &[[u8; 12]], depth: u16) -> [u8; ROOT_SIZE] {
        let mut r = [0u8; ROOT_SIZE];
        let h = header(entries.len() as u16, 4, depth);
        r[..12].copy_from_slice(&h);
        for (i, e) in entries.iter().enumerate() {
            r[12 * (i + 1)..12 * (i + 2)].copy_from_slice(e);
        }
        r
    }

    fn block(entries: &[[u8; 12]], depth: u16) -> [u8; BLOCK_SIZE] {
        let mut b = [0u8; BLOCK_SIZE];
        let h = header(entries.len() as u16, 340, depth);
        b[..12].copy_from_slice(&h);
        for (i, e) in entries.iter().enumerate() {
            b[12 * (i + 1)..12 * (i + 2)].copy_from_slice(e);
        }
        b
    }

    fn read(root: &[u8; ROOT_SIZE], disk: &HashMap<u32, [u8; BLOCK_SIZE]>) -> Result<ExtentMap, Error<()>> {
        ExtentMap::read(root, TOTAL, |at, buf: &mut [u8; BLOCK_SIZE]| {
            buf.copy_from_slice(disk.get(&at).ok_or(())?);
            Ok(())
        })
    }

    fn none() -> HashMap<u32, [u8; BLOCK_SIZE]> {
        HashMap::new()
    }

    #[test]
    fn an_empty_file() {
        let map = read(&root(&[], 0), &none()).unwrap();
        assert!(map.extents().is_empty());
        assert_eq!(map.lookup(0, 10), Run::Hole { len: 10 });
    }

    #[test]
    fn lookups_in_one_leaf() {
        let map = read(&root(&[leaf(0, 3, 100), leaf(10, 2, 200)], 0), &none()).unwrap();
        assert_eq!(map.lookup(0, 100), Run::Mapped { start: 100, len: 3 });
        assert_eq!(map.lookup(1, 100), Run::Mapped { start: 101, len: 2 });
        assert_eq!(map.lookup(1, 1), Run::Mapped { start: 101, len: 1 });
        assert_eq!(map.lookup(3, 100), Run::Hole { len: 7 });
        assert_eq!(map.lookup(3, 2), Run::Hole { len: 2 });
        assert_eq!(map.lookup(11, 100), Run::Mapped { start: 201, len: 1 });
        assert_eq!(map.lookup(12, 100), Run::Hole { len: 100 });
    }

    #[test]
    fn a_hole_at_the_start() {
        let map = read(&root(&[leaf(5, 1, 100)], 0), &none()).unwrap();
        assert_eq!(map.lookup(0, 100), Run::Hole { len: 5 });
    }

    #[test]
    fn a_tree_of_two_levels_and_three() {
        let mut disk = none();
        disk.insert(50, block(&[leaf(0, 2, 100), leaf(2, 2, 300)], 0));
        disk.insert(51, block(&[leaf(10, 1, 400)], 0));
        let two = root(&[index(0, 50), index(10, 51)], 1);
        let map = read(&two, &disk).unwrap();
        assert_eq!(map.lookup(3, 9), Run::Mapped { start: 301, len: 1 });
        assert_eq!(map.lookup(10, 9), Run::Mapped { start: 400, len: 1 });

        disk.insert(60, block(&[index(0, 50), index(10, 51)], 1));
        let three = root(&[index(0, 60)], 2);
        assert_eq!(read(&three, &disk).unwrap(), map);
    }

    #[test]
    fn extents_that_follow_each_other_become_one() {
        let map = read(&root(&[leaf(0, 2, 100), leaf(2, 2, 102), leaf(4, 2, 200)], 0), &none()).unwrap();
        assert_eq!(map.extents().len(), 2);
        assert_eq!(map.lookup(0, 100), Run::Mapped { start: 100, len: 4 });
    }

    #[test]
    fn what_was_not_written_reads_as_zeros() {
        // A length over 32768 says so, and the rest is the length.
        let map = read(&root(&[leaf(0, 1, 100), leaf(1, 32768 + 5, 101), leaf(6, 1, 106)], 0), &none()).unwrap();
        assert_eq!(map.lookup(0, 100), Run::Mapped { start: 100, len: 1 });
        assert_eq!(map.lookup(1, 100), Run::Hole { len: 5 });
        assert_eq!(map.lookup(6, 100), Run::Mapped { start: 106, len: 1 });
    }

    #[test]
    fn exactly_32768_blocks_is_written() {
        let map = ExtentMap::read(&root(&[leaf(0, 32768, 1)], 0), 40000, |_, _: &mut [u8; BLOCK_SIZE]| Err::<(), ()>(())).unwrap();
        assert_eq!(map.lookup(32767, 9), Run::Mapped { start: 32768, len: 1 });
    }

    fn bad(result: Result<ExtentMap, Error<()>>) {
        assert!(matches!(result, Err(Error::Bad(_) | Error::TooHigh)), "{result:?}");
    }

    #[test]
    fn nonsense_is_an_error() {
        // No magic.
        let mut r = root(&[leaf(0, 1, 100)], 0);
        r[0] = 0;
        bad(read(&r, &none()));
        // More entries than the node says fit, or than it has room for.
        let mut r = root(&[leaf(0, 1, 100)], 0);
        r[2] = 5;
        bad(read(&r, &none()));
        let mut r = root(&[leaf(0, 1, 100)], 0);
        r[2] = 5;
        r[4] = 5;
        bad(read(&r, &none()));
        // Too deep.
        bad(read(&root(&[], 6), &none()));
        // An extent of nothing, past the disk, at block 0, or with high bits.
        bad(read(&root(&[leaf(0, 0, 100)], 0), &none()));
        bad(read(&root(&[leaf(0, 1, TOTAL)], 0), &none()));
        bad(read(&root(&[leaf(0, 2, TOTAL - 1)], 0), &none()));
        bad(read(&root(&[leaf(0, 1, 0)], 0), &none()));
        let mut high = leaf(0, 1, 100);
        high[6] = 1;
        bad(read(&root(&[high], 0), &none()));
        // Out of order, overlapping, and past the end of a file.
        bad(read(&root(&[leaf(5, 1, 100), leaf(1, 1, 200)], 0), &none()));
        bad(read(&root(&[leaf(0, 3, 100), leaf(2, 1, 200)], 0), &none()));
        bad(read(&root(&[leaf(u32::MAX, 2, 100)], 0), &none()));
    }

    #[test]
    fn a_bad_child_is_an_error() {
        // A child at the wrong depth, its own parent, past the disk, and with high bits.
        let mut disk = none();
        disk.insert(50, block(&[index(0, 51)], 1));
        bad(read(&root(&[index(0, 50)], 1), &disk));
        disk.insert(70, block(&[index(0, 70)], 2));
        bad(read(&root(&[index(0, 70)], 3), &disk));
        bad(read(&root(&[index(0, TOTAL)], 1), &none()));
        bad(read(&root(&[index(0, 0)], 1), &none()));
        let mut high = index(0, 50);
        high[8] = 1;
        bad(read(&root(&[high], 1), &none()));
        // Children out of order.
        disk.insert(52, block(&[leaf(5, 1, 100)], 0));
        disk.insert(53, block(&[leaf(0, 1, 200)], 0));
        bad(read(&root(&[index(5, 52), index(6, 53)], 1), &disk));
    }

    #[test]
    fn a_failed_read_is_an_error() {
        assert_eq!(read(&root(&[index(0, 50)], 1), &none()), Err(Error::Read(())));
    }

    #[test]
    fn no_garbage_makes_it_panic() {
        // Whatever the bytes, it answers.
        let mut seed = 0x1234_5678u32;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        for _ in 0..20000 {
            let mut r = [0u8; ROOT_SIZE];
            for b in r.iter_mut() {
                *b = next() as u8;
            }
            r[0] = 0x0a;
            r[1] = 0xf3;
            let mut b = vec![0u8; BLOCK_SIZE];
            for x in b.iter_mut() {
                *x = next() as u8;
            }
            b[0] = 0x0a;
            b[1] = 0xf3;
            let _ = ExtentMap::read(&r, TOTAL, |_, buf: &mut [u8; BLOCK_SIZE]| {
                buf.copy_from_slice(&b);
                Ok::<(), ()>(())
            });
        }
    }
}
