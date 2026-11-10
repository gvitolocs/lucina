// Verus MODEL (not a proof of the shipped kernel; project rule D-011): the AArch64 4 KiB-granule, 48-bit, 4-level
// address-space walk that Asterinas v0.19.0 relies on, and its index arithmetic.
//
// Mirrors:
//   third_party/asterinas/ostd/src/mm/page_table/mod.rs  pte_index, pte_index_bit_offset, nr_pte_index_bits,
//                                                        page_walk (the software walk under #[cfg(ktest)])
//   third_party/asterinas/ostd/src/arch/arm/mm/mod.rs    PagingConsts (BASE_PAGE_SIZE 4096, NR_LEVELS 4,
//                                                        ADDRESS_WIDTH 48, VA_SIGN_EXT true, PTE_SIZE 8)
// Levels are the Asterinas ones: 1 = leaf table (4 KiB), 4 = root; ARM ARM level = 4 - level.
// Descriptor decoding (`kind`, `out_addr`, `table_addr`) is the same spec as in descriptor.rs (Verus checks each
// file on its own, so the few lines are repeated; keep them in sync).
//
// ARM ARM (DDI 0487) D8.2 "The VMSAv8-64 translation process" / "Translation table walk": with a 4 KiB granule and
// TxSZ = 16, the walk starts at ARM level 0 and consumes VA[47:39], [38:30], [29:21], [20:12]; the low 12 bits are
// the page offset. Bits [63:48] only select TTBR0 (all zero) or TTBR1 (all one); Asterinas loads the SAME root
// into both (activate_page_table), so the walk of a VA does not depend on bits [63:48] (lemma_ttbr_halves_share).
// UNSURE: the exact section numbers (D8.2.x) differ between ARM ARM revisions.
//
// vostd plug-in shape: vostd keeps the architecture behind `ostd/specs/arch/model.rs::ArchAddressSpaceModel`
// plus `PagingConstsTrait` spec fns (`BASE_PAGE_SIZE()`, `NR_LEVELS()`, `ADDRESS_WIDTH()`, `PTE_SIZE()`) and
// `lemma_arch_specific_consts_properties` (x86: index bits 9, `pte_index_bit_offset(4) == 39`, 256 * 2^39 == 2^47).
// `AArch64Consts` below states the same facts for AArch64 so an `Aarch64Arch: ArchAddressSpaceModel` can reuse them.

use vstd::prelude::*;

verus! {

// ---------------------------------------------------------------- paging constants
pub struct AArch64Consts;

impl AArch64Consts {
    pub open spec fn base_page_size() -> int { 4096 }
    pub open spec fn nr_levels() -> int { 4 }
    pub open spec fn address_width() -> int { 48 }
    pub open spec fn pte_size() -> int { 8 }
    /// nr_subpage_per_huge = BASE_PAGE_SIZE / PTE_SIZE
    pub open spec fn nr_subpage_per_huge() -> int { Self::base_page_size() / Self::pte_size() }
}

/// nr_pte_index_bits = nr_subpage_per_huge.ilog2(); here 512 = 2^9.
pub open spec fn nr_pte_index_bits() -> int { 9 }

/// pte_index_bit_offset: BASE_PAGE_SIZE.ilog2() + nr_pte_index_bits * (level - 1)
pub open spec fn pte_index_bit_offset(level: u8) -> int {
    12 + nr_pte_index_bits() * (level as int - 1)
}

/// The same offset, written as literals so bit-vector proofs can use it.
pub open spec fn shift(l: u8) -> u64 {
    if l == 1 { 12 } else if l == 2 { 21 } else if l == 3 { 30 } else { 39 }
}

/// Size in bytes of the range an entry at level `l` covers: 4 KiB, 2 MiB, 1 GiB, 512 GiB.
pub open spec fn size(l: u8) -> u64 {
    if l == 1 { 0x1000 } else if l == 2 { 0x20_0000 } else if l == 3 { 0x4000_0000 } else { 0x80_0000_0000 }
}

/// pte_index: (va >> pte_index_bit_offset(level)) & (nr_subpage_per_huge - 1)
pub open spec fn pte_index(va: u64, l: u8) -> u64 {
    if l == 1 { (va >> 12) & 511 } else if l == 2 { (va >> 21) & 511 } else if l == 3 { (va >> 30) & 511 } else { (va >> 39) & 511 }
}

/// Start of the block of `size(l)` bytes that contains `va`.
pub open spec fn entry_base(va: u64, l: u8) -> u64 {
    va & !((size(l) - 1) as u64)
}

pub proof fn lemma_consts()
    ensures
        AArch64Consts::nr_subpage_per_huge() == 512,
        512 == 0x200,
        forall|l: u8| 1 <= l <= 4 ==> #[trigger] pte_index_bit_offset(l) == shift(l) as int,
        forall|l: u8| 1 <= l <= 4 ==> #[trigger] size(l) == 1u64 << shift(l),
        pte_index_bit_offset(4) == 39,
        // the root table covers exactly the 48-bit space, the lower 256 entries the lower 2^47
        512 * 0x80_0000_0000int == 0x1_0000_0000_0000int,
        256 * 0x80_0000_0000int == 0x8000_0000_0000int,
        AArch64Consts::nr_subpage_per_huge() * AArch64Consts::pte_size() == AArch64Consts::base_page_size(),
{
    assert forall|l: u8| 1 <= l <= 4 implies #[trigger] size(l) == 1u64 << shift(l) by {
        if l == 1 { assert(0x1000u64 == 1u64 << 12u64) by (bit_vector); }
        else if l == 2 { assert(0x20_0000u64 == 1u64 << 21u64) by (bit_vector); }
        else if l == 3 { assert(0x4000_0000u64 == 1u64 << 30u64) by (bit_vector); }
        else { assert(0x80_0000_0000u64 == 1u64 << 39u64) by (bit_vector); }
    }
}

