#!/usr/bin/env bash
# Shows that services/lucina-wm-rules/src/{snap_rules,mru_rules,tile_rules}.rs are what keeps Verus on them: each line plants one bug
# in a copy of a shipped file, turned into what Verus reads exactly as scripts/verify-shipped.sh does, and Verus must refuse every
# mutant. Run on buildhost from the repo root, under verify-gate: verify-gate -- verification/juno-wm-rules/mutants.sh
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

fail=0
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
    else
        refused=$((refused + 1))
        echo "refused: $1: $from -> $to ($(grep -c "^error" "$work/verus.out") errors)"
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
    //@ assert(forall|m: int|" "        ids[i] = ids[i - 1];
        i -= 1;
    }
    //@ assert(forall|m: int|"
try mru_rules "    let mut i = len;
    while i > 0" "    let mut i = len;
    while i > 1"
try mru_rules "    ids[0] = id;
    //@ assert(forall|v: T| old(ids)@.take(len as int).contains(v) ==> ids@.take(len as int + 1)" "    ids[1] = id;
    //@ assert(forall|v: T| old(ids)@.take(len as int).contains(v) ==> ids@.take(len as int + 1)"
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
    //@ assert(0 <= avail" "        2000
    } else {
        ratio_permille
    };
    //@ assert(0 <= avail"
try tile_rules "    if 2 * min_w > w {" "    if 2 * min_w < w {"
try tile_rules "    let lo = (min_w * 1000 + w - 1) / w;" "    let lo = (min_w * 1000) / w;"
try tile_rules "    let hi = 1000 - lo;" "    let hi = 1000 - lo + 1;"
try tile_rules "    if w <= 0 || min_w <= 0 {" "    if w < 0 || min_w <= 0 {"
try tile_rules "    } else if ratio > hi {
        hi" "    } else if ratio > hi {
        ratio"
echo "refused $refused mutants"
exit "$fail"
