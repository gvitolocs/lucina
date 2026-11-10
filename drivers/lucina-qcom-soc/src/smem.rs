//! Qualcomm shared memory, Linux 7.1 `drivers/soc/qcom/smem.c` (GPL-2.0-only). Adapted from narf
//! `drivers/soc/qcom/src/smem.rs` (GPL-2.0-or-later, not run on silicon).
//!
//! Corrections from the recheck against Linux: `SMEM_HOST_COUNT` is 25 here, not 20; `probe` now refuses a region
//! whose `initialized` is not 1 or whose `reserved` is not 0, as `qcom_smem_probe` does; walks are bounds-checked before
//! each read, so a bad length cannot read outside the partition.
//!
//! Layout: `struct smem_header` at offset 0 (proc_comm, `version[32]`, `initialized`, `free_offset`, `available`,
//! `reserved`, 512 TOC entries), the partition table `$TOC` in the last 4 KiB. `version[7] >> 16` selects global
//! partition (12) or global heap (11).
//!
//! Reads touch only what Linux's probe touches: the header's version and flags, the table, and the partitions of
//! the global host and of the application processor. Partitions between other processors are never opened, since
//! they may be protected from this exception level.

use alloc::vec::Vec;

use crate::hwlock::{HwLockError, HwSpinlock};
use crate::io::RegIo;

pub const SMEM_MASTER_SBL_VERSION_INDEX: u32 = 7;
pub const SMEM_GLOBAL_HEAP_VERSION: u32 = 11;
pub const SMEM_GLOBAL_PART_VERSION: u32 = 12;
pub const SMEM_ITEM_LAST_FIXED: u32 = 8;
pub const SMEM_ITEM_COUNT: u32 = 512;
pub const SMEM_HOST_APPS: u16 = 0;
pub const SMEM_GLOBAL_HOST: u16 = 0xfffe;
pub const SMEM_HOST_COUNT: u16 = 25;
pub const SMEM_PRIVATE_CANARY: u16 = 0xa5a5;
pub const PTABLE_MAGIC: [u8; 4] = *b"$TOC";
pub const PART_MAGIC: [u8; 4] = *b"$PRT";
pub const INFO_MAGIC: [u8; 4] = *b"SIII";

/// The modem's host id here: `qcom,remote-pid = <1>` of `smp2p-mpss` and of its `glink-edge`.
pub const HOST_MODEM: u16 = 1;
/// The audio DSP's host id: `qcom,remote-pid = <2>` of `smp2p-lpass` and of its `glink-edge`.
pub const HOST_ADSP: u16 = 2;
/// SMP2P items of the modem edge, `qcom,smem = <435 428>` (`smp2p-mpss`): 435 inbound, 428 outbound.
pub const ITEM_SMP2P_MODEM_IN: u32 = 435;
pub const ITEM_SMP2P_MODEM_OUT: u32 = 428;
/// The modem's crash reason string (`crash_reason_smem` of `mpss_resource_init`).
pub const ITEM_MODEM_CRASH_REASON: u32 = 421;
/// GLINK descriptor and FIFOs (`qcom_glink_smem.c`).
pub const ITEM_GLINK_DESCRIPTOR: u32 = 478;
pub const ITEM_GLINK_FIFO0: u32 = 479;
pub const ITEM_GLINK_FIFO1: u32 = 480;

const HDR_VERSION: u32 = 64;
const HDR_INITIALIZED: u32 = HDR_VERSION + 128;
const HDR_FREE_OFFSET: u32 = HDR_INITIALIZED + 4;
const HDR_AVAILABLE: u32 = HDR_INITIALIZED + 8;
const HDR_RESERVED: u32 = HDR_INITIALIZED + 12;
const HDR_TOC: u32 = HDR_INITIALIZED + 16;
const PTABLE_HDR: u32 = 32;
const PTABLE_ENTRY: u32 = 48;
const PART_HDR: u32 = 32;
const PRIV_ENTRY: u32 = 16;
const PTABLE_SIZE: u32 = 0x1000;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SmemError {
    /// `initialized != 1` or `reserved != 0`: "SMEM is not initialized by SBL".
    NotInitialized,
    UnsupportedVersion(u32),
    BadPtable,
    BadPartition,
    BadCanary,
    NotFound,
    BadItem,
    Exists,
    NoSpace,
    OutOfBounds,
    Lock(HwLockError),
}

