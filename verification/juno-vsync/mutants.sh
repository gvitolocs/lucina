#!/usr/bin/env bash
# Shows that verification/juno-vsync/rules.rs can fail: each line plants one bug in a copy of the shipped file, and Verus must refuse it.
# Run on buildhost from the repo root, under verify-gate (or on buildhost2 with verus-remote).
set -uo pipefail
verus=${VERUS:-verus}
[ -x /home/user/tools/verus/verus-x86-linux/verus ] && verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
work=$(mktemp -d "${TMPDIR:-/tmp}/juno-vsync-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
fail=0 refused=0 total=0
src=drivers/lucina-adreno/src/scan_rules.rs
try() {
    local from=$1 to=$2
    local shown="${from//$'\n'/\\n} -> ${to//$'\n'/\\n}"
    total=$((total + 1))
    mkdir -p "$work/drivers/lucina-adreno/src" "$work/verification/juno-vsync"
    cp $src "$work/drivers/lucina-adreno/src/"
    cp verification/juno-vsync/rules.rs "$work/verification/juno-vsync/"
    local target=$work/$src
    local text; text=$(<"$target")
    case "$text" in
        *"$from"*) ;;
        *) echo "NOT FOUND: ${shown% -> *}"; fail=1; return ;;
    esac
    printf "%s\n" "${text/"$from"/"$to"}" > "$target"
    if cmp -s "$target" $src; then echo "NOT CHANGED: ${shown% -> *}"; fail=1; return; fi
    if "$verus" "$work/verification/juno-vsync/rules.rs" > "$work/out" 2>&1; then
        echo "NOT REFUSED: $shown"; fail=1
    else
        refused=$((refused + 1))
        echo "refused: $shown ($(grep -c "^error" "$work/out") errors)"
    fi
}
# The batch starts while the scan is on its rows, or waits when it need not.
try 'r >= height || r as i64 >= bottom as i64' 'r >= height || r as i64 > bottom as i64'
try 'height <= 0 || r < 0 || r >= height' 'height <= 0 || r <= 0 || r >= height'
try 'height <= 0 || r < 0 || r >= height' 'height <= 0 || r < 0 || r > height'
try 'None => Scan::Go,' 'None => Scan::WaitVblank,'
try '            } else {
                Scan::WaitVblank' '            } else {
                Scan::Go'
try 'r as i64 >= bottom as i64' 'r as i64 >= bottom as i64 - 1'
echo "juno-vsync mutants: $refused of $total refused"
exit $fail
