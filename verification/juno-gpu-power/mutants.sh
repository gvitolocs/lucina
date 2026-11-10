#!/usr/bin/env bash
# Shows that verification/juno-gpu-power/rules.rs can fail: each line plants one bug in a copy of the shipped
# drivers/lucina-adreno/src/power.rs, and Verus must refuse it. Run on buildhost from the repo root, under verify-gate.
set -uo pipefail
verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
src=drivers/lucina-adreno/src/power.rs
work=$(mktemp -d "${TMPDIR:-/tmp}/juno-gpu-power-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
fail=0
try() {
    local from=$1 to=$2
    mkdir -p "$work/drivers/lucina-adreno/src" "$work/verification/juno-gpu-power"
    cp "$src" "$work/$src"
    cp verification/juno-gpu-power/rules.rs "$work/verification/juno-gpu-power/"
    local target=$work/$src
    local text; text=$(<"$target")
    case "$text" in
        *"$from"*) ;;
        *) echo "NOT FOUND: $from"; fail=1; return ;;
    esac
    printf "%s\n" "${text/"$from"/"$to"}" > "$target"
    if cmp -s "$target" "$src"; then echo "NOT CHANGED: $from"; fail=1; return; fi
    if "$verus" "$work/verification/juno-gpu-power/rules.rs" > "$work/out" 2>&1; then
        echo "NOT REFUSED: $from -> $to"; fail=1
    else
        echo "refused: $from -> $to ($(grep -c "^error" "$work/out") errors)"
    fi
}
# A suspended GPU written to without a resume: the submit-against-suspend race.
try 'State::Suspended => Action::ResumeThenRun,' 'State::Suspended => Action::Run,'
# A failed GPU taking batches.
try 'State::Failed => Action::Refuse,' 'State::Failed => Action::Run,'
# The timer suspending a GPU with a batch on the ring.
try 'active && !p.busy && p.idle_ms > 0 && now_ms' 'active && p.idle_ms > 0 && now_ms'
# Suspending before the period is over, or with suspending off.
try 'now_ms >= p.last_busy_ms && now_ms - p.last_busy_ms >= p.idle_ms
}' 'now_ms >= p.last_busy_ms && now_ms - p.last_busy_ms + 1 >= p.idle_ms
}'
try 'active && !p.busy && p.idle_ms > 0 && now_ms >= p.last_busy_ms' 'active && !p.busy && now_ms >= p.last_busy_ms'
# A failed resume counted as a good one.
try 'Outcome::Failed => Power { state: State::Failed, last_busy_ms: p.last_busy_ms, idle_ms: p.idle_ms, busy: p.busy, suspends: p.suspends, resumes },' 'Outcome::Failed => Power { state: State::Active, last_busy_ms: p.last_busy_ms, idle_ms: p.idle_ms, busy: p.busy, suspends: p.suspends, resumes },'
# A fence that leaves the GPU busy, or the last-busy time going back.
try 'Power { state: p.state, last_busy_ms, idle_ms: p.idle_ms, busy: false, suspends: p.suspends, resumes: p.resumes }' 'Power { state: p.state, last_busy_ms, idle_ms: p.idle_ms, busy: true, suspends: p.suspends, resumes: p.resumes }'
try 'let last_busy_ms = if now_ms > p.last_busy_ms { now_ms } else { p.last_busy_ms };' 'let last_busy_ms = now_ms;'
# The idle timer told to wait while suspended.
try 'if active && !p.busy && p.idle_ms > 0 {' 'if p.idle_ms > 0 {'
# The command line taking more than a minute.
try 'if ms == 0 || ms > 60000 { None } else { Some(ms) }' 'if ms == 0 { None } else { Some(ms) }'
exit $fail