impl From<HwLockError> for SmemError {
    fn from(e: HwLockError) -> Self {
        SmemError::Lock(e)
    }
}

/// An item: byte offset within the region and usable length.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct SmemItem {
    pub offset: u32,
    pub len: u32,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct Partition {
    base: u32,
    size: u32,
    cacheline: u32,
}

/// One row of the partition table, for the report.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct TableEntry {
    pub offset: u32,
    pub size: u32,
    pub host0: u16,
    pub host1: u16,
    pub cacheline: u32,
}

pub struct Smem<R> {
    mem: R,
    size: u32,
    item_count: u32,
    global: Option<Partition>,
    partitions: [Option<Partition>; SMEM_HOST_COUNT as usize],
    version: u32,
}

const fn align(v: u32, a: u32) -> u32 {
    (v.wrapping_add(a).wrapping_sub(1)) & !(a.wrapping_sub(1))
}

impl<R: RegIo> Smem<R> {
    fn magic(&self, off: u32) -> [u8; 4] {
        self.mem.read32(off).to_le_bytes()
    }
    fn lo16(&self, off: u32) -> u16 {
        self.mem.read32(off) as u16
    }
    fn hi16(&self, off: u32) -> u16 {
        (self.mem.read32(off) >> 16) as u16
    }

    /// `qcom_smem_probe` over a `size`-byte region (no hardware lock: this reads only what firmware set before boot).
    pub fn probe(mem: R, size: u32) -> Result<Self, SmemError> {
        if size < PTABLE_SIZE + HDR_TOC {
            return Err(SmemError::OutOfBounds);
        }
        let mut s = Self {
            mem,
            size,
            item_count: SMEM_ITEM_COUNT,
            global: None,
            partitions: [None; SMEM_HOST_COUNT as usize],
            version: 0,
        };
        if s.mem.read32(HDR_INITIALIZED) != 1 || s.mem.read32(HDR_RESERVED) != 0 {
            return Err(SmemError::NotInitialized);
        }
        s.version = s
            .mem
            .read32(HDR_VERSION + SMEM_MASTER_SBL_VERSION_INDEX * 4)
            >> 16;
        match s.version {
            SMEM_GLOBAL_PART_VERSION => {
                s.set_global_partition()?;
                s.item_count = s.read_item_count();
            }
            SMEM_GLOBAL_HEAP_VERSION => s.item_count = SMEM_ITEM_COUNT,
            other => return Err(SmemError::UnsupportedVersion(other)),
        }
        s.enumerate_partitions(SMEM_HOST_APPS)?;
        Ok(s)
    }

    /// The scheme: 11 global heap, 12 global partition.
    pub fn version(&self) -> u32 {
        self.version
    }
    pub fn item_count(&self) -> u32 {
        self.item_count
    }
    pub fn mem(&self) -> &R {
        &self.mem
    }
    /// Whether a private partition is open for `host`.
    pub fn has_partition(&self, host: u16) -> bool {
        host < SMEM_HOST_COUNT && self.partitions[host as usize].is_some()
    }
    /// Whether the global partition was found.
    pub fn has_global_partition(&self) -> bool {
        self.global.is_some()
    }

    fn ptable(&self) -> Option<u32> {
        let p = self.size - PTABLE_SIZE;
        if self.magic(p) != PTABLE_MAGIC || self.mem.read32(p + 4) != 1 {
            return None;
        }
        Some(p)
    }

