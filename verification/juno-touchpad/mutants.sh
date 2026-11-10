#!/usr/bin/env bash
# Shows that Verus can fail on drivers/lucina-i2c-hid/src/tp_rules.rs: each line plants one bug in a copy of the shipped file, and Verus must refuse it.
# Run on buildhost from the repo root, under verify-gate.
set -uo pipefail
verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
file=drivers/lucina-i2c-hid/src/tp_rules.rs
work=$(mktemp -d "${TMPDIR:-/tmp}/juno-touchpad-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
fail=0
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
        echo "refused: $from -> $to"
    fi
}
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
try "GestureStep { next: GestureState::None, stop_scroll: true, buildup: false }" "GestureStep { next: GestureState::None, stop_scroll: false, buildup: false }"
try "GestureStep { next: GestureState::PinchStart, stop_scroll: true, buildup: false }" "GestureStep { next: GestureState::Scroll, stop_scroll: false, buildup: false }"
try "GestureEvent::FingerDetected => GestureStep { next: GestureState::Unknown, stop_scroll: false, buildup: false }," "GestureEvent::FingerDetected => GestureStep { next: GestureState::Scroll, stop_scroll: false, buildup: false },"
try "GestureEvent::ScrollStart => GestureStep { next: GestureState::ScrollStart, stop_scroll: false, buildup: true }," "GestureEvent::ScrollStart => GestureStep { next: GestureState::ScrollStart, stop_scroll: true, buildup: true },"
# Finger count: all fingers up without ending, a switch that waits while not debouncing, a pending count kept.
try "FingerUpdate { count: 0, pending: 0, end: true, reset: false, arm: false }" "FingerUpdate { count: 0, pending: 0, end: false, reset: false, arm: false }"
try "FingerUpdate { count: active, pending: 0, end: false, reset: resettable, arm: false }" "FingerUpdate { count, pending: active, end: false, reset: resettable, arm: true }"
try "let none = FingerUpdate { count, pending: 0," "let none = FingerUpdate { count, pending,"
# Notches: rounding instead of truncation, the remainder dropped.
try "let lo = sum / 120;" "let lo = (sum + 60) / 120;"
try "(lo, sum - lo * 120)" "(lo, 0)"
exit "$fail"
