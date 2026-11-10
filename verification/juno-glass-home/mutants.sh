#!/usr/bin/env bash
# Shows that apps/lucina-glass/src/home_rules.rs is what keeps Verus on it: each line plants one bug in a copy of the
# shipped file (stack, next_frame, arrow_score, wrap, booked, slide), turned into what Verus reads exactly as
# scripts/verify-shipped.sh does, and Verus must refuse every mutant.
# Run on buildhost from the repo root, under verify-gate: verify-gate -- verification/juno-glass-home/mutants.sh
# (or on buildhost2: verus-remote <a copy holding apps/lucina-glass/src/home_rules.rs and this script> env VERUS=verus verification/juno-glass-home/mutants.sh)
set -uo pipefail
verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
rules=apps/lucina-glass/src/home_rules.rs
work=$(mktemp -d "${TMPDIR:-/tmp}/juno-glass-home-mutants.XXXXXX")
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
        echo "refused: $from -> $to"
        killed=$((killed + 1))
    fi
}
# The column stack: a card that fits lies wholly above the cursor, the next one starts after the gap.
try "Slot { fits: true, start: cursor, next: cursor + h + gap }" "Slot { fits: true, start: cursor + 1, next: cursor + h + gap }"
try "Slot { fits: true, start: cursor, next: cursor + h + gap }" "Slot { fits: true, start: cursor, next: cursor + h }"
# (the parentheses keep the < out of the as i64 < sequence, which the verus! macro parser would reject)
try "if cursor as i64 + h as i64 <= bottom as i64 {" "if (cursor as i64 + h as i64) < bottom as i64 {"
try "    if cursor as i64 + h as i64 <= bottom as i64 {" "    if cursor as i64 <= bottom as i64 {"
# The frame rule: never two frames closer than the period, resting only when the animation and the ambient are done.
try "        let due = last + period;" "        let due = last + period - 1;"
try "        return if due > now { due } else { now };" "        return 0;"
try "    if ambient == 0 || ambient_until <= now {" "    if ambient == 0 {"
try "let step = if ambient > period { ambient } else { period };" "let step = if ambient > period { period } else { ambient };"
# The arrow: the focus moves only to a centre that lies the arrow's way, at its very edge included.
try "        (-dx, dy)" "        (dx, dy)"
try "        (-dy, dx)" "        (dy, dx)"
try "if along <= 0 {" "if along < 0 {"
# The score is exact: across counts four times, on the right axis for each arrow.
try "        (-dy, dx)" "        (-dy, dy)"
try "    (along as u64) + 4 * (side as u64)" "    (along as u64) + 2 * (side as u64)"
# The ring: one step back, none, or one step on, wrapping at both ends, never a bad index.
try "        if i + 1 == n { 0 } else { i + 1 }" "        i + 1"
try "        if i + 1 == n { 0 } else { i + 1 }" "        if i + 1 == n { n } else { i + 1 }"
try "        if i == 0 { n - 1 } else { i - 1 }" "        if i == 0 { n } else { i - 1 }"
try "    } else {
        i
    }" "    } else {
        i + 1
    }"
# The booked share: at most 100, 0 with no window, exactly 100 when the day is full.
try "    if window == 0 {
        return 0;
    }" "    if window == 0 {
        return 100;
    }"
try "        return 100;" "        return 99;"
# The slide: the whole width at the start, 0 at the end, never outside.
try "    width - gone as u32" "    gone as u32"
try "    let gone = (width as u64) * (t as u64) / 65536;" "    let gone = (width as u64) * (t as u64) / 65536 + 1;"
tried=$(grep -c "^try " "$0")
echo "refused $killed of $tried"
[ "$killed" -eq "$tried" ] || fail=1
exit "$fail"