    fn ptable_entries(&self) -> u32 {
        self.ptable()
            .map(|p| self.mem.read32(p + 8))
            .unwrap_or(0)
            .min((PTABLE_SIZE - PTABLE_HDR) / PTABLE_ENTRY)
    }

    /// `qcom_smem_get_item_count`.
    fn read_item_count(&self) -> u32 {
        let Some(p) = self.ptable() else {
            return SMEM_ITEM_COUNT;
        };
        let info = p + PTABLE_HDR + self.ptable_entries() * PTABLE_ENTRY;
        if info + 20 > self.size || self.magic(info) != INFO_MAGIC {
            return SMEM_ITEM_COUNT;
        }
        self.lo16(info + 16) as u32
    }

    /// Entry `i` of the partition table.
    pub fn table_entry(&self, i: u32) -> Option<TableEntry> {
        if i >= self.ptable_entries() {
            return None;
        }
        let e = self.ptable()? + PTABLE_HDR + i * PTABLE_ENTRY;
        Some(TableEntry {
            offset: self.mem.read32(e),
            size: self.mem.read32(e + 4),
            host0: self.lo16(e + 12),
            host1: self.hi16(e + 12),
            cacheline: self.mem.read32(e + 16),
        })
    }

    /// `qcom_smem_partition_header`.
    fn partition(&self, e: TableEntry) -> Result<Partition, SmemError> {
        let in_region = e
            .offset
            .checked_add(e.size)
            .is_some_and(|end| end <= self.size);
        if !in_region || e.size < PART_HDR || !e.offset.is_multiple_of(4) {
            return Err(SmemError::BadPartition);
        }
        let off = e.offset;
        if self.magic(off) != PART_MAGIC
            || self.lo16(off + 4) != e.host0
            || self.hi16(off + 4) != e.host1
            || self.mem.read32(off + 8) != e.size
            || self.mem.read32(off + 12) > e.size
        {
            return Err(SmemError::BadPartition);
        }
        Ok(Partition {
            base: off,
            size: e.size,
            cacheline: e.cacheline.max(1),
        })
    }

    /// `qcom_smem_set_global_partition`.
    fn set_global_partition(&mut self) -> Result<(), SmemError> {
        if self.ptable().is_none() {
            return Err(SmemError::BadPtable);
        }
        for i in 0..self.ptable_entries() {
            let Some(e) = self.table_entry(i) else { break };
            if e.offset != 0
                && e.size != 0
                && e.host0 == SMEM_GLOBAL_HOST
                && e.host1 == SMEM_GLOBAL_HOST
            {
                self.global = Some(self.partition(e)?);
                return Ok(());
            }
        }
        Err(SmemError::BadPtable)
    }

    /// `qcom_smem_enumerate_partitions`.
    fn enumerate_partitions(&mut self, local: u16) -> Result<(), SmemError> {
        for i in 0..self.ptable_entries() {
            let Some(e) = self.table_entry(i) else { break };
            if e.offset == 0 || e.size == 0 {
                continue;
            }
            let remote = if e.host0 == local {
                e.host1
            } else if e.host1 == local {
                e.host0
            } else {
                continue;
            };
            if remote >= SMEM_HOST_COUNT || self.partitions[remote as usize].is_some() {
                return Err(SmemError::BadPartition);
            }
            self.partitions[remote as usize] = Some(self.partition(e)?);
        }
        Ok(())
    }

    fn part_for(&self, host: u16) -> Option<Partition> {
        if host < SMEM_HOST_COUNT
            && let Some(p) = self.partitions[host as usize]
        {
            return Some(p);
        }
        self.global
    }

    /// `qcom_smem_get`.
    pub fn get(&self, host: u16, item: u32) -> Result<SmemItem, SmemError> {
        if item >= self.item_count {
            return Err(SmemError::BadItem);
        }
        match self.part_for(host) {
            Some(p) => self.get_private(p, item),
            None => self.get_global(item),
        }
    }