// ---------------------------------------------------------------- index extraction
/// The four indices, with the 12-bit page offset, rebuild every 48-bit VA exactly, so [47:12] is covered with no gap
/// and no overlap; every index is below 512.
pub proof fn lemma_index_cover(va: u64)
    requires
        va < 0x1_0000_0000_0000,
    ensures
        pte_index(va, 1) < 512 && pte_index(va, 2) < 512 && pte_index(va, 3) < 512 && pte_index(va, 4) < 512,
        va == (pte_index(va, 4) << 39) | (pte_index(va, 3) << 30) | (pte_index(va, 2) << 21) | (pte_index(va, 1) << 12)
            | (va & 0xFFF),
        (va >> 12) == (pte_index(va, 4) << 27) | (pte_index(va, 3) << 18) | (pte_index(va, 2) << 9) | pte_index(va, 1),
{
    assert(pte_index(va, 1) < 512 && pte_index(va, 2) < 512 && pte_index(va, 3) < 512 && pte_index(va, 4) < 512) by (bit_vector);
    assert(va == (pte_index(va, 4) << 39) | (pte_index(va, 3) << 30) | (pte_index(va, 2) << 21) | (pte_index(va, 1) << 12)
        | (va & 0xFFF)) by (bit_vector)
        requires va < 0x1_0000_0000_0000;
    assert((va >> 12) == (pte_index(va, 4) << 27) | (pte_index(va, 3) << 18) | (pte_index(va, 2) << 9) | pte_index(va, 1)) by (bit_vector)
        requires va < 0x1_0000_0000_0000;
}

/// The indices read only VA bits [47:12]: bits [63:48] and [11:0] never matter.
pub proof fn lemma_index_only_47_12(va: u64, va2: u64, l: u8)
    requires
        1 <= l <= 4,
        va & 0x0000_FFFF_FFFF_F000 == va2 & 0x0000_FFFF_FFFF_F000,
    ensures
        pte_index(va, l) == pte_index(va2, l),
{
    assert(pte_index(va, l) == pte_index(va2, l)) by (bit_vector)
        requires
            va & 0x0000_FFFF_FFFF_F000 == va2 & 0x0000_FFFF_FFFF_F000,
            1 <= l <= 4;
}

