#!/usr/bin/env bash
# Shows that verification/juno-surface/rules.rs can fail: each line plants one bug in a copy of the shipped file
# (services/lucina-surface-rules/src/rules.rs), and Verus must refuse every mutant by a failed check. Prints "baseline: verifies" first and
# "mutants: N refused by verification, W refused without a verification run" at the end (docs/ci/PROOF-WORKFLOW.md).
# Run on buildhost from the repo root, under verify-gate: verify-gate -- verification/juno-surface/mutants.sh
set -uo pipefail
verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
shipped=services/lucina-surface-rules/src/rules.rs
work=$(mktemp -d "${TMPDIR:-/tmp}/juno-surface-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
mkdir -p "$work/services/lucina-surface-rules/src" "$work/verification/juno-surface"
cp verification/juno-surface/rules.rs "$work/verification/juno-surface/"
target=$work/$shipped

# The unchanged file must verify, or nothing below means anything. `step` needs more solver resources than the Verus default,
# so every run here uses the same --rlimit 20 as the unit itself (tools/ci/trust-allow.tsv).
cp "$shipped" "$target"
if ! "$verus" --rlimit 20 "$work/verification/juno-surface/rules.rs" > "$work/base.out" 2>&1; then
    echo "BASE FAILS"
    cat "$work/base.out"
    exit 1
fi
echo "baseline: verifies"

fail=0
total=0
refused=0
weak=0
# A mutant counts as refused only when Verus ran and names a failed check. No verification run (a compile or other error), or a
# resource limit as the only error, is `weak`: it is not counted as refused and it fails the script.
judge() {
    local what=$1
    if "$verus" --rlimit 20 "$work/verification/juno-surface/rules.rs" > "$work/verus.out" 2>&1; then
        echo "NOT REFUSED: $what"
        fail=1
    elif grep -q 'verification results' "$work/verus.out" \
        && grep -qE "^error: (postcondition not satisfied|precondition not satisfied|requires not satisfied|invariant not satisfied|assertion failed|possible arithmetic underflow/overflow)" "$work/verus.out"; then
        refused=$((refused + 1))
        echo "refused: $what :: $(grep -m1 -E "^error: (postcondition|precondition|requires|invariant|assertion|possible arithmetic)" "$work/verus.out")"
    elif ! grep -q 'verification results' "$work/verus.out"; then
        weak=$((weak + 1))
        echo "refused (NO VERIFICATION RUN: compile or other error): $what ($(grep -c "^error" "$work/verus.out") errors)"
        fail=1
    else
        weak=$((weak + 1))
        echo "REFUSED ONLY BY THE RESOURCE LIMIT OR SCAFFOLDING: $what :: $(grep -m1 "^error" "$work/verus.out")"
        fail=1
    fi
}

try() {
    local from=$1 to=$2
    total=$((total + 1))
    local text; text=$(<"$shipped")
    case "$text" in
        *"$from"*) ;;
        *) echo "NOT FOUND: $from"; fail=1; return ;;
    esac
    local mut; mut=${text/"$from"/"$to"}
    if [ "$mut" = "$text" ]; then echo "NOT CHANGED: $from"; fail=1; return; fi
    printf "%s\n" "$mut" > "$target"
    judge "$from -> $to"
}

# Two replacements in one mutant (the first match of each): a statement and the code changed together, so that only a
# clause stated elsewhere (an invariant in `wf`, an ensures) can refuse it.
try_both() {
    local from1=$1 to1=$2 from2=$3 to2=$4
    total=$((total + 1))
    local text; text=$(<"$shipped")
    case "$text" in
        *"$from1"*) ;;
        *) echo "NOT FOUND: $from1"; fail=1; return ;;
    esac
    local mut; mut=${text/"$from1"/"$to1"}
    case "$mut" in
        *"$from2"*) ;;
        *) echo "NOT FOUND: $from2"; fail=1; return ;;
    esac
    mut=${mut/"$from2"/"$to2"}
    if [ "$mut" = "$text" ]; then echo "NOT CHANGED: $from1 / $from2"; fail=1; return; fi
    printf "%s\n" "$mut" > "$target"
    judge "$from1 -> $to1 AND $from2 -> $to2"
}