    fn get_global(&self, item: u32) -> Result<SmemItem, SmemError> {
        if item >= SMEM_ITEM_COUNT {
            return Err(SmemError::BadItem);
        }
        let e = HDR_TOC + item * 16;
        if self.mem.read32(e) == 0 {
            return Err(SmemError::NotFound);
        }
        let offset = self.mem.read32(e + 4);
        let len = self.mem.read32(e + 8);
        if offset.checked_add(len).is_none_or(|end| end > self.size) {
            return Err(SmemError::OutOfBounds);
        }
        Ok(SmemItem { offset, len })
    }

    fn get_private(&self, p: Partition, item: u32) -> Result<SmemItem, SmemError> {
        let end_part = p.base + p.size;
        let mut e = p.base + PART_HDR;
        let end = p.base + self.mem.read32(p.base + 12).min(p.size);
        // Uncached list, growing up from the header.
        while e < end {
            if e.checked_add(PRIV_ENTRY).is_none_or(|x| x > end_part) || !e.is_multiple_of(4) {
                return Err(SmemError::OutOfBounds);
            }
            if self.lo16(e) != SMEM_PRIVATE_CANARY {
                return Err(SmemError::BadCanary);
            }
            let size = self.mem.read32(e + 4);
            let pad_data = self.lo16(e + 8) as u32;
            let pad_hdr = self.hi16(e + 8) as u32;
            if self.hi16(e) as u32 == item {
                if size > p.size || pad_data > size {
                    return Err(SmemError::OutOfBounds);
                }
                let at = e + PRIV_ENTRY + pad_hdr;
                if at.checked_add(size).is_none_or(|x| x > end_part) {
                    return Err(SmemError::OutOfBounds);
                }
                return Ok(SmemItem {
                    offset: at,
                    len: size - pad_data,
                });
            }
            e = e
                .checked_add(PRIV_ENTRY + pad_hdr)
                .and_then(|x| x.checked_add(size))
                .ok_or(SmemError::OutOfBounds)?;
        }
        if e > end_part {
            return Err(SmemError::OutOfBounds);
        }
        // Cached list, growing down from the end.
        let step = align(PRIV_ENTRY, p.cacheline);
        if step < PRIV_ENTRY || step > p.size {
            return Err(SmemError::OutOfBounds);
        }
        let last = self.mem.read32(p.base + 16);
        if last > p.size {
            return Err(SmemError::OutOfBounds);
        }
        let last = p.base + last;
        let mut e = end_part - step;
        while e > last {
            if !e.is_multiple_of(4) {
                return Err(SmemError::OutOfBounds);
            }
            if self.lo16(e) != SMEM_PRIVATE_CANARY {
                return Err(SmemError::BadCanary);
            }
            let size = self.mem.read32(e + 4);
            let pad_data = self.lo16(e + 8) as u32;
            if self.hi16(e) as u32 == item {
                if size > p.size || pad_data > size || size > e - p.base {
                    return Err(SmemError::OutOfBounds);
                }
                return Ok(SmemItem {
                    offset: e - size,
                    len: size - pad_data,
                });
            }
            e = size
                .checked_add(step)
                .and_then(|back| e.checked_sub(back))
                .ok_or(SmemError::OutOfBounds)?;
            if e < p.base {
                return Err(SmemError::OutOfBounds);
            }
        }
        Err(SmemError::NotFound)
    }

    /// `qcom_smem_alloc`, under the hardware lock. Never called in `lucina.wifi=report`.
    pub fn alloc<L: HwSpinlock>(
        &self,
        lock: &L,
        host: u16,
        item: u32,
        size: u32,
    ) -> Result<(), SmemError> {
        if item < SMEM_ITEM_LAST_FIXED || item >= self.item_count {
            return Err(SmemError::BadItem);
        }
        lock.lock()?;
        let r = match self.part_for(host) {
            Some(p) => self.alloc_private(p, item, size),
            None => self.alloc_global(item, size),
        };
        lock.unlock()?;
        r
    }

