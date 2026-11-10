#!/usr/bin/env bash
# Shows that verification/juno-fnkeys/rules.rs can fail: each line plants one bug in a copy of the shipped file, and Verus must refuse it.
# Run on buildhost from the repo root, under verify-gate (or on buildhost2 with verus-remote).
set -uo pipefail
verus=${VERUS:-verus}
[ -x /home/user/tools/verus/verus-x86-linux/verus ] && verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
work=$(mktemp -d "${TMPDIR:-/tmp}/juno-fnkeys-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
fail=0 refused=0 total=0
src=apps/lucina-shell/src/fnkeys.rs
try() {
    local from=$1 to=$2
    local shown="${from//$'\n'/\\n} -> ${to//$'\n'/\\n}"
    total=$((total + 1))
    mkdir -p "$work/apps/lucina-shell/src" "$work/verification/juno-fnkeys"
    cp $src "$work/apps/lucina-shell/src/"
    cp verification/juno-fnkeys/rules.rs "$work/verification/juno-fnkeys/"
    local target=$work/$src
    local text; text=$(<"$target")
    case "$text" in
        *"$from"*) ;;
        *) echo "NOT FOUND: ${shown% -> *}"; fail=1; return ;;
    esac
    printf "%s\n" "${text/"$from"/"$to"}" > "$target"
    if cmp -s "$target" $src; then echo "NOT CHANGED: ${shown% -> *}"; fail=1; return; fi
    if "$verus" "$work/verification/juno-fnkeys/rules.rs" > "$work/out" 2>&1; then
        echo "NOT REFUSED: $shown"; fail=1
    else
        refused=$((refused + 1))
        echo "refused: $shown ($(grep -c "^error" "$work/out") errors)"
    fi
}
# Keys swapped or lost.
try 'KEY_BRIGHTNESSUP => Action::Brightness(true),' 'KEY_BRIGHTNESSUP => Action::Brightness(false),'
try 'KEY_VOLUMEDOWN => Action::Volume(false),' 'KEY_VOLUMEDOWN => Action::Volume(true),'
try 'KEY_MUTE => Action::Mute,' 'KEY_MUTE => Action::Other,'
try 'KEY_RFKILL => Action::Radio,' 'KEY_RFKILL => Action::Mute,'
try 'KEY_F21 => Action::Touchpad,' 'KEY_F21 => Action::Other,'
try 'KEY_DISPLAY_OFF => Action::DisplayOff,' 'KEY_DISPLAY_OFF => Action::Settings,'
# A toggle repeats, or a level does not.
try 'matches!(a, Action::Brightness(_) | Action::Volume(_))' 'matches!(a, Action::Brightness(_) | Action::Volume(_) | Action::Mute)'
try 'matches!(a, Action::Brightness(_) | Action::Volume(_))' 'matches!(a, Action::Brightness(_))'
# The volume leaves its range or moves the wrong way.
try 'if level < VOLUME_MAX {
            level + 1
        } else {
            VOLUME_MAX
        }' 'level + 1'
try 'if level < VOLUME_MAX {
            level + 1
        } else {
            VOLUME_MAX
        }' 'if level < VOLUME_MAX {
            level + 2
        } else {
            VOLUME_MAX
        }'
try '    } else if level > 0 {
        level - 1' '    } else if level > 0 {
        level'
try 'let level = if level > VOLUME_MAX {
        VOLUME_MAX
    } else {
        level
    };' 'let level = level;'
# The decibels go above the program's own level.
try '-((VOLUME_MAX - level) as i32) * VOLUME_STEP_DB
}' '((VOLUME_MAX - level) as i32) * VOLUME_STEP_DB
}'
# The backlight leaves its range, or a press switches the screen off.
try 'if level > BRIGHTNESS_MAX - BRIGHTNESS_STEP {
            BRIGHTNESS_MAX
        } else {
            level + BRIGHTNESS_STEP
        }' 'level + BRIGHTNESS_STEP'
try '    } else if level > 0 {
        1' '    } else if level > 0 {
        0'
try 'let level = if level > BRIGHTNESS_MAX {
        BRIGHTNESS_MAX
    } else {
        level
    };' 'let level = level;'
echo "juno-fnkeys mutants: $refused of $total refused"
exit $fail
