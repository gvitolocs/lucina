// SPDX-License-Identifier: GPL-2.0-only
//! The rules that keep a program's GPU work inside its own memory, as pure functions that Verus checks as shipped
//! (`verification/juno-gpu-vm/`). Everything that decides whether a user address, a command buffer or a buffer object is
//! allowed goes through here; [`crate::gpuvm`] and [`crate::submit`] only build on these answers.
//!
//! The address space (`docs/research/juno-gpu-3d.md`, "Address spaces"):
//! - Context bank 0 of the GPU's SMMU translates, stage 1, 4 KiB granule, `T0SZ` 25: a 39-bit space and a walk that starts at
//!   level 1. Linux uses 48 bits and TTBR1 for its kernel half; this driver keeps TTBR1 off (`EPD1`) and gives every table
//!   the few kernel objects the command processor reads while that table is active (see [`crate::gpuvm`]).
//! - Physical memory is below `KERNEL_END` (36-bit output, RAM at 0x8000_0000 up). A program's window starts at
//!   [`USER_BASE`], 64 GiB, far above it: no user address is ever a kernel one, and the level 1 slots below 64 never hold a
//!   program's mapping.
//! - Every table shares ASID 0, as Linux's `msm_iommu_pagetable_params` does: `CP_SMMU_TABLE_UPDATE` invalidates the TLB.
//!
//! Descriptors are `io-pgtable-arm.c`'s for stage 1 (`arm_lpae_prot_to_pte`): attribute index 0 (MAIR 0x44, normal
//! non-cacheable, as the GPU is not coherent here), outer shareable, access flag, not global, `AP_UNPRIV` always (the A618
//! has no `APRIV`: Linux maps nothing privileged on it), `AP_RDONLY` for read-only.
#![allow(unexpected_cfgs)]

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

/// Bits of a GPU address.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const VA_BITS: u64 = 39;
/// One past the last GPU address.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const VA_END: u64 = 0x80_0000_0000;
/// `TCR.T0SZ` for [`VA_BITS`].
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const T0SZ: u32 = 25;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const PAGE: u64 = 0x1000;
/// The first address of a program's window: 64 GiB, level 1 slot 64.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const USER_BASE: u64 = 0x10_0000_0000;
/// Kernel objects (physical addresses mapped one to one) live below this: the SMMU's 36-bit output.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const KERNEL_END: u64 = 0x10_0000_0000;
/// The program's own fence page, written by the ring after each of its submits. Never part of a buffer object.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const FENCE_IOVA: u64 = USER_BASE;
/// Buffer objects go from here on: the first 2 MiB of the window are the kernel's.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const BO_BASE: u64 = USER_BASE + 0x20_0000;

/// Descriptor bits (`io-pgtable-arm.c`).
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const PTE_VALID: u64 = 1;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const PTE_TABLE: u64 = 2;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const PTE_AP_UNPRIV: u64 = 0x40;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const PTE_AP_RDONLY: u64 = 0x80;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const PTE_SH_OUTER: u64 = 0x200;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const PTE_AF: u64 = 0x400;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const PTE_NG: u64 = 0x800;
/// Output address bits of a descriptor (`ARM_LPAE_PTE_ADDR_MASK` for 4 KiB pages, 48 bits).
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const PTE_ADDR: u64 = 0x0000_ffff_ffff_f000;
/// What every leaf carries besides its address and `AP_RDONLY`.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const PTE_LEAF_ATTRS: u64 = 0xe40;

