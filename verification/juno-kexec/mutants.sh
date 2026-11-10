#!/usr/bin/env bash
# Shows that verification/juno-kexec/rules.rs can fail: each line plants one bug in a copy of the shipped file (or of the
# proof file, for the model of the walk), and Verus must refuse it. Run on buildhost from the repo root, under verify-gate.
set -uo pipefail
verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
work=$(mktemp -d "${TMPDIR:-/tmp}/juno-kexec-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
fail=0
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
        echo "refused in $file: $shown ($(grep -c "^error" "$work/out") errors)"
    fi
}
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
[ "$fail" = 0 ] && echo "all mutants refused" || echo "SOME MUTANTS SURVIVED"
exit $fail
