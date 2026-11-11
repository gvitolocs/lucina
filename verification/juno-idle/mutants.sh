#!/usr/bin/env bash
# Shows that verification/juno-idle/rules.rs can fail: each line plants one bug in a copy of the shipped file, and Verus must refuse it.
# Run on buildhost from the repo root, under verify-gate.
set -uo pipefail
verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
work=$(mktemp -d "${TMPDIR:-/tmp}/juno-idle-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
fail=0
refused=0
weak=0
try() {
    try_in lib.rs "$@"
}
# try_in <file in kernel/lucina-idle/src> <from> <to>
try_in() {
    local file=$1 from=$2 to=$3
    rm -rf "$work/crates" "$work/verification"
    mkdir -p "$work/kernel/lucina-idle/src" "$work/verification/juno-idle"
    cp kernel/lucina-idle/src/*.rs "$work/kernel/lucina-idle/src/"
    cp verification/juno-idle/rules.rs "$work/verification/juno-idle/"
    local target=$work/kernel/lucina-idle/src/$file
    local text; text=$(<"$target")
    case "$text" in
        *"$from"*) ;;
        *) echo "NOT FOUND: $from"; fail=1; return ;;
    esac
    printf "%s\n" "${text/"$from"/"$to"}" > "$target"
    if cmp -s "$target" "kernel/lucina-idle/src/$file"; then echo "NOT CHANGED: $from"; fail=1; return; fi
    if "$verus" "$work/verification/juno-idle/rules.rs" > "$work/out" 2>&1; then
        echo "NOT REFUSED: $from -> $to"; fail=1
    else
        if grep -qE 'verification results|simplifies to|not satisfied|assertion failed|possible arithmetic|may not terminate|cannot show|recommendation not met' "$work/out" && ! grep -q 'error\[E' "$work/out"; then
            refused=$((refused + 1))
            echo "refused: $from -> $to ($(grep -c "^error" "$work/out") errors)"
        else
            echo "refused (NO VERIFICATION RUN: compile or other error): $from -> $to ($(grep -c "^error" "$work/out") errors)"
            weak=$((weak+1))
        fi
    fi
}
# The unchanged files must verify, or every "refused" below would mean nothing.
if "$verus" verification/juno-idle/rules.rs > "$work/baseline" 2>&1; then
    echo "baseline: verifies"
else
    cat "$work/baseline"; echo "mutants: the unchanged files do not verify"; exit 1
fi
# Its own entry must not count.
try 'if i != me && wake_at[i]' 'if wake_at[i]'
# A CPU waking after the deadline does not cover it.
try 'wake_at[i] <= deadline {' 'wake_at[i] < deadline + 1000 {'
# An absent CPU covers nothing.
try 'if i != me && wake_at[i] != ABSENT && ' 'if i != me && '
# Covered means sleep to the cap, else the event.
try 'Some(e) if !covered_by_other => {' 'Some(e) if covered_by_other => {'
try 'if e < cap {' 'if e > cap {'
try '        _ => cap,' '        _ => cap / 2,'
# The cluster (domain.rs): only the last CPU down, the deepest state that fits, nothing with an IPI pending.
try_in domain.rs 'if self.member[i] && !self.down[i] {' 'if !self.down[i] {'
try_in domain.rs '        self.down[cpu] = true;
        self.all_down()' '        self.down[cpu] = true;
        true'
try_in domain.rs 'if member[i] && next_wakeup_ns[i] < earliest {' 'if next_wakeup_ns[i] < earliest {'
try_in domain.rs '(idle_ns as i128) >= (state.residency_ns as i128) + (state.power_off_latency_ns as i128)' '(idle_ns as i128) >= (state.residency_ns as i128)'
try_in domain.rs '    if idle_ns <= 0 || ipi_pending {' '    if idle_ns <= 0 {'
try_in domain.rs '    if idle_ns <= 0 || ipi_pending {' '    if idle_ns < 0 || ipi_pending {'
# The broadcast (broadcast.rs): the earliest sleeper, exactly the due ones.
try_in broadcast.rs 'Some(b) => d < b.deadline,' 'Some(b) => d > b.deadline,'
try_in broadcast.rs 'if i != cpu && self.deadline[i] <= deadline {' 'if i != cpu && self.deadline[i] < deadline {'
try_in broadcast.rs 'let due = self.deadline[i] <= now;' 'let due = self.deadline[i] < now;'
try_in broadcast.rs '            if due {
                self.deadline[i] = ASLEEP_NONE;
            }' ''
# PSCI parameters (states.rs).
try_in states.rs 'let mask = if extended { EXTENDED_MASK } else { ORIGINAL_MASK };' 'let mask = if extended { ORIGINAL_MASK } else { EXTENDED_MASK };'
try_in states.rs 'let bit = if extended { EXTENDED_TYPE } else { ORIGINAL_TYPE };' 'let bit = EXTENDED_TYPE;'
# Selection (admit.rs): never past the latency budget, never past the next timer, never deeper than teo's choice.
try_in admit.rs '    wakes_in_time && pays_off' '    pays_off'
try_in admit.rs '    wakes_in_time && pays_off' '    wakes_in_time'
try_in admit.rs '    wakes_in_time && pays_off' '    wakes_in_time || pays_off'
try_in admit.rs '        if !disabled[i] && fits(states[i], latency_ns, length_ns) {' '        if fits(states[i], latency_ns, length_ns) {'
try_in admit.rs '        if !disabled[i] && fits(states[i], latency_ns, length_ns) {' '        if !disabled[i] {'
try_in admit.rs '    let mut i = choice;
    #[cfg_attr(verus_keep_ghost, verus_spec(
        invariant
            i <= choice,
            choice < states.len(),
            states.len() == disabled.len(),
            forall' '    let mut i = states.len() - 1;
    #[cfg_attr(verus_keep_ghost, verus_spec(
        invariant
            i <= choice,
            choice < states.len(),
            states.len() == disabled.len(),
            forall'
# The broadcast hand-over (admit.rs gate): a state that stops the timer only with the CPU on the broadcast.
try_in admit.rs '    if !timer_stop[choice] || on_broadcast {' '    if !timer_stop[choice] {'
try_in admit.rs '    if !timer_stop[choice] || on_broadcast {' '    if true {'
try_in admit.rs '        if !disabled[i] && !timer_stop[i] && fits(states[i], latency_ns, length_ns) {' '        if !disabled[i] && fits(states[i], latency_ns, length_ns) {'
# The last CPU (domain.rs cluster_off): no cluster state while a member runs.
try_in domain.rs '        if !self.all_down() {
            return None;
        }
        choose' '        choose'
echo "mutants: $refused refused by verification, $weak refused without a verification run"
if [ "${MUTANTS_STRICT:-0}" = "1" ] && [ "$weak" -gt 0 ]; then exit 1; fi
exit $fail
