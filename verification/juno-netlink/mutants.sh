#!/usr/bin/env bash
# Shows that verification/juno-netlink/rules.rs can fail: each line plants one bug in a copy of the shipped file, and Verus must refuse it.
# Run on buildhost from the repo root, under verify-gate.
set -uo pipefail
verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
work=$(mktemp -d "${TMPDIR:-/tmp}/juno-netlink-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
fail=0
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
        echo "refused: $from -> $to ($(grep -c '^error' "$work/out") errors)"
    fi
}
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
