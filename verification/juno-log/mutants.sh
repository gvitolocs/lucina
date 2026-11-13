#!/usr/bin/env bash
# Shows that kernel/lucina-log/src/rules.rs is what keeps Verus on it: each line plants one bug in a copy of the shipped
# file, turned into what Verus reads exactly as scripts/verify-shipped.sh does, and Verus must refuse every mutant.
# Run on buildhost from the repo root, under verify-gate: verify-gate -- bash verification/juno-log/mutants.sh
set -uo pipefail
verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
rules=kernel/lucina-log/src/rules.rs
work=$(mktemp -d "${TMPDIR:-/tmp}/juno-log-mutants.XXXXXX")
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

fail=0 refused=0 weak=0
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
    elif grep -q 'verification results' "$work/verus.out"; then
        refused=$((refused + 1))
        echo "refused: $from -> $to ($(grep -c "^error" "$work/verus.out") errors)"
    else
        # Refused before any verification ran (a compile or other error): that says nothing about the statements.
        weak=$((weak + 1))
        fail=1
        echo "refused WITHOUT A VERIFICATION RUN (compile or other error): $from -> $to"
        cat "$work/verus.out"
    fi
}
# The rate limiter: burst is a hard ceiling, held records are counted, the report is exactly what the window held, the window ends on time.
try "                if w.admitted < burst {" "                if w.admitted <= burst {"
try "                            suppressed: if w.suppressed < u64::MAX {
                                w.suppressed + 1" "                            suppressed: if w.suppressed < u64::MAX {
                                w.suppressed"
try "                    report: w.suppressed," "                    report: 0,"
try "            if w.start_ms <= now_ms && now_ms - w.start_ms < window_ms {" "            if w.start_ms <= now_ms && now_ms - w.start_ms <= window_ms {"
try "                            admitted: w.admitted + 1," "                            admitted: w.admitted,"
# Added by the prover (Opus): a clock that went back must start a new window, a held record must not pass, a report must not
# ride on a record inside the window, the new window must start now from 0 held, and the first window holds its own record.
try "            if w.start_ms <= now_ms && now_ms - w.start_ms < window_ms {" "            if now_ms - w.start_ms < window_ms {"
try "                        admit: false," "                        admit: true,"
try "                        admit: true,
                        report: 0," "                        admit: true,
                        report: w.suppressed,"
try "                    report: w.suppressed,
                    win: Window {
                        start_ms: now_ms,
                        admitted: 1,
                        suppressed: 0," "                    report: w.suppressed,
                    win: Window {
                        start_ms: now_ms,
                        admitted: 1,
                        suppressed: w.suppressed,"
try "                    report: w.suppressed,
                    win: Window {
                        start_ms: now_ms," "                    report: w.suppressed,
                    win: Window {
                        start_ms: w.start_ms,"
try "            report: 0,
            win: Window {
                start_ms: now_ms,
                admitted: 1," "            report: 0,
            win: Window {
                start_ms: now_ms,
                admitted: 0,"
# The lazy re-check: a clock that went back is due, and a second is due.
try "        Some(l) => now_ms < l || now_ms - l >= interval_ms," "        Some(l) => now_ms - l >= interval_ms,"
try "        Some(l) => now_ms < l || now_ms - l >= interval_ms," "        Some(l) => now_ms < l || now_ms - l > interval_ms,"
# The key alphabet and the byte budget.
try "    (b >= 97 && b <= 122) || (b >= 48 && b <= 57) || b == 95" "    (b >= 97 && b <= 122) || (b >= 48 && b <= 58) || b == 95"
try "    if used < max { max - used } else { 0 }" "    if used <= max { max - used + 1 } else { 0 }"
# Added by the prover (Opus): the first look is due, over budget is 0. The key alphabet follows docs/design/juno-logging.md
# section 2 since AMD-lucina-log-rules-01: `.` IS a key byte, so the earlier mutant "accept `.`" is now "refuse `.`", and the
# neighbours `-` (45) and `/` (47) must stay out.
try "        None => true,
        Some(l) => now_ms < l" "        None => false,
        Some(l) => now_ms < l"
try "    (b >= 97 && b <= 122) || (b >= 48 && b <= 57) || b == 95 || b == 46" "    (b >= 97 && b <= 122) || (b >= 48 && b <= 57) || b == 95"
try "    (b >= 97 && b <= 122) || (b >= 48 && b <= 57) || b == 95 || b == 46" "    (b >= 97 && b <= 122) || (b >= 48 && b <= 57) || b == 95 || b == 45"
try "    (b >= 97 && b <= 122) || (b >= 48 && b <= 57) || b == 95 || b == 46" "    (b >= 97 && b <= 122) || (b >= 48 && b <= 57) || b == 95 || b == 46 || b == 47"
try "    if used < max { max - used } else { 0 }" "    if used < max { max - used } else { 1 }"
echo "mutants: $refused refused by verification, $weak refused without a verification run (each of those fails the run)"
exit "$fail"