#[cfg(verus_keep_ghost)]
verus! {

/// [`user_range`], as a formula.
pub open spec fn is_user_range(iova: u64, len: u64) -> bool {
    iova % PAGE == 0 && len % PAGE == 0 && len > 0 && iova >= BO_BASE && len <= VA_END && iova <= VA_END - len
}

/// A page-aligned address below 2^36 with flags below bit 12 or-ed in: the address bits give back the address, and the low
/// bits are the flags'.
proof fn lemma_entry(phys: u64, flags: u64)
    requires phys % 0x1000 == 0, phys < 0x10_0000_0000, flags < 0x1000,
    ensures
        (phys | flags) & 0x0000_ffff_ffff_f000 == phys,
        (phys | flags) & 0xfff == flags,
{
    assert(phys % 0x1000 == 0 && phys < 0x10_0000_0000 && flags < 0x1000 ==> (phys | flags) & 0x0000_ffff_ffff_f000 == phys
        && (phys | flags) & 0xfff == flags) by (bit_vector);
}

proof fn lemma_low_bits(f: u64)
    requires f < 0x1000,
    ensures
        f & 3 == (f & 0xfff) & 3,
        f & 0x80 == (f & 0xfff) & 0x80,
        f & 0x400 == (f & 0xfff) & 0x400,
{
    assert(f < 0x1000 ==> f & 3 == (f & 0xfff) & 3 && f & 0x80 == (f & 0xfff) & 0x80 && f & 0x400 == (f & 0xfff) & 0x400)
        by (bit_vector);
}

/// Bits of an entry below bit 12 depend only on the flags.
proof fn lemma_or_low(phys: u64, flags: u64)
    requires phys % 0x1000 == 0, flags < 0x1000,
    ensures
        (phys | flags) & 3 == flags & 3,
        (phys | flags) & 0x80 == flags & 0x80,
        (phys | flags) & 0x400 == flags & 0x400,
{
    assert(phys % 0x1000 == 0 && flags < 0x1000 ==> (phys | flags) & 3 == flags & 3 && (phys | flags) & 0x80 == flags & 0x80
        && (phys | flags) & 0x400 == flags & 0x400) by (bit_vector);
}

} // verus!

/// One level's slot of an address: level 1 is bits 38..30, 2 is 29..21, 3 is 20..12.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires 1 <= level <= 3,
    ensures
        r < 512,
        level == 1 ==> r == (iova / 0x4000_0000) % 512,
        level == 2 ==> r == (iova / 0x20_0000) % 512,
        level == 3 ==> r == (iova / 0x1000) % 512,
))]
pub fn slot(iova: u64, level: u64) -> u64 {
    let shift_unit: u64 = if level == 1 {
        0x4000_0000
    } else if level == 2 {
        0x20_0000
    } else {
        0x1000
    };
    (iova / shift_unit) % 512
}

/// Whether `[iova, iova + len)` may hold a program's buffer object: page aligned, not empty, inside the window after the
/// kernel's first 2 MiB, no overflow.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures r == is_user_range(iova, len),
))]
pub fn user_range(iova: u64, len: u64) -> bool {
    iova % PAGE == 0 && len % PAGE == 0 && len > 0 && iova >= BO_BASE && len <= VA_END && iova <= VA_END - len
}

/// A leaf descriptor for one 4 KiB page at `phys`, read-only or not.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires phys % PAGE == 0, phys < KERNEL_END,
    ensures
        r & PTE_ADDR == phys,
        r & 3 == 3,
        (r & PTE_AP_RDONLY != 0) == read_only,
        r & PTE_AF != 0,
))]
pub fn page_entry(phys: u64, read_only: bool) -> u64 {
    // VALID | TABLE | LEAF_ATTRS, and AP_RDONLY (the test `flags_are_the_named_bits` checks the sums).
    let flags: u64 = if read_only { 0xec3 } else { 0xe43 };
    #[cfg(verus_keep_ghost)]
    proof! {
        lemma_entry(phys, flags);
        lemma_or_low(phys, flags);
        assert(0xe43u64 & 3 == 3 && 0xec3u64 & 3 == 3 && 0xe43u64 & 0x80 == 0 && 0xec3u64 & 0x80 == 0x80
            && 0xe43u64 & 0x400 == 0x400 && 0xec3u64 & 0x400 == 0x400) by (bit_vector);
    }
    phys | flags
}

