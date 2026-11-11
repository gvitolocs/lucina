#!/usr/bin/env bash
# Shows that services/lucina-wm-rules/src/{snap_rules,mru_rules,tile_rules}.rs are what keeps Verus on them: each line plants one bug
# in a copy of a shipped file, turned into what Verus reads exactly as scripts/verify-shipped.sh does, and Verus must refuse every
# mutant. Run on buildhost from the repo root, under verify-gate: verify-gate -- verification/juno-wm-rules/mutants.sh
# A mutant counts as refused only when Verus reports a failed `ensures` (postcondition) or loop invariant: a refusal by a ghost
# `assert` alone, a call's precondition or a compile error does not count, since those are the proof's scaffolding and not its
# contract. Each line prints the clause that failed.
set -uo pipefail
verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
dir=services/lucina-wm-rules/src
work=$(mktemp -d "${TMPDIR:-/tmp}/juno-wm-rules-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT

# What Verus reads from a shipped file, exactly the mechanical steps of scripts/verify-shipped.sh.
make_verus_input() {
    local body
    body=$(sed -E -e '/\/\/@omit$/d' -e 's/-> (.*[^ ]) \/\/@ret$/-> (ret: \1)/' -e 's/^( *)\/\/@ ?/\1\/*proof*\/ /' "$1")
    printf 'use vstd::prelude::*;\nverus! {\n%s\n}\nfn main() {}\n' "$body"
}

# A snapshot without these files must not read as a surviving mutant.
for name in snap_rules mru_rules tile_rules; do
    if [ ! -f "$dir/$name.rs" ]; then
        echo "FILE MISSING $dir/$name.rs" >&2
        exit 2
    fi
done

# The unchanged files must verify, or nothing below means anything.
for name in snap_rules mru_rules tile_rules; do
    make_verus_input "$dir/$name.rs" > "$work/base-$name.rs"
    if ! "$verus" "$work/base-$name.rs" > "$work/base.out" 2>&1; then
        echo "BASE FAILS: $name"
        cat "$work/base.out"
        exit 1
    fi
done
echo "baseline: verifies"

fail=0
weak=0
refused=0
try() {
    local rules=$dir/$1.rs from=$2 to=$3
    if [ ! -f "$rules" ]; then
        echo "FILE MISSING $rules" >&2
        exit 2
    fi
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
        echo "NOT REFUSED: $1: $from -> $to"
        cat "$work/verus.out"
        fail=1
    elif grep -qE "^error: (postcondition not satisfied|invariant not satisfied)" "$work/verus.out"; then
        refused=$((refused + 1))
        # The failing clause: the first `-->` of the error points at its line in what Verus read.
        local at clause
        at=$(grep -A1 -m1 -E "^error: (postcondition|invariant) not satisfied" "$work/verus.out" | grep -m1 -oE ":[0-9]+:[0-9]+$" | cut -d: -f2)
        clause=$(sed -n "${at:-1}p" "$out" | sed -E 's|^ *(/\*proof\*/)? *||')
        echo "refused: $1: $(printf %s "$from" | head -1) -> $(printf %s "$to" | head -1) :: $(grep -m1 -E "^error: (postcondition|invariant)" "$work/verus.out") :: $clause"
    elif ! grep -q 'verification results' "$work/verus.out"; then
        # main's check, kept: no verification ran (a compile or other error). Never counted as refused, and it fails the script.
        echo "refused (NO VERIFICATION RUN: compile or other error): $1: $from -> $to ($(grep -c "^error" "$work/verus.out") errors)"
        weak=$((weak + 1))
        fail=1
    else
        echo "REFUSED ONLY BY SCAFFOLDING: $1: $from -> $to :: $(grep -m1 "^error" "$work/verus.out")"
        fail=1
    fi
}

# snap_rules: the target stays at or below `last`, within one point of `start` without long swipes, and is the exact clamp with them.
try snap_rules "    let mut hi: i64 = last as i64;" "    let mut hi: i64 = last as i64 - 1;"
try snap_rules "    let mut lo: i64 = 0;" "    let mut lo: i64 = -1;"
try snap_rules "            lo = s - 1;" "            lo = s - 2;"
try snap_rules "            hi = s + 1;" "            hi = s + 2;"
try snap_rules "        if s + 1 < hi {" "        if s + 1 > hi {"
try snap_rules "        if s - 1 > lo {" "        if s - 1 < lo {"
try snap_rules "    } else if rounded > hi {
        hi" "    } else if rounded > hi {
        rounded"
try snap_rules "    if clamped > last as i64 {
        last
    } else {
        clamped as usize
    }" "    clamped as usize"

