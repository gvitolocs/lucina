#!/usr/bin/env bash
# Shows that boards/fortuna-voice/src/rules.rs is what keeps Verus on it: each line plants one bug in a copy of the shipped
# file, turned into what Verus reads exactly as scripts/verify-shipped.sh does, and Verus must refuse every mutant.
# Run on buildhost from the repo root, under verify-gate: verify-gate -- verification/fortuna-voice/mutants.sh
# (or on buildhost2: verus-remote <a copy holding boards/fortuna-voice/src/rules.rs and this script> env VERUS=verus verification/fortuna-voice/mutants.sh)
set -uo pipefail
verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
rules=boards/fortuna-voice/src/rules.rs
work=$(mktemp -d "${TMPDIR:-/tmp}/fortuna-voice-mutants.XXXXXX")
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

# clamps one too wide
try '    } else if i > 88 {' '    } else if i > 89 {'
try '    } else if v > 32767 {' '    } else if v > 32768 {'
# reconstruct, next_predictor, next_index: the wrong divisor, no clamp, no step down
try '    ((2 * d + 1) * step) / 8' '    ((2 * d + 1) * step) / 4'
try '        clamp_sample(pred - d)' '        pred - d'
try '        _ => -1,' '        _ => 0,'
# the record's framing: a block short, a block too many, the RIFF size, the bound, the cap
try '        full + 1
    }' '        full
    }'
try '    if samples % BLOCK_SAMPLES == 0 {' '    if samples % BLOCK_SAMPLES == 1 {'
try '    Some((data, data + 52))' '    Some((data, data + 60))'
try '    blocks <= (0xffff_ffffu32 - 60) / 256
}' '    blocks <= (0xffff_ffffu32 - 60) / 255
}'
try '    samples as u64 >= rate as u64 * cap_secs as u64' '    samples as u64 > rate as u64 * cap_secs as u64'

# the waveform strip's slices: never empty, never past the end, starting where the iPhone's do
try '    let start = b * n / count;' '    let start = (b + 1) * n / count;'
try '    if end > start {' '    if end >= start {'
try '        (start, start + 1)' '        (start, start)'
try '    let end = (b + 1) * n / count;' '    let end = (b + 2) * n / count;'

echo "mutants refused: $killed"
exit "$fail"
