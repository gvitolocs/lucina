#!/usr/bin/env bash
# Shows that verification/juno-sleep/rules.rs can fail: each `try` plants one bug in a copy of the shipped file (one or more
# replacements, applied together), and Verus must refuse it. The `both` ones change the code and its own spec alike, so only
# the properties of the protocol model (`step` in rules.rs) can catch them. Run from the repo root, on buildhost2:
#   ~/.local/bin/verus-remote . bash verification/juno-sleep/mutants.sh
set -uo pipefail
verus=${VERUS:-verus}
[ -x /home/user/tools/verus/verus-x86-linux/verus ] && verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
work=$(mktemp -d "${TMPDIR:-/tmp}/juno-sleep-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
src=kernel/lucina-sleep/src/lib.rs
fail=0 refused=0 total=0
try() {
    local name=$1; shift
    total=$((total + 1))
    mkdir -p "$work/kernel/lucina-sleep/src" "$work/verification/juno-sleep"
    cp "$src" "$work/kernel/lucina-sleep/src/"
    cp verification/juno-sleep/rules.rs "$work/verification/juno-sleep/"
    local target=$work/$src text
    text=$(<"$target")
    while [ $# -ge 2 ]; do
        case "$text" in
            *"$1"*) ;;
            *) echo "NOT FOUND ($name): $1"; fail=1; return ;;
        esac
        text=${text/"$1"/"$2"}
        shift 2
    done
    printf "%s\n" "$text" > "$target"
    if cmp -s "$target" "$src"; then echo "NOT CHANGED ($name)"; fail=1; return; fi
    if "$verus" "$work/verification/juno-sleep/rules.rs" > "$work/out" 2>&1; then
        echo "NOT REFUSED: $name"; fail=1
    else
        refused=$((refused + 1))
        echo "refused: $name ($(grep -c "^error" "$work/out") errors)"
    fi
}
# The order.
try "two steps swapped in step_at" '    } else if i == 1 {
        Step::Input' '    } else if i == 1 {
        Step::Gpu'
try "resume from the bottom, not the top" 'Action::Resume(step_at(done - 1))' 'Action::Resume(step_at(0))'
try "a resume that does not pop" 'Sleep { done: s.done - 1, phase: Phase::Resuming { aborted } }' 'Sleep { done: s.done, phase: Phase::Resuming { aborted } }'
try "a suspend that counts twice" 'Sleep { done: s.done + 1, phase: Phase::Suspending }' 'Sleep { done: s.done + 2, phase: Phase::Suspending }'
try "a failed suspend counted (code)" '                if ok {
                    Sleep' '                if true {
                    Sleep'
try "resuming ends in suspending" '        Phase::Resuming { .. } => {
            if s.done == 0 {
                (AWAKE, Action::Idle)' '        Phase::Resuming { .. } => {
            if s.done == 0 {
                (Sleep { done: 0, phase: Phase::Suspending }, Action::Idle)'
# The wake and the armed check, code only.
try "a wake ignored while suspending (code)" 'if inp.wake || (s.done == index_of(Step::WakeArmed)' 'if false || (s.done == index_of(Step::WakeArmed)'
try "nothing armed is not checked (code)" '(s.done == index_of(Step::WakeArmed) && !inp.armed)' 'false'
# The same bugs in the code and its spec alike: only the protocol properties can refuse them.
try "both: a wake ignored while suspending" \
    'if inp.wake || (s.done == index_of(Step::WakeArmed)' 'if (s.done == index_of(Step::WakeArmed)' \
    'if inp.wake || (s.done == index_of_spec(Step::WakeArmed)' 'if (s.done == index_of_spec(Step::WakeArmed)'
try "both: nothing armed is not checked" \
    '(s.done == index_of(Step::WakeArmed) && !inp.armed)' 'false' \
    '(s.done == index_of_spec(Step::WakeArmed) && !inp.armed)' 'false'
try "both: the armed check on the wrong step" \
    '(s.done == index_of(Step::WakeArmed) && !inp.armed)' '(s.done == index_of(Step::CpuIdle) && !inp.armed)' \
    '(s.done == index_of_spec(Step::WakeArmed) && !inp.armed)' '(s.done == index_of_spec(Step::CpuIdle) && !inp.armed)'
try "both: a failed suspend counted" \
    '                if ok {
                    Sleep' '                if true {
                    Sleep' \
    'if ok { r == (Sleep { done: (s.done + 1) as u8' 'if true { r == (Sleep { done: (s.done + 1) as u8'
# The PDC words.
try "an extra PDC word" '        || offset == cfg_d
' '        || offset == cfg_d
        || offset == 0x1c
'
try "the lid's pin wrong" 'pub const LID_PIN: u32 = 49;' 'pub const LID_PIN: u32 = 48;'
try "the keyboard's pin wrong" 'pub const KEYBOARD_PIN: u32 = 67;' 'pub const KEYBOARD_PIN: u32 = 66;'
try "the touchpad's pin wrong" 'pub const TOUCHPAD_PIN: u32 = 33;' 'pub const TOUCHPAD_PIN: u32 = 32;'
try "the keyboard's bank dropped (code)" ' || offset == bank_c ||' ' ||'
try "the bank words after version 3.2 too" '(version < PDC_VERSION_3_2 && (offset == bank_a' '(version <= PDC_VERSION_3_2 && (offset == bank_a'
try "the CFG register's base wrong" 'pub const IRQ_I_CFG: u32 = 0x110;' 'pub const IRQ_I_CFG: u32 = 0x100;'
try "the adapter port's PD pin wrong" 'pub const PDIC1_PIN: u32 = 42;' 'pub const PDIC1_PIN: u32 = 43;'
try "the other port's PD pin wrong" 'pub const PDIC0_PIN: u32 = 50;' 'pub const PDIC0_PIN: u32 = 51;'
try "the PD pins' CFG words dropped from the spec" '        || offset == 0x110 + 4 * 42 || offset == 0x110 + 4 * 50
' ''
echo "$refused of $total refused"
exit $fail
