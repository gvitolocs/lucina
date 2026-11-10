//! Block access and the write-back cache in front of it.
//!
//! The file system code reads and writes arbitrary byte ranges. The device only sees whole
//! [`CACHE_BLOCK`]-byte blocks at multiples of that size (the last one may be shorter when the device size is not
//! a multiple), which is what a disk with 4096-byte logical blocks needs: the FAT sector size of the volume never
//! decides what the device is asked for. Dirty blocks are written back in a fixed order (file data, then the FAT,
//! then directories, then the boot sector and FSInfo) with a device flush after each of those classes, so the order
//! holds on a device that reorders what is in its cache.

use alloc::{collections::BTreeMap, vec, vec::Vec};

use crate::error::{Error, Result};

/// Size of a cache block and of every device transfer.
pub const CACHE_BLOCK: usize = 4096;

/// Class of a write, which orders the write-back: lower classes reach the device first.
pub(crate) const CLASS_DATA: u8 = 0;
pub(crate) const CLASS_FAT: u8 = 1;
pub(crate) const CLASS_DIR: u8 = 2;
pub(crate) const CLASS_BOOT: u8 = 3;

/// The device under a volume: a byte-addressed array that is read and written in whole cache blocks.
pub trait BlockIo {
    /// Size of the device (or partition) in bytes.
    fn size(&self) -> u64;
    /// Reads `buf.len()` bytes at `offset`. `offset` is a multiple of [`CACHE_BLOCK`] and `buf.len()` at most that.
    fn read(&mut self, offset: u64, buf: &mut [u8]) -> Result<()>;
    /// Writes `buf` at `offset`; same alignment as [`BlockIo::read`].
    fn write(&mut self, offset: u64, buf: &[u8]) -> Result<()>;
    /// Makes everything written so far durable (a cache flush command).
    fn flush(&mut self) -> Result<()>;
}

struct Block {
    data: Vec<u8>,
    dirty: bool,
    class: u8,
    tick: u64,
}

/// A write-back block cache.
pub(crate) struct Cache<D: BlockIo> {
    dev: D,
    size: u64,
    blocks: BTreeMap<u64, Block>,
    /// Clean blocks only, oldest first: tick -> block number.
    lru: BTreeMap<u64, u64>,
    tick: u64,
    capacity: usize,
    dirty: usize,
    /// A device write or flush failed. What is on the medium is unknown from then on, so every later sync reports it:
    /// a retried fsync must not succeed over data that may never have landed.
    failed: bool,
}

impl<D: BlockIo> Cache<D> {
    pub(crate) fn new(dev: D, capacity_blocks: usize) -> Self {
        let size = dev.size();
        Cache {
            dev,
            size,
            blocks: BTreeMap::new(),
            lru: BTreeMap::new(),
            tick: 0,
            capacity: capacity_blocks.max(16),
            dirty: 0,
            failed: false,
        }
    }

    pub(crate) fn device(&self) -> &D {
        &self.dev
    }

    pub(crate) fn into_device(self) -> D {
        self.dev
    }

    fn next_tick(&mut self) -> u64 {
        self.tick += 1;
        self.tick
    }

    fn valid_len(&self, blk: u64) -> usize {
        let off = blk * CACHE_BLOCK as u64;
        (self.size.saturating_sub(off)).min(CACHE_BLOCK as u64) as usize
    }

    /// Makes block `blk` resident; `fill` reads it from the device, otherwise it starts zeroed.
    fn ensure(&mut self, blk: u64, fill: bool) -> Result<()> {
        if let Some(b) = self.blocks.get_mut(&blk) {
            if !b.dirty {
                let old = b.tick;
                self.tick += 1;
                b.tick = self.tick;
                self.lru.remove(&old);
                self.lru.insert(self.tick, blk);
            }
            return Ok(());
        }
        self.make_room()?;
        let mut data = vec![0u8; CACHE_BLOCK];
        if fill {
            let len = self.valid_len(blk);
            if len == 0 {
                return Err(Error::Io);
            }
            self.dev.read(blk * CACHE_BLOCK as u64, &mut data[..len])?;
        }
        let tick = self.next_tick();
        self.lru.insert(tick, blk);
        self.blocks.insert(
            blk,
            Block {
                data,
                dirty: false,
                class: 0,
                tick,
            },
        );
        Ok(())
    }

    fn make_room(&mut self) -> Result<()> {
        while self.blocks.len() >= self.capacity {
            if self.lru.is_empty() {
                self.write_back()?;
                continue;
            }
            let (&tick, &blk) = self.lru.iter().next().expect("lru is not empty");
            self.lru.remove(&tick);
            self.blocks.remove(&blk);
        }
        Ok(())
    }

