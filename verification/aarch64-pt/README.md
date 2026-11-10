# AArch64 page-table model (T-VER-04)

A Verus **model** of the VMSAv8-64 stage-1 descriptors and the 4-level address-space walk that Asterinas v0.19.0
uses on AArch64. It is a model of the code, not a proof of the shipped kernel (project rule D-011): the exec
functions are copies of the Asterinas source with a few mechanical changes, and nothing ties them to the compiled
kernel except reading the two side by side. It is meant as the AArch64 base for vostd-style page-table proofs,
which have no AArch64 architecture spec today (see `docs/research/vostd-coverage.md`).

Run: `verify-gate -- scripts/verify.sh` (all files), or one file with
`verify-gate -- /home/user/tools/verus/verus-x86-linux/verus verification/aarch64-pt/<file>.rs`.
Mutants: `scripts/verify-mutations.sh` (six entries for this directory).

## Files

| File | Content |
| --- | --- |
| `descriptor.rs` | Descriptor bit fields, hardware decode specs (`kind`, `out_addr`, `table_addr`, AttrIndx, AP, SH, AF, nG, PXN, UXN, table attributes), Asterinas' `PageTableEntry` as exec code (`is_present`, `is_last`, `paddr`, `prop`, `pt_flags`, `new_page`, `new_pt`, `from_repr`, `to_repr`) with spec mirrors, round-trip and hardware-agreement lemmas, TCR_EL1/MAIR_EL1 checks. |
| `walk.rs` | Paging constants, `pte_index`, the abstract-memory walk `walk`, and lemmas on index coverage, disjointness, block ranges and the walk itself. Also the vostd plug-in note (`AArch64Consts`). |

`walk.rs` repeats the three small decode specs from `descriptor.rs`, because Verus checks each file on its own.
Keep them in sync when editing.

## What is proved

