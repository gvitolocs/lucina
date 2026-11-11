#!/usr/bin/env bash
# Shows that verification/juno-ec/rules.rs can fail: each line plants one bug in a copy of the shipped file, and Verus must refuse it.
# Run on buildhost from the repo root, under verify-gate (or on buildhost2 with verus-remote).
set -uo pipefail
verus=${VERUS:-verus}
[ -x /home/user/tools/verus/verus-x86-linux/verus ] && verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
work=$(mktemp -d "${TMPDIR:-/tmp}/juno-ec-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
fail=0 refused=0 total=0
weak=0
src=drivers/lucina-i2c-hid/src/samsung_ec.rs
try() {
    local from=$1 to=$2
    local shown="${from//$'\n'/\\n} -> ${to//$'\n'/\\n}"
    total=$((total + 1))
    mkdir -p "$work/drivers/lucina-i2c-hid/src" "$work/verification/juno-ec"
    cp $src "$work/drivers/lucina-i2c-hid/src/"
    cp verification/juno-ec/rules.rs "$work/verification/juno-ec/"
    local target=$work/$src
    local text; text=$(<"$target")
    case "$text" in
        *"$from"*) ;;
        *) echo "NOT FOUND: ${shown% -> *}"; fail=1; return ;;
    esac
    printf "%s\n" "${text/"$from"/"$to"}" > "$target"
    if cmp -s "$target" $src; then echo "NOT CHANGED: ${shown% -> *}"; fail=1; return; fi
    if "$verus" "$work/verification/juno-ec/rules.rs" > "$work/out" 2>&1; then
        echo "NOT REFUSED: $shown"; fail=1
    else
        if grep -qE 'verification results|simplifies to|not satisfied|assertion failed|possible arithmetic|may not terminate|cannot show|recommendation not met' "$work/out" && ! grep -q 'error\[E' "$work/out"; then
            refused=$((refused + 1))
            echo "refused: $shown ($(grep -c "^error" "$work/out") errors)"
        else
            echo "refused (NO VERIFICATION RUN: compile or other error): $shown ($(grep -c "^error" "$work/out") errors)"
            weak=$((weak+1))
        fi
    fi
}
# The unchanged files must verify, or every "refused" below would mean nothing.
if "$verus" verification/juno-ec/rules.rs > "$work/baseline" 2>&1; then
    echo "baseline: verifies"
else
    cat "$work/baseline"; echo "mutants: the unchanged files do not verify"; exit 1
fi
# A read that is not an event is taken as one, or the code or value is taken from the wrong byte.
try 'if buf[0] != TYPE_EVENT {' 'if buf[0] == 0xff {'
try 'let value = buf[3];' 'let value = buf[1];'
try 'Some(match buf[2] {' 'Some(match buf[1] {'
# The codes are mixed up.
try 'CODE_FN_LOCK => EcEvent::FnLock(state(value)),' 'CODE_FN_LOCK => EcEvent::CapsLock(state(value)),'
try 'CODE_SCREEN => EcEvent::Screen(state(value)),' 'CODE_SCREEN => EcEvent::Other { code: 9, value },'
# A value is read wrongly.
try '        1 => Some(true),' '        1 => Some(false),'
try '        0 => Some(false),' '        0 => None,'
# An unknown value does not flip, or a known one is ignored.
try 'None => !previous,' 'None => previous,'
try 'Some(on) => on,' 'Some(on) => !on,'
# The swap ignores the keyboard's Fn Lock.
try 'hot_keys_wanted != keyboard_fn_lock' 'hot_keys_wanted'
echo "juno-ec mutants: $refused of $total refused"
echo "mutants: $refused refused by verification, $weak refused without a verification run"
if [ "${MUTANTS_STRICT:-0}" = "1" ] && [ "$weak" -gt 0 ]; then exit 1; fi
exit $fail
