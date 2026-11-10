#!/usr/bin/env bash
# Shows that boards/fortuna-look/src/rules.rs is what keeps Verus on it: each line plants one bug in a copy of the shipped
# file, turned into what Verus reads exactly as scripts/verify-shipped.sh does, and Verus must refuse every mutant.
# Run on buildhost from the repo root, under verify-gate: verify-gate -- verification/fortuna-look/mutants.sh
# (or on buildhost2: verus-remote <a copy holding boards/fortuna-look/src/rules.rs and this script> env VERUS=verus verification/fortuna-look/mutants.sh)
set -uo pipefail
verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
rules=boards/fortuna-look/src/rules.rs
work=$(mktemp -d "${TMPDIR:-/tmp}/fortuna-look-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT

make_verus_input() {
    local body
    body=$(sed -E -e '/\/\/@omit$/d' -e 's/-> (.*[^ ]) \/\/@ret$/-> (ret: \1)/' -e 's/^( *)\/\/@ ?/\1\/*proof*\/ /' "$1")
    printf 'use vstd::prelude::*;\nverus! {\n%s\n}\nfn main() {}\n' "$body"
}

make_verus_input "$rules" > "$work/base.rs"
if ! "$verus" "$work/base.rs" > "$work/base.out" 2>&1; then
    echo "BASE FAILS"
    cat "$work/base.out"
    exit 1
fi

fail=0
killed=0
try() {
    local from=$1 to=$2
    local text; text=$(<"$rules")
    case "$text" in
        *"$from"*) ;;
        *) echo "NOT FOUND: $from"; fail=1; return ;;
    esac
    local mut; mut=${text/"$from"/"$to"}
    if [ "$mut" = "$text" ]; then echo "NOT CHANGED: $from"; fail=1; return; fi
    printf "%s\n" "$mut" > "$work/mutant-src.rs"
    make_verus_input "$work/mutant-src.rs" > "$work/mutant.rs"
    if "$verus" "$work/mutant.rs" > "$work/verus.out" 2>&1; then
        echo "NOT REFUSED: $from -> $to"
        fail=1
    else
        killed=$((killed + 1))
        echo "refused: $from -> $to"
    fi
}

# first_line: the selection on the top line instead of the second, the grid scrolled one past its end, the last page
# entered one line late
try 'let want = if sel > 0 { sel - 1 } else { 0 };' 'let want = if sel > 0 { sel } else { 0 };'
try 'if total > lines { total - lines } else { 0 }' 'if total > lines { total - lines + 1 } else { 0 }'
try 'if want > last_first {' 'if want > last_first + 1 {'
# fit_dims: a cover one pixel taller than its box, one narrower than the true shape
try 'if tall <= bh as u32 {' 'if tall <= bh as u32 + 1 {'
try '(bw, tall as i32)' '(bw - 1, tall as i32)'
# clip: a pixel past the right edge, the left edge kept at 0 for a rectangle starting at 1
try 'let x1 = if r.0 + r.2 < sw { r.0 + r.2 } else { sw };' 'let x1 = if r.0 + r.2 < sw { r.0 + r.2 } else { sw + 1 };'
try 'let x0 = if r.0 > 0 { r.0 } else { 0 };' 'let x0 = if r.0 > 1 { r.0 } else { 0 };'
# overlaps: rectangles that only touch counted as overlapping, an empty one counted
try '        && a.0 < b.0 + b.2' '        && a.0 <= b.0 + b.2'
try 'let r = a.2 > 0' 'let r = a.2 >= 0'
# union: the right edge taken from one rectangle only
try 'let x1 = if a.0 + a.2 > b.0 + b.2 { a.0 + a.2 } else { b.0 + b.2 };' 'let x1 = if a.0 + a.2 > b.0 + b.2 { a.0 + a.2 } else { a.0 + a.2 };'

echo "mutants refused: $killed"
exit "$fail"
