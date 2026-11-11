#!/usr/bin/env bash
# Shows that apps/lucina-glass/src/browser_rules.rs is what keeps Verus on it: each line plants one bug in a copy of the shipped
# file (hit area, hit test, focus, switch thumb, stepper, progress fraction and fill, text scroll, caret, tab width, popover,
# sheet, dialog), turned into what Verus reads exactly as scripts/verify-shipped.sh does, and Verus must refuse every mutant.
# Run on buildhost from the repo root, under verify-gate: verify-gate -- verification/glass-browser/mutants.sh
set -uo pipefail
verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
rules=apps/lucina-glass/src/browser_rules.rs
work=$(mktemp -d "${TMPDIR:-/tmp}/glass-browser-mutants.XXXXXX")
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
grep -q "verification results" "$work/base.out" || { echo "BASE: no verification ran"; cat "$work/base.out"; exit 1; }
echo "baseline: verifies"

fail=0
weak=0
killed=0
total=0
try() {
    local from=$1 to=$2
    total=$((total + 1))
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
    elif grep -q "verification results" "$work/verus.out"; then
        echo "refused: $from -> $to"
        killed=$((killed + 1))
    else
        # Verus stopped on a compile or other error: nothing was verified, so the mutant is not refused by the proof.
        echo "NOT REFUSED BY VERIFICATION (no verification ran: compile or other error): $from -> $to"
        weak=$((weak + 1))
        fail=1
    fi
}
# The hit area: never smaller than the minimum, never short of what is drawn, grown evenly about it.
try "    if len >= min { len } else { min }" "    if len >= min { min } else { len }"
try "        x: x - (nw - w) / 2,
        y: y - (nh - h) / 2,
        w: nw,
        h: nh," "        x: x,
        y: y - (nh - h) / 2,
        w: nw,
        h: nh,"
try "        x: x - (nw - w) / 2,
        y: y - (nh - h) / 2,
        w: nw,
        h: nh," "        x: x - (nw - w),
        y: y - (nh - h) / 2,
        w: nw,
        h: nh,"
try "        x: x - (nw - w) / 2,
        y: y - (nh - h) / 2,
        w: nw,
        h: nh," "        x: x - (nw - w) / 2,
        y: y - (nh - h) / 2,
        w: nw,
        h: h,"
try "    let nw = grow_to(w, min);" "    let nw = if w >= min { w } else { min + (min - w) };"
# The hit test: left and top edges in, right and bottom edges out.
try "    px as i64 >= b.x as i64
        && (px as i64) < b.x as i64 + b.w as i64" "    px as i64 > b.x as i64
        && (px as i64) < b.x as i64 + b.w as i64"
try "(px as i64) < b.x as i64 + b.w as i64" "(px as i64) <= b.x as i64 + b.w as i64"
try "&& (py as i64) < b.y as i64 + b.h as i64" "&& (py as i64) <= b.y as i64 + b.h as i64"
# The tap: the hit area grown to the minimum, not just the drawn rectangle (refused by tap's own postcondition).
try "    let b = hit_box(x, y, w, h, min);" "    let b = hit_box(x, y, w, h, 0);"
# Focus: one step, wrapping at both ends, never a bad index.
try "    } else if cur + 1 == n {
        0
    } else {
        cur + 1
    }" "    } else if cur + 1 == n {
        n
    } else {
        cur + 1
    }"
try "        if cur == 0 { n - 1 } else { cur - 1 }" "        if cur == 0 { 0 } else { cur - 1 }"
try "    } else if cur >= n {
        n - 1
    } else {
        cur
    }" "    } else if cur >= n {
        n
    } else {
        cur
    }"
# The switch thumb stays inside its track, margin to margin.
try "    (x + pad, x + w - pad - d)" "    (x + pad, x + w - pad)"
try "    (x + pad, x + w - pad - d)" "    (x, x + w - pad - d)"
# The stepper: one step, clamped to its range at both ends.
try "    let t = if up {
        v as i64 + step as i64
    } else {
        v as i64 - step as i64
    };" "    let t = if up {
        v as i64 + step as i64
    } else {
        v as i64 + step as i64
    };"
try "    if t < lo as i64 {
        lo
    } else if t > hi as i64 {
        hi
    } else {" "    if t < lo as i64 {
        hi
    } else if t > hi as i64 {
        hi
    } else {"
try "    } else if t > hi as i64 {
        hi
    } else {" "    } else if t > hi as i64 {
        hi - 1
    } else {"
try "    if up { v < hi } else { v > lo }
}" "    if up { v <= hi } else { v > lo }
}"
# The download fraction and the fill: 0 for nothing or an unknown total, the whole at the end, never past it.
try "    if total == 0 {
        return 0;
    }" "    if total == 0 {
        return UNIT;
    }"
try "    let p = scaled / (total as u128);" "    let p = scaled / (total as u128) + 1;"
try "    let p = scaled / (total as u128);" "    let p = scaled / (total as u128) / 2;"
try "    let v = (w as u64) * (t as u64) / (UNIT as u64);" "    let v = (w as u64) * (t as u64) / (UNIT as u64) + 1;"
try "    v as u32
}" "    if v > 0 && v < w as u64 { (v - 1) as u32 } else { v as u32 }
}"
# The text field: the caret stays in view, the scroll does not move for a caret already in view, arrows stop at the ends.
try "    if caret < scroll {
        caret
    } else if caret > scroll + view {
        caret - view
    } else {" "    if caret < scroll {
        caret
    } else if caret > scroll + view {
        caret - view + 1
    } else {"
try "    if caret < scroll {
        caret
    } else if caret > scroll + view {" "    if caret < scroll {
        scroll
    } else if caret > scroll + view {"
try "        if caret == 0 { 0 } else { caret - 1 }" "        caret - 1"
try "    } else if caret >= len {
        len
    } else {
        caret + 1
    }" "    } else {
        caret + 1
    }"
try "    ms % period < period - period / 2" "    ms % period <= period - period / 2"
try "    ms % period < period - period / 2" "    ms % period == 0"
try "    if period == 0 {
        return true;
    }" "    if period == 0 {
        return false;
    }"
# A tab's width: all of them fit, none wider than the cap.
try "    let free = (avail as i64 - gap as i64 * (n as i64 - 1)) as u64;" "    let free = avail as u64;"
try "    let w = if each > max as u64 { max as u64 } else { each };" "    let w = each;"
try "    let w = if each > max as u64 { max as u64 } else { each };" "    let w: u64 = 0;"
# The popover, sheet and dialog stay on the screen.
try "    if x < margin as i64 {
        margin
    } else if x > screen as i64 - margin as i64 - w as i64 {
        screen - margin - w
    } else {" "    if x < margin as i64 {
        margin
    } else if x > screen as i64 - margin as i64 - w as i64 {
        screen - w
    } else {"
try "    if x < margin as i64 {
        margin
    }" "    if x < 0 {
        margin
    }"
try "    if x < margin as i64 {
        margin
    }" "    if x < margin as i64 {
        screen - margin - w
    }"
try "    screen_h - margin - total" "    screen_h - total"
try "    start + (outer - len) / 2" "    start + (outer - len)"
echo "mutants refused: $killed of $total"
echo "mutants: $killed refused by verification, $weak refused without a verification run"
[ "$killed" -eq "$total" ] || fail=1
exit "$fail"
