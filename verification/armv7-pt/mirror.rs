// Verus MODEL (not a proof of the shipped kernel; project rule D-011): the ARMv7-A short-descriptor walk through
// the hardware first-level table that mirrors an OSTD root, against OSTD's own two-level walk.
//
// Mirrors:
//   kernel/patches (ostd/src/arch/armv7/mm/mod.rs)  PagingConsts (BASE_PAGE_SIZE 4096, NR_LEVELS 2,
//                                                   ADDRESS_WIDTH 32, PTE_SIZE 4): a root of 1024 entries of
//                                                   4 MiB, leaf nodes of 1024 small-page descriptors of 4 KiB
//   kernel/patches (ostd/src/arch/armv7/mm/l1.rs)   mirror_of: root entry i, a table at `pa`, becomes the
//                                                   first-level entries 4i + k = (pa + 1024 k) | 0b01; a 4 MiB
//                                                   page at `pa` becomes the sections 4i + k = (pa + k MiB) | attrs
//   ostd/src/mm/page_table/mod.rs                   pte_index, pte_index_bit_offset
//
// ARM ARM (DDI 0406C) B3.5 "Short-descriptor translation table format": the walk reads the first-level descriptor
// at TTBR0 + 4 * VA[31:20] (TTBCR.N = 0); a page table descriptor (bits[1:0] = 01) gives the second-level table
// base in bits[31:10], and the walk reads the small-page descriptor at base + 4 * VA[19:12]; a section descriptor
// (bits[1:0] = 10) gives the output address bits[31:20] and keeps VA[19:0].
// UNSURE: the B3.5.x subsection numbers differ between ARM ARM revisions.
//
// What is shown, for every 32-bit VA:
//   1. lemma_table_mirror: the hardware walk through the mirror of a table entry reads exactly the leaf entry that
//      OSTD's walk reads (same address), so the leaf node really is four hardware second-level tables;
//   2. lemma_section_mirror: a section of the mirror of a 4 MiB page translates VA to the address that OSTD's
//      entry gives (pa + VA[21:0]);
//   3. lemma_slots: mirror entry 4i + k is the one the hardware reads for VA exactly when root entry i is the one
//      OSTD reads, so the four entries of a slot cover the root entry's 4 MiB and nothing else.

use vstd::prelude::*;