# mru_rules: the moved id lands at place 0, the others keep their order, nothing is lost or doubled.
try mru_rules "    let id = ids[idx];" "    let id = ids[0];"
try mru_rules "    while i > 0
    //@ invariant
    //@     i <= idx, idx < len <= MAX," "    while i > 1
    //@ invariant
    //@     i <= idx, idx < len <= MAX,"
try mru_rules "        ids[i] = ids[i - 1];
        i -= 1;
    }
    ids[0] = id;
    //@ proof { move_to_front_ids" "        ids[i] = ids[i - 1];
        i -= 1;
    }
    //@ proof { move_to_front_ids"
try mru_rules "    let mut i = len;
    while i > 0" "    let mut i = len;
    while i > 1"
try mru_rules "    ids[0] = id;
    //@ proof { push_front_ids" "    ids[1] = id;
    //@ proof { push_front_ids"
try mru_rules "        ids[i] = ids[i + 1];" "        ids[i + 1] = ids[i];"
try mru_rules "    while i + 1 < len
" "    while i + 2 < len
"
try mru_rules "    let mut i = idx;
    while i + 1 < len" "    let mut i = idx + 1;
    while i + 1 < len"

# tile_rules: the two sides and the paddings fit in the work width, both sides stay positive, the divider keeps both sides wide enough.
try tile_rules "        inner - padding
    } else {" "        inner
    } else {"
try tile_rules "            lw = avail - 1;" "            lw = avail;"
try tile_rules "    (lw, avail - lw)" "    (lw, avail - lw + 1)"
try tile_rules "    if avail >= 2 {" "    if avail >= 3 {"
try tile_rules "        1000
    } else {
        ratio_permille
    };
    // No overflow" "        2000
    } else {
        ratio_permille
    };
    // No overflow"
try tile_rules "    if 2 * min_w > w {" "    if 2 * min_w < w {"
try tile_rules "    let lo = (min_w * 1000 + w - 1) / w;" "    let lo = (min_w * 1000) / w;"
try tile_rules "    let hi = 1000 - lo;" "    let hi = 1000 - lo + 1;"
try tile_rules "    if w <= 0 || min_w <= 0 {" "    if w < 0 || min_w <= 0 {"
try tile_rules "    } else if ratio > hi {
        hi" "    } else if ratio > hi {
        ratio"

# snap_rules: without long swipes the target is the exact clamp of the rounded point (review of 2026-11-11, survivors 1-2, two more).
try snap_rules "            hi = s + 1;
        }
" "            hi = s + 1;
        }
        lo = hi;
"
try snap_rules "            hi = s + 1;
        }
" "            hi = s + 1;
        }
        hi = lo;
"
try snap_rules "            lo = s - 1;" "            lo = s;"
try snap_rules "            hi = s + 1;" "            hi = s;"

# tile_rules::split_widths: the ratio decides the left width exactly (survivors 3-4), a negative ratio counts as 0.
try tile_rules "    (lw, avail - lw)" "    (avail - lw, lw)"
try tile_rules "    let mut lw = avail * ratio / 1000;" "    let mut lw = avail / 2;"
try tile_rules "    let ratio = if ratio_permille < 0 {
        0
    } else if ratio_permille > 1000 {
        1000
    } else {
        ratio_permille
    };
    // No overflow" "    let ratio = if ratio_permille < 0 {
        1
    } else if ratio_permille > 1000 {
        1000
    } else {
        ratio_permille
    };
    // No overflow"