/// Two addresses with different indices at one level lie in different, non-overlapping blocks of that level.
pub proof fn lemma_distinct_idx_disjoint(va1: u64, va2: u64, l: u8)
    requires
        1 <= l <= 4,
        va1 < 0x1_0000_0000_0000,
        va2 < 0x1_0000_0000_0000,
        pte_index(va1, l) != pte_index(va2, l),
    ensures
        entry_base(va1, l) <= va1 < entry_base(va1, l) + size(l),
        entry_base(va2, l) <= va2 < entry_base(va2, l) + size(l),
        entry_base(va1, l) + size(l) <= entry_base(va2, l) || entry_base(va2, l) + size(l) <= entry_base(va1, l),
{
    if l == 1 {
        assert(entry_base(va1, 1) <= va1 && va1 < entry_base(va1, 1) + 0x1000) by (bit_vector) requires va1 < 0x1_0000_0000_0000;
        assert(entry_base(va2, 1) <= va2 && va2 < entry_base(va2, 1) + 0x1000) by (bit_vector) requires va2 < 0x1_0000_0000_0000;
        assert(entry_base(va1, 1) + 0x1000 <= entry_base(va2, 1) || entry_base(va2, 1) + 0x1000 <= entry_base(va1, 1)) by (bit_vector)
            requires va1 < 0x1_0000_0000_0000, va2 < 0x1_0000_0000_0000, pte_index(va1, 1) != pte_index(va2, 1);
    } else if l == 2 {
        assert(entry_base(va1, 2) <= va1 && va1 < entry_base(va1, 2) + 0x20_0000) by (bit_vector) requires va1 < 0x1_0000_0000_0000;
        assert(entry_base(va2, 2) <= va2 && va2 < entry_base(va2, 2) + 0x20_0000) by (bit_vector) requires va2 < 0x1_0000_0000_0000;
        assert(entry_base(va1, 2) + 0x20_0000 <= entry_base(va2, 2) || entry_base(va2, 2) + 0x20_0000 <= entry_base(va1, 2)) by (bit_vector)
            requires va1 < 0x1_0000_0000_0000, va2 < 0x1_0000_0000_0000, pte_index(va1, 2) != pte_index(va2, 2);
    } else if l == 3 {
        assert(entry_base(va1, 3) <= va1 && va1 < entry_base(va1, 3) + 0x4000_0000) by (bit_vector) requires va1 < 0x1_0000_0000_0000;
        assert(entry_base(va2, 3) <= va2 && va2 < entry_base(va2, 3) + 0x4000_0000) by (bit_vector) requires va2 < 0x1_0000_0000_0000;
        assert(entry_base(va1, 3) + 0x4000_0000 <= entry_base(va2, 3) || entry_base(va2, 3) + 0x4000_0000 <= entry_base(va1, 3)) by (bit_vector)
            requires va1 < 0x1_0000_0000_0000, va2 < 0x1_0000_0000_0000, pte_index(va1, 3) != pte_index(va2, 3);
    } else {
        assert(entry_base(va1, 4) <= va1 && va1 < entry_base(va1, 4) + 0x80_0000_0000) by (bit_vector) requires va1 < 0x1_0000_0000_0000;
        assert(entry_base(va2, 4) <= va2 && va2 < entry_base(va2, 4) + 0x80_0000_0000) by (bit_vector) requires va2 < 0x1_0000_0000_0000;
        assert(entry_base(va1, 4) + 0x80_0000_0000 <= entry_base(va2, 4) || entry_base(va2, 4) + 0x80_0000_0000 <= entry_base(va1, 4)) by (bit_vector)
            requires va1 < 0x1_0000_0000_0000, va2 < 0x1_0000_0000_0000, pte_index(va1, 4) != pte_index(va2, 4);
    }
}

/// Same table, same index, same block (the converse): equal index and equal higher bits give equal blocks.
pub proof fn lemma_same_idx_same_block(va1: u64, va2: u64, l: u8)
    requires
        1 <= l <= 4,
        va1 >> shift(l) == va2 >> shift(l),
    ensures
        entry_base(va1, l) == entry_base(va2, l),
        pte_index(va1, l) == pte_index(va2, l),
{
    if l == 1 {
        assert(entry_base(va1, 1) == entry_base(va2, 1) && pte_index(va1, 1) == pte_index(va2, 1)) by (bit_vector)
            requires va1 >> 12 == va2 >> 12;
    } else if l == 2 {
        assert(entry_base(va1, 2) == entry_base(va2, 2) && pte_index(va1, 2) == pte_index(va2, 2)) by (bit_vector)
            requires va1 >> 21 == va2 >> 21;
    } else if l == 3 {
        assert(entry_base(va1, 3) == entry_base(va2, 3) && pte_index(va1, 3) == pte_index(va2, 3)) by (bit_vector)
            requires va1 >> 30 == va2 >> 30;
    } else {
        assert(entry_base(va1, 4) == entry_base(va2, 4) && pte_index(va1, 4) == pte_index(va2, 4)) by (bit_vector)
            requires va1 >> 39 == va2 >> 39;
    }
}

