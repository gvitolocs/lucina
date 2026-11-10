#!/usr/bin/env bash
# Shows that boards/fortuna-ring/src/rules.rs is what keeps Verus on it: each line plants one bug in a copy of the shipped
# file, turned into what Verus reads exactly as scripts/verify-shipped.sh does, and Verus must refuse every mutant.
# Run on buildhost from the repo root, under verify-gate: verify-gate -- verification/fortuna-ring/mutants.sh
# (or on buildhost2: verus-remote <a copy holding boards/fortuna-ring/src/rules.rs and this script> env VERUS=verus verification/fortuna-ring/mutants.sh)
set -uo pipefail
verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
rules=boards/fortuna-ring/src/rules.rs
work=$(mktemp -d "${TMPDIR:-/tmp}/fortuna-ring-mutants.XXXXXX")
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

# used: off by one on either side of the wrap
try '        wr - rd
    } else {' '        wr - rd + 1
    } else {'
try '(u32::MAX - rd) + wr + 1' '(u32::MAX - rd) + wr'
# room: lets one more in than is free
try 'let free = cap - used(wr, rd);' 'let free = cap - used(wr, rd) + 1;'
# take: takes one more than is there
try '    let u = used(wr, rd);' '    let u = used(wr, rd) + 1;'
# after_push / after_pop: the wrong step across the wrap
try '        k - (u32::MAX - wr) - 1' '        k - (u32::MAX - wr)'
try '        k - (u32::MAX - rd) - 1' '        k - (u32::MAX - rd)'
# runs: the first run past the end of the buffer
try '    let left = cap - s;' '    let left = cap - s + 1;'
# cap_ok: a size that does not divide 2^32
try '0x1_0000_0000u64 % (cap as u64) == 0' '0x1_0000_0000u64 % (cap as u64) <= 1'
# room: hands over everything even when the ring is full
try '        free
    }' '        n
    }'
# spec_cap_ok: any size, so slots jump at the counter wrap (lemma_slot_continuous must fail)
try '0 < cap && cap <= MAX_CAP && 0x1_0000_0000int % (cap as int) == 0' '0 < cap && cap <= MAX_CAP'

echo "mutants refused: $killed"
exit "$fail"
