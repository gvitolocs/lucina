#!/usr/bin/env bash
# Shows that Verus can fail on drivers/lucina-modem-rpmh/src/vote_rules.rs: each line plants one bug in a copy of the shipped file, and
# Verus must refuse it. Run on buildhost from the repo root, under verify-gate.
set -uo pipefail
verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
file=drivers/lucina-modem-rpmh/src/vote_rules.rs
work=$(mktemp -d "${TMPDIR:-/tmp}/juno-rpmh-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
fail=0
try() {
    local from=$1 to=$2
    local text; text=$(<"$file")
    case "$text" in
        *"$from"*) ;;
        *) echo "NOT FOUND: $from"; fail=1; return ;;
    esac
    printf '%s\n' "${text/"$from"/"$to"}" > "$work/mutant.rs"
    local body; body=$(sed -E -e '/\/\/@omit$/d' -e 's/-> (.*[^ ]) \/\/@ret$/-> (ret: \1)/' -e 's/^( *)\/\/@ ?/\1\/*proof*\/ /' "$work/mutant.rs")
    printf 'use vstd::prelude::*;\nverus! {\n%s\n}\nfn main() {}\n' "$body" > "$work/rules.rs"
    if "$verus" "$work/rules.rs" > "$work/out" 2>&1; then
        echo "NOT REFUSED: $from -> $to"; fail=1
    else
        echo "refused: $from -> $to"
    fi
}
# A falling table let through.
try 'if t.level[i - 1] > t.level[i] {
            return false;
        }' ''
# The first corner at or above the state, not past it.
try 'if t.level[i] >= state {' 'if t.level[i] > state {'
# A holder whose vote is not counted, a holder that does not turn the domain on, the child's state dropped.
try 'if want[c] > state {' 'if want[c] < state {'
try '            any = true;
' ''
try 'let mut state: u32 = if child_on { child_state } else { 0 };' 'let mut state: u32 = 0;'
# Before sync a domain stays at its top; after it, never below the first non-zero corner while held, and 0 when nobody holds it.
try 'if !synced {
        return t.count - 1;
    }' 'if !synced {
        return 0;
    }'
try 'if c >= t.enable_corner {' 'if true {'
try 'if !on {
        return 0;
    }' 'if !on {
        return t.enable_corner;
    }'
# XO off while someone holds it.
try 'if holds[c] {
            return true;
        }' ''
# Waiting only on the way up.
try '    corner > last
}' '    corner >= last
}'
exit "$fail"