/// A level 1 block descriptor: 1 GiB at `phys`, read-write. Only the kernel's own table has these.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires phys % 0x4000_0000 == 0, phys < KERNEL_END,
    ensures r & PTE_ADDR == phys, r & 3 == 1,
))]
pub fn block_entry(phys: u64) -> u64 {
    // VALID | LEAF_ATTRS.
    let flags: u64 = 0xe41;
    #[cfg(verus_keep_ghost)]
    proof! {
        assert(phys % 0x1000 == 0) by (nonlinear_arith) requires phys % 0x4000_0000 == 0;
        lemma_entry(phys, flags);
        lemma_or_low(phys, flags);
        assert(0xe41u64 & 3 == 1) by (bit_vector);
    }
    phys | flags
}

/// A descriptor pointing at the next level's table at `phys`.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires phys % PAGE == 0, phys < KERNEL_END,
    ensures r & PTE_ADDR == phys, r & 3 == 3,
))]
pub fn table_entry(phys: u64) -> u64 {
    // VALID | TABLE.
    let flags: u64 = 3;
    #[cfg(verus_keep_ghost)]
    proof! {
        lemma_entry(phys, flags);
        lemma_or_low(phys, flags);
        assert(3u64 & 3 == 3) by (bit_vector);
    }
    phys | flags
}

/// One command buffer of a submit, as the program names it: a buffer object, a byte offset into it and a length in words.
/// Ok(GPU address) if `[offset, offset + 4 * dwords)` is inside the object and the length fits `CP_INDIRECT_BUFFER`'s 20-bit
/// size; `bo_iova` and `bo_size` are the object's own, which [`user_range`] accepted when it was made.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires is_user_range(bo_iova, bo_size),
    ensures
        r.is_some() <==> (offset % 4 == 0 && dwords > 0 && dwords <= IB_MAX_DWORDS && offset <= bo_size
            && 4 * dwords <= bo_size - offset),
        r.is_some() ==> r.unwrap() == bo_iova + offset
            && r.unwrap() >= bo_iova
            && r.unwrap() + 4 * dwords <= bo_iova + bo_size
            && r.unwrap() % 4 == 0,
))]
pub fn command_buffer(bo_iova: u64, bo_size: u64, offset: u64, dwords: u64) -> Option<u64> {
    if offset % 4 != 0 || dwords == 0 || dwords > IB_MAX_DWORDS || offset > bo_size || 4 * dwords > bo_size - offset {
        return None;
    }
    Some(bo_iova + offset)
}

/// `CP_INDIRECT_BUFFER_2_IB_SIZE` is bits 0..19.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const IB_MAX_DWORDS: u64 = 0xf_ffff;
/// Command buffers in one submit.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const MAX_COMMAND_BUFFERS: u64 = 64;
/// Buffer objects one submit may name.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const MAX_SUBMIT_BOS: u64 = 1024;

/// What one program and all programs together may hold in buffer objects, in bytes. RAM is 3.7 GB with other users.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const CONTEXT_BYTES_LIMIT: u64 = 1024 * 1024 * 1024;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const TOTAL_BYTES_LIMIT: u64 = 1536 * 1024 * 1024;
/// Buffer objects one program may hold.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const MAX_CONTEXT_BOS: u64 = 4096;

/// Whether a new object of `size` bytes fits both budgets, given what the program (`ctx_bytes`) and everyone (`total_bytes`)
/// hold now. Budgets that are already over (they never are) refuse everything.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures r <==> (size > 0 && ctx_bytes <= CONTEXT_BYTES_LIMIT && total_bytes <= TOTAL_BYTES_LIMIT
        && size <= CONTEXT_BYTES_LIMIT - ctx_bytes && size <= TOTAL_BYTES_LIMIT - total_bytes),
        r ==> ctx_bytes + size <= CONTEXT_BYTES_LIMIT && total_bytes + size <= TOTAL_BYTES_LIMIT,
))]
pub fn may_allocate(ctx_bytes: u64, total_bytes: u64, size: u64) -> bool {
    size > 0
        && ctx_bytes <= CONTEXT_BYTES_LIMIT
        && total_bytes <= TOTAL_BYTES_LIMIT
        && size <= CONTEXT_BYTES_LIMIT - ctx_bytes
        && size <= TOTAL_BYTES_LIMIT - total_bytes
}

