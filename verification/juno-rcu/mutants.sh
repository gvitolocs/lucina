#!/usr/bin/env bash
# Shows that verification/juno-rcu/rules.rs can fail: each line plants one bug in a copy of the shipped file, and Verus must refuse it.
# Run on buildhost from the repo root, under verify-gate.
set -uo pipefail
verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
work=$(mktemp -d "${TMPDIR:-/tmp}/juno-rcu-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
fail=0
refused=0
weak=0
try() {
    local from=$1 to=$2
    mkdir -p "$work/kernel/lucina-rcu/src" "$work/verification/juno-rcu"
    cp kernel/lucina-rcu/src/lib.rs "$work/kernel/lucina-rcu/src/"
    cp verification/juno-rcu/rules.rs "$work/verification/juno-rcu/"
    local target=$work/kernel/lucina-rcu/src/lib.rs
    local text; text=$(<"$target")
    case "$text" in
        *"$from"*) ;;
        *) echo "NOT FOUND: $from"; fail=1; return ;;
    esac
    printf "%s\n" "${text/"$from"/"$to"}" > "$target"
    if cmp -s "$target" kernel/lucina-rcu/src/lib.rs; then echo "NOT CHANGED: $from"; fail=1; return; fi
    if "$verus" "$work/verification/juno-rcu/rules.rs" > "$work/out" 2>&1; then
        echo "NOT REFUSED: $from -> $to"; fail=1
    else
        if grep -qE 'verification results|simplifies to|not satisfied|assertion failed|possible arithmetic|may not terminate|cannot show|recommendation not met' "$work/out" && ! grep -q 'error\[E' "$work/out"; then
            refused=$((refused + 1))
            echo "refused: $from -> $to ($(grep -c "^error" "$work/out") errors)"
        else
            echo "refused (NO VERIFICATION RUN: compile or other error): $from -> $to ($(grep -c "^error" "$work/out") errors)"
            weak=$((weak+1))
        fi
    fi
}
# The unchanged files must verify, or every "refused" below would mean nothing.
if "$verus" verification/juno-rcu/rules.rs > "$work/baseline" 2>&1; then
    echo "baseline: verifies"
else
    cat "$work/baseline"; echo "mutants: the unchanged files do not verify"; exit 1
fi
# A CPU counts once, for itself only.
try 'self.passed |= 1u64 << cpu;' 'self.passed |= 3u64 << cpu;'
# A CPU outside `all` counts for nothing.
try 'if cpu >= 64 || all & (1u64 << cpu) == 0 {' 'if cpu >= 64 {'
# A new grace period starts with nobody passed.
try '        self.passed = 0;
        self.complete = false;' '        self.complete = false;'
# The items come back only when every CPU in `all` passed.
try 'if self.passed & all != all {' 'if self.passed & all == 0 {'
# What comes back is the running grace period's items, not the waiting ones.
try 'core::mem::swap(&mut self.ready, &mut self.current);' 'core::mem::swap(&mut self.ready, &mut self.next);'
# The waiting items start the next grace period, they are not dropped or stranded.
try 'core::mem::swap(&mut self.current, &mut self.next);' 'self.next = Queue::new();'
try 'if !self.next.is_empty() {' 'if false {'
# A retire when nothing runs starts a grace period, and the monitor watches it.
try '        if !self.complete {
            return;
        }
        self.restart();' '        if true {
            return;
        }
        self.restart();'
try '        self.restart();
        self.monitoring = true;' '        self.restart();'
# A grace period that ended is marked complete.
try '        self.complete = true;
        core::mem::swap(&mut self.ready' '        core::mem::swap(&mut self.ready'
# The queue (patch 1059). A chunk is moved to the list when it is full, not before or after.
try 'if self.fill.len() == CHUNK {' 'if self.fill.len() == 1 {'
try 'if self.fill.len() == CHUNK {' 'if self.fill.len() == CHUNK + 1 {'
# Pushing a full chunk keeps the older ones linked behind it.
try 'self.full = Some(Box::new(Chunk { items, older }));' 'self.full = Some(Box::new(Chunk { items, older: None }));'
# Taking the newest chunk keeps the older ones.
try '                self.full = older;' '                self.full = None;'
# The chunk being filled is the newest: it is taken before any full chunk.
try 'if self.fill.len() > 0 {' 'if self.fill.len() > 1 {'
# A queue with only full chunks is not empty.
try 'self.full.is_none() && self.fill.len() == 0' 'self.fill.len() == 0'
# The ready items (patch 1112). A grace period ends only once the ready items before it are out, or they would be lost.
try '        if !self.ready.is_empty() {
            return;
        }
        self.complete = true;' '        self.complete = true;'
# With no grace period left, the monitor watches as long as items are ready.
try 'self.monitoring = !self.ready.is_empty();' 'self.monitoring = false;'
try 'if self.complete && self.ready.is_empty() {' 'if self.complete {'
# What comes out is what was ready, not the running grace period's items.
try 'let out = self.ready.take_last();' 'let out = self.current.take_last();'
# The budget (patch 1112): the first chunk always, never past the count, never past the time.
try '    if taken == 0 {
        return true;
    }' '    if taken == 0 {
        return false;
    }'
try 'taken < budget.chunks && elapsed_micros < budget.micros' 'taken <= budget.chunks && elapsed_micros < budget.micros'
try 'taken < budget.chunks && elapsed_micros < budget.micros' 'taken < budget.chunks'
# The flood budget (patch 1181): a flood lifts the count and gets the flood time; never below the base; below the mark
# the base's time and the larger count.
try '    if pending > QHIMARK {' '    if pending > QHIMARK + 1 {'
try '        return Budget { chunks: u32::MAX, micros };' '        return Budget { chunks: u32::MAX - 1, micros };'
try 'let micros = if base.micros > FLOOD_MICROS { base.micros } else { FLOOD_MICROS };' 'let micros = FLOOD_MICROS;'
try 'let chunks = if (base.chunks as u64) >= share { base.chunks } else { share as u32 };' 'let chunks = share as u32;'
try '    Budget { chunks, micros: base.micros }' '    Budget { chunks, micros: FLOOD_MICROS }'
try 'let share = (pending / 128 + 127) / 128;' 'let share = (pending / 128 + 128) / 128;'
echo "mutants: $refused refused by verification, $weak refused without a verification run"
if [ "${MUTANTS_STRICT:-0}" = "1" ] && [ "$weak" -gt 0 ]; then exit 1; fi
exit $fail
