#!/usr/bin/env bash
# Shows that Verus can fail on drivers/lucina-i2c-hid/src/tp_rules.rs: each line plants one bug in a copy of the shipped file, and Verus must refuse it.
# Run on buildhost from the repo root, under verify-gate.
set -uo pipefail
verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
file=drivers/lucina-i2c-hid/src/tp_rules.rs
work=$(mktemp -d "${TMPDIR:-/tmp}/juno-touchpad-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
fail=0
refused=0
weak=0
try() {
    local from=$1 to=$2
    local text; text=$(<"$file")
    case "$text" in
        *"$from"*) ;;
        *) echo "NOT FOUND: $from"; fail=1; return ;;
    esac
    printf '%s\n' "${text/"$from"/"$to"}" > "$work/mutant.rs"
    local body; body=$(sed -E -e '/\/\/@omit$/d' -e 's/-> (.*[^ ]) \/\/@ret$/-> (ret: \1)/' -e 's/^( *)\/\/@ ?/\1\/*proof*\/ /' "$work/mutant.rs")
    printf 'use vstd::prelude::*;\nverus! {\n%s\n}\nfn main() {}\n' "$body" > "$work/rules.rs"
    if "$verus" "$work/rules.rs" > "$work/out" 2>&1; then
        echo "NOT REFUSED: $from -> $to"; fail=1
    else
        if grep -qE 'verification results|simplifies to|not satisfied|assertion failed|possible arithmetic|may not terminate|cannot show|recommendation not met' "$work/out" && ! grep -q 'error\[E' "$work/out"; then
            refused=$((refused + 1))
            echo "refused: $from -> $to"
        else
            echo "refused (NO VERIFICATION RUN: compile or other error): $from -> $to"
            weak=$((weak+1))
        fi
    fi
}
# The unchanged files must verify, or every "refused" below would mean nothing.
base_body=$(sed -E -e '/\/\/@omit$/d' -e 's/-> (.*[^ ]) \/\/@ret$/-> (ret: \1)/' -e 's/^( *)\/\/@ ?/\1\/*proof*\/ /' "$file")
printf 'use vstd::prelude::*;\nverus! {\n%s\n}\nfn main() {}\n' "$base_body" > "$work/base.rs"
if "$verus" "$work/base.rs" > "$work/baseline" 2>&1; then
    echo "baseline: verifies"
else
    cat "$work/baseline"; echo "mutants: the unchanged files do not verify"; exit 1
fi
# Slots: a free slot that was used in this frame taken, the key test dropped, slots past n searched.
try "if !active[i] && !used[i] {" "if !active[i] {"
try "if active[i] && keys[i] == key {" "if active[i] {"
try "    while i < n
    //@ invariant n <= SLOTS, i <= n, forall|j: int| 0 <= j < i ==> !(active[j] && keys[j] == key)," "    while i < SLOTS
    //@ invariant n <= SLOTS, i <= SLOTS, forall|j: int| 0 <= j < i ==> !(active[j] && keys[j] == key),"
# Frames: a count of 0 taken as a new frame without a new scan time, one contact too many, a frame ending early.
try "(num_received == 0 && scan_changed) || count != 0" "(num_received == 0) || count != 0"
try "!have_count || num_received < num_expected
}" "!have_count || num_received <= num_expected
}"
try "    num_received >= num_expected
}" "    num_received + 1 >= num_expected
}"
# Gestures: a scroll that ends without its stop, a stop outside a scroll, a pinch start that keeps scrolling.
try "                r.stop_scroll = true;
                r.cancel_timer = true;
                r.next = GestureState::None;" "                r.cancel_timer = true;
                r.next = GestureState::None;"
try "                r.stop_scroll = true;
                r.cancel_timer = true;
                r.init_pinch = true;
                r.next = GestureState::PinchStart;" "                r.cancel_timer = true;
                r.init_pinch = true;
                r.next = GestureState::Scroll;"
try "GestureEvent::FingerDetected => r.next = GestureState::Unknown," "GestureEvent::FingerDetected => r.next = GestureState::Scroll,"
try "                r.buildup = true;
                r.next = GestureState::ScrollStart;" "                r.buildup = true;
                r.stop_scroll = true;
                r.next = GestureState::ScrollStart;"
# Hold, swipe and pinch notifications: a dropped end, a begin from the wrong state, a cancel flag swapped, a hold end missing on
# motion, a swipe end missing on a finger change, a pinch begin without PINCH_START.
try "                r.end = Some((GestureKind::Hold, is_cancel(event)));" "                r.end = None;"
try "GestureEvent::HoldAndMotionStart => r.next = GestureState::HoldAndMotion," "GestureEvent::HoldAndMotionStart => {
                r.begin = Some(GestureKind::Hold);
                r.next = GestureState::HoldAndMotion;
            }"
