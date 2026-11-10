#!/usr/bin/env bash
# Shows that verification/juno-modem-witness/rules.rs can fail: each line plants one bug in a copy of the shipped file
# (or of the proof file, for the model's steps), and Verus must refuse it.
# Run on buildhost from the repo root, under verify-gate.
set -uo pipefail
verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
work=$(mktemp -d "${TMPDIR:-/tmp}/juno-modem-witness-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
fail=0
try() {
    local file=$1 from=$2 to=$3
    # Shown on one line: a planted bug may span lines.
    local shown="${from//$'\n'/\\n} -> ${to//$'\n'/\\n}"
    mkdir -p "$work/kernel/lucina-crash/src" "$work/verification/juno-modem-witness"
    cp kernel/lucina-crash/src/witness.rs "$work/kernel/lucina-crash/src/"
    cp verification/juno-modem-witness/rules.rs "$work/verification/juno-modem-witness/"
    local target=$work/$file
    local text; text=$(<"$target")
    case "$text" in
        *"$from"*) ;;
        *) echo "NOT FOUND in $file: ${shown% -> *}"; fail=1; return ;;
    esac
    printf "%s\n" "${text/"$from"/"$to"}" > "$target"
    if cmp -s "$target" "$file"; then echo "NOT CHANGED in $file: ${shown% -> *}"; fail=1; return; fi
    if "$verus" "$work/verification/juno-modem-witness/rules.rs" > "$work/out" 2>&1; then
        echo "NOT REFUSED in $file: $shown"; fail=1
    else
        echo "refused in $file: $shown ($(grep -c "^error" "$work/out") errors)"
    fi
}
lib=kernel/lucina-crash/src/witness.rs
rules=verification/juno-modem-witness/rules.rs
# Any byte goes into the record as it is.
try "$lib" '    if b >= 0x20 && b <= 0x7e { b } else { b'"'"'?'"'"' }' '    b'
# The scan ignores the cap: it may read the whole buffer.
try "$lib" '    let limit = if raw.len() < cap { raw.len() } else { cap };' '    let limit = raw.len();'
# The scan does not stop at the NUL.
try "$lib" '        if raw[n] == 0 {
            return n;
        }' '        if false {
            return n;
        }'
# The read ignores the buffer: as much as the item says.
try "$lib" '    if item_len < cap { item_len } else { cap }' '    item_len'
# The gate always says yes.
try "$lib" '    seen & bit == 0
}' '    true
}'
# The model's mask never takes the bit (no fetch_or): every call would write.
try "$rules" '        here + writes(m | x, bits.drop_first(), b)' '        here + writes(m, bits.drop_first(), b)'
[ "$fail" = 0 ] && echo "all mutants refused" || echo "SOME MUTANTS SURVIVED"
exit $fail
