#!/usr/bin/env bash
# Shows that verification/juno-netlink/rules.rs can fail: each line plants one bug in a copy of the shipped file, and Verus must refuse it.
# Run on buildhost from the repo root, under verify-gate.
set -uo pipefail
verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
work=$(mktemp -d "${TMPDIR:-/tmp}/juno-netlink-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
fail=0
refused=0
weak=0
try() {
    local from=$1 to=$2
    mkdir -p "$work/kernel/lucina-netlink/src" "$work/verification/juno-netlink"
    cp kernel/lucina-netlink/src/walk.rs "$work/kernel/lucina-netlink/src/"
    cp verification/juno-netlink/rules.rs "$work/verification/juno-netlink/"
    local target=$work/kernel/lucina-netlink/src/walk.rs
    local text; text=$(<"$target")
    case "$text" in
        *"$from"*) ;;
        *) echo "NOT FOUND: $from"; fail=1; return ;;
    esac
    printf "%s\n" "${text/"$from"/"$to"}" > "$target"
    if cmp -s "$target" kernel/lucina-netlink/src/walk.rs; then echo "NOT CHANGED: $from"; fail=1; return; fi
    if "$verus" "$work/verification/juno-netlink/rules.rs" > "$work/out" 2>&1; then
        echo "NOT REFUSED: $from -> $to"; fail=1
    else
        if grep -qE 'verification results|simplifies to|not satisfied|assertion failed|possible arithmetic|may not terminate|cannot show|recommendation not met' "$work/out" && ! grep -q 'error\[E' "$work/out"; then
            refused=$((refused + 1))
            echo "refused: $from -> $to ($(grep -c '^error' "$work/out") errors)"
        else
            echo "refused (NO VERIFICATION RUN: compile or other error): $from -> $to ($(grep -c '^error' "$work/out") errors)"
            weak=$((weak+1))
        fi
    fi
}
# The unchanged files must verify, or every "refused" below would mean nothing.
if "$verus" verification/juno-netlink/rules.rs > "$work/baseline" 2>&1; then
    echo "baseline: verifies"
else
    cat "$work/baseline"; echo "mutants: the unchanged files do not verify"; exit 1
fi
try 'if n > usize::MAX - 3 {' 'if n > usize::MAX - 2 {'
try 'Some(up - up % 4)' 'Some(up - up % 4 + 4)'
try '((buf[pos + 3] as u32) << 24u32)' '((buf[pos + 4] as u32) << 24u32)'
try 'if pos > len || len - pos < 16 {' 'if len - pos < 16 {'
try 'if n < 16 || n > len - pos {' 'if n < 15 || n > len - pos {'
try 'if n < 16 || n > len - pos {' 'if n < 16 || n > len - pos + 1 {'
try 'if pos > end || end - pos < 4 {' 'if pos > end || end - pos < 1 {'
try 'if n < 4 || n > end - pos {' 'if n < 3 || n > end - pos {'
try 'if n < 4 || n > end - pos {' 'if n < 4 || n > end - pos + 1 {'
[ "$fail" = 0 ] && echo "mutants: every planted bug was refused" || echo "mutants: FAILED"
echo "mutants: $refused refused by verification, $weak refused without a verification run"
if [ "${MUTANTS_STRICT:-0}" = "1" ] && [ "$weak" -gt 0 ]; then exit 1; fi