try "                r.end = Some((GestureKind::Hold, is_cancel(event)));" "                r.end = Some((GestureKind::Hold, !is_cancel(event)));"
try "                r.end = Some((GestureKind::Hold, true));
                r.cancel_timer = true;
                r.next = GestureState::PointerMotion;" "                r.cancel_timer = true;
                r.next = GestureState::PointerMotion;"
try "                r.end = Some((GestureKind::Swipe, true));" "                r.end = None;"
try "            GestureEvent::PinchBegin => {
                r.next = GestureState::Pinch;" "            GestureEvent::SwipeBegin => {
                r.next = GestureState::Pinch;"
try "                r.init_pinch = true;
                r.next = GestureState::PinchStart;
            }
            _ => {}
        },
        GestureState::Hold => match event {" "                r.init_pinch = true;
                r.begin = Some(GestureKind::Pinch);
                r.next = GestureState::PinchStart;
            }
            _ => {}
        },
        GestureState::Hold => match event {"
try "                r.next = GestureState::Swipe;
                r.begin = Some(GestureKind::Swipe);" "                r.next = GestureState::Swipe;"
try "                r.next = GestureState::HoldAndMotion;
                r.begin = Some(GestureKind::Hold);" "                r.next = GestureState::HoldAndMotion;"
try "                r.end = Some((GestureKind::Pinch, true));" "                r.end = Some((GestureKind::Pinch, false));"
try "                r.end = Some((GestureKind::Swipe, is_cancel(event)));" "                r.end = Some((GestureKind::Pinch, is_cancel(event)));"
try "                r.hold_buildup = true;" "                r.buildup = true;"
try "                r.end = Some((GestureKind::Hold, true));
                r.cancel_timer = true;
                r.init_pinch = true;" "                r.end = Some((GestureKind::Hold, true));
                r.cancel_timer = true;"
# Finger count: all fingers up without ending, a switch that waits while not debouncing, a pending count kept.
try "FingerUpdate {
            count: 0,
            pending: 0,
            end: true,
            reset: false,
            arm: false,
        }" "FingerUpdate {
            count: 0,
            pending: 0,
            end: false,
            reset: false,
            arm: false,
        }"
try "FingerUpdate {
            count: active,
            pending: 0,
            end: false,
            reset: resettable,
            arm: false,
        }" "FingerUpdate {
            count,
            pending: active,
            end: false,
            reset: resettable,
            arm: true,
        }"
try "let none = FingerUpdate {
        count,
        pending: 0," "let none = FingerUpdate {
        count,
        pending,"
# Notches: rounding instead of truncation, the remainder dropped.
try "let lo = sum / 120;" "let lo = (sum + 60) / 120;"
try "(lo, sum - lo * 120)" "(lo, 0)"
# Gesture step, prover's additions: a hold end while the hold goes on, both buildups dropped, a hold timer left running on a
# pinch start, Hold + PinchStart ignored, and events a state does not take acted on (SwipeStart, PointerMotion, Unknown).
try "GestureEvent::HoldAndMotionStart => r.next = GestureState::HoldAndMotion," "GestureEvent::HoldAndMotionStart => {
                r.end = Some((GestureKind::Hold, true));
                r.next = GestureState::HoldAndMotion;
            }"
try "r.buildup = true;" "r.buildup = false;"
try "r.hold_buildup = true;" "r.hold_buildup = false;"
try "                r.stop_scroll = true;
                r.cancel_timer = true;
                r.init_pinch = true;" "                r.stop_scroll = true;
                r.init_pinch = true;"
try "                r.end = Some((GestureKind::Hold, true));
                r.cancel_timer = true;
                r.init_pinch = true;
                r.next = GestureState::PinchStart;" "                r.end = Some((GestureKind::Hold, true));
                r.cancel_timer = true;
                r.next = GestureState::None;"
try "            GestureEvent::SwipeBegin => {
                r.next = GestureState::Swipe;" "            GestureEvent::PointerMotionStart => r.next = GestureState::PointerMotion,
            GestureEvent::SwipeBegin => {
                r.next = GestureState::Swipe;"
try "            // The caller sends this only for one finger" "            GestureEvent::ScrollStart => r.next = GestureState::ScrollStart,
            // The caller sends this only for one finger"
try "            // The hold timer keeps running" "            GestureEvent::FingerSwitchTimeout => r.next = GestureState::None,
            // The hold timer keeps running"
echo "mutants: $refused refused by verification, $weak refused without a verification run"
if [ "${MUTANTS_STRICT:-0}" = "1" ] && [ "$weak" -gt 0 ]; then exit 1; fi
exit "$fail"
