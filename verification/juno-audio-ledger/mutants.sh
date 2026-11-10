#!/usr/bin/env bash
# Shows that verification/juno-audio-ledger/rules.rs can fail: each line plants one bug in a copy of the shipped file, and Verus
# must refuse it. Run on buildhost from the repo root, under verify-gate.
set -uo pipefail
verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
work=$(mktemp -d "${TMPDIR:-/tmp}/juno-audio-ledger-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
fail=0
refused=0
total=0
try() {
    local from=$1 to=$2
    local shown="${from//$'\n'/\\n} -> ${to//$'\n'/\\n}"
    total=$((total + 1))
    mkdir -p "$work/drivers/lucina-audio-ledger/src" "$work/verification/juno-audio-ledger"
    cp drivers/lucina-audio-ledger/src/lib.rs "$work/drivers/lucina-audio-ledger/src/"
    cp verification/juno-audio-ledger/rules.rs "$work/verification/juno-audio-ledger/"
    local target=$work/drivers/lucina-audio-ledger/src/lib.rs
    local text; text=$(<"$target")
    case "$text" in
        *"$from"*) ;;
        *) echo "NOT FOUND: ${shown% -> *}"; fail=1; return ;;
    esac
    printf "%s\n" "${text/"$from"/"$to"}" > "$target"
    if cmp -s "$target" drivers/lucina-audio-ledger/src/lib.rs; then echo "NOT CHANGED: ${shown% -> *}"; fail=1; return; fi
    if "$verus" "$work/verification/juno-audio-ledger/rules.rs" > "$work/out" 2>&1; then
        echo "NOT REFUSED: $shown"; fail=1
    else
        refused=$((refused + 1))
        echo "refused: $shown ($(grep -c "^error" "$work/out") errors)"
    fi
}
# The next slot wraps at the ring's end, not one past it.
try 'if s >= periods { s - periods } else { s }' 'if s > periods { s - periods } else { s }'
# The next slot is the one after the last out, not one further.
try 'let s = l.oldest + l.count;' 'let s = l.oldest + l.count + 1;'
# Only the oldest slot is taken back, not any slot.
try 'if l.count == 0 || slot != l.oldest {' 'if l.count == 0 || slot >= periods {'
# Nothing is taken back when nothing is out.
try 'if l.count == 0 || slot != l.oldest {' 'if slot != l.oldest {'
# A slot taken back loses its data bit.
try '    let data = l.data & !(1u32 << slot);
    let oldest' '    let data = l.data;
    let oldest'
# The oldest wraps at the ring's end.
try 'let oldest = if l.oldest + 1 >= periods { 0 } else { l.oldest + 1 };' 'let oldest = if l.oldest + 1 > periods { 0 } else { l.oldest + 1 };'
# The answer says whether the slot held samples.
try 'let had = (l.data >> slot) & 1 == 1;' 'let had = true;'
# A queued period's data bit follows whether it holds samples.
try 'let data = if has_data { l.data | (1u32 << slot) }' 'let data = if true { l.data | (1u32 << slot) }'
# The count goes up by one per hand-out.
try 'Ledger { oldest: l.oldest, count: l.count + 1, data }' 'Ledger { oldest: l.oldest, count: l.count, data }'
# Samples are out while any data bit is set, not while anything is out.
try '    l.data != 0
}' '    l.count != 0
}'
# At most `limit` out.
try '    l.count < limit
}' '    l.count <= limit
}'
# A period of samples is never an underrun, nor one after the program finished.
try 'bytes == 0 && had_data && !tone && !source_finished
}' 'bytes == 0 && had_data && !tone
}'
# The microphone drops exactly what does not fit.
try 'if total > cap { (cap, total - cap) } else { (total, 0) }' 'if total > cap { (cap - 1, total - cap + 1) } else { (total, 0) }'
try 'if total > cap { (cap, total - cap) } else { (total, 0) }' 'if total > cap { (cap, 0) } else { (total, 0) }'
# Reverse undo starts at the last step taken, not one past it.
try 'Undo::Reverse => taken - 1 - i,' 'Undo::Reverse => taken - i,'
# Windows' order is the order taken, not reversed.
try 'Undo::AsTaken => i,' 'Undo::AsTaken => taken - 1 - i,'
# One data bit per slot: more slots than bits overflow the shift.
try 'pub const MAX_PERIODS: u32 = 32;' 'pub const MAX_PERIODS: u32 = 33;'
# A slot is out only when it is among the `count` after the oldest.
try 's < p && dist(l.oldest, s, p) < l.count as int' 's < p && dist(l.oldest, s, p) <= l.count as int'
echo "$refused of $total mutants refused"
exit $fail
