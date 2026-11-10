// SPDX-License-Identifier: GPL-2.0-only
//! GPU address spaces: the stage 1 tables that context bank 0 of the GPU's SMMU walks, one per program plus the kernel's.
//! After Linux's `io-pgtable-arm.c` (the walk and its descriptors) and `msm_iommu.c` (one table per `msm_context`, all on
//! ASID 0, switched by the ring with `CP_SMMU_TABLE_UPDATE`). Every decision about what may be mapped is
//! [`crate::gpu_rules`]'s.
//!
//! Two kinds of table:
//! - [`Space::kernel`]: physical memory one to one in 1 GiB blocks, for the kernel's own batches (screen fills, blits, the
//!   YUV quad), which carry physical addresses as they did when the bank let everything through. Only the ring, which only
//!   the kernel writes, ever switches to it.
//! - [`Space::user`]: a program's window from [`USER_BASE`] on, its own fence page at [`FENCE_IOVA`], and the kernel
//!   objects the command processor reads while the program's table is active (the ring and the SQE firmware, read-only),
//!   one to one at their physical addresses. Those come from a [`Stub`]: tables built once and linked from every program's
//!   level 1 slots below 64, which a program's own mappings never reach ([`gpu_rules::user_range`]).
//!
//! The tables live in uncached memory that the kernel provides through [`TableMemory`]; the walk is non-cacheable
//! (`TCR.IRGN0`/`ORGN0` 0), so a write is seen by the next walk without a cache clean. A mapping removed while the GPU may
//! still use it needs a TLB invalidation of ASID 0 before its memory is reused: the kernel's job (`smmu::Smmu::invalidate`).

use alloc::vec::Vec;

use crate::gpu_rules::{self, BO_BASE, FENCE_IOVA, KERNEL_END, PAGE, PTE_ADDR, USER_BASE, VA_END};
use crate::{Result, err};

/// Entries per table (4 KiB of 64-bit descriptors).
pub const ENTRIES: usize = 512;
/// A level 1 slot covers 1 GiB.
const L1_SPAN: u64 = 0x4000_0000;

/// Pages for tables, in memory the SMMU reads, addressed physically.
pub trait TableMemory {
    /// A zeroed, page-aligned page below [`KERNEL_END`]; its physical address.
    fn alloc_table(&mut self) -> Option<u64>;
    /// Gives back a page [`Self::alloc_table`] gave.
    fn free_table(&mut self, phys: u64);
    fn read(&self, table: u64, index: usize) -> u64;
    fn write(&mut self, table: u64, index: usize, value: u64);
}

/// What a page is mapped as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    ReadWrite,
    ReadOnly,
}

/// Tables that map the kernel objects every program's table needs, built once. Level 1 entries, to copy into a program's
/// level 1 table at the same slots.
#[derive(Clone, Debug, Default)]
pub struct Stub {
    pub l1: Vec<(usize, u64)>,
    tables: Vec<u64>,
}

impl Stub {
    /// Maps each `(phys, pages)` read-only one to one. Every page must be below [`KERNEL_END`].
    pub fn new<M: TableMemory>(mem: &mut M, objects: &[(u64, u64)]) -> Result<Self> {
        let mut stub = Stub::default();
        let root = mem.alloc_table().ok_or(crate::Error { what: "gpuvm: no memory for the stub", value: 0 })?;
        stub.tables.push(root);
        for &(phys, pages) in objects {
            for page in 0..pages {
                let at = phys + page * PAGE;
                if at % PAGE != 0 || at >= KERNEL_END {
                    return err("gpuvm: a kernel object outside physical memory", (at >> 12) as u32);
                }
                let l2 = walk_or_make(mem, root, at, 1, &mut stub.tables)?;
                let l3 = walk_or_make(mem, l2, at, 2, &mut stub.tables)?;
                let index = gpu_rules::slot(at, 3) as usize;
                let entry = gpu_rules::page_entry(at, true);
                let old = mem.read(l3, index);
                if old != 0 && old != entry {
                    return err("gpuvm: the stub maps a page twice", (at >> 12) as u32);
                }
                mem.write(l3, index, entry);
            }
        }
        for index in 0..ENTRIES {
            let entry = mem.read(root, index);
            if entry != 0 {
                stub.l1.push((index, entry));
            }
        }
        // The root was only scaffolding; its entries are copied, the tables below stay.
        mem.free_table(root);
        stub.tables.remove(0);
        Ok(stub)
    }

