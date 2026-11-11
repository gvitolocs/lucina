#!/usr/bin/env bash
# Shows that verification/juno-kexec/rules.rs can fail: each line plants one bug in a copy of the shipped file (or of the
# proof file, for the model of the walk), and Verus must refuse it. Run on buildhost from the repo root, under verify-gate.
set -uo pipefail
verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
work=$(mktemp -d "${TMPDIR:-/tmp}/juno-kexec-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
fail=0
refused=0
weak=0
try() {
    local file=$1 from=$2 to=$3
    local shown="${from//$'\n'/\\n} -> ${to//$'\n'/\\n}"
    mkdir -p "$work/kernel/lucina-kexec/src" "$work/verification/juno-kexec"
    cp kernel/lucina-kexec/src/pages.rs "$work/kernel/lucina-kexec/src/"
    cp verification/juno-kexec/rules.rs "$work/verification/juno-kexec/"
    local target=$work/$file
    local text; text=$(<"$target")
    case "$text" in
        *"$from"*) ;;
        *) echo "NOT FOUND in $file: ${shown% -> *}"; fail=1; return ;;
    esac
    printf "%s\n" "${text/"$from"/"$to"}" > "$target"
    if cmp -s "$target" "$file"; then echo "NOT CHANGED in $file: ${shown% -> *}"; fail=1; return; fi
    if "$verus" "$work/verification/juno-kexec/rules.rs" > "$work/out" 2>&1; then
        echo "NOT REFUSED in $file: $shown"; fail=1
    else
        if grep -qE 'verification results|simplifies to|not satisfied|assertion failed|possible arithmetic|may not terminate|cannot show|recommendation not met' "$work/out" && ! grep -q 'error\[E' "$work/out"; then
            refused=$((refused + 1))
            echo "refused in $file: $shown ($(grep -c "^error" "$work/out") errors)"
        else
            echo "refused (NO VERIFICATION RUN: compile or other error) in $file: $shown ($(grep -c "^error" "$work/out") errors)"
            weak=$((weak+1))
        fi
    fi
}
# The unchanged files must verify, or every "refused" below would mean nothing.
if "$verus" verification/juno-kexec/rules.rs > "$work/baseline" 2>&1; then
    echo "baseline: verifies"
else
    cat "$work/baseline"; echo "mutants: the unchanged files do not verify"; exit 1
fi
lib=kernel/lucina-kexec/src/pages.rs
rules=verification/juno-kexec/rules.rs
# step tests destination before done: a list's end could read as a destination.
try "$lib" '    if entry & IND_DONE != 0 {
        Step::Done
    } else if entry & IND_DESTINATION != 0 {
        Step::Destination(addr)' '    if entry & IND_DESTINATION != 0 {
        Step::Destination(addr)
    } else if entry & IND_DONE != 0 {
        Step::Done'
# step copies from the entry itself, flag bits and all.
try "$lib" '        Step::Copy(addr)' '        Step::Copy(entry)'
# encode clears the page instead of adding the flag.
try "$lib" '    page | flag' '    page & flag'
# The source flag is the done flag: a copy would end the list.
try "$lib" 'pub const IND_SOURCE: u64 = 0x8;' 'pub const IND_SOURCE: u64 = 0x4;'
# The flag mask is one page wide: the page's own lowest bit is taken for a flag.
try "$lib" 'pub const LOW: u64 = PAGE - 1;' 'pub const LOW: u64 = PAGE;'
# The walk's destination moves two pages per copy.
try "$rules" 'Step::Copy(s) => seq![Copy { dst: dest, src: s }] + walk(es, i + 1, dest + (PAGE as int)),' 'Step::Copy(s) => seq![Copy { dst: dest, src: s }] + walk(es, i + 1, dest + 2 * (PAGE as int)),'
# The walk's destination does not move: every page lands on the first.
try "$rules" 'Step::Copy(s) => seq![Copy { dst: dest, src: s }] + walk(es, i + 1, dest + (PAGE as int)),' 'Step::Copy(s) => seq![Copy { dst: dest, src: s }] + walk(es, i + 1, dest),'
# The list puts the image after the initramfs's destination.
try "$rules" 'seq![w | IND_DESTINATION] + sources(image) + seq![d | IND_DESTINATION] + sources(initrd)' 'seq![d | IND_DESTINATION] + sources(image) + seq![w | IND_DESTINATION] + sources(initrd)'
# Staging's guarantee dropped for the list pages: a copy could overwrite the list.
try "$rules" '        forall|k: int| 0 <= k < list_pages.len() ==> !inside(#[trigger] list_pages[k] as int, w as int, wlen) && !inside(list_pages[k] as int, d as int, dlen),' '        true,'
# The window may be smaller than the image.
try "$rules" '        image.len() * (PAGE as int) <= wlen,' '        true,'
# destination checks alignment off by one: 4095 instead of the page, so an unaligned page could pass.
try "$lib" '    assert_eq!(page % PAGE, 0, "a destination is page-aligned");' '    assert_eq!(page % (PAGE - 1), 0, "a destination is page-aligned");'
# destination points one page past the page it was given.
try "$lib" '    encode(page, IND_DESTINATION)
}' '    encode(page + PAGE, IND_DESTINATION)
}'
# source marks its page as a link instead of a copy.
try "$lib" '    encode(page, IND_SOURCE)' '    encode(page, IND_INDIRECTION)'
# initrd_destination: the overlap check with the window removed.
try "$lib" '    if !(start < window.end && window.start < end)
        && clear(regions, start, end)' '    if clear(regions, start, end)'
