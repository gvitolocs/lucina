#!/usr/bin/env bash
# Shows that kernel/lucina-spinwatch/src/rules.rs is what keeps Verus on it: each line plants one bug in a copy of the shipped
# file, turned into what Verus reads exactly as scripts/verify-shipped.sh does, and Verus must refuse every mutant.
# Run on buildhost from the repo root, under verify-gate: verify-gate -- verification/juno-spinwatch/mutants.sh
set -uo pipefail
verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
rules=kernel/lucina-spinwatch/src/rules.rs
work=$(mktemp -d "${TMPDIR:-/tmp}/juno-spinwatch-mutants.XXXXXX")
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
        if grep -q 'verification results' "$work/verus.out"; then
            refused=$((refused + 1))
            echo "refused: $from -> $to ($(grep -c "^error" "$work/verus.out") errors)"
        else
            echo "refused (NO VERIFICATION RUN: compile or other error): $from -> $to ($(grep -c "^error" "$work/verus.out") errors)"
            weak=$((weak+1))
        fi
    fi
}
# The wait: a counter behind the start is no wait.
try "    now.saturating_sub(start)" "    now.wrapping_sub(start)"
# Off is off.
try "    if let Mode::Off = mode {
        return Action::Nothing;
    }" ""
# Five seconds act, from the fifth second on, and crash means crash.
try "    if w >= act_after {" "    if w > act_after {"
try "            Mode::Crash => Action::Crash," "            Mode::Crash => Action::Report,"
# A report is said once.
try "                Stage::Reported => Action::Nothing," "                Stage::Reported => Action::Report,"
# A second marks, from the first second on, once.
try "    if w >= mark_after {" "    if w > mark_after {"
try "            Stage::Quiet => Action::Mark," "            Stage::Quiet => Action::Nothing,"
try "            Stage::Quiet => Action::Mark,
            _ => Action::Nothing," "            Stage::Quiet => Action::Mark,
            _ => Action::Mark,"
# A report or a crash is the last word.
try "        Action::Report | Action::Crash => Stage::Reported," "        Action::Report | Action::Crash => Stage::Marked,"
# A CPU that waits for the lock does not hold it; one that waits for nothing may; one that waits for another lock may too.
try "    if cpu >= waits.len() || lock == 0 || waits[cpu] == lock {" "    if cpu >= waits.len() || lock == 0 {"
try "    if cpu >= waits.len() || lock == 0 || waits[cpu] == lock {" "    if cpu >= waits.len() || waits[cpu] == lock {"
try "    if waits[cpu] == 0 {" "    if waits[cpu] != lock {"
# The count of waiters.
try "        if lock != 0 && waits[k] == lock {" "        if waits[k] == lock {"
try "            count += 1;" "            count += 2;"
echo "mutants: $refused refused by verification, $weak refused without a verification run"
if [ "${MUTANTS_STRICT:-0}" = "1" ] && [ "$weak" -gt 0 ]; then exit 1; fi
exit "$fail"