    pub fn tables(&self) -> &[u64] {
        &self.tables
    }
}

/// The next level's table under `table`'s slot for `iova` at `level`, made if missing. Refuses a block.
fn walk_or_make<M: TableMemory>(mem: &mut M, table: u64, iova: u64, level: u64, owned: &mut Vec<u64>) -> Result<u64> {
    let index = gpu_rules::slot(iova, level) as usize;
    let entry = mem.read(table, index);
    if entry & 3 == 3 {
        return Ok(entry & PTE_ADDR);
    }
    if entry != 0 {
        return err("gpuvm: a block where a table should be", index as u32);
    }
    let next = mem.alloc_table().ok_or(crate::Error { what: "gpuvm: no memory for a table", value: level as u32 })?;
    owned.push(next);
    mem.write(table, index, gpu_rules::table_entry(next));
    Ok(next)
}

/// One address space: its level 1 table (`TTBR0`) and the tables it made.
#[derive(Debug)]
pub struct Space {
    pub root: u64,
    owned: Vec<u64>,
    user: bool,
}

impl Space {
    /// The kernel's table: `[start, end)` of physical memory one to one, rounded out to 1 GiB blocks.
    pub fn kernel<M: TableMemory>(mem: &mut M, start: u64, end: u64) -> Result<Self> {
        let root = mem.alloc_table().ok_or(crate::Error { what: "gpuvm: no memory for the kernel table", value: 0 })?;
        let first = start / L1_SPAN;
        let last = end.div_ceil(L1_SPAN).min(KERNEL_END / L1_SPAN);
        for block in first..last {
            mem.write(root, block as usize, gpu_rules::block_entry(block * L1_SPAN));
        }
        Ok(Self { root, owned: alloc::vec![root], user: false })
    }

    /// A program's table: the stub's slots, and its fence page `fence_phys` read-write at [`FENCE_IOVA`].
    pub fn user<M: TableMemory>(mem: &mut M, stub: &Stub, fence_phys: u64) -> Result<Self> {
        let root = mem.alloc_table().ok_or(crate::Error { what: "gpuvm: no memory for a table", value: 0 })?;
        let mut space = Self { root, owned: alloc::vec![root], user: true };
        for &(index, entry) in &stub.l1 {
            if index as u64 >= gpu_rules::slot(USER_BASE, 1) {
                space.destroy(mem);
                return err("gpuvm: a stub slot inside the user window", index as u32);
            }
            mem.write(root, index, entry);
        }
        if let Err(e) = space.map_one(mem, FENCE_IOVA, fence_phys, Access::ReadWrite) {
            space.destroy(mem);
            return Err(e);
        }
        Ok(space)
    }

    /// Maps one page; the slot must be empty. Only inside the window.
    fn map_one<M: TableMemory>(&mut self, mem: &mut M, iova: u64, phys: u64, access: Access) -> Result<()> {
        if !self.user || iova < USER_BASE || iova >= VA_END || iova % PAGE != 0 {
            return err("gpuvm: outside the program's window", (iova >> 12) as u32);
        }
        if phys % PAGE != 0 || phys >= KERNEL_END {
            return err("gpuvm: not a page of memory", (phys >> 12) as u32);
        }
        let l2 = walk_or_make(mem, self.root, iova, 1, &mut self.owned)?;
        let l3 = walk_or_make(mem, l2, iova, 2, &mut self.owned)?;
        let index = gpu_rules::slot(iova, 3) as usize;
        if mem.read(l3, index) != 0 {
            return err("gpuvm: already mapped", (iova >> 12) as u32);
        }
        mem.write(l3, index, gpu_rules::page_entry(phys, access == Access::ReadOnly));
        Ok(())
    }

    /// Maps `pages` at `iova..` for a buffer object: [`gpu_rules::user_range`] must accept the range, and no page of it may be
    /// mapped already. All or nothing: a refusal leaves the table as it was.
    pub fn map<M: TableMemory>(&mut self, mem: &mut M, iova: u64, pages: &[u64], access: Access) -> Result<()> {
        let len = (pages.len() as u64).saturating_mul(PAGE);
        if !gpu_rules::user_range(iova, len) {
            return err("gpuvm: not a range a buffer object may take", (iova >> 12) as u32);
        }
        for (n, &phys) in pages.iter().enumerate() {
            if let Err(e) = self.map_one(mem, iova + n as u64 * PAGE, phys, access) {
                self.unmap(mem, iova, n as u64);
                return Err(e);
            }
        }
        Ok(())
    }

