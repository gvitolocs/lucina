#!/usr/bin/env bash
# Shows that verification/juno-wifi-wake/rules.rs can fail: each `try` plants one bug in a copy of the shipped file, and Verus must
# refuse it. The `both` ones change the code and its own spec alike. Run on buildhost from the repo root, under verify-gate:
#   ~/.local/bin/verify-gate -- bash verification/juno-wifi-wake/mutants.sh
set -uo pipefail
verus=${VERUS:-verus}
[ -x /home/user/tools/verus/verus-x86-linux/verus ] && verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
work=$(mktemp -d "${TMPDIR:-/tmp}/juno-wifi-wake-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
src=drivers/lucina-wifi-wake/src/lib.rs
fail=0 refused=0 total=0
try() {
    local name=$1; shift
    total=$((total + 1))
    mkdir -p "$work/drivers/lucina-wifi-wake/src" "$work/verification/juno-wifi-wake"
    cp drivers/lucina-wifi-wake/src/*.rs "$work/drivers/lucina-wifi-wake/src/"
    cp verification/juno-wifi-wake/rules.rs "$work/verification/juno-wifi-wake/"
    local target=$work/$src text
    text=$(<"$target")
    while [ $# -ge 2 ]; do
        case "$text" in *"$1"*) ;; *) echo "NOT FOUND ($name): $1"; fail=1; return ;; esac
        text=${text/"$1"/"$2"}
        shift 2
    done
    printf "%s\n" "$text" > "$target"
    if cmp -s "$target" "$src"; then echo "NOT CHANGED ($name)"; fail=1; return; fi
    if "$verus" "$work/verification/juno-wifi-wake/rules.rs" > "$work/out" 2>&1; then
        echo "NOT REFUSED: $name"; fail=1
    else
        refused=$((refused + 1)); echo "refused: $name ($(grep -c "^error" "$work/out") errors)"
    fi
}
try "work left behind ignored (code)" \
    '            if left_behind {
                Sleep::No
            } else if busy {' '            if false {
                Sleep::No
            } else if busy {'
try "both: a deadline ignored" \
    '                    Some(d) => {
                        if d > now {
                            Sleep::For(d - now)' '                    Some(_d) => {
                        if true {
                            Sleep::Forever' \
    '                Some(d) => if d > now { Sleep::For((d - now) as u64) } else { Sleep::No },' '                Some(_d) => Sleep::Forever,'
try "both: wakes after the deadline" \
    '                            Sleep::For(d - now)' '                            Sleep::For(d - now + 1)' \
    'Sleep::For((d - now) as u64)' 'Sleep::For((d - now + 1) as u64)'
try "both: a slower poll" 'pub const POLL_MS: u64 = 1;' 'pub const POLL_MS: u64 = 2;'
try "both: the control device dropped" 'pub const ALL: u32 = 0xffff;' 'pub const ALL: u32 = 0x7fff;'
try "both: two engines on one bit" 'pub const CE_BITS: u32 = 0xfff;' 'pub const CE_BITS: u32 = 0x7ff;'
try "a raise lost (code)" '    pending | (source & ALL)
}' '    pending
}'
try "may sleep with work left (code)" '    pending == 0 && !left_behind
}' '    pending == 0
}'
try "an engine past the last (code)" '    if id < CE_COUNT { 1u32 << id } else { 0 }' '    if id <= CE_COUNT { 1u32 << id } else { 0 }'
try "both: the later deadline" \
    '        (Some(x), Some(y)) => Some(if x < y { x } else { y }),
        (Some(x), None) => Some(x),
        (None, y) => y,
    }
}

/// How' '        (Some(x), Some(y)) => Some(if x < y { y } else { x }),
        (Some(x), None) => Some(x),
        (None, y) => y,
    }
}

/// How' \
    '        (Some(x), Some(y)) => Some(if x < y { x } else { y }),
        (Some(x), None) => Some(x),
        (None, y) => y,
    }
}

pub open spec fn plan_spec' '        (Some(x), Some(y)) => Some(if x < y { y } else { x }),
        (Some(x), None) => Some(x),
        (None, y) => y,
    }
}

pub open spec fn plan_spec'
try "both: the poll stops yielding" \
    '            if busy {
                Sleep::Yield
            } else {
                Sleep::For(POLL_MS)' '            if false {
                Sleep::Yield
            } else {
                Sleep::For(POLL_MS)' \
    '        Mode::Poll => if busy { Sleep::Yield } else { Sleep::For(POLL_MS) },' '        Mode::Poll => Sleep::For(POLL_MS),'
try "both: traffic sleeps on the lines" \
    '            } else if busy {
                Sleep::Yield
            } else {
                match deadline {' '            } else if false {
                Sleep::Yield
            } else {
                match deadline {' \
    '        } else if busy {
            Sleep::Yield
        } else {
            match deadline {' '        } else if false {
            Sleep::Yield
        } else {
            match deadline {'
echo "$refused of $total refused"
exit $fail
