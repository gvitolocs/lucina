#!/usr/bin/env bash
# Shows that verification/juno-audio-wake/rules.rs can fail: each line plants one bug in a copy of the shipped file, and Verus
# must refuse it. Run from the repo root (buildhost: under verify-gate; or buildhost2: verus-remote <copy> bash verification/...).
set -uo pipefail
verus=${VERUS:-$(command -v verus || echo /home/user/tools/verus/verus-x86-linux/verus)}
work=$(mktemp -d "${TMPDIR:-/tmp}/juno-audio-wake-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
fail=0
refused=0
total=0
try() {
    local from=$1 to=$2
    local shown="${from//$'\n'/\\n} -> ${to//$'\n'/\\n}"
    total=$((total + 1))
    mkdir -p "$work/drivers/lucina-audio-wake/src" "$work/verification/juno-audio-wake"
    cp drivers/lucina-audio-wake/src/lib.rs "$work/drivers/lucina-audio-wake/src/"
    cp verification/juno-audio-wake/rules.rs "$work/verification/juno-audio-wake/"
    local target=$work/drivers/lucina-audio-wake/src/lib.rs
    local text; text=$(<"$target")
    case "$text" in
        *"$from"*) ;;
        *) echo "NOT FOUND: ${shown% -> *}"; fail=1; return ;;
    esac
    printf "%s\n" "${text/"$from"/"$to"}" > "$target"
    if cmp -s "$target" drivers/lucina-audio-wake/src/lib.rs; then echo "NOT CHANGED: ${shown% -> *}"; fail=1; return; fi
    if "$verus" "$work/verification/juno-audio-wake/rules.rs" > "$work/out" 2>&1; then
        echo "NOT REFUSED: $shown"; fail=1
    else
        refused=$((refused + 1))
        echo "refused: $shown ($(grep -c "^error" "$work/out") errors)"
    fi
}
# A raise must add its own bit: a handler that leaves the word as it was loses the event.
try '    pending | source
}' '    pending
}'
# A raise must keep what was raised before.
try '    pending | source
}' '    source
}'
# Two sources on one bit: a FILES raise would look like the watchdog.
try 'pub const FILES: u32 = 8;' 'pub const FILES: u32 = 4;'
# The drain leaves the word clear; one that leaves it set never lets the thread sleep.
try '    (pending, 0)' '    (pending, pending)'
# The drain takes what was raised.
try '    (pending, 0)' '    (0, 0)'
# No sleep with GLINK data left behind by the per-poll limit.
try '    pending == 0 && !left_behind
}' '    pending == 0
}'
# No sleep with something raised since the drain.
try '    pending == 0 && !left_behind
}' '    !left_behind
}'
# An answer times out one millisecond after the limit, not at it.
try "        since + limit + 1
    }
}" "        since + limit
    }
}"
# Saturate before the sum overflows.
try "    if limit >= u64::MAX - since {
        u64::MAX" "    if limit > u64::MAX - since {
        u64::MAX"
# Asking again is due at the limit, not one later.
try "        since + limit
    }
}" "        since + limit + 1
    }
}"
# No answer deadline unless the stream waits for the DSP.
try "    if waiting {
        Some(first_after(sent_at, limit))
    } else {
        None
    }" "    Some(first_after(sent_at, limit))"
# No log line with nothing open: that was the idle timer.
try "    if open {
        Some(first_at(last, every))
    } else {
        None
    }" "    Some(first_at(last, every))"
# The earlier of two, not the later.
try "            if y < x {
                Some(y)
            } else {
                Some(x)
            }" "            if y < x {
                Some(x)
            } else {
                Some(y)
            }"
# One deadline alone still counts.
try '(None, _) => b,' '(None, _) => None,'
# GLINK's expiry counts.
try 'let r = earliest(earliest(earliest(d.stream, d.mic), d.link), d.beat_at);' 'let r = earliest(earliest(d.stream, d.mic), d.beat_at);'
# Giving up on the framework counts while it is not ready.
try 'earliest(earliest(r, d.ask_at), Some(d.give_up_at))' 'earliest(r, d.ask_at)'
# Once it is ready, the start-up deadlines are gone.
try 'if d.ready { r } else' 'if !d.ready { r } else'
# With no deadline the wait has no timer, rather than none at all (a busy loop).
try 'None => Sleep::Forever,' 'None => Sleep::No,'
# A wait for a deadline that is due now is no wait.
try 'Some(d) if d > now => Sleep::For(d - now),' 'Some(d) if d >= now => Sleep::For(d - now),'
# A timed wait lasts until the deadline, not a millisecond more.
try 'Some(d) if d > now => Sleep::For(d - now),' 'Some(d) if d > now => Sleep::For(d - now + 1),'
echo "$refused of $total mutants refused"
exit $fail
