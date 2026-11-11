#!/usr/bin/env bash
# Shows that kernel/lucina-oom/src/rules.rs is what keeps Verus on it: each line plants one bug in a copy of the shipped
# file, turned into what Verus reads exactly as scripts/verify-shipped.sh does, and Verus must refuse every mutant.
# Run on buildhost from the repo root, under verify-gate: verify-gate -- verification/juno-oom/mutants.sh
set -uo pipefail
verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
rules=kernel/lucina-oom/src/rules.rs
work=$(mktemp -d "${TMPDIR:-/tmp}/juno-oom-mutants.XXXXXX")
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
# The score: -1000 is never a candidate, adj is scaled to the machine, rss counts.
try "    if adj == ADJ_MIN {
        return None;" "    if adj == ADJ_MIN + 1 {
        return None;"
try "    let scale = (total / 1000) as i64;" "    let scale = (total / 100) as i64;"
try "    Some(rss as i64 + adj as i64 * scale)" "    Some(adj as i64 * scale)"
# The choice: the highest, the last of equals, nobody when all are None.
try "            if best.is_none() || s >= best_score {" "            if best.is_none() || s > best_score {"
try "            if best.is_none() || s >= best_score {" "            if best.is_none() || s <= best_score {"
try "            if best.is_none() || s >= best_score {" "            if s >= best_score {"
try "        k += 1;
    }
    best" "        k += 1;
    }
    if best.is_none() { Some(0) } else { best }"
# The reserve stays free.
try "    free > reserve" "    free >= reserve"
# exec: protected gets -1000, inherited full protection drops to 0, anything else stays.
try "    if protected {
        ADJ_MIN" "    if protected {
        ADJ_MIN + 1"
try "    } else if inherited == ADJ_MIN {
        0" "    } else if inherited == ADJ_MIN {
        ADJ_MIN"
try "    } else {
        inherited
    }" "    } else {
        0
    }"
echo "mutants: $refused refused by verification, $weak refused without a verification run"
if [ "${MUTANTS_STRICT:-0}" = "1" ] && [ "$weak" -gt 0 ]; then exit 1; fi
exit "$fail"