    fn alloc_private(&self, p: Partition, item: u32, size: u32) -> Result<(), SmemError> {
        let end_part = p.base + p.size;
        let free = self.mem.read32(p.base + 12);
        let cached = self.mem.read32(p.base + 16);
        if free > p.size || cached > p.size {
            return Err(SmemError::OutOfBounds);
        }
        let end = p.base + free;
        let cached = p.base + cached;
        let mut hdr = p.base + PART_HDR;
        while hdr < end {
            if hdr + PRIV_ENTRY > end_part || !hdr.is_multiple_of(4) {
                return Err(SmemError::OutOfBounds);
            }
            if self.lo16(hdr) != SMEM_PRIVATE_CANARY {
                return Err(SmemError::BadCanary);
            }
            if self.hi16(hdr) as u32 == item {
                return Err(SmemError::Exists);
            }
            hdr = hdr
                .checked_add(PRIV_ENTRY + self.hi16(hdr + 8) as u32)
                .and_then(|x| x.checked_add(self.mem.read32(hdr + 4)))
                .ok_or(SmemError::OutOfBounds)?;
        }
        if hdr > end_part {
            return Err(SmemError::OutOfBounds);
        }
        let asize = align(size, 8);
        if asize < size {
            return Err(SmemError::NoSpace);
        }
        let alloc_size = PRIV_ENTRY.checked_add(asize).ok_or(SmemError::NoSpace)?;
        if hdr.checked_add(alloc_size).is_none_or(|x| x > cached) {
            return Err(SmemError::NoSpace);
        }
        self.mem
            .write32(hdr, SMEM_PRIVATE_CANARY as u32 | (item << 16));
        self.mem.write32(hdr + 4, asize);
        self.mem.write32(hdr + 8, asize - size);
        self.mem.write32(hdr + 12, 0);
        // Device memory keeps these stores in order, so the entry is complete before the free offset moves (Linux's
        // wmb()).
        self.mem.write32(p.base + 12, free + alloc_size);
        Ok(())
    }

    fn alloc_global(&self, item: u32, size: u32) -> Result<(), SmemError> {
        if item >= SMEM_ITEM_COUNT {
            return Err(SmemError::BadItem);
        }
        let e = HDR_TOC + item * 16;
        if self.mem.read32(e) != 0 {
            return Err(SmemError::Exists);
        }
        let asize = align(size, 8);
        let avail = self.mem.read32(HDR_AVAILABLE);
        if asize < size || asize > avail {
            return Err(SmemError::NoSpace);
        }
        let free = self.mem.read32(HDR_FREE_OFFSET);
        self.mem.write32(e + 4, free);
        self.mem.write32(e + 8, asize);
        self.mem.write32(e, 1);
        self.mem.write32(HDR_FREE_OFFSET, free + asize);
        self.mem.write32(HDR_AVAILABLE, avail - asize);
        Ok(())
    }

    /// At most `max` bytes of an item, e.g. the crash reason string.
    /// Copies the start of `item` into `out`, no more than either holds, without allocating, and returns how many bytes.
    /// The modem witness (kernel patch 1057) reads the crash reason this way from an interrupt handler.
    pub fn read_item_into(&self, item: SmemItem, out: &mut [u8]) -> usize {
        let n = (item.len as usize).min(out.len());
        crate::io::read_bytes(&self.mem, item.offset, &mut out[..n]);
        n
    }

    pub fn read_item(&self, item: SmemItem, max: usize) -> Vec<u8> {
        let n = (item.len as usize).min(max);
        let mut out = alloc::vec![0; n];
        crate::io::read_bytes(&self.mem, item.offset, &mut out);
        out
    }
}
