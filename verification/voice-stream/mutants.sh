#!/usr/bin/env bash
# Shows that drivers/lucina-voice-stream/src/rules.rs is what keeps Verus on it: each line plants one bug in a copy of the shipped
# file, turned into what Verus reads exactly as scripts/verify-shipped.sh does, and Verus must refuse every mutant.
# Run on buildhost from the repo root, under verify-gate: verify-gate -- verification/voice-stream/mutants.sh
# (or on buildhost2: verus-remote <a copy holding drivers/lucina-voice-stream/src/rules.rs and this script> env VERUS=verus verification/voice-stream/mutants.sh)
set -uo pipefail
verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
rules=drivers/lucina-voice-stream/src/rules.rs
work=$(mktemp -d "${TMPDIR:-/tmp}/voice-stream-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT

make_verus_input() {
    local body
    body=$(sed -E -e '/\/\/@omit$/d' -e 's/-> (.*[^ ]) \/\/@ret$/-> (ret: \1)/' -e 's/^( *)\/\/@ ?/\1\/*proof*\/ /' "$1")
    printf 'use vstd::prelude::*;\nverus! {\n%s\n}\nfn main() {}\n' "$body"
}

make_verus_input "$rules" > "$work/base.rs"
if ! "$verus" "$work/base.rs" > "$work/base.out" 2>&1; then
    echo "BASE FAILS"
    cat "$work/base.out"
    exit 1
fi
echo "baseline: verifies"

fail=0
weak=0
killed=0
total=0
try() {
    local from=$1 to=$2
    total=$((total + 1))
    local text; text=$(<"$rules")
    case "$text" in
        *"$from"*) ;;
        *) echo "NOT FOUND: $from"; fail=1; return ;;
    esac
    local mut; mut=${text/"$from"/"$to"}
    if [ "$mut" = "$text" ]; then echo "NOT CHANGED: $from"; fail=1; return; fi
    printf "%s\n" "$mut" > "$work/mutant-src.rs"
    make_verus_input "$work/mutant-src.rs" > "$work/mutant.rs"
    if "$verus" "$work/mutant.rs" > "$work/verus.out" 2>&1; then
        echo "NOT REFUSED: $from -> $to"
        fail=1
    else
        if grep -qE 'verification results|simplifies to|not satisfied|assertion failed|possible arithmetic|may not terminate|cannot show|recommendation not met' "$work/verus.out" && ! grep -q 'error\[E' "$work/verus.out"; then
            killed=$((killed + 1))
            echo "refused: $from -> $to"
        else
            echo "refused (NO VERIFICATION RUN: compile or other error): $from -> $to"
            weak=$((weak+1))
        fi
    fi
}

# a tap out of its 16-bit range; the clamp one too wide; the window count off by one at either end
try '        15 => 9550,' '        15 => 40000,'
try '    } else if v > 32767 {' '    } else if v > 32768 {'
try '        (blen - phase - TAPS) / FACTOR + 1' '        (blen - phase - TAPS) / FACTOR'
try '    if blen < phase + TAPS {' '    if blen < phase + TAPS + 1 {'
# the constants the proofs are written against
try 'pub const HIST: usize = 30;' 'pub const HIST: usize = 29;'
try 'pub const TAPS: usize = 31;' 'pub const TAPS: usize = 32;'
try 'pub const FACTOR: usize = 3;' 'pub const FACTOR: usize = 2;'
# the filter reads past its window, or sums with a gain the bound does not allow
try '        let x = buf[s + k] as i64;' '        let x = buf[s + k + 1] as i64;'
try '        acc = acc + c * x;' '        acc = acc + c * x * 2;'
# the decimator reads one sample late, writes an extra sample, skips a window, or hands on a wrong phase
try '        let s = phase + FACTOR * j;' '        let s = phase + FACTOR * j + 1;'
try '        out.push(filter_at(buf, s));' '        out.push(filter_at(buf, s));
        out.push(0);'