// ---------------------------------------------------------------- leaf (block/page) translation
/// Output of a leaf at level `l` whose output address is `o`: `o` plus the VA offset inside the block
/// (the source: `frame_paddr + (vaddr & (page_size::<C>(cur_level) - 1))`).
pub open spec fn leaf_pa(o: u64, va: u64, l: u8) -> u64 {
    (o + (va & ((size(l) - 1) as u64))) as u64
}

/// A block/page at level `l` maps one `size(l)`-aligned VA range one-to-one onto the `size(l)`-aligned PA range
/// starting at `o`: offsets are kept, the image is exactly [o, o + size), and every PA in it is hit.
pub proof fn lemma_block_maps_aligned_range(o: u64, va: u64, l: u8)
    requires
        1 <= l <= 4,
        o & ((size(l) - 1) as u64) == 0,
        o < 0x1_0000_0000_0000,
    ensures
        o <= leaf_pa(o, va, l) < o + size(l),
        leaf_pa(o, va, l) & ((size(l) - 1) as u64) == va & ((size(l) - 1) as u64),
        leaf_pa(o, va, l) as int - o as int == va as int - entry_base(va, l) as int,
        leaf_pa(o, va, l) < 0x1_0000_0000_0000,
        entry_base(va, l) & ((size(l) - 1) as u64) == 0,
{
    if l == 1 {
        assert(o <= leaf_pa(o, va, 1) && leaf_pa(o, va, 1) < o + 0x1000) by (bit_vector)
            requires o & 0xFFF == 0, o < 0x1_0000_0000_0000;
        assert(leaf_pa(o, va, 1) & 0xFFF == va & 0xFFF) by (bit_vector) requires o & 0xFFF == 0, o < 0x1_0000_0000_0000;
        assert(leaf_pa(o, va, 1) as int - o as int == va as int - entry_base(va, 1) as int) by (bit_vector)
            requires o & 0xFFF == 0, o < 0x1_0000_0000_0000;
        assert(leaf_pa(o, va, 1) < 0x1_0000_0000_0000) by (bit_vector) requires o & 0xFFF == 0, o < 0x1_0000_0000_0000;
        assert(entry_base(va, 1) & 0xFFF == 0) by (bit_vector);
    } else if l == 2 {
        assert(o <= leaf_pa(o, va, 2) && leaf_pa(o, va, 2) < o + 0x20_0000) by (bit_vector)
            requires o & 0x1F_FFFF == 0, o < 0x1_0000_0000_0000;
        assert(leaf_pa(o, va, 2) & 0x1F_FFFF == va & 0x1F_FFFF) by (bit_vector) requires o & 0x1F_FFFF == 0, o < 0x1_0000_0000_0000;
        assert(leaf_pa(o, va, 2) as int - o as int == va as int - entry_base(va, 2) as int) by (bit_vector)
            requires o & 0x1F_FFFF == 0, o < 0x1_0000_0000_0000;
        assert(leaf_pa(o, va, 2) < 0x1_0000_0000_0000) by (bit_vector) requires o & 0x1F_FFFF == 0, o < 0x1_0000_0000_0000;
        assert(entry_base(va, 2) & 0x1F_FFFF == 0) by (bit_vector);
    } else if l == 3 {
        assert(o <= leaf_pa(o, va, 3) && leaf_pa(o, va, 3) < o + 0x4000_0000) by (bit_vector)
            requires o & 0x3FFF_FFFF == 0, o < 0x1_0000_0000_0000;
        assert(leaf_pa(o, va, 3) & 0x3FFF_FFFF == va & 0x3FFF_FFFF) by (bit_vector) requires o & 0x3FFF_FFFF == 0, o < 0x1_0000_0000_0000;
        assert(leaf_pa(o, va, 3) as int - o as int == va as int - entry_base(va, 3) as int) by (bit_vector)
            requires o & 0x3FFF_FFFF == 0, o < 0x1_0000_0000_0000;
        assert(leaf_pa(o, va, 3) < 0x1_0000_0000_0000) by (bit_vector) requires o & 0x3FFF_FFFF == 0, o < 0x1_0000_0000_0000;
        assert(entry_base(va, 3) & 0x3FFF_FFFF == 0) by (bit_vector);
    } else {
        assert(o <= leaf_pa(o, va, 4) && leaf_pa(o, va, 4) < o + 0x80_0000_0000) by (bit_vector)
            requires o & 0x7F_FFFF_FFFF == 0, o < 0x1_0000_0000_0000;
        assert(leaf_pa(o, va, 4) & 0x7F_FFFF_FFFF == va & 0x7F_FFFF_FFFF) by (bit_vector) requires o & 0x7F_FFFF_FFFF == 0, o < 0x1_0000_0000_0000;
        assert(leaf_pa(o, va, 4) as int - o as int == va as int - entry_base(va, 4) as int) by (bit_vector)
            requires o & 0x7F_FFFF_FFFF == 0, o < 0x1_0000_0000_0000;
        assert(leaf_pa(o, va, 4) < 0x1_0000_0000_0000) by (bit_vector) requires o & 0x7F_FFFF_FFFF == 0, o < 0x1_0000_0000_0000;
        assert(entry_base(va, 4) & 0x7F_FFFF_FFFF == 0) by (bit_vector);
    }
}

