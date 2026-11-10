#!/usr/bin/env bash
# Non-vacuity check for the Verus proofs: each mutation below weakens the code or strengthens a claim into a
# false one, and Verus must REJECT every mutant. A mutant that verifies means the proof does not constrain
# what it claims to. On buildhost run it through the gate:  verify-gate -- scripts/verify-mutations.sh
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# file | sed expression (must change the file) | what it breaks. The fields are split on "|", so the sed
# expression cannot contain one.
mutations=(
    'verification/bootimg/align.rs|s/r - x < p,/r - x < p - 1,/|align_up: claims a gap smaller than possible'
    'verification/bootimg/align.rs|s/let s = x + (p - 1);/let s = x + p;/|align_up: off-by-one rounding in the code'
    'verification/bootimg/layout.rs|s/\&\&\& next - (off + size) < p$/\&\&\& next - (off + size) < p - 1/|layout: claims tighter packing than possible'
    'verification/bootimg/layout.rs|s/let n = (len as u64 + (p - 1)) \/ p;/let n = (len as u64 + p) \/ p;/|pages: off-by-one rounding in the code'
    'verification/bootimg/layout.rs|s/let kernel_page: u64 = 1;/let kernel_page: u64 = 0;/|layout: kernel overlaps the header page'
    'verification/aarch64-pt/descriptor.rs|s/parse_flags!(flags, 4, 51)/parse_flags!(flags, 4, 52)/|aarch64 new_page: DIRTY encoded in bit 52 (Contiguous) instead of 51'
    'verification/aarch64-pt/descriptor.rs|s/parse_flags!(!flags, 2, 54)/parse_flags!(!flags, 2, 53)/|aarch64 new_page: user X written to PXN instead of UXN'
    'verification/aarch64-pt/descriptor.rs|s/\&\&\& sh(e) == 3  \/\/ inner shareable/\&\&\& sh(e) == 2  \/\/ inner shareable/|aarch64 descriptor: claims outer-shareable instead of inner'
    'verification/aarch64-pt/descriptor.rs|s/pub const PHYS_ADDR_MASK: u64 = 0x0000_FFFF_FFFF_F000;/pub const PHYS_ADDR_MASK: u64 = 0x0000_FFFF_FFFF_F800;/|aarch64 PTE: address mask overlaps the nG bit'
    'verification/aarch64-pt/walk.rs|s/{ (va >> 39) \& 511 }/{ (va >> 38) \& 511 }/|aarch64 walk: root index taken from VA bit 38'
    'verification/aarch64-pt/walk.rs|s/else if l == 3 { 0x4000_0000 } else { 0x80_0000_0000 }/else if l == 3 { 0x2000_0000 } else { 0x80_0000_0000 }/|aarch64 walk: 1 GiB block size wrong'
    'verification/armv7-pt/mirror.rs|/spec fn mirror_table_entry/s/1024 \* k/512 * k/|armv7 mirror: coarse tables 512 bytes apart instead of 1 KiB'
    'verification/armv7-pt/mirror.rs|s/(va >> 12u32) \& 255u32 }/(va >> 12u32) \& 511u32 }/|armv7 walk: second-level index with 9 bits instead of 8'
    'verification/armv7-pt/mirror.rs|/spec fn mirror_section_entry/,/^}/s/k << 20u32/k << 21u32/|armv7 mirror: sections 2 MiB apart instead of 1 MiB'
    'verification/armv7-pt/mirror.rs|s/(va >> 20u32) \& 3u32 }/(va >> 21u32) \& 3u32 }/|armv7 mirror: slot entry from VA bits 22-21'
    'verification/fdt/header.rs|s/    if off_rsv >= total {/    if off_rsv > total {/|fdt header: reservation block may start at totalsize'
    'verification/fdt/header.rs|s/size_struct % 4 != 0/size_struct % 2 != 0/|fdt header: structure size only checked to be even'
    'verification/fdt/header.rs|s/size_strings > total - off_strings/size_strings > total - off_struct/|fdt header: strings block bounded by the wrong offset'
    'verification/fdt/header.rs|s/\&\&\& h.off_rsv < h.total$/\&\&\& h.off_rsv + 16 <= h.total/|fdt header: claims room for a whole reservation entry'
    'verification/vostd-port/frame_linked_list.rs|s/        self.list.size += 1;/        self.list.size += 0;/|linked list insert_before: size not updated'
    'verification/vostd-port/frame_linked_list.rs|s/res@.order == owner.order.insert(0, frame.paddr),/res@.order == owner.order.push(frame.paddr),/|linked list push_front: claims the frame goes to the back'
)

status=0
for m in "${mutations[@]}"; do
    IFS='|' read -r file expr what <<< "$m"
    mutant="$work/$(basename "$file" .rs)-mutant.rs"
    sed "$expr" "$root/$file" > "$mutant"
    if cmp -s "$root/$file" "$mutant"; then
        echo "STALE $what: the sed expression no longer matches $file"
        status=1
    elif "$verus" "$mutant" > "$mutant.log" 2>&1; then
        echo "FAIL  $what: the mutant VERIFIED"
        status=1
    else
        echo "ok    $what: rejected ($(grep -m1 -o 'verification results:: .*' "$mutant.log" || echo 'compile error'))"
    fi
done
exit $status