/// The budgets after an object of `size` bytes went: never below zero.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        size <= held ==> r == held - size,
        size > held ==> r == 0,
        r <= held,
))]
pub fn released(held: u64, size: u64) -> u64 {
    if size <= held { held - size } else { 0 }
}

/// Fences: the ring writes the low 32 bits of a 64-bit sequence into the program's fence page. `seen` is what the page
/// says, `retired` the last sequence the kernel already knows finished, `submitted` the last one queued. Returns the
/// sequence now known finished: `retired` moved forward by how far `seen` is past it, but never beyond `submitted`. A page
/// that a program's own command buffer overwrote can only say "later" up to its own last submit, which is the program's
/// own business (see the record, "Fences a program can write").
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires retired <= submitted, submitted - retired < 0x8000_0000,
    ensures
        retired <= r <= submitted,
        r == retired || r % 0x1_0000_0000 == seen as u64,
))]
pub fn fence_reached(seen: u32, retired: u64, submitted: u64) -> u64 {
    // How far `seen` is past `retired`, modulo 2^32.
    let low = (retired % 0x1_0000_0000) as u32;
    let ahead: u64 = if seen >= low { (seen - low) as u64 } else { seen as u64 + 0x1_0000_0000 - low as u64 };
    #[cfg(verus_keep_ghost)]
    proof! {
        assert(retired == (retired / 0x1_0000_0000) * 0x1_0000_0000 + low) by (nonlinear_arith)
            requires low == retired % 0x1_0000_0000;
        assert((retired + ahead) % 0x1_0000_0000 == seen) by (nonlinear_arith)
            requires
                retired == (retired / 0x1_0000_0000) * 0x1_0000_0000 + low,
                low < 0x1_0000_0000,
                seen < 0x1_0000_0000,
                (seen >= low ==> ahead == seen - low),
                (seen < low ==> ahead == seen + 0x1_0000_0000 - low);
    }
    if ahead == 0 || ahead > submitted - retired {
        // Behind (an old value) or ahead of anything queued (a forged or stale page): nothing new.
        return retired;
    }
    retired + ahead
}

