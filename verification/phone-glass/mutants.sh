#!/usr/bin/env bash
# Shows that apps/lucina-glass/src/rules.rs is what keeps Verus on it: each line plants one bug in a copy of the shipped file,
# turned into what Verus reads exactly as scripts/verify-shipped.sh does, and Verus must refuse every mutant.
# Run on buildhost from the repo root, under verify-gate: verify-gate -- verification/phone-glass/mutants.sh
# (or on buildhost2: verus-remote <a copy holding apps/lucina-glass/src/rules.rs and this script> env VERUS=verus verification/phone-glass/mutants.sh)
set -uo pipefail
verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
rules=apps/lucina-glass/src/rules.rs
work=$(mktemp -d "${TMPDIR:-/tmp}/phone-glass-mutants.XXXXXX")
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
            echo "refused: $from -> $to"
            killed=$((killed + 1))
        else
            echo "refused (NO VERIFICATION RUN: compile or other error): $from -> $to"
            weak=$((weak+1))
        fi
    fi
}
# Progress and interpolation: done must be exactly done, the ends exact, nothing past them.
try "        return UNIT;" "        return UNIT - 1;"
try "        (a as i64 + step as i64) as i32" "        (a as i64 + step as i64 + 1) as i32"
try "        (a as i64 - step as i64) as i32" "        (b as i64 + step as i64) as i32"
try "    v as u32
}" "    (v + 1) as u32
}"
try "    if done >= total {
        return 100;" "    if done >= total {
        return 99;"
# The row rule: cells tile the row exactly, gap apart.
try "let b = free * (i as u64 + 1) / (n as u64);" "let b = free * (i as u64 + 2) / (n as u64);"
try "let x0 = start as i64 + a as i64 + gap as i64 * i as i64;" "let x0 = start as i64 + a as i64;"
# The month on the grid.
try "if is_leap(y) { 29 } else { 28 }" "if is_leap(y) { 29 } else { 27 }"
try "    (sun + 6) % 7" "    (sun + 6) % 8"
try "    (first + days + WEEK - 1) / WEEK" "    (first + days) / WEEK"
try "    (i / WEEK, i % WEEK)" "    (i / WEEK, i % WEEK + 1)"
try "{ 0 } else { i - first + 1 }" "{ 0 } else { i - first }"
# Pager widths, stagger cap, timer digits.
try "    (big - grow, small + grow)" "    (big - grow, small + grow + 1)"
try "if d > cap as u64 { cap } else { d as u32 }" "if d > cap as u64 + 1 { cap } else { d as u32 }"
try "    let s = if secs > 5999 { 5999 } else { secs };" "    let s = secs;"
# Layout boxes: a pane keeps to its parent on both axes, and the check says exactly that.
try "    Box2 { x: a.start, y: b.start, w: a.len, h: b.len }" "    Box2 { x: a.start, y: b.start, w: a.len + 1, h: b.len }"
try "    Box2 { x: a.start, y: b.start, w: a.len, h: b.len }" "    Box2 { x: a.start, y: a.start, w: a.len, h: b.len }"
try "&& y as i64 <= b.y as i64 && b.y as i64 + b.h as i64 <= y as i64 + h as i64" "&& y as i64 <= b.y as i64"
echo "$killed mutants refused"
echo "mutants: $killed refused by verification, $weak refused without a verification run"
if [ "${MUTANTS_STRICT:-0}" = "1" ] && [ "$weak" -gt 0 ]; then exit 1; fi
exit "$fail"
