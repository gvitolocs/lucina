#!/usr/bin/env bash
# Shows that verification/juno-input/rules.rs can fail: each `try` plants one bug in a copy of the shipped file, and Verus must
# refuse it. The `both` ones change the code and its own spec alike. Run from the repo root, on buildhost2:
#   ~/.local/bin/verus-remote . bash verification/juno-input/mutants.sh
set -uo pipefail
verus=${VERUS:-verus}
[ -x /home/user/tools/verus/verus-x86-linux/verus ] && verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
work=$(mktemp -d "${TMPDIR:-/tmp}/juno-input-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
src=drivers/lucina-i2c-hid/src/setup.rs
fail=0 refused=0 total=0
try() {
    local name=$1; shift
    total=$((total + 1))
    mkdir -p "$work/drivers/lucina-i2c-hid/src" "$work/verification/juno-input"
    cp drivers/lucina-i2c-hid/src/setup.rs drivers/lucina-i2c-hid/src/wait.rs "$work/drivers/lucina-i2c-hid/src/"
    cp verification/juno-input/rules.rs "$work/verification/juno-input/"
    local target=$work/${file:-$src} text
    text=$(<"$target")
    while [ $# -ge 2 ]; do
        case "$text" in *"$1"*) ;; *) echo "NOT FOUND ($name): $1"; fail=1; return ;; esac
        text=${text/"$1"/"$2"}
        shift 2
    done
    printf "%s\n" "$text" > "$target"
    if cmp -s "$target" "${file:-$src}"; then echo "NOT CHANGED ($name)"; fail=1; return; fi
    if "$verus" "$work/verification/juno-input/rules.rs" > "$work/out" 2>&1; then
        echo "NOT REFUSED: $name"; fail=1
    else
        refused=$((refused + 1)); echo "refused: $name ($(grep -c "^error" "$work/out") errors)"
    fi
}
try "a retry past the last try (code)" 'failed == 0 || failed >= SETUP_TRIES { None } else { Some(SETUP_RETRY_MS) }
}

#[cfg' 'failed == 0 || failed > SETUP_TRIES { None } else { Some(SETUP_RETRY_MS) }
}

#[cfg'
try "both: five tries" 'pub const SETUP_TRIES: u32 = 4;' 'pub const SETUP_TRIES: u32 = 5;'
try "both: no pause" 'pub const SETUP_RETRY_MS: u32 = 1000;' 'pub const SETUP_RETRY_MS: u32 = 0;'
try "both: a retry with nothing failed" \
    'pub fn setup_retry_ms(failed: u32) -> Option<u32> {
    if failed == 0 || failed >= SETUP_TRIES' 'pub fn setup_retry_ms(failed: u32) -> Option<u32> {
    if failed >= SETUP_TRIES' \
    'pub open spec fn setup_retry_spec(failed: u32) -> Option<u32> {
    if failed == 0 || failed >= SETUP_TRIES' 'pub open spec fn setup_retry_spec(failed: u32) -> Option<u32> {
    if failed >= SETUP_TRIES'
try "both: retries forever" \
    'pub fn setup_retry_ms(failed: u32) -> Option<u32> {
    if failed == 0 || failed >= SETUP_TRIES { None }' 'pub fn setup_retry_ms(failed: u32) -> Option<u32> {
    if failed == 0 { None }' \
    'pub open spec fn setup_retry_spec(failed: u32) -> Option<u32> {
    if failed == 0 || failed >= SETUP_TRIES { None }' 'pub open spec fn setup_retry_spec(failed: u32) -> Option<u32> {
    if failed == 0 { None }'
# Patch 1193: the HID thread's wait (wait.rs).
file=drivers/lucina-i2c-hid/src/wait.rs
try "test reports wait for a line that never fires (code)" '    if !line_irq || !line_moved || test_on {
        Wait::Tick
    } else {
        match deadline_us {
            None => Wait::Line,
            Some(d) => Wait::LineFor(if d > now_us { d - now_us } else { 0 }),' '    if !line_irq || !line_moved {
        Wait::Tick
    } else {
        match deadline_us {
            None => Wait::Line,
            Some(d) => Wait::LineFor(if d > now_us { d - now_us } else { 0 }),'
try "a timer ignored (code)" '            Some(d) => Wait::LineFor(if d > now_us { d - now_us } else { 0 }),' '            Some(_d) => Wait::Line,'
try "both: wakes after the timer" \
    '            Some(d) => Wait::LineFor(if d > now_us { d - now_us } else { 0 }),' '            Some(d) => Wait::LineFor(if d > now_us { d - now_us + 1 } else { 0 }),' \
    '            Some(d) => Wait::LineFor(if d > now_us { (d - now_us) as u64 } else { 0 }),' '            Some(d) => Wait::LineFor(if d > now_us { (d - now_us + 1) as u64 } else { 0 }),'
try "both: an unmoved line trusted" \
    '    if !line_irq || !line_moved || test_on {
        Wait::Tick
    } else {
        match deadline_us {
            None => Wait::Line,
            Some(d) => Wait::LineFor(if d > now_us { d - now_us } else { 0 }),' '    if !line_irq || test_on {
        Wait::Tick
    } else {
        match deadline_us {
            None => Wait::Line,
            Some(d) => Wait::LineFor(if d > now_us { d - now_us } else { 0 }),' \
    '    if !line_irq || !line_moved || test_on {
        Wait::Tick
    } else {
        match deadline_us {
            None => Wait::Line,
            Some(d) => Wait::LineFor(if d > now_us { (d - now_us) as u64 } else { 0 }),' '    if !line_irq || test_on {
        Wait::Tick
    } else {
        match deadline_us {
            None => Wait::Line,
            Some(d) => Wait::LineFor(if d > now_us { (d - now_us) as u64 } else { 0 }),'
try "both: the later timer" \
    '        (Some(x), Some(y)) => Some(if x < y { x } else { y }),
        (Some(x), None) => Some(x),
        (None, y) => y,
    }
}

#[cfg' '        (Some(x), Some(y)) => Some(if x < y { y } else { x }),
        (Some(x), None) => Some(x),
        (None, y) => y,
    }
}

#[cfg' \
    '        (Some(x), Some(y)) => Some(if x < y { x } else { y }),
        (Some(x), None) => Some(x),
        (None, y) => y,
    }
}

} // verus!' '        (Some(x), Some(y)) => Some(if x < y { y } else { x }),
        (Some(x), None) => Some(x),
        (None, y) => y,
    }
}

} // verus!'
try "both: one timer dropped" \
    '        (Some(x), None) => Some(x),
        (None, y) => y,
    }
}

#[cfg' '        (Some(_x), None) => None,
        (None, y) => y,
    }
}

#[cfg' \
    '        (Some(x), None) => Some(x),
        (None, y) => y,
    }
}

} // verus!' '        (Some(_x), None) => None,
        (None, y) => y,
    }
}

} // verus!'
echo "$refused of $total refused"
exit $fail
