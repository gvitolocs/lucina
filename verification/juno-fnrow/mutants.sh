#!/usr/bin/env bash
# Shows that verification/juno-fnrow/rules.rs can fail: each line plants one bug in a copy of the shipped file, and Verus must refuse it.
# Run on buildhost from the repo root, under verify-gate (or on buildhost2 with verus-remote).
set -uo pipefail
verus=${VERUS:-verus}
[ -x /home/user/tools/verus/verus-x86-linux/verus ] && verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
work=$(mktemp -d "${TMPDIR:-/tmp}/juno-fnrow-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
fail=0 refused=0 total=0
weak=0
src=drivers/lucina-i2c-hid/src/fnrow.rs
try() {
    local from=$1 to=$2
    local shown="${from//$'\n'/\\n} -> ${to//$'\n'/\\n}"
    total=$((total + 1))
    mkdir -p "$work/drivers/lucina-i2c-hid/src" "$work/verification/juno-fnrow"
    cp $src "$work/drivers/lucina-i2c-hid/src/"
    cp verification/juno-fnrow/rules.rs "$work/verification/juno-fnrow/"
    local target=$work/$src
    local text; text=$(<"$target")
    case "$text" in
        *"$from"*) ;;
        *) echo "NOT FOUND: ${shown% -> *}"; fail=1; return ;;
    esac
    printf "%s\n" "${text/"$from"/"$to"}" > "$target"
    if cmp -s "$target" $src; then echo "NOT CHANGED: ${shown% -> *}"; fail=1; return; fi
    if "$verus" "$work/verification/juno-fnrow/rules.rs" > "$work/out" 2>&1; then
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
if "$verus" verification/juno-fnrow/rules.rs > "$work/baseline" 2>&1; then
    echo "baseline: verifies"
else
    cat "$work/baseline"; echo "mutants: the unchanged files do not verify"; exit 1
fi
# A row key loses its hot key, or gets another one.
try 'KEY_F2 => Some(KEY_BRIGHTNESSDOWN),' 'KEY_F2 => Some(KEY_BRIGHTNESSUP),'
try 'KEY_F5 => Some(KEY_F21),' 'KEY_F5 => None,'
try 'KEY_F12 => Some(KEY_RFKILL),' 'KEY_F12 => Some(KEY_F12),'
# The way back does not match the way there.
try 'KEY_F3 => Some(KEY_BRIGHTNESSUP),' 'KEY_F3 => Some(KEY_BRIGHTNESSDOWN),'
try 'KEY_MUTE => Some(KEY_F6),' 'KEY_MUTE => Some(KEY_F7),'
try 'KEY_VOLUMEDOWN => Some(KEY_F7),' 'KEY_VOLUMEDOWN => Some(KEY_F8),'
try 'KEY_CONFIG => Some(KEY_F1),' 'KEY_CONFIG => None,'
try 'KEY_DISPLAY_OFF => Some(KEY_F9),' 'KEY_DISPLAY_OFF => Some(68),'
# A key outside the row is changed, or the order of the two lookups breaks the inverse.
try '    } else {
        #[cfg(verus_keep_ghost)]' '    } else if code == 30 {
        return 31;
    } else {
        #[cfg(verus_keep_ghost)]'
try 'if let Some(h) = hot_key(code) {
        h' 'if let Some(h) = hot_key(code) {
        code'
echo "juno-fnrow mutants: $refused of $total refused"
echo "mutants: $refused refused by verification, $weak refused without a verification run"
if [ "${MUTANTS_STRICT:-0}" = "1" ] && [ "$weak" -gt 0 ]; then exit 1; fi
exit $fail
