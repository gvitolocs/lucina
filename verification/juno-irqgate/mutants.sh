#!/usr/bin/env bash
# Shows that verification/juno-irqgate/rules.rs can fail: each line plants one bug in a copy of the shipped file, and Verus must refuse it.
# Run on buildhost from the repo root, under verify-gate (or on buildhost2 with verus-remote).
set -uo pipefail
verus=${VERUS:-verus}
[ -x /home/user/tools/verus/verus-x86-linux/verus ] && verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
work=$(mktemp -d "${TMPDIR:-/tmp}/juno-irqgate-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
fail=0 refused=0 total=0
try() {
    local from=$1 to=$2
    local shown="${from//$'\n'/\\n} -> ${to//$'\n'/\\n}"
    total=$((total + 1))
    mkdir -p "$work/kernel/lucina-irqgate/src" "$work/verification/juno-irqgate"
    cp kernel/lucina-irqgate/src/lib.rs "$work/kernel/lucina-irqgate/src/"
    cp verification/juno-irqgate/rules.rs "$work/verification/juno-irqgate/"
    local target=$work/kernel/lucina-irqgate/src/lib.rs
    local text; text=$(<"$target")
    case "$text" in
        *"$from"*) ;;
        *) echo "NOT FOUND: ${shown% -> *}"; fail=1; return ;;
    esac
    printf "%s\n" "${text/"$from"/"$to"}" > "$target"
    if cmp -s "$target" kernel/lucina-irqgate/src/lib.rs; then echo "NOT CHANGED: ${shown% -> *}"; fail=1; return; fi
    if "$verus" "$work/verification/juno-irqgate/rules.rs" > "$work/out" 2>&1; then
        echo "NOT REFUSED: $shown"; fail=1
    else
        refused=$((refused + 1))
        echo "refused: $shown ($(grep -c "^error" "$work/out") errors)"
    fi
}
# The handler leaves a level line enabled: the storm of juno-charging's 1131.
try 'Fired { gate: Gate { armed: false, pending }, line: Line::Disable' 'Fired { gate: Gate { armed: false, pending }, line: Line::Keep'
# The handler leaves the gate armed with something pending.
try 'Fired { gate: Gate { armed: false, pending }' 'Fired { gate: Gate { armed: g.armed, pending }'
# The handler drops what an earlier interrupt left pending.
try 'let pending = held | status;' 'let pending = status;'
# The handler does not wake the thread.
try 'wake: pending != 0 }' 'wake: false }'
# A status of 0 (not this device's) masks the line of a sleeping thread.
try 'if status == 0 {' 'if status == 1 {'
# The thread enables the line with an event already pending.
try 'if g.pending != 0 {' 'if g.pending == u32::MAX {'
# The thread drops what was pending when it armed.
try 'take: g.pending, sleep: false' 'take: 0, sleep: false'
# The thread sleeps without enabling the line.
try 'line: Line::Enable, take: 0, sleep: true' 'line: Line::Keep, take: 0, sleep: true'
# A timeout leaves the line enabled while nobody waits.
try 'line: if g.armed { Line::Disable } else { Line::Keep }' 'line: Line::Keep'
# A take leaves the gate armed.
try 'Taken { gate: Gate { armed: false, pending: 0 }' 'Taken { gate: Gate { armed: g.armed, pending: 0 }'
# The take drops the bits.
try 'take: g.pending }' 'take: 0 }'
# The thread wakes on the wrong condition.
try 'pub fn woken(g: Gate) -> bool {
    g.pending != 0' 'pub fn woken(g: Gate) -> bool {
    g.armed'
echo "juno-irqgate mutants: $refused of $total refused"
exit $fail