/// Inside one block the translation is injective, and it is onto [o, o + size).
pub proof fn lemma_block_bijective(o: u64, va1: u64, va2: u64, pa: u64, l: u8)
    requires
        1 <= l <= 4,
        o & ((size(l) - 1) as u64) == 0,
        o < 0x1_0000_0000_0000,
        entry_base(va1, l) == entry_base(va2, l),
        leaf_pa(o, va1, l) == leaf_pa(o, va2, l),
        o <= pa < o + size(l),
        va1 < 0x1_0000_0000_0000,
    ensures
        va1 == va2,
        leaf_pa(o, (entry_base(va1, l) | ((pa - o) as u64)) as u64, l) == pa,
        entry_base(va1, l) | ((pa - o) as u64) >= entry_base(va1, l),
        (entry_base(va1, l) | ((pa - o) as u64)) < entry_base(va1, l) + size(l),
{
    if l == 1 {
        assert(va1 == va2) by (bit_vector)
            requires o & 0xFFF == 0, o < 0x1_0000_0000_0000, entry_base(va1, 1) == entry_base(va2, 1), leaf_pa(o, va1, 1) == leaf_pa(o, va2, 1);
        assert(leaf_pa(o, (entry_base(va1, 1) | ((pa - o) as u64)) as u64, 1) == pa) by (bit_vector)
            requires o & 0xFFF == 0, o < 0x1_0000_0000_0000, o <= pa, pa < o + 0x1000;
        assert(entry_base(va1, 1) | ((pa - o) as u64) >= entry_base(va1, 1) && (entry_base(va1, 1) | ((pa - o) as u64)) < entry_base(va1, 1) + 0x1000) by (bit_vector)
            requires o <= pa, pa < o + 0x1000, o < 0x1_0000_0000_0000, va1 < 0x1_0000_0000_0000;
    } else if l == 2 {
        assert(va1 == va2) by (bit_vector)
            requires o & 0x1F_FFFF == 0, o < 0x1_0000_0000_0000, entry_base(va1, 2) == entry_base(va2, 2), leaf_pa(o, va1, 2) == leaf_pa(o, va2, 2);
        assert(leaf_pa(o, (entry_base(va1, 2) | ((pa - o) as u64)) as u64, 2) == pa) by (bit_vector)
            requires o & 0x1F_FFFF == 0, o < 0x1_0000_0000_0000, o <= pa, pa < o + 0x20_0000;
        assert(entry_base(va1, 2) | ((pa - o) as u64) >= entry_base(va1, 2) && (entry_base(va1, 2) | ((pa - o) as u64)) < entry_base(va1, 2) + 0x20_0000) by (bit_vector)
            requires o <= pa, pa < o + 0x20_0000, o < 0x1_0000_0000_0000, va1 < 0x1_0000_0000_0000;
    } else if l == 3 {
        assert(va1 == va2) by (bit_vector)
            requires o & 0x3FFF_FFFF == 0, o < 0x1_0000_0000_0000, entry_base(va1, 3) == entry_base(va2, 3), leaf_pa(o, va1, 3) == leaf_pa(o, va2, 3);
        assert(leaf_pa(o, (entry_base(va1, 3) | ((pa - o) as u64)) as u64, 3) == pa) by (bit_vector)
            requires o & 0x3FFF_FFFF == 0, o < 0x1_0000_0000_0000, o <= pa, pa < o + 0x4000_0000;
        assert(entry_base(va1, 3) | ((pa - o) as u64) >= entry_base(va1, 3) && (entry_base(va1, 3) | ((pa - o) as u64)) < entry_base(va1, 3) + 0x4000_0000) by (bit_vector)
            requires o <= pa, pa < o + 0x4000_0000, o < 0x1_0000_0000_0000, va1 < 0x1_0000_0000_0000;
    } else {
        assert(va1 == va2) by (bit_vector)
            requires o & 0x7F_FFFF_FFFF == 0, o < 0x1_0000_0000_0000, entry_base(va1, 4) == entry_base(va2, 4), leaf_pa(o, va1, 4) == leaf_pa(o, va2, 4);
        assert(leaf_pa(o, (entry_base(va1, 4) | ((pa - o) as u64)) as u64, 4) == pa) by (bit_vector)
            requires o & 0x7F_FFFF_FFFF == 0, o < 0x1_0000_0000_0000, o <= pa, pa < o + 0x80_0000_0000;
        assert(entry_base(va1, 4) | ((pa - o) as u64) >= entry_base(va1, 4) && (entry_base(va1, 4) | ((pa - o) as u64)) < entry_base(va1, 4) + 0x80_0000_0000) by (bit_vector)
            requires o <= pa, pa < o + 0x80_0000_0000, o < 0x1_0000_0000_0000, va1 < 0x1_0000_0000_0000;
    }
}

