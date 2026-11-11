#!/usr/bin/env bash
# Shows that verification/juno-cpufreq/rules.rs can fail: each line plants one bug in a copy of the shipped file, and Verus must refuse it.
# Run from the repo root, under verify-gate.
set -uo pipefail
verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
work=$(mktemp -d "${TMPDIR:-/tmp}/juno-cpufreq-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
fail=0
refused=0
weak=0
try() {
    local from=$1 to=$2
    mkdir -p "$work/kernel/lucina-cpufreq/src" "$work/verification/juno-cpufreq"
    cp kernel/lucina-cpufreq/src/lib.rs "$work/kernel/lucina-cpufreq/src/"
    cp kernel/lucina-cpufreq/src/rules.rs "$work/kernel/lucina-cpufreq/src/"
    cp verification/juno-cpufreq/rules.rs "$work/verification/juno-cpufreq/"
    local target=$work/kernel/lucina-cpufreq/src/rules.rs
    local text; text=$(<"$target")
    case "$text" in
        *"$from"*) ;;
        *) echo "NOT FOUND: $from"; fail=1; return ;;
    esac
    printf "%s\n" "${text/"$from"/"$to"}" > "$target"
    if cmp -s "$target" kernel/lucina-cpufreq/src/rules.rs; then echo "NOT CHANGED: $from"; fail=1; return; fi
    if "$verus" "$work/verification/juno-cpufreq/rules.rs" > "$work/out" 2>&1; then
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
if "$verus" verification/juno-cpufreq/rules.rs > "$work/baseline" 2>&1; then
    echo "baseline: verifies"
else
    cat "$work/baseline"; echo "mutants: the unchanged files do not verify"; exit 1
fi
# The row the cap allows, not the one above it.
try 'let top = cap.min(steps.len().saturating_sub(1));' 'let top = (cap + 1).min(steps.len().saturating_sub(1));'
# The first row that reaches the wanted clock, not the first one past it.
try 'while i < top && steps[i].khz < khz {' 'while i < top && steps[i].khz <= khz {'
# One step at a time, not two.
try '        i += 1;' '        i += 2;'
# 25% above what is used, not none.
try '(used * 5 / 4).min(u64::from(max_khz)) as u32' '(used * 4 / 4).min(u64::from(max_khz)) as u32'
# Two busy ticks in a row is the top clock, not three.
try '    if load.busy_run >= BUSY_RUN_FOR_MAX {' '    if load.busy_run > BUSY_RUN_FOR_MAX {'
echo "mutants: $refused refused by verification, $weak refused without a verification run"
if [ "${MUTANTS_STRICT:-0}" = "1" ] && [ "$weak" -gt 0 ]; then exit 1; fi
exit $fail