# tile_rules::clamp_ratio: a divider dragged past the limit stops at the nearest allowed ratio (survivors 5-6), not one short of it,
# and a room too narrow for two sides gives 500.
try tile_rules "    let kept = if ratio < lo {
        lo" "    let kept = if ratio < lo {
        500"
try tile_rules "    } else if ratio > hi {
        hi
    } else {
        ratio
    };
    //@ let ghost ki" "    } else if ratio > hi {
        500
    } else {
        ratio
    };
    //@ let ghost ki"
try tile_rules "    let hi = 1000 - lo;" "    let hi = 1000 - lo - 1;"
try tile_rules "        return 500;" "        return ratio;"

# tile_rules::split_span: the left part starts `padding` after `x`, the right one exactly `padding` after the left one ends.
try tile_rules "    let right_x = x + padding + lw + padding;" "    let right_x = x + padding + lw;"
try tile_rules "    let left_x = x + padding;" "    let left_x = x;"
try tile_rules "    ((left_x, lw), (right_x, rw))" "    ((left_x, lw), (right_x, lw))"

# tile_rules::split_geom: both rectangles start `padding` below the top and are the work height less two paddings high.
try tile_rules "    let iy = y + padding;" "    let iy = y;"
try tile_rules "    ((left.0, iy, left.1, ih), (right.0, iy, right.1, ih))" "    ((left.0, iy, left.1, ih), (right.0, iy, left.1, ih))"
try tile_rules "        h - 2 * padding
    } else {
        0
    };
    let iy" "        h - padding
    } else {
        0
    };
    let iy"

# tile_rules::inset: the maximized tile.
try tile_rules "    (x + padding, y + padding, iw, ih)" "    (x, y + padding, iw, ih)"
try tile_rules "        w - 2 * padding
    } else {
        0
    };
    let ih" "        w - padding
    } else {
        0
    };
    let ih"

# tile_rules::quarter: the quarter is the part of each half split it names.
try tile_rules "    let vs = if bottom { vb } else { vt };" "    let vs = if bottom { vt } else { vb };"
try tile_rules "    let (vt, vb) = split_span(y, h, padding, 500);" "    let (vt, vb) = split_span(y, h, padding, 400);"
try tile_rules "    (hs.0, vs.0, hs.1, vs.1)
}" "    (hs.0, vs.0, hs.1, hs.1)
}"

# mru_rules::index_of: a found place holds the id, and None means the id is not listed.
try mru_rules "        if ids[i].0 == id.0 {" "        if ids[i].0 != id.0 {"
try mru_rules "    while i < len && i < MAX
" "    while i + 1 < len && i < MAX
"
# mru_rules::focus: a full list refuses only a new app; a new app is counted.
try mru_rules "            if *len < MAX {" "            if *len + 1 < MAX {"
try mru_rules "                push_front(ids, *len, id);
                *len += 1;" "                push_front(ids, *len, id);"
# mru_rules::remove: the ended app goes, from its own place, and is no longer counted.
try mru_rules "            remove_at(ids, *len, i);
            *len -= 1;" "            remove_at(ids, *len, i);"
try mru_rules "            remove_at(ids, *len, i);" "            remove_at(ids, *len, 0);"
try mru_rules "            Some(i)
        }
        None => None," "            Some(0)
        }
        None => None,"
echo "refused $refused mutants"
echo "mutants: $refused refused by verification, $weak refused without a verification run"
if [ "${MUTANTS_STRICT:-0}" = "1" ] && [ "$weak" -gt 0 ]; then exit 1; fi
exit "$fail"