// ---------------------------------------------------------------- the walk
pub enum Kind {
    Invalid,
    Table,
    Block,
    Page,
}

pub open spec fn kind(d: u64, l: u8) -> Kind {
    if d & 1 == 0 {
        Kind::Invalid
    } else if l == 1 {
        if d & 2 != 0 { Kind::Page } else { Kind::Invalid }
    } else if d & 2 != 0 {
        Kind::Table
    } else if l == 4 {
        Kind::Invalid
    } else {
        Kind::Block
    }
}

pub open spec fn out_addr(d: u64, l: u8) -> u64 {
    d & 0x0000_FFFF_FFFF_F000 & !((size(l) - 1) as u64)
}

pub open spec fn table_addr(d: u64) -> u64 {
    d & 0x0000_FFFF_FFFF_F000
}

/// Physical memory as seen by the walker: the 8-byte word at each 8-byte-aligned physical address.
pub type Mem = spec_fn(u64) -> u64;

/// Address of the entry for `va` in the table at `tpa`, at level `l` (the source: `(pt_addr as *mut C::E).add(offset)`).
pub open spec fn entry_addr(tpa: u64, va: u64, l: u8) -> u64 {
    (tpa + 8 * pte_index(va, l)) as u64
}

/// The 4-level walk: read the entry, then stop (invalid), descend (table) or return the translation (block/page).
pub open spec fn walk(mem: Mem, tpa: u64, va: u64, l: u8) -> Option<u64>
    decreases l,
{
    if l == 0 || l > 4 {
        None
    } else {
        let d = mem(entry_addr(tpa, va, l));
        match kind(d, l) {
            Kind::Invalid => None,
            Kind::Table => if l == 1 { None } else { walk(mem, table_addr(d), va, (l - 1) as u8) },
            Kind::Block => Some(leaf_pa(out_addr(d, l), va, l)),
            Kind::Page => Some(leaf_pa(out_addr(d, l), va, l)),
        }
    }
}

