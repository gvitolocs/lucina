#!/usr/bin/env bash
# Shows that verification/juno-taskcpu/rules.rs can fail: each line plants one bug in a copy of the shipped file
# (or of the proof file, for the model's steps), and Verus must refuse it.
# Run on buildhost from the repo root, under verify-gate.
set -uo pipefail
verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
work=$(mktemp -d "${TMPDIR:-/tmp}/juno-taskcpu-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
fail=0
try() {
    local file=$1 from=$2 to=$3
    # Shown on one line: a planted bug may span lines.
    local shown="${from//$'\n'/\\n} -> ${to//$'\n'/\\n}"
    mkdir -p "$work/kernel/lucina-taskcpu/src" "$work/verification/juno-taskcpu"
    cp kernel/lucina-taskcpu/src/lib.rs "$work/kernel/lucina-taskcpu/src/"
    cp verification/juno-taskcpu/rules.rs "$work/verification/juno-taskcpu/"
    local target=$work/$file
    local text; text=$(<"$target")
    case "$text" in
        *"$from"*) ;;
        *) echo "NOT FOUND in $file: ${shown% -> *}"; fail=1; return ;;
    esac
    printf "%s\n" "${text/"$from"/"$to"}" > "$target"
    if cmp -s "$target" "$file"; then echo "NOT CHANGED in $file: ${shown% -> *}"; fail=1; return; fi
    if "$verus" "$work/verification/juno-taskcpu/rules.rs" > "$work/out" 2>&1; then
        echo "NOT REFUSED in $file: $shown"; fail=1
    else
        echo "refused in $file: $shown ($(grep -c "^error" "$work/out") errors)"
    fi
}
lib=kernel/lucina-taskcpu/src/lib.rs
rules=verification/juno-taskcpu/rules.rs
# decode maps DEAD to On.
try "$lib" '        Slot::Dead' '        Slot::On(raw)'
# decode maps DEAD to Free.
try "$lib" '        Slot::Dead' '        Slot::Free'
# after_failed_claim rechecks even a DEAD slot instead of refusing.
try "$lib" '        Wake::Refuse' '        Wake::Recheck(prev)'
# DEAD is NONE: exit's store is indistinguishable from "on no CPU".
try "$lib" 'pub const DEAD: u32 = u32::MAX - 1;' 'pub const DEAD: u32 = u32::MAX;'
# NONE moves off u32::MAX.
try "$lib" 'pub const NONE: u32 = u32::MAX;' 'pub const NONE: u32 = u32::MAX - 2;'
# encode_on returns DEAD, so a claim lands on the dead value.
try "$lib" 'pub fn encode_on(cpu: u32) -> u32 { cpu }' 'pub fn encode_on(cpu: u32) -> u32 { DEAD }'
# The model's exit stores NONE (the old bug).
try "$rules" '    m.current = None;
    m.slot = DEAD;
    m.exited = true;' '    m.current = None;
    m.slot = NONE;
    m.exited = true;'
# The model's recheck under the run-queue lock is skipped: enqueue whatever the slot says.
try "$rules" '        WakerPc::Recheck(p) => {
            if m.slot == NONE {' '        WakerPc::Recheck(p) => {
            if true {'
# The model's claim CAS succeeds on a DEAD slot too.
try "$rules" '    if m.slot == NONE {
        let raw = encode_on(sel);' '    if m.slot == NONE || m.slot == DEAD {
        let raw = encode_on(sel);'
exit $fail