    /// Reads `buf.len()` bytes at byte offset `off`.
    pub(crate) fn read_at(&mut self, off: u64, buf: &mut [u8]) -> Result<()> {
        let end = off.checked_add(buf.len() as u64).ok_or(Error::Io)?;
        if end > self.size {
            return Err(Error::Io);
        }
        let mut done = 0usize;
        while done < buf.len() {
            let pos = off + done as u64;
            let blk = pos / CACHE_BLOCK as u64;
            let in_blk = (pos % CACHE_BLOCK as u64) as usize;
            let n = (CACHE_BLOCK - in_blk).min(buf.len() - done);
            self.ensure(blk, true)?;
            let b = self.blocks.get(&blk).expect("resident");
            buf[done..done + n].copy_from_slice(&b.data[in_blk..in_blk + n]);
            done += n;
        }
        Ok(())
    }

    /// Writes `buf` at byte offset `off` into the cache (read-modify-write for partial blocks).
    pub(crate) fn write_at(&mut self, off: u64, buf: &[u8], class: u8) -> Result<()> {
        let end = off.checked_add(buf.len() as u64).ok_or(Error::Io)?;
        if end > self.size {
            return Err(Error::Io);
        }
        let mut done = 0usize;
        while done < buf.len() {
            let pos = off + done as u64;
            let blk = pos / CACHE_BLOCK as u64;
            let in_blk = (pos % CACHE_BLOCK as u64) as usize;
            let n = (CACHE_BLOCK - in_blk).min(buf.len() - done);
            // A write that covers the whole valid part of the block needs no read first.
            let whole = in_blk == 0 && n >= self.valid_len(blk);
            self.ensure(blk, !whole)?;
            let b = self.blocks.get_mut(&blk).expect("resident");
            b.data[in_blk..in_blk + n].copy_from_slice(&buf[done..done + n]);
            if !b.dirty {
                b.dirty = true;
                self.lru.remove(&b.tick);
                self.dirty += 1;
            }
            b.class = b.class.max(class);
            done += n;
        }
        if self.dirty >= self.capacity / 2 {
            self.write_back()?;
        }
        Ok(())
    }

    /// Zeroes `len` bytes at `off`.
    pub(crate) fn zero_at(&mut self, off: u64, len: u64, class: u8) -> Result<()> {
        let zeros = [0u8; CACHE_BLOCK];
        let mut done = 0u64;
        while done < len {
            let n = ((len - done) as usize).min(CACHE_BLOCK);
            // Keep block alignment so that whole blocks are overwritten without being read.
            let pos = off + done;
            let to_boundary = CACHE_BLOCK - (pos % CACHE_BLOCK as u64) as usize;
            let n = n.min(to_boundary);
            self.write_at(pos, &zeros[..n], class)?;
            done += n as u64;
        }
        Ok(())
    }

    /// Asks the device to make what it has durable; a failure sticks (see [`Cache::failed`]).
    fn device_flush(&mut self) -> Result<()> {
        let r = self.dev.flush();
        if r.is_err() {
            self.failed = true;
        }
        r
    }

    /// Writes every dirty block to the device, lowest class first. A device may reorder writes until a flush returns,
    /// so each class ends in a flush: the classes reach the medium in order, and nothing written here can be overtaken
    /// by what is written later.
    fn write_back(&mut self) -> Result<()> {
        if self.failed {
            return Err(Error::Io);
        }
        let mut order: Vec<(u8, u64)> = self
            .blocks
            .iter()
            .filter(|(_, b)| b.dirty)
            .map(|(&n, b)| (b.class, n))
            .collect();
        order.sort_unstable();
        let mut class = None;
        for (c, blk) in order {
            if class.is_some_and(|prev| prev != c) {
                self.device_flush()?;
            }
            class = Some(c);
            let len = self.valid_len(blk);
            let tick = self.next_tick();
            let b = self.blocks.get_mut(&blk).expect("resident");
            if let Err(e) = self.dev.write(blk * CACHE_BLOCK as u64, &b.data[..len]) {
                self.failed = true;
                return Err(e);
            }
            b.dirty = false;
            b.class = 0;
            b.tick = tick;
            self.dirty -= 1;
            self.lru.insert(tick, blk);
        }
        if class.is_some() {
            self.device_flush()?;
        }
        Ok(())
    }

    /// Writes everything back and makes it durable. With nothing dirty there is nothing to flush: every earlier write
    /// back ended in a flush. After a failed write or flush this fails for good.
    pub(crate) fn flush(&mut self) -> Result<()> {
        self.write_back()
    }

    /// Forgets every clean block, so that the next reads come from the device.
    pub(crate) fn drop_clean(&mut self) {
        let clean: Vec<u64> = self.lru.values().copied().collect();
        for blk in clean {
            self.blocks.remove(&blk);
        }
        self.lru.clear();
    }
}