/// A 4 KiB-aligned table address below 2^48 keeps every entry of the walk inside the table's frame.
pub proof fn lemma_entry_in_table(tpa: u64, va: u64, l: u8)
    requires
        1 <= l <= 4,
        tpa & 0xFFF == 0,
        tpa < 0x1_0000_0000_0000,
    ensures
        entry_addr(tpa, va, l) & 7 == 0,
        entry_addr(tpa, va, l) >> 12 == tpa >> 12,
        entry_addr(tpa, va, l) < 0x1_0000_0000_0000,
{
    assert(pte_index(va, l) < 512) by (bit_vector) requires 1 <= l <= 4;
    assert(entry_addr(tpa, va, l) & 7 == 0 && entry_addr(tpa, va, l) >> 12 == tpa >> 12 && entry_addr(tpa, va, l) < 0x1_0000_0000_0000) by (bit_vector)
        requires tpa & 0xFFF == 0, tpa < 0x1_0000_0000_0000, pte_index(va, l) < 512, entry_addr(tpa, va, l) == (tpa + 8 * pte_index(va, l)) as u64;
}

/// Every leaf's output address is below 2^48 and the result keeps the in-block offset.
pub proof fn lemma_walk_page_offset(mem: Mem, tpa: u64, va: u64, l: u8)
    requires
        l <= 4,
    ensures
        walk(mem, tpa, va, l) is Some ==> (walk(mem, tpa, va, l).unwrap() & 0xFFF == va & 0xFFF
            && walk(mem, tpa, va, l).unwrap() < 0x1_0000_0000_0000),
    decreases l,
{
    if l >= 1 {
        let d = mem(entry_addr(tpa, va, l));
        match kind(d, l) {
            Kind::Table => {
                if l > 1 {
                    lemma_walk_page_offset(mem, table_addr(d), va, (l - 1) as u8);
                }
            },
            Kind::Block | Kind::Page => {
                let o = out_addr(d, l);
                if l == 1 {
                    assert(o & 0xFFF == 0 && o < 0x1_0000_0000_0000) by (bit_vector) requires o == out_addr(d, 1);
                    lemma_block_maps_aligned_range(o, va, 1);
                    assert(leaf_pa(o, va, 1) & 0xFFF == va & 0xFFF) by (bit_vector) requires o & 0xFFF == 0, o < 0x1_0000_0000_0000;
                } else if l == 2 {
                    assert(o & 0x1F_FFFF == 0 && o < 0x1_0000_0000_0000) by (bit_vector) requires o == out_addr(d, 2);
                    lemma_block_maps_aligned_range(o, va, 2);
                    assert(leaf_pa(o, va, 2) & 0xFFF == va & 0xFFF) by (bit_vector) requires o & 0x1F_FFFF == 0, o < 0x1_0000_0000_0000;
                } else if l == 3 {
                    assert(o & 0x3FFF_FFFF == 0 && o < 0x1_0000_0000_0000) by (bit_vector) requires o == out_addr(d, 3);
                    lemma_block_maps_aligned_range(o, va, 3);
                    assert(leaf_pa(o, va, 3) & 0xFFF == va & 0xFFF) by (bit_vector) requires o & 0x3FFF_FFFF == 0, o < 0x1_0000_0000_0000;
                } else {
                    assert(o & 0x7F_FFFF_FFFF == 0 && o < 0x1_0000_0000_0000) by (bit_vector) requires o == out_addr(d, 4);
                    lemma_block_maps_aligned_range(o, va, 4);
                    assert(leaf_pa(o, va, 4) & 0xFFF == va & 0xFFF) by (bit_vector) requires o & 0x7F_FFFF_FFFF == 0, o < 0x1_0000_0000_0000;
                }
            },
            Kind::Invalid => {},
        }
    }
}