    /// Clears `pages` pages from `iova` on. Tables stay (they are freed with the space). The caller invalidates the TLB.
    pub fn unmap<M: TableMemory>(&mut self, mem: &mut M, iova: u64, pages: u64) {
        for n in 0..pages {
            let at = iova + n * PAGE;
            if at < BO_BASE || at >= VA_END {
                continue;
            }
            if let Some((l3, index)) = self.leaf(mem, at) {
                mem.write(l3, index, 0);
            }
        }
    }

    /// The level 3 table and slot that hold `iova`'s page, if the tables above exist.
    fn leaf<M: TableMemory>(&self, mem: &M, iova: u64) -> Option<(u64, usize)> {
        let l1 = mem.read(self.root, gpu_rules::slot(iova, 1) as usize);
        if l1 & 3 != 3 {
            return None;
        }
        let l2 = mem.read(l1 & PTE_ADDR, gpu_rules::slot(iova, 2) as usize);
        if l2 & 3 != 3 {
            return None;
        }
        Some((l2 & PTE_ADDR, gpu_rules::slot(iova, 3) as usize))
    }

    /// What the hardware's walk makes of `iova`: the physical address and the access, or `None` for a fault.
    pub fn translate<M: TableMemory>(&self, mem: &M, iova: u64) -> Option<(u64, Access)> {
        if iova >= VA_END {
            return None;
        }
        let l1 = mem.read(self.root, gpu_rules::slot(iova, 1) as usize);
        match l1 & 3 {
            1 => return Some(((l1 & PTE_ADDR) + iova % L1_SPAN, access_of(l1))),
            3 => {}
            _ => return None,
        }
        let l2 = mem.read(l1 & PTE_ADDR, gpu_rules::slot(iova, 2) as usize);
        if l2 & 3 != 3 {
            return None;
        }
        let l3 = mem.read(l2 & PTE_ADDR, gpu_rules::slot(iova, 3) as usize);
        if l3 & 3 != 3 {
            return None;
        }
        Some(((l3 & PTE_ADDR) | (iova & 0xfff), access_of(l3)))
    }

    /// Frees every table this space made (never the stub's). The GPU must no longer use it.
    pub fn destroy<M: TableMemory>(&mut self, mem: &mut M) {
        for table in self.owned.drain(..) {
            mem.free_table(table);
        }
    }

    pub fn tables(&self) -> usize {
        self.owned.len()
    }
}