# initrd_destination: the overlap check with reserved (false) regions removed.
try "$lib" '        if !ok && start < r.end && r.start < end {' '        if !ok && start < r.end && r.start < end && false {'
# covered: a region also holds its end address, so a range may run one byte past the RAM.
try "$lib" '        if ok && r.start <= at && at < r.end {' '        if ok && r.start <= at && at <= r.end {'
# initrd_destination keeps the highest candidate instead of the lowest.
try "$lib" '        Some(b) if b <= start => Some(b),' '        Some(b) if b >= start => Some(b),'
# Rounding up stops one short of the multiple.
try "$lib" '        x.checked_add(m - rem)' '        x.checked_add(m - rem - 1)'
# ListPage::push fills the link slot too: a full page has no room left for its link.
try "$lib" '        if self.used == ENTRIES_PER_PAGE - 1 {' '        if self.used == ENTRIES_PER_PAGE {'
# ListPage::link writes the link into the last slot whatever was pushed: a link into the middle of a page.
try "$lib" '        self.entries[self.used] = next | IND_INDIRECTION;' '        self.entries[ENTRIES_PER_PAGE - 1] = next | IND_INDIRECTION;'
# walk_entries resumes a linked page at its second slot.
try "$lib" '                at = 0;' '                at = 1;'
# walk_entries reports a copy the wrong way round.
try "$lib" '                copies.push((d, addr));' '                copies.push((addr, d));'
# walk_entries does not move the destination on after a copy.
try "$lib" '                dest = Some(d + PAGE);' '                dest = Some(d);'
# is_entry looks at the page bits instead of the flags.
try "$lib" '    entry & FLAGS != 0' '    entry & LOW != 0'
# The layout links each page to itself (wrong link index).
try "$rules" '    &&& forall|k: int| 0 <= k < pages.len() - 1 ==> #[trigger] pages[k][per_page()] == addrs[k + 1] | IND_INDIRECTION' '    &&& forall|k: int| 0 <= k < pages.len() - 1 ==> #[trigger] pages[k][per_page()] == addrs[k] | IND_INDIRECTION'
# The layout flattens each page backwards (flattening order wrong).
try "$rules" '            ==> #[trigger] pages[k][s] == es[per_page() * k + s]' '            ==> #[trigger] pages[k][s] == es[per_page() * k + (per_page() - 1 - s)]'
[ "$fail" = 0 ] && echo "all mutants refused" || echo "SOME MUTANTS SURVIVED"
echo "mutants: $refused refused by verification, $weak refused without a verification run"
if [ "${MUTANTS_STRICT:-0}" = "1" ] && [ "$weak" -gt 0 ]; then exit 1; fi
exit $fail