# Free: the last reference and the last batch, nothing else.
try "    if b.refs != 0 || b.gpu_pending != 0 {" "    if b.refs > 1 || b.gpu_pending != 0 {"
try "    if b.refs != 0 || b.gpu_pending != 0 {" "    if b.refs != 0 {"
try "            if b.gpu_failed {" "            if false {"
try "                        n.phase = Phase::Freed;
                        n.charged = false;" "                        n.phase = Phase::Freed;"
# Bytes are returned once: a purged buffer's are already back; a quarantined buffer's never are.
try "            n.phase = Phase::Freed;
            Ok((n, bud))" "            n.phase = Phase::Freed;
            Ok((n, Budget { used: bud.used - b.size, limit: bud.limit }))"
try "                n.phase = Phase::Quarantined;
                Ok((n, bud))" "                n.phase = Phase::Quarantined;
                n.charged = false;
                Ok((n, Budget { used: bud.used - b.size, limit: bud.limit }))"
try "                    n.phase = Phase::Purged;
                    n.charged = false;
                    Ok((n, nbud))" "                    n.phase = Phase::Purged;
                    n.charged = false;
                    Ok((n, bud))"
# Purge: only purgeable, unmapped, in no running batch, from Live.
try "            if !b.purgeable || b.mappings != 0 ||" "            if b.mappings != 0 ||"
try "|| b.mappings != 0 || b.gpu_slots_in_batch != 0" "|| b.gpu_slots_in_batch != 0"
# (Dropping the gpu_slots_in_batch test alone is not planted: wf says slot uses in a batch imply a pending batch, so the
# gpu_pending test already covers it and the two behave the same.)
try "|| b.gpu_slots_in_batch != 0 || b.gpu_pending != 0 {" "|| b.gpu_slots_in_batch != 0 {"
try "                Phase::Live => {}
                _ => return Err(Refusal::PurgeDenied)," "                _ => {}"
# A purged buffer is never mapped, bound or named by a batch.
try "            if let Phase::Purged = b.phase {
                return Err(Refusal::Purged);
            }
            if b.refs == u32::MAX {
                return Err(Refusal::Overflow);
            }
            n.mappings" "            if b.refs == u32::MAX {
                return Err(Refusal::Overflow);
            }
            n.mappings"
try "            if let Phase::Purged = b.phase {
                return Err(Refusal::Purged);
            }
            if b.refs == u32::MAX {
                return Err(Refusal::Overflow);
            }
            n.gpu_slots" "            if b.refs == u32::MAX {
                return Err(Refusal::Overflow);
            }
            n.gpu_slots"
try "            if let Phase::Purged = b.phase {
                return Err(Refusal::Purged);
            }
            if b.gpu_pending == u32::MAX" "            if b.gpu_pending == u32::MAX"
# Restore: charged again, live again, contents lost.
try "let nbud = match charge(bud, b.size) {" "let nbud = match Ok::<Budget, NoRoom>(bud) {"
try "                    n.phase = Phase::Live;
                    n.charged = true;" "                    n.charged = true;"
try "                    n.contents_lost = true;" "                    n.contents_lost = false;"
# References: every op moves refs with its own counter; dead buffers take nothing.
try "            n.refs = b.refs - 1;
            settle(n, bud)" "            settle(n, bud)"
try "            n.mappings = b.mappings - 1;
            n.refs = b.refs - 1;" "            n.mappings = b.mappings - 1;"
try "            if b.fds == 0 {" "            if b.refs == 0 {"
try "        Phase::Freed | Phase::Quarantined => return Err(Refusal::Dead)," "        Phase::Freed | Phase::Quarantined => {}"
try "            if b.refs == u32::MAX {" "            if false {"
# Batches: a retire never leaves slot uses named by no batch, nor takes more than were named.
try " || (b.gpu_slots_in_batch - slots > 0 && b.gpu_pending - 1 == 0)" ""
try "            if slots > b.gpu_slots_in_batch
                ||" "            if"