verus! {

/// OSTD: pte_index(va, 2) = va >> 22 (12 + 10 bits), pte_index(va, 1) = (va >> 12) & 1023.
pub open spec fn root_index(va: u32) -> u32 { va >> 22u32 }
pub open spec fn leaf_index(va: u32) -> u32 { (va >> 12u32) & 1023u32 }

/// The address of the leaf entry that OSTD's walk reads, in a leaf node at `node` (4 KiB aligned).
pub open spec fn ostd_leaf_entry_addr(node: u32, va: u32) -> u32 { (node + 4 * leaf_index(va)) as u32 }

/// The hardware: the first-level index, and the second-level index into a 1 KiB table.
pub open spec fn l1_index(va: u32) -> u32 { va >> 20u32 }
pub open spec fn l2_index(va: u32) -> u32 { (va >> 12u32) & 255u32 }

/// mirror_of for a table entry: first-level entry k of the slot.
pub open spec fn mirror_table_entry(node: u32, k: u32) -> u32 { ((node + 1024 * k) as u32) | 1u32 }

/// The hardware's second-level table base from a page table descriptor: bits[31:10].
pub open spec fn coarse_base(desc: u32) -> u32 { desc & 0xffff_fc00u32 }

/// The address of the small-page descriptor that the hardware reads for `va` through first-level entry `desc`.
pub open spec fn hw_l2_entry_addr(desc: u32, va: u32) -> u32 { (coarse_base(desc) + 4 * l2_index(va)) as u32 }

/// The mirror entry the hardware reads for va, within its slot: VA[21:20].
pub open spec fn slot_entry(va: u32) -> u32 { (va >> 20u32) & 3u32 }

pub proof fn lemma_slots(va: u32)
    ensures
        l1_index(va) == 4 * root_index(va) + slot_entry(va),
        slot_entry(va) < 4,
        root_index(va) < 1024,
{
    assert(l1_index(va) == 4 * root_index(va) + slot_entry(va)) by (bit_vector)
        requires l1_index(va) == va >> 20u32, root_index(va) == va >> 22u32, slot_entry(va) == (va >> 20u32) & 3u32;
    assert(slot_entry(va) < 4) by (bit_vector) requires slot_entry(va) == (va >> 20u32) & 3u32;
    assert(root_index(va) < 1024) by (bit_vector) requires root_index(va) == va >> 22u32;
}

/// 1. Through the mirror, the hardware reads the leaf entry that OSTD reads.
pub proof fn lemma_table_mirror(node: u32, va: u32)
    requires
        node & 0xfffu32 == 0,
        node <= 0xffff_f000u32,
    ensures
        hw_l2_entry_addr(mirror_table_entry(node, slot_entry(va)), va) == ostd_leaf_entry_addr(node, va),
{
    let k = slot_entry(va);
    assert(k < 4) by (bit_vector) requires k == (va >> 20u32) & 3u32;
    // node + 1024 k fits in 32 bits and keeps bits[9:0] clear, so setting bit 0 and masking bits[9:0] gives it back.
    assert(coarse_base(mirror_table_entry(node, k)) == (node + 1024 * k) as u32) by (bit_vector)
        requires
            node & 0xfffu32 == 0,
            node <= 0xffff_f000u32,
            k < 4,
            mirror_table_entry(node, k) == ((node + 1024 * k) as u32) | 1u32,
            coarse_base(mirror_table_entry(node, k)) == mirror_table_entry(node, k) & 0xffff_fc00u32;
    // 1024 k + 4 (VA[19:12]) == 4 (VA[21:12]): the four 1 KiB tables are consecutive 256-entry quarters.
    assert(1024 * k + 4 * l2_index(va) == 4 * leaf_index(va)) by (bit_vector)
        requires
            k == (va >> 20u32) & 3u32,
            l2_index(va) == (va >> 12u32) & 255u32,
            leaf_index(va) == (va >> 12u32) & 1023u32;
    assert(node + 4 * leaf_index(va) <= 0xffff_ffff) by (bit_vector)
        requires node <= 0xffff_f000u32, node & 0xfffu32 == 0, leaf_index(va) == (va >> 12u32) & 1023u32;
}

/// mirror_of for a 4 MiB page at `pa`: section k of the slot, with attributes in bits[19:2].
pub open spec fn mirror_section_entry(pa: u32, k: u32, attrs: u32) -> u32 {
    ((pa + (k << 20u32)) as u32) | attrs
}

/// The hardware's output address through a section descriptor.
pub open spec fn hw_section_out(desc: u32, va: u32) -> u32 { (desc & 0xfff0_0000u32) | (va & 0x000f_ffffu32) }

/// OSTD's output address through a 4 MiB page at `pa`.
pub open spec fn ostd_huge_out(pa: u32, va: u32) -> u32 { pa | (va & 0x003f_ffffu32) }

/// 2. A section of the mirror translates like OSTD's 4 MiB entry.
pub proof fn lemma_section_mirror(pa: u32, va: u32, attrs: u32)
    requires
        pa & 0x003f_ffffu32 == 0,
        attrs & 0xfff0_0000u32 == 0,
    ensures
        hw_section_out(mirror_section_entry(pa, slot_entry(va), attrs), va) == ostd_huge_out(pa, va),
{
    let k = slot_entry(va);
    assert(hw_section_out(mirror_section_entry(pa, k, attrs), va) == ostd_huge_out(pa, va)) by (bit_vector)
        requires
            pa & 0x003f_ffffu32 == 0,
            attrs & 0xfff0_0000u32 == 0,
            k == (va >> 20u32) & 3u32,
            mirror_section_entry(pa, k, attrs) == ((pa + (k << 20u32)) as u32) | attrs,
            hw_section_out(mirror_section_entry(pa, k, attrs), va)
                == (mirror_section_entry(pa, k, attrs) & 0xfff0_0000u32) | (va & 0x000f_ffffu32),
            ostd_huge_out(pa, va) == pa | (va & 0x003f_ffffu32);
}

fn main() {}

} // verus!