`descriptor.rs`
- Every exec function equals its spec mirror (`ensures r == s_...`), so the lemmas apply to the code.
- `lemma_new_page_roundtrip`: for levels 1..=4, `new_page` then Asterinas' own decode gives back the address, the
  flags (with ACCESSED forced on, the source's FIXME), the privileged flags and the cache policy; the entry is
  present and `is_last`.
- `lemma_new_pt_roundtrip`, `lemma_repr_roundtrip`: the same for page-table entries (levels 2..=4) and for
  `to_repr(from_repr(r))` as a whole (Absent, PageTable, Mapped).
- `lemma_new_page_hw`: what the MMU sees for a readable mapping at level 1..=3 with a block-aligned address:
  Page (level 1) or Block (levels 2, 3); output address equals the input; AttrIndx 0 (Normal WB) or 1 (Device-nGnRnE);
  SH inner shareable; AF = 1; nG = not GLOBAL; AP[2] = read-only iff not writable; AP[1] = USER; user pages have PXN
  set and kernel pages have UXN set (the kernel never executes user pages, EL0 never executes kernel pages); the
  X flag clears the matching XN bit.
- `lemma_new_pt_hw`: `new_pt` gives a hardware Table descriptor with the right address and table attributes
  (bits 63:59) all zero, so a table restricts nothing.
- `lemma_decode_agrees_hw`: at levels 2 and 3, for every valid descriptor Asterinas' `is_last` agrees with the MMU on
  block vs table.
- `lemma_out_addr_aligned`: the hardware output address of a block/page is aligned to its block size
  (4 KiB, 2 MiB, 1 GiB, 512 GiB) and below 2^48, for any descriptor bits.
- `lemma_tcr_config`, `lemma_mair_config`: the TCR_EL1 and MAIR_EL1 values written in `bsp_boot.S` select a 4 KiB
  granule (TG0 = 0, TG1 = 2), T0SZ = T1SZ = 16 (48-bit VA), IPS = 5, HA = HD = 0, and Attr0 = 0xFF, Attr1 = 0x00.

`walk.rs`
- `lemma_consts`: 512 entries of 8 bytes per 4 KiB table, index offsets 12/21/30/39 equal Asterinas' formula
  `12 + 9 * (level - 1)`, sizes are `1 << offset`, the root covers 2^48 bytes.
- `lemma_index_cover`: for any 48-bit VA the four indices plus the 12-bit offset rebuild the VA exactly, and
  `va >> 12` is the concatenation of the four indices: bits [47:12] are covered with no gap or overlap.
- `lemma_index_only_47_12`: the indices read only VA bits [47:12].
- `lemma_distinct_idx_disjoint`, `lemma_same_idx_same_block`: different index at a level means disjoint blocks
  of that level's size; equal higher bits means the same block.
- `lemma_block_maps_aligned_range`, `lemma_block_bijective`: a block at level L maps the `2^(12+9*(L-1))`-aligned range
  one-to-one onto the aligned PA range `[o, o + size)`; the in-block offset is preserved.
- `lemma_entry_in_table`: with a 4 KiB-aligned table address, every entry read stays 8-byte aligned inside the same frame.
- `lemma_walk_page_offset`, `lemma_walk_same_page`, `lemma_ttbr_halves_share`: a successful walk returns an
  address below 2^48 that keeps the page offset; addresses that agree on bits [47:12] translate to the same frame (or
  both fault); the TTBR0 and TTBR1 copies of an address translate alike, as Asterinas loads the same root in both.

## Mapping to Asterinas (third_party/asterinas, v0.19.0)

| Model | Source |
| --- | --- |
| consts `VALID`..`HIGH_IGN2`, `PHYS_ADDR_MASK` | `ostd/src/arch/arm/mm/mod.rs` `PteFlags`, `PageTableEntry::PHYS_ADDR_MASK` |
| `parse_flags!` | same file, `parse_flags!` (bit numbers instead of `.bits().ilog2()`) |
| `is_present`, `is_last`, `paddr`, `prop`, `pt_flags`, `new_page`, `new_pt` | same names in `impl PageTableEntry` |
| `from_repr`, `to_repr`, `Repr` | `unsafe impl PteTrait for PageTableEntry`, `PteScalar` in `ostd/src/mm/page_table/mod.rs` |
| `pte_index`, `shift`, `size`, `entry_addr`, `walk` | `pte_index`, `pte_index_bit_offset`, `page_walk` in `ostd/src/mm/page_table/mod.rs` |
| `tcr_el1`, `mair_el1` | `ostd/src/arch/arm/boot/bsp_boot.S` (`TCR_*`, `MAIR_*`) |
| `AArch64Consts` | `PagingConsts` in `arch/arm/mm/mod.rs`; plugs where vostd uses `PagingConstsTrait` / `ArchAddressSpaceModel` |

Deviations from the source: `usize` is `u64`; `PagingLevel` is `u8`; `PageProperty` and the flag sets are plain
`u8` with the page_prop.rs bit values; `CachePolicy` is `uncacheable: bool` (other policies panic in the real code);
`new_page` is a chain of `let` instead of `flags |=`; `debug_assert_eq!` on the address is a `requires`.
Levels follow Asterinas (1 = 4 KiB leaf, 4 = root; ARM level = 4 - level).

## Findings

1. **Level-4 mappings are invalid on the MMU.** `new_page` at level 4 clears NON_HUGE, so bits[1:0] = 0b01, a
   reserved encoding at ARM level 0 with a 4 KiB granule (`lemma_level4_mapping_is_hw_invalid`). Asterinas' own
   `is_last` reads it as `Mapped`, so the software view and the MMU disagree (`lemma_reserved_encodings`).
   `PagingConsts::HIGHEST_TRANSLATION_LEVEL` is 4 for this port. Whether the generic code ever creates a level-4
   leaf was not checked; if it can, that is a bug.
2. **The address check is a debug assertion.** `new_page`/`new_pt` check `paddr & !PHYS_ADDR_MASK == 0` only under
   `debug_assert_eq!`. All round-trip lemmas need it as a precondition; a release build with a wider address would
   corrupt the attribute bits. Also, `new_page` does not check that `paddr` is aligned to the level's block size; the
   hardware lemmas need that as a precondition (`lemma_new_page_hw`).
3. **PROT_NONE shape.** A mapping without R has VALID = 0 but SH_INNER set, so Asterinas calls it present while the
   MMU faults on it (intended, `lemma_prot_none`).

## Not modelled / unsure

- No TLB, no concurrency or memory ordering of the walker, no break-before-make; the walk reads one abstract word
  per entry.
- Not modelled: hardware AF/dirty update (TCR HA/HD are 0 here), contiguous bit, GP/BTI bit 50, PBHA, 52-bit PA/LPA2,
  stage 2, TBI/MTE, ASIDs. DBM (bit 51) is only used as Asterinas' software DIRTY flag.
- Table-descriptor bits 58:55 are treated as software-use; with FEAT_HPDS2 and TCR.HWU* they could mean PBHA. UNSURE.
- ARM ARM section numbers and some field names are from memory of DDI 0487 and differ between revisions; the bit
  positions are what the proofs use. They should be checked against the manual once.
- `kind` for level 1 with bits[1:0] = 0b01 and for level 4 blocks follows D8 (reserved); assumed, not looked up.
- The model walk follows the software walk (`page_walk`, `cfg(ktest)`), not the MMU's exact fault behaviour (it maps
  every non-leaf, non-table case to "no translation").
- The abstract memory is total (`spec_fn(u64) -> u64`); there is no notion of unmapped or device frames.
- Linking this to the compiled kernel (for example by comparing the model to the source token by token, as
  `kernel/lucina-bootimg` does) is not done.