/// Whether a buffer object last named by submit `last_use` may give its memory back once `retired` is known finished.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures r <==> last_use <= retired,
))]
pub fn may_release(last_use: u64, retired: u64) -> bool {
    last_use <= retired
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_of_the_three_levels() {
        let iova = 0x12_3456_7000u64;
        assert_eq!(slot(iova, 1), (iova >> 30) & 0x1ff);
        assert_eq!(slot(iova, 2), (iova >> 21) & 0x1ff);
        assert_eq!(slot(iova, 3), (iova >> 12) & 0x1ff);
        assert_eq!(slot(USER_BASE, 1), 64);
        assert_eq!(slot(VA_END - PAGE, 1), 511);
    }

    #[test]
    fn the_user_window() {
        assert!(user_range(BO_BASE, PAGE));
        assert!(user_range(VA_END - PAGE, PAGE));
        assert!(!user_range(VA_END - PAGE, 2 * PAGE));
        assert!(!user_range(FENCE_IOVA, PAGE));
        assert!(!user_range(0x9bc0_0000, PAGE));
        assert!(!user_range(BO_BASE + 1, PAGE));
        assert!(!user_range(BO_BASE, 0));
        assert!(!user_range(BO_BASE, PAGE + 1));
        assert!(!user_range(u64::MAX & !0xfff, PAGE));
        assert!(!user_range(BO_BASE, u64::MAX & !0xfff));
    }

    #[test]
    fn flags_are_the_named_bits() {
        assert_eq!(PTE_LEAF_ATTRS, PTE_AF | PTE_NG | PTE_SH_OUTER | PTE_AP_UNPRIV);
        assert_eq!((PTE_AP_UNPRIV, PTE_AP_RDONLY, PTE_SH_OUTER, PTE_AF, PTE_NG), (1 << 6, 1 << 7, 2 << 8, 1 << 10, 1 << 11));
        assert_eq!(PTE_VALID | PTE_TABLE | PTE_LEAF_ATTRS, 0xe43);
        assert_eq!(PTE_VALID | PTE_TABLE | PTE_LEAF_ATTRS | PTE_AP_RDONLY, 0xec3);
        assert_eq!(PTE_VALID | PTE_LEAF_ATTRS, 0xe41);
    }

    #[test]
    fn entries_have_linuxs_bits() {
        // The same leaf the GMU's table has used since stage 1: 0xe43 around the address.
        assert_eq!(page_entry(0xa123_4000, false), 0xa123_4000 | 0xe43);
        assert_eq!(page_entry(0xa123_4000, true), 0xa123_4000 | 0xe43 | (1 << 7));
        assert_eq!(block_entry(0x8000_0000), 0x8000_0000 | 0xe41);
        assert_eq!(table_entry(0x9000_1000), 0x9000_1003);
    }

    #[test]
    fn command_buffers_stay_inside_their_object() {
        let (iova, size) = (BO_BASE, 0x2000);
        assert_eq!(command_buffer(iova, size, 0, 1), Some(iova));
        assert_eq!(command_buffer(iova, size, 0x1ffc, 1), Some(iova + 0x1ffc));
        assert_eq!(command_buffer(iova, size, 0x1ffc, 2), None);
        assert_eq!(command_buffer(iova, size, 0, 0x800), Some(iova));
        assert_eq!(command_buffer(iova, size, 0, 0x801), None);
        assert_eq!(command_buffer(iova, size, 2, 1), None);
        assert_eq!(command_buffer(iova, size, 0x2000, 1), None);
        assert_eq!(command_buffer(iova, size, u64::MAX - 3, 1), None);
        assert_eq!(command_buffer(iova, size, 0, 0), None);
        assert_eq!(command_buffer(iova, 0x100_0000, 0, IB_MAX_DWORDS + 1), None);
    }

    #[test]
    fn budgets() {
        assert!(may_allocate(0, 0, PAGE));
        assert!(may_allocate(CONTEXT_BYTES_LIMIT - PAGE, 0, PAGE));
        assert!(!may_allocate(CONTEXT_BYTES_LIMIT - PAGE, 0, 2 * PAGE));
        assert!(!may_allocate(0, TOTAL_BYTES_LIMIT, PAGE));
        assert!(!may_allocate(0, 0, 0));
        assert!(!may_allocate(0, 0, u64::MAX));
        assert_eq!(released(10, 4), 6);
        assert_eq!(released(4, 10), 0);
    }

    #[test]
    fn fences_move_forward_only_within_what_was_queued() {
        assert_eq!(fence_reached(5, 3, 9), 5);
        assert_eq!(fence_reached(3, 3, 9), 3);
        assert_eq!(fence_reached(2, 3, 9), 3);
        assert_eq!(fence_reached(10, 3, 9), 3);
        assert_eq!(fence_reached(9, 3, 9), 9);
        // Across the 32-bit wrap.
        let retired = 0xffff_fffe;
        assert_eq!(fence_reached(1, retired, retired + 5), retired + 3);
        assert_eq!(fence_reached(0xffff_fffd, retired, retired + 5), retired);
        assert!(may_release(4, 4) && !may_release(5, 4));
    }
}
