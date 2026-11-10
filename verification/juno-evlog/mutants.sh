#!/usr/bin/env bash
# Shows that kernel/lucina-evlog/src/rules.rs is what keeps Verus on it (the PMIC rules' mutants are in verification/juno-pon): each line plants one bug in a copy of the shipped
# file, turned into what Verus reads exactly as scripts/verify-shipped.sh does, and Verus must refuse every mutant.
# Run on buildhost from the repo root, under verify-gate: verify-gate -- verification/juno-evlog/mutants.sh
set -uo pipefail
verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
rules=kernel/lucina-evlog/src/rules.rs
work=$(mktemp -d "${TMPDIR:-/tmp}/juno-evlog-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT

# What Verus reads from the shipped file, exactly the mechanical steps of scripts/verify-shipped.sh.
make_verus_input() {
    local body
    body=$(sed -E -e '/\/\/@omit$/d' -e 's/-> (.*[^ ]) \/\/@ret$/-> (ret: \1)/' -e 's/^( *)\/\/@ ?/\1\/*proof*\/ /' "$1")
    printf 'use vstd::prelude::*;\nverus! {\n%s\n}\nfn main() {}\n' "$body"
}

# The unchanged file must verify, or nothing below means anything.
make_verus_input "$rules" > "$work/base.rs"
if ! "$verus" "$work/base.rs" > "$work/base.out" 2>&1; then
    echo "BASE FAILS"
    cat "$work/base.out"
    exit 1
fi

fail=0
try() {
    local from=$1 to=$2
    local text; text=$(<"$rules")
    case "$text" in
        *"$from"*) ;;
        *) echo "NOT FOUND: $from"; fail=1; return ;;
    esac
    local mut; mut=${text/"$from"/"$to"}
    if [ "$mut" = "$text" ]; then echo "NOT CHANGED: $from"; fail=1; return; fi
    local in=$work/mutant-src.rs out=$work/mutant.rs
    printf "%s\n" "$mut" > "$in"
    make_verus_input "$in" > "$out"
    if "$verus" "$out" > "$work/verus.out" 2>&1; then
        echo "NOT REFUSED: $from -> $to"
        cat "$work/verus.out"
        fail=1
    else
        echo "refused: $from -> $to ($(grep -c "^error" "$work/verus.out") errors)"
    fi
}
# The warning gate: held within the window, counted exactly, and the count reported by the next one through.
try "            if g.last_ms <= now_ms && now_ms - g.last_ms < window_ms {" "            if g.last_ms <= now_ms && now_ms - g.last_ms <= window_ms {"
try "                        held: g.held + 1," "                        held: g.held,"
try "                    held_back: g.held," "                    held_back: 0,"
# Retention: never the newest segment, every one past its age, and no more than needed to fit.
try "    while n + 1 < sizes.len() && (n < too_old || left > limit)" "    while n < sizes.len() && (n < too_old || left > limit)"
try "    while n + 1 < sizes.len() && (n < too_old || left > limit)" "    while n + 1 < sizes.len() && (n < too_old || left >= limit)"
try "    while n + 1 < sizes.len() && (n < too_old || left > limit)" "    while n + 1 < sizes.len() && left > limit"
exit "$fail"