fn access_of(entry: u64) -> Access {
    if entry & gpu_rules::PTE_AP_RDONLY != 0 { Access::ReadOnly } else { Access::ReadWrite }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::collections::{BTreeMap, BTreeSet};

    use super::*;

    #[derive(Default)]
    struct Mem {
        pages: BTreeMap<u64, [u64; ENTRIES]>,
        next: u64,
        freed: BTreeSet<u64>,
    }

    impl TableMemory for Mem {
        fn alloc_table(&mut self) -> Option<u64> {
            let phys = 0x9000_0000 + self.next * PAGE;
            self.next += 1;
            self.pages.insert(phys, [0; ENTRIES]);
            Some(phys)
        }
        fn free_table(&mut self, phys: u64) {
            assert!(self.pages.remove(&phys).is_some(), "freed twice or never made: {:#x}", phys);
            self.freed.insert(phys);
        }
        fn read(&self, table: u64, index: usize) -> u64 {
            self.pages[&table][index]
        }
        fn write(&mut self, table: u64, index: usize, value: u64) {
            self.pages.get_mut(&table).unwrap()[index] = value;
        }
    }

    #[test]
    fn the_kernel_table_maps_ram_one_to_one() {
        let mut mem = Mem::default();
        let k = Space::kernel(&mut mem, 0x8000_0000, 0x1_7f00_0000).unwrap();
        assert_eq!(k.translate(&mem, 0x9bc0_0000), Some((0x9bc0_0000, Access::ReadWrite)));
        assert_eq!(k.translate(&mem, 0x1_7eff_fffc), Some((0x1_7eff_fffc, Access::ReadWrite)));
        assert_eq!(k.translate(&mem, 0x7fff_f000), None);
        assert_eq!(k.translate(&mem, 0x1_8000_0000), None);
        assert_eq!(k.translate(&mem, USER_BASE), None);
    }

    #[test]
    fn a_program_sees_its_objects_the_ring_read_only_and_nothing_else() {
        let mut mem = Mem::default();
        let ring = 0xa000_0000;
        let sqe = 0xa040_0000;
        let stub = Stub::new(&mut mem, &[(ring, 8), (sqe, 3)]).unwrap();
        let fence = 0xa080_0000;
        let mut a = Space::user(&mut mem, &stub, fence).unwrap();
        let mut b = Space::user(&mut mem, &stub, 0xa080_1000).unwrap();
        assert_eq!(a.translate(&mem, ring + 0x7ffc), Some((ring + 0x7ffc, Access::ReadOnly)));
        assert_eq!(a.translate(&mem, ring + 0x8000), None);
        assert_eq!(a.translate(&mem, sqe), Some((sqe, Access::ReadOnly)));
        assert_eq!(a.translate(&mem, FENCE_IOVA), Some((fence, Access::ReadWrite)));
        assert_eq!(b.translate(&mem, FENCE_IOVA), Some((0xa080_1000, Access::ReadWrite)));
        // The screen and every other physical page: not there.
        assert_eq!(a.translate(&mem, 0x9bc0_0000), None);
        assert_eq!(a.translate(&mem, 0x8000_0000), None);

        let pages = [0xb000_0000, 0xb000_5000, 0xb123_0000];
        a.map(&mut mem, BO_BASE, &pages, Access::ReadWrite).unwrap();
        assert_eq!(a.translate(&mem, BO_BASE + 0x1004), Some((0xb000_5004, Access::ReadWrite)));
        assert_eq!(a.translate(&mem, BO_BASE + 0x3000), None);
        assert_eq!(b.translate(&mem, BO_BASE), None);
        // Twice, or partly over it: refused, and the refusal leaves nothing behind.
        assert!(a.map(&mut mem, BO_BASE, &pages, Access::ReadWrite).is_err());
        assert!(a.map(&mut mem, BO_BASE + 0x1000, &[0xc000_0000, 0xc000_1000], Access::ReadOnly).is_err());
        assert_eq!(a.translate(&mem, BO_BASE + 0x1000), Some((0xb000_5000, Access::ReadWrite)));
        // Not in the window: the kernel's slots, the fence page, past the end.
        assert!(a.map(&mut mem, ring, &[0xc000_0000], Access::ReadWrite).is_err());
        assert!(a.map(&mut mem, FENCE_IOVA, &[0xc000_0000], Access::ReadWrite).is_err());
        assert!(a.map(&mut mem, VA_END, &[0xc000_0000], Access::ReadWrite).is_err());
        assert!(a.map(&mut mem, BO_BASE + 0x10_0000, &[0x10_0000_0000], Access::ReadWrite).is_err());
        a.unmap(&mut mem, BO_BASE, 3);
        assert_eq!(a.translate(&mem, BO_BASE), None);
        b.map(&mut mem, BO_BASE, &[0xb000_0000], Access::ReadOnly).unwrap();
        assert_eq!(b.translate(&mem, BO_BASE), Some((0xb000_0000, Access::ReadOnly)));

        // Destroying frees what each made and never the stub.
        let stub_tables: BTreeSet<u64> = stub.tables().iter().copied().collect();
        a.destroy(&mut mem);
        b.destroy(&mut mem);
        assert!(stub_tables.iter().all(|t| mem.pages.contains_key(t)));
        assert!(mem.freed.is_disjoint(&stub_tables));
    }

    #[test]
    fn a_partial_map_is_undone() {
        let mut mem = Mem::default();
        let stub = Stub::new(&mut mem, &[(0xa000_0000, 1)]).unwrap();
        let mut a = Space::user(&mut mem, &stub, 0xa080_0000).unwrap();
        a.map(&mut mem, BO_BASE + 0x2000, &[0xb000_0000], Access::ReadWrite).unwrap();
        assert!(a.map(&mut mem, BO_BASE, &[0xc000_0000, 0xc000_1000, 0xc000_2000], Access::ReadWrite).is_err());
        assert_eq!(a.translate(&mem, BO_BASE), None);
        assert_eq!(a.translate(&mem, BO_BASE + 0x1000), None);
        assert_eq!(a.translate(&mem, BO_BASE + 0x2000), Some((0xb000_0000, Access::ReadWrite)));
    }
}