# Budget: checked arithmetic, exact boundary.
try "    if size > bud.limit - bud.used {" "    if false {"
try "    if size > bud.limit - bud.used {" "    if size >= bud.limit - bud.used {"
try "    if size > bud.used {" "    if false {"
# Sizes: pitch a multiple of 64 at least width * 4; page-aligned bytes at least pitch * height; limits.
try "let p = (bytes + 63) / 64 * 64;" "let p = (bytes + 59) / 64 * 64;"
try "let p = (bytes + 63) / 64 * 64;" "let p = (bytes + 64) / 64 * 64;"
try "    if width == 0 || width >= MAX_DIM {" "    if width == 0 || width > MAX_DIM {"
try "let b = (raw + 4095) / 4096 * 4096;" "let b = (raw + 4094) / 4096 * 4096;"
try "pitch > MAX_PITCH ||" "pitch > MAX_PITCH + 1 ||"
try "height >= MAX_DIM {" "height > MAX_DIM {"
# Sync rows: non-empty, inside the buffer, no overflow of the count.
try "        if h == 0 || y >= height || h > height - y {" "        if y >= height || h > height - y {"
try "        if h == 0 || y >= height || h > height - y {" "        if h == 0 || h > height - y {"
try "        if h == 0 || y >= height || h > height - y {" "        if h == 0 || y >= height || h > height - y + 1 {"
try "        if h as u64 > u64::MAX - total {" "        if false {"

# Review X1-X12 (requirement surface-rules-R5): counters moved to the wrong field, an op that refuses or accepts
# everything, a flag that is not set or set wrongly. Each must be refused by the statements.
try "            n.mappings = b.mappings + 1;" "            n.fds = b.fds + 1;" # X1 Map counts an fd
try "            if b.fds == 0 {
                return Err(Refusal::NoDescriptor);" "            if true {
                return Err(Refusal::NoDescriptor);" # X2 Close always refused
try "            n.gpu_failed = true;" "            n.gpu_failed = b.gpu_failed;" # X3 GpuFail a no-op
try "            n.fds = b.fds - 1;" "            n.fds = b.fds - 1;
            n.gpu_failed = false;" # X4 Close clears the GPU failure
try "        total = total + h as u64;" "        total = total + 1;" # X5 rows counted wrong
try "        if h == 0 || y >= height || h > height - y {" "        if true {" # X6 rows_ok always None
try "    let b = Buf {
        refs: 1,
        fds: 1,
        mappings: 0," "    let b = Buf {
        refs: 1,
        fds: 0,
        mappings: 1," # X7 new counts a mapping, not a descriptor
try "            if b.mappings == 0 {
                return Err(Refusal::NotMapped);" "            if true {
                return Err(Refusal::NotMapped);" # X8 Unmap always refused
try "    if size == 0 {
        return Err(Refusal::BadSize);" "    if true {
        return Err(Refusal::BadSize);" # X9 new always refused
try "                    n.purgeable = false;
                    n.contents_lost = true;" "                    n.contents_lost = true;" # X10 restore keeps it purgeable
# Purge: the GPU must not be able to reach the bytes any more (R-5).
try "            if b.gpu_slots != 0 || b.gpu_failed {" "            if b.gpu_failed {" # X11 bound slots ignored
try "            if b.gpu_slots != 0 || b.gpu_failed {" "            if b.gpu_slots != 0 {" # X12 a failed GPU ignored

# Spec side of R-5 (review F-7): the statement `accepts` loses a Purge condition the code still checks.
try "            && b.gpu_slots == 0 && !b.gpu_failed," "            ," # S1 accepts drops both R-5 conjuncts
try "            && b.gpu_slots == 0 && !b.gpu_failed," "            && !b.gpu_failed," # S2 accepts drops gpu_slots == 0
try "            && b.gpu_slots == 0 && !b.gpu_failed," "            && b.gpu_slots == 0," # S3 accepts drops !gpu_failed
# Statement and code weakened together (review F-1): only the R-5 clauses of `wf` and of `step` refuse these.
try_both "            && b.gpu_slots == 0 && !b.gpu_failed," "            && !b.gpu_failed," \
    "            if b.gpu_slots != 0 || b.gpu_failed {" "            if b.gpu_failed {" # C1 purge with a slot bound
try_both "            && b.gpu_slots == 0 && !b.gpu_failed," "            && b.gpu_slots == 0," \
    "            if b.gpu_slots != 0 || b.gpu_failed {" "            if b.gpu_slots != 0 {" # C2 purge on a failed GPU
try_both "        Op::GpuBind => !(b.phase is Purged) && b.refs < u32::MAX," "        Op::GpuBind => b.refs < u32::MAX," \
    "        Op::GpuBind => {
            if let Phase::Purged = b.phase {
                return Err(Refusal::Purged);
            }" "        Op::GpuBind => {" # C3 bind a slot to a purged buffer

echo "refused $refused/$total"
echo "mutants: $refused refused by verification, $weak refused without a verification run"
exit "$fail"
