#!/usr/bin/env bash
# Shows that apps/lucina-glass/src/slide_rules.rs is what keeps Verus on it: each line plants one bug in the code of a copy of
# the shipped file (the off and drift caps, the forward and the back spans of the page slide), turned into what Verus reads
# exactly as scripts/verify-shipped.sh does, and Verus must refuse every mutant. No line touches a `//@` line: the statement
# stays, the code is wrong.
# Run on buildhost from the repo root, under verify-gate: verify-gate -- verification/juno-perf-slide/mutants.sh
# (or on buildhost2: verus-remote <a copy holding apps/lucina-glass/src/slide_rules.rs and this script> env VERUS=verus verification/juno-perf-slide/mutants.sh)
set -uo pipefail
verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
rules=apps/lucina-glass/src/slide_rules.rs
work=$(mktemp -d "${TMPDIR:-/tmp}/juno-perf-slide-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT

make_verus_input() {
    local body
    body=$(sed -E -e '/\/\/@omit$/d' -e 's/-> (.*[^ ]) \/\/@ret$/-> (ret: \1)/' -e 's/^( *)\/\/@ ?/\1\/*proof*\/ /' "$1")
    printf 'use vstd::prelude::*;\nverus! {\n%s\n}\nfn main() {}\n' "$body"
}

# The unchanged file must verify, or every "refused" below would mean nothing.
make_verus_input "$rules" > "$work/base.rs"
if ! "$verus" "$work/base.rs" > "$work/base.out" 2>&1; then
    echo "BASE FAILS"
    cat "$work/base.out"
    echo "mutants: the unchanged file does not verify"
    exit 1
fi
echo "baseline: verifies"

fail=0
weak=0
killed=0
try() {
    local from=$1 to=$2
    local text; text=$(<"$rules")
    case "$from$to" in
        *"//@"*) echo "NOT CODE (a planted bug may not touch a proof line): $from"; fail=1; return ;;
    esac
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
        if grep -q "verification results" "$work/verus.out"; then
            echo "refused: $from -> $to"
            killed=$((killed + 1))
        else
            echo "refused (NO VERIFICATION RUN: compile or other error): $from -> $to"
            weak=$((weak+1))
        fi
    fi
}
# The caps: an offset or a drift beyond the width counts as the width, not as nothing.
try "let off = if off < w { off } else { w };" "let off = if off < w { off } else { 0 };"
try "let drift = if drift < w { drift } else { w };" "let drift = if drift < w { drift } else { 0 };"
# The direction picks the branch.
try "if forward {" "if !forward {"
# Forward. The leaving page is shown up to the arriving one, but never beyond its own last column.
try "let shown = if edge < w - drift { edge } else { w - drift };" "let shown = if edge < w { edge } else { w };"
# Black only where the leaving page has run out: not instead of it.
try "let shown = if edge < w - drift { edge } else { w - drift };" "let shown = 0;"
# One column too many of the leaving page where the arriving one already is (the reviewer's mutant).
try "let shown = if edge < w - drift { edge } else { w - drift };" "let shown = if edge < w - drift { edge + 1 } else { w - drift };"
# The leaving page reads from the column it drifted to.
try "back_src: drift," "back_src: 0,"
# It starts at the left edge of the screen.
try "back_dst: 0," "back_dst: 1,"
# The gap follows it, and is as long as what is left before the arriving page.
try "gap_dst: shown," "gap_dst: 0,"
try "gap_len: edge - shown," "gap_len: 0,"
try "gap_len: edge - shown," "gap_len: edge - shown + 1,"
# The arriving page: its first column, at its edge, down to the right end of the row.
try "front_src: 0," "front_src: edge,"
try "front_dst: edge," "front_dst: shown,"
try "front_len: w - edge," "front_len: w - shown,"
# Back. The arriving page ends `off` columns before the right end of the row.
try "let edge = w - off;" "let edge = off;"
try "let edge = w - off;" "let edge = w - drift;"
# The leaving page starts after the arriving one, or at its own first column when that is further right; not later.
try "let start = if edge > drift { edge } else { drift };" "let start = w;"
try "let start = if edge > drift { edge } else { drift };" "let start = if edge > drift { edge } else { w };"
# It reads from the column that drifted to there.
try "back_src: start - drift," "back_src: start,"
try "back_src: start - drift," "back_src: 0,"
try "back_dst: start," "back_dst: edge,"
try "back_len: w - start," "back_len: w - edge,"
# The gap lies between the two pages.
try "gap_dst: edge," "gap_dst: start,"
try "gap_dst: edge," "gap_dst: edge + 1,"
try "gap_len: start - edge," "gap_len: 0,"
# The arriving page shows its columns from `off` on, from the left edge of the screen.
try "front_src: off," "front_src: 0,"
try "front_dst: 0," "front_dst: 1,"
try "front_len: edge," "front_len: w,"
tried=$(grep -c "^try " "$0")
echo "refused $killed of $tried"
[ "$killed" -eq "$tried" ] || fail=1
echo "mutants: $killed refused by verification, $weak refused without a verification run"
exit "$fail"