try '        j += 1;' '        j += 2;'
try '    phase + FACTOR * n - (buf.len() - HIST)' '    phase + FACTOR * n - (buf.len() - HIST) + 1'
# the expansion drops a sample of the four, hands on the midpoint, or skips an input sample
try '        out.push(x);
        out.push(x);
        last = x;' '        out.push(x);
        last = x;'
try '        last = x;
        i += 1;' '        last = mid;
        i += 1;'
try '        i += 1;
    }
    last' '        i += 2;
    }
    last'
# a split sample counted wrong
try '    (n / 2, n % 2)' '    (n / 2, n % 2 + 1)'
# a cancel or an error must end any turn
try '    if ev == EV_CANCEL || ev == EV_ERROR {
        return IDLE;' '    if ev == EV_CANCEL {
        return IDLE;'
# only a press starts a turn
try '        if ev == EV_PRESS {
            STARTING' '        if ev != EV_PRESS {
            STARTING'
# only the server's listening opens the stream
try '        if ev == EV_LISTENING {
            STREAMING' '        if ev == EV_END {
            STREAMING'
# the end and the endpoint both stop the frames; a done does too
try '        if ev == EV_END || ev == EV_ENDPOINT {
            WAITING' '        if ev == EV_END {
            WAITING'
try '        } else if ev == EV_DONE {
            IDLE' '        } else if ev == EV_DONE {
            STREAMING'
# a waiting turn does not start streaming on its own
try '    } else {
        WAITING
    }
}' '    } else {
        STREAMING
    }
}'
# frames go only while streaming and are held only while starting
try '    if p == STREAMING {
        FRAME_SEND' '    if p == WAITING {
        FRAME_SEND'
try '    } else if p == STARTING {
        FRAME_HOLD' '    } else if p == IDLE {
        FRAME_HOLD'
try 'pub const STREAMING: u8 = 2;' 'pub const STREAMING: u8 = 1;'
# the hold buffer stays within its cap
try '        cap - held
    }' '        cap
    }'
try '    if add <= cap - held {
        add' '    if add <= cap - held {
        add + 1'
# the note table: a fifth note drops the oldest and says so; nothing else is lost; a remove really removes
try '        (Notes { k0: n.k1, k1: n.k2, k2: n.k3, k3: k }, n.k0)' '        (Notes { k0: n.k1, k1: n.k2, k2: n.k3, k3: k }, 0)'
try '        (Notes { k0: n.k1, k1: n.k2, k2: n.k3, k3: k }, n.k0)' '        (Notes { k0: n.k1, k1: n.k2, k2: n.k3, k3: k }, n.k1)'
try '        (Notes { k0: n.k0, k1: n.k1, k2: n.k2, k3: k }, 0)' '        (Notes { k0: n.k0, k1: n.k1, k2: n.k2, k3: n.k3 }, 0)'
try '        Notes { k0: n.k1, k1: n.k2, k2: n.k3, k3: 0 }
    } else if n.k1 == k {' '        Notes { k0: n.k1, k1: n.k2, k2: n.k3, k3: n.k3 }
    } else if n.k1 == k {'
try '        Notes { k0: n.k0, k1: n.k2, k2: n.k3, k3: 0 }' '        Notes { k0: n.k0, k1: n.k2, k2: n.k3, k3: n.k3 }'
try '        Notes { k0: n.k0, k1: n.k1, k2: n.k2, k3: 0 }
    } else {' '        Notes { k0: n.k0, k1: n.k1, k2: n.k2, k3: n.k3 }
    } else {'
try '    k != 0 && (n.k0 == k || n.k1 == k || n.k2 == k || n.k3 == k)
}' '    n.k0 == k || n.k1 == k || n.k2 == k || n.k3 == k
}'

echo "mutants refused: $killed of $total"
echo "mutants: $killed refused by verification, $weak refused without a verification run"
if [ "${MUTANTS_STRICT:-0}" = "1" ] && [ "$weak" -gt 0 ]; then exit 1; fi
exit "$fail"
