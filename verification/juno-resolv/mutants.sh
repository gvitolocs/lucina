#!/usr/bin/env bash
# Shows that verification/juno-resolv/rules.rs can fail: each line plants one bug in a copy of the shipped file, and Verus must refuse it.
# Run on buildhost from the repo root, under verify-gate.
set -uo pipefail
verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
work=$(mktemp -d "${TMPDIR:-/tmp}/juno-resolv-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
fail=0
refused=0
weak=0
try() {
    local from=$1 to=$2
    mkdir -p "$work/kernel/lucina-resolv/src" "$work/verification/juno-resolv"
    cp kernel/lucina-resolv/src/wire.rs "$work/kernel/lucina-resolv/src/"
    cp verification/juno-resolv/rules.rs "$work/verification/juno-resolv/"
    local target=$work/kernel/lucina-resolv/src/wire.rs
    local text; text=$(<"$target")
    case "$text" in
        *"$from"*) ;;
        *) echo "NOT FOUND: $from"; fail=1; return ;;
    esac
    printf "%s\n" "${text/"$from"/"$to"}" > "$target"
    if cmp -s "$target" kernel/lucina-resolv/src/wire.rs; then echo "NOT CHANGED: $from"; fail=1; return; fi
    if "$verus" "$work/verification/juno-resolv/rules.rs" > "$work/out" 2>&1; then
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
if "$verus" verification/juno-resolv/rules.rs > "$work/baseline" 2>&1; then
    echo "baseline: verifies"
else
    cat "$work/baseline"; echo "mutants: the unchanged files do not verify"; exit 1
fi
# Names: length, label length, double dots, a dot at the start.
try 'n == 0 || n > MAX_NAME ||' 'n == 0 || n > MAX_NAME + 1 ||'
try 'if run == MAX_LABEL {' 'if run == MAX_LABEL + 1 {'
try 'if run == 0 {' 'if run == 0 && n > 300 {'
try "name[0] == b'.' || " ''
# The query's bytes: one question, the terminating zero.
try $'    out.push(0);\n    out.push(1);' $'    out.push(0);\n    out.push(2);'
try $'            out.push(0);\n            out.push((qtype >> 8) as u8);' $'            out.push((qtype >> 8) as u8);'
# A reply: QR, the id, every byte of the question.
try 'reply[2] & 0x80 == 0 || ' ''
try 'reply[0] != query[0] || ' ''
try 'if reply[i] != query[i] {' 'if reply[i] != query[i] && i > 12 {'
try 'reply[5] != 1 {' 'reply[5] == 0 {'
# Walking the reply: a pointer's two bytes, the record header's ten, the answer cap, an address's length.
try 'if buf.len() - p >= 2 {' 'if buf.len() - p >= 1 {'
try 'if reply.len() - end < 10 {' 'if reply.len() - end < 9 {'
try 'out.len() < MAX_ANSWERS {' 'out.len() <= MAX_ANSWERS {'
try 'rdlen == want_len {' 'rdlen <= want_len {'
[ "$fail" = 0 ] && echo "mutants: every planted bug was refused" || echo "mutants: FAILED"
echo "mutants: $refused refused by verification, $weak refused without a verification run"
if [ "${MUTANTS_STRICT:-0}" = "1" ] && [ "$weak" -gt 0 ]; then exit 1; fi
