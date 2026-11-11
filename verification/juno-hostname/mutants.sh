#!/usr/bin/env bash
# Shows that kernel/lucina-hostname/src/rules.rs is what keeps Verus on it: each line plants one bug in a copy of the shipped
# file, turned into what Verus reads exactly as scripts/verify-shipped.sh does, and Verus must refuse every mutant.
# Run on buildhost from the repo root, under verify-gate: verify-gate -- verification/juno-hostname/mutants.sh
set -uo pipefail
verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
rules=kernel/lucina-hostname/src/rules.rs
work=$(mktemp -d "${TMPDIR:-/tmp}/juno-hostname-mutants.XXXXXX")
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
echo "baseline: verifies"

fail=0
refused=0
weak=0
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
        if grep -qE 'verification results|simplifies to|not satisfied|assertion failed|possible arithmetic|may not terminate|cannot show|recommendation not met' "$work/verus.out" && ! grep -q 'error\[E' "$work/verus.out"; then
            refused=$((refused + 1))
            echo "refused: $from -> $to ($(grep -c "^error" "$work/verus.out") errors)"
        else
            echo "refused (NO VERIFICATION RUN: compile or other error): $from -> $to ($(grep -c "^error" "$work/verus.out") errors)"
            weak=$((weak+1))
        fi
    fi
}
# The bound: 64 bytes at most, at least one.
try "pub const MAX: usize = 64;" "pub const MAX: usize = 65;"
try "    if n == 0 || n > MAX {" "    if n > MAX {"
try "    if n == 0 || n > MAX {" "    if n == 0 || n > MAX + 1 {"
# The characters: each range, the hyphen, and every byte looked at.
try "(0x30 <= b && b <= 0x39)" "(0x30 <= b && b <= 0x3a)"
try "(0x41 <= b && b <= 0x5a)" "(0x40 <= b && b <= 0x5a)"
try "(0x61 <= b && b <= 0x7a)" "(0x61 <= b && b <= 0x7b)"
try "|| b == HYPHEN" "|| b == 0x5f"
try "    if name[0] == HYPHEN || name[n - 1] == HYPHEN {" "    if name[0] == HYPHEN {"
try "    if name[0] == HYPHEN || name[n - 1] == HYPHEN {" "    if name[n - 1] == HYPHEN {"
try "    let mut i = 0;" "    let mut i = 1;"
try "        if !allowed(name[i]) {" "        if !allowed(name[0]) {"
try "    true
}" "    n > 1
}"
echo "mutants: $refused refused by verification, $weak refused without a verification run"
if [ "${MUTANTS_STRICT:-0}" = "1" ] && [ "$weak" -gt 0 ]; then exit 1; fi
exit "$fail"
