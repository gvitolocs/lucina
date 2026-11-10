#!/usr/bin/env bash
# Shows that verification/juno-gpu-faults/rules.rs can fail: each line plants one bug in a copy of the shipped
# drivers/lucina-adreno/src/fault_log.rs, and Verus must refuse it. Run on buildhost from the repo root (or through verus-remote).
set -uo pipefail
verus=${VERUS:-$(command -v verus || echo /home/user/tools/verus/verus-x86-linux/verus)}
src=drivers/lucina-adreno/src/fault_log.rs
work=$(mktemp -d "${TMPDIR:-/tmp}/juno-gpu-faults-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
fail=0
try() {
    local from=$1 to=$2
    mkdir -p "$work/drivers/lucina-adreno/src" "$work/verification/juno-gpu-faults"
    cp "$src" "$work/$src"
    cp verification/juno-gpu-faults/rules.rs "$work/verification/juno-gpu-faults/"
    local target=$work/$src
    local text; text=$(<"$target")
    case "$text" in
        *"$from"*) ;;
        *) echo "NOT FOUND: $from"; fail=1; return ;;
    esac
    printf "%s\n" "${text/"$from"/"$to"}" > "$target"
    if cmp -s "$target" "$src"; then echo "NOT CHANGED: $from"; fail=1; return; fi
    if "$verus" "$work/verification/juno-gpu-faults/rules.rs" > "$work/out" 2>&1; then
        echo "NOT REFUSED: $from -> $to"; fail=1
    else
        echo "refused: $from -> $to ($(grep -c "^error" "$work/out") errors)"
    fi
}
# A new record over the oldest, or past the array.
try "let slot = (r.head + r.len) % CAPACITY;" "let slot = r.head % CAPACITY;"
try "let slot = (r.head + r.len) % CAPACITY;" "let slot = r.head + r.len;"
# Writing into a full ring, or dropping without counting.
try "    if r.len < CAPACITY {
        let slot" "    if r.len <= CAPACITY {
        let slot"
try "dropped: r.dropped.saturating_add(1) }, None)" "dropped: r.dropped }, None)"
# Taking out of order, or forgetting to move the head.
try "(Ring { head: (r.head + 1) % CAPACITY, len: r.len - 1, dropped: r.dropped }, Some(r.head))" "(Ring { head: r.head, len: r.len - 1, dropped: r.dropped }, Some(r.head))"
try "(Ring { head: (r.head + 1) % CAPACITY, len: r.len - 1, dropped: r.dropped }, Some(r.head))" "(Ring { head: (r.head + 1) % CAPACITY, len: r.len - 1, dropped: r.dropped }, Some((r.head + 1) % CAPACITY))"
# Taking from an empty ring.
try "    if r.len == 0 {
        return (r, None);" "    if r.len == 0 {
        return (r, Some(0));"
# Blaming with the ASID left in.
try "    ttbr0 % 0x1_0000_0000_0000
}" "    ttbr0
}"
exit $fail
