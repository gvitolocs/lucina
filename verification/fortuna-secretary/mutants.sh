#!/usr/bin/env bash
# Shows that boards/fortuna-secretary/src/rules.rs is what keeps Verus on it: each line plants one bug in a copy of the shipped
# file, turned into what Verus reads exactly as scripts/verify-shipped.sh does, and Verus must refuse every mutant.
# Run on buildhost from the repo root, under verify-gate: verify-gate -- verification/fortuna-secretary/mutants.sh
# (or on buildhost2: verus-remote <a copy holding boards/fortuna-secretary/src/rules.rs and this script> env VERUS=verus verification/fortuna-secretary/mutants.sh)
set -uo pipefail
verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
rules=boards/fortuna-secretary/src/rules.rs
work=$(mktemp -d "${TMPDIR:-/tmp}/fortuna-secretary-mutants.XXXXXX")
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

# hex_value, push_hex: a wrong digit value, a chunk size past the cap
try '        (b - 87) as u32' '        (b - 86) as u32'
try '    if v > MAX_CHUNK {' '    if v > MAX_CHUNK + 1 {'
# body_take: more than the cap, the larger of the two
try '    if n > cap - held {' '    if n > cap - held + 1 {'
try 'let n = if remaining < avail { remaining } else { avail };' 'let n = if remaining < avail { avail } else { remaining };'
# pairing_action: a poll of an expired code, a sixth code, a poll before its time
try '    if has_code && now < expires {' '    if has_code && now <= expires {'
try '    if codes >= MAX_CODES {' '    if codes > MAX_CODES {'
try '    if !active || now < next {' '    if !active {'
# token_shape: the wrong prefix
try 't[2] != 99 || t[3] != 95' 't[2] != 99 || t[3] != 96'

# the voice socket's frames
try '    let base = if n < 126 {' '    let base = if n <= 126 {'
try '    } else if n <= 65535 {
        4' '    } else if n < 65535 {
        4'
try '    if masked {
        base + 4' '    if masked {
        base + 2'
try '    } else if l7 == 126 {
        2' '    } else if l7 == 126 {
        8'
try '    if !fin || rsv != 0 || masked || len > WS_MAX_PAYLOAD {' '    if !fin || rsv != 0 || len > WS_MAX_PAYLOAD {'
try '    if !fin || rsv != 0 || masked || len > WS_MAX_PAYLOAD {' '    if !fin || masked || len > WS_MAX_PAYLOAD {'
try '((opcode == 8 || opcode == 9 || opcode == 10) && len <= 125)
}' '((opcode == 8 || opcode == 9 || opcode == 10) && len <= 126)
}'
try '    opcode == 1 || opcode == 2 ||' '    opcode == 0 || opcode == 1 || opcode == 2 ||'

echo "mutants refused: $killed"
exit "$fail"
