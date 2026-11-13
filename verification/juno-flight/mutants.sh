#!/usr/bin/env bash
# Shows that kernel/lucina-flight/src/rules.rs is what keeps Verus on it: each line plants one bug in a copy of the shipped
# file, turned into what Verus reads exactly as scripts/verify-shipped.sh does, and Verus must refuse every mutant.
# Run on buildhost from the repo root, under verify-gate: verify-gate -- verification/juno-flight/mutants.sh
set -uo pipefail
verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
rules=kernel/lucina-flight/src/rules.rs
work=$(mktemp -d "${TMPDIR:-/tmp}/juno-flight-mutants.XXXXXX")
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
# judge: equal is in order, lower is behind, higher is a gap from exactly what was due to exactly what came.
# A duplicate judged in order.
try "    if seq == expected {" "    if seq <= expected {"
# A gap judged behind (dropped in silence), and a duplicate judged a gap.
try "    } else if seq < expected {" "    } else if seq > expected {"
# A gap one too long at the end, one too short at the start.
try "            from: expected,
            to: seq," "            from: expected,
            to: seq + 1,"
try "            from: expected,
            to: seq," "            from: expected + 1,
            to: seq,"
# An empty gap reported for the record that was due.
try "        Seq::InOrder
    } else if" "        Seq::Gap { from: expected, to: seq }
    } else if"
# A duplicate judged in order.
try "        Seq::Behind
    } else" "        Seq::InOrder
    } else"
# after: one higher, and no wrap at the top.
try "    if seq < u64::MAX { seq + 1 } else { u64::MAX }" "    if seq < u64::MAX { seq + 2 } else { u64::MAX }"
try "    if seq < u64::MAX { seq + 1 } else { u64::MAX }" "    if seq < u64::MAX { seq + 1 } else { 0 }"
try "    if seq < u64::MAX { seq + 1 } else { u64::MAX }" "    if seq <= u64::MAX { seq + 1 } else { u64::MAX }"
# take_step: an in-order record that does not move the cursor (the next one would be a duplicate), one that overflows at the
# top instead of staying there.
try "        Seq::InOrder => (after(seq), Taken::Keep { lost: None })," "        Seq::InOrder => (seq, Taken::Keep { lost: None }),"
try "        Seq::InOrder => (after(seq), Taken::Keep { lost: None })," "        Seq::InOrder => (seq + 1, Taken::Keep { lost: None }),"
# A gap kept but not reported (silent loss), reported one short at either end.
try "                lost: Some((from, to))," "                lost: None,"
try "                lost: Some((from, to))," "                lost: Some((from, to - 1)),"
try "                lost: Some((from, to))," "                lost: Some((from + 1, to)),"
# A gap that leaves the cursor at the record that came (it would be kept twice), or at what was due (the same range would be
# reported again by the next record), or one past what was due.
try "        Seq::Gap { from, to } => (
            after(seq)," "        Seq::Gap { from, to } => (
            seq,"
try "        Seq::Gap { from, to } => (
            after(seq)," "        Seq::Gap { from, to } => (
            expected,"
try "        Seq::Gap { from, to } => (
            after(seq)," "        Seq::Gap { from, to } => (
            after(from),"
# A duplicate that moves the cursor (on, or back to the duplicate), or is kept.
try "        Seq::Behind => (expected, Taken::Duplicate)," "        Seq::Behind => (after(expected), Taken::Duplicate),"
try "        Seq::Behind => (expected, Taken::Duplicate)," "        Seq::Behind => (seq, Taken::Duplicate),"
try "        Seq::Behind => (expected, Taken::Duplicate)," "        Seq::Behind => (expected, Taken::Keep { lost: None }),"
# skip_step: never back, always past `to`, no further.
try "    if to > expected { to } else { expected }" "    if to > expected { to } else { to }"
try "    if to > expected { to } else { expected }" "    if to > expected { expected } else { expected }"
try "    if to > expected { to } else { expected }" "    if to > expected { to } else { 0 }"
try "    if to > expected { to } else { expected }" "    if to < expected { to } else { expected }"
# Added after the review of record (GitHub Copilot, gpt-5.6-luna): a gap reported reversed, a gap that advances with `to + 1`
# (overflows at the top instead of staying there), a skip that lands one past `to` (the record at `to` would then be dropped).
try "                lost: Some((from, to))," "                lost: Some((to, from)),"
try "        Seq::Gap { from, to } => (
            after(seq)," "        Seq::Gap { from, to } => (
            to + 1,"
try "    if to > expected { to } else { expected }" "    if to > expected { to + 1 } else { expected }"
echo "mutants: $refused refused by verification, $weak refused without a verification run"
if [ "${MUTANTS_STRICT:-0}" = "1" ] && [ "$weak" -gt 0 ]; then exit 1; fi
exit "$fail"