/// Same page, same frame: two addresses that agree on bits [47:12] translate to the same frame (or both fault);
/// bits [63:48] and [11:0] are irrelevant.
pub proof fn lemma_walk_same_page(mem: Mem, tpa: u64, va: u64, va2: u64, l: u8)
    requires
        l <= 4,
        va & 0x0000_FFFF_FFFF_F000 == va2 & 0x0000_FFFF_FFFF_F000,
    ensures
        walk(mem, tpa, va, l) is Some == walk(mem, tpa, va2, l) is Some,
        walk(mem, tpa, va, l) is Some ==> walk(mem, tpa, va, l).unwrap() >> 12 == walk(mem, tpa, va2, l).unwrap() >> 12,
    decreases l,
{
    if l >= 1 {
        lemma_index_only_47_12(va, va2, l);
        let d = mem(entry_addr(tpa, va, l));
        assert(entry_addr(tpa, va, l) == entry_addr(tpa, va2, l));
        match kind(d, l) {
            Kind::Table => {
                if l > 1 {
                    lemma_walk_same_page(mem, table_addr(d), va, va2, (l - 1) as u8);
                }
            },
            Kind::Block | Kind::Page => {
                let o = out_addr(d, l);
                if l == 1 {
                    assert(o & 0xFFF == 0 && o < 0x1_0000_0000_0000) by (bit_vector) requires o == out_addr(d, 1);
                    assert(leaf_pa(o, va, 1) >> 12 == leaf_pa(o, va2, 1) >> 12) by (bit_vector)
                        requires va & 0x0000_FFFF_FFFF_F000 == va2 & 0x0000_FFFF_FFFF_F000, o & 0xFFF == 0, o < 0x1_0000_0000_0000;
                } else if l == 2 {
                    assert(o & 0x1F_FFFF == 0 && o < 0x1_0000_0000_0000) by (bit_vector) requires o == out_addr(d, 2);
                    assert(leaf_pa(o, va, 2) >> 12 == leaf_pa(o, va2, 2) >> 12) by (bit_vector)
                        requires va & 0x0000_FFFF_FFFF_F000 == va2 & 0x0000_FFFF_FFFF_F000, o & 0x1F_FFFF == 0, o < 0x1_0000_0000_0000;
                } else if l == 3 {
                    assert(o & 0x3FFF_FFFF == 0 && o < 0x1_0000_0000_0000) by (bit_vector) requires o == out_addr(d, 3);
                    assert(leaf_pa(o, va, 3) >> 12 == leaf_pa(o, va2, 3) >> 12) by (bit_vector)
                        requires va & 0x0000_FFFF_FFFF_F000 == va2 & 0x0000_FFFF_FFFF_F000, o & 0x3FFF_FFFF == 0, o < 0x1_0000_0000_0000;
                } else {
                    assert(o & 0x7F_FFFF_FFFF == 0 && o < 0x1_0000_0000_0000) by (bit_vector) requires o == out_addr(d, 4);
                    assert(leaf_pa(o, va, 4) >> 12 == leaf_pa(o, va2, 4) >> 12) by (bit_vector)
                        requires va & 0x0000_FFFF_FFFF_F000 == va2 & 0x0000_FFFF_FFFF_F000, o & 0x7F_FFFF_FFFF == 0, o < 0x1_0000_0000_0000;
                }
            },
            Kind::Invalid => {},
        }
    }
}

/// Asterinas loads one root into TTBR0 and TTBR1, so the lower (user) and upper (kernel, bits [63:48] all ones)
/// copies of the same 48-bit address translate identically. For example the kernel VA 0xFFFF_8000_0000_0000 + x
/// walks like the TTBR0 VA 0x0000_8000_0000_0000 + x.
pub proof fn lemma_ttbr_halves_share(mem: Mem, root: u64, va: u64)
    ensures
        walk(mem, root, va, 4) is Some == walk(mem, root, va & 0x0000_FFFF_FFFF_FFFF, 4) is Some,
        walk(mem, root, va, 4) is Some ==> walk(mem, root, va, 4).unwrap() >> 12
            == walk(mem, root, va & 0x0000_FFFF_FFFF_FFFF, 4).unwrap() >> 12,
{
    assert(va & 0x0000_FFFF_FFFF_F000 == (va & 0x0000_FFFF_FFFF_FFFF) & 0x0000_FFFF_FFFF_F000) by (bit_vector);
    lemma_walk_same_page(mem, root, va, va & 0x0000_FFFF_FFFF_FFFF, 4);
}

fn main() {}

} // verus!
