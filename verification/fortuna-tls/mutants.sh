#!/usr/bin/env bash
# Shows that boards/fortuna-tls/src/rules.rs is what keeps Verus on it: each line plants one bug in a copy of the shipped
# file, turned into what Verus reads exactly as scripts/verify-shipped.sh does, and Verus must refuse every mutant.
# Run on buildhost from the repo root, under verify-gate: verify-gate -- verification/fortuna-tls/mutants.sh
# (or on buildhost2: verus-remote <a copy holding boards/fortuna-tls/src/rules.rs and this script> env VERUS=verus verification/fortuna-tls/mutants.sh)
set -uo pipefail
verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
rules=boards/fortuna-tls/src/rules.rs
work=$(mktemp -d "${TMPDIR:-/tmp}/fortuna-tls-mutants.XXXXXX")
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
echo "baseline: verifies"

fail=0
weak=0
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
        if grep -qE 'verification results|simplifies to|not satisfied|assertion failed|possible arithmetic|may not terminate|cannot show|recommendation not met' "$work/verus.out" && ! grep -q 'error\[E' "$work/verus.out"; then
            killed=$((killed + 1))
            echo "refused: $from -> $to"
        else
            echo "refused (NO VERIFICATION RUN: compile or other error): $from -> $to"
            weak=$((weak+1))
        fi
    fi
}

# clock_set: the build day itself refused, any clock accepted; chain_len_ok: one more intermediate, no certificate
try '    clock >= built' '    clock > built'
try '    clock >= built
}' '    clock >= 0
}'
try '    n >= 1 && n <= MAX_INTERMEDIATES + 1' '    n >= 1 && n <= MAX_INTERMEDIATES + 2'
try '    n >= 1 && n <= MAX_INTERMEDIATES + 1' '    n <= MAX_INTERMEDIATES + 1'

# the signature cache's slot: wraps to 0, never leaves the array, never skips a slot
try '    if i + 1 >= SIG_CACHE {' '    if i + 1 > SIG_CACHE {'
try '        i + 1
    }
}' '        i + 2
    }
}'
try '    if i + 1 >= SIG_CACHE {
        0' '    if i + 1 >= SIG_CACHE {
        1'

echo "mutants refused: $killed"
echo "mutants: $killed refused by verification, $weak refused without a verification run"
if [ "${MUTANTS_STRICT:-0}" = "1" ] && [ "$weak" -gt 0 ]; then exit 1; fi
exit "$fail"
