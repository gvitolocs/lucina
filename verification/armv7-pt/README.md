# ARMv7-A first-level mirror model (P1)

A Verus **model** of how the ARMv7-A port of OSTD (`kernel/patches/0021-*`, `ostd/src/arch/armv7/mm/`) lets the
short-descriptor MMU walk OSTD's page tables. It is a model of the code, not a proof of the shipped kernel (project
rule D-011): the specs restate the source's arithmetic, and only reading the two side by side ties them together.

OSTD's page tables on ARMv7-A have two levels of 1024 word-sized entries: a root whose entries map 4 MiB and leaf
nodes whose entries are hardware small-page descriptors. The MMU reads the leaf nodes directly, as four 1 KiB
second-level tables each, but walks its own first-level table (4096 entries of 1 MiB), which mirrors the root:
root entry `i` becomes first-level entries `4i .. 4i + 4` (`mirror_of` in `l1.rs`).

Run: `verify-gate -- /home/user/tools/verus/verus-x86-linux/verus verification/armv7-pt/mirror.rs`, or all proofs
with `verify-gate -- scripts/verify.sh`. Mutants: `scripts/verify-mutations.sh` (four entries for this directory).

## What is proved

For every 32-bit virtual address:

- `lemma_slots`: the first-level entry that the hardware reads is `4 * root_index + VA[21:20]`, so the four entries
  of a slot are exactly the 4 MiB of their root entry.
- `lemma_table_mirror`: through the mirror of a root entry that points to a leaf node, the hardware reads the
  same leaf entry (same physical address) as OSTD's software walk (`pte_index` at levels 2 and 1).
- `lemma_section_mirror`: through the mirror of a 4 MiB page (only in the boot page table), the hardware
  translates to the same physical address as OSTD's entry.

## What is not covered

- When the mirror is filled and dropped (first-level translation faults, root activation, TLB flushes covering a
  whole root entry): an argument in `l1.rs` and the ktests, not a proof.
- The descriptor attribute bits (memory types through TEX remapping, AP, XN, the software bits): checked by the
  ktests (`cast_frame_pte_preserves_repr`, `map_preserves_page_property`, `l1::test`), not modelled here.
- Table walk coherency with the data cache (the Cortex-A9's `ACTLR.SMP`), which only real hardware can show.
