#!/usr/bin/env bash
# Shows that verification/juno-standby/rules.rs can fail: each `try` plants one bug in a copy of the shipped file (one or more
# replacements, applied together), and Verus must refuse it. The `both` ones change the code and its own spec alike, so only
# the field-level lemmas of rules.rs can catch them. Run from the repo root, on buildhost2:
#   ~/.local/bin/verus-remote . bash verification/juno-standby/mutants.sh
set -uo pipefail
verus=${VERUS:-verus}
[ -x /home/user/tools/verus/verus-x86-linux/verus ] && verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
work=$(mktemp -d "${TMPDIR:-/tmp}/juno-standby-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
src=kernel/lucina-standby/src/lib.rs
fail=0 refused=0 total=0
weak=0
try() {
    local name=$1; shift
    total=$((total + 1))
    mkdir -p "$work/kernel/lucina-standby/src" "$work/verification/juno-standby"
    cp "$src" "$work/kernel/lucina-standby/src/"
    cp verification/juno-standby/rules.rs "$work/verification/juno-standby/"
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
    if "$verus" "$work/verification/juno-standby/rules.rs" > "$work/out" 2>&1; then
        echo "NOT REFUSED: $name"; fail=1
    else
        if grep -qE 'verification results|simplifies to|not satisfied|assertion failed|possible arithmetic|may not terminate|cannot show|recommendation not met' "$work/out" && ! grep -q 'error\[E' "$work/out"; then
            refused=$((refused + 1))
            echo "refused: $name ($(grep -c "^error" "$work/out") errors)"
        else
            echo "refused (NO VERIFICATION RUN: compile or other error): $name ($(grep -c "^error" "$work/out") errors)"
            weak=$((weak+1))
        fi
    fi
}
# The unchanged files must verify, or every "refused" below would mean nothing.
if "$verus" verification/juno-standby/rules.rs > "$work/baseline" 2>&1; then
    echo "baseline: verifies"
else
    cat "$work/baseline"; echo "mutants: the unchanged files do not verify"; exit 1
fi
# Code only: the `ensures` of the function itself refuses these.
try "a system hold ignored asleep (code)" '            if s.system_holds > 0 {
                return Decision::Stay;
            }
            match standby_at(s, t)' '            if false {
                return Decision::Stay;
            }
            match standby_at(s, t)'
try "the sleep ignores system holds (code)" '    if s.system_holds > 0 {
        return false;' '    if false {
        return false;'
try "the standby a second early (code)" 'if now >= at { Decision::Standby }' 'if now + 1_000_000 >= at { Decision::Standby }'
try "a screen-off that sleeps (code)" 'if now >= at { Decision::ScreenOff }' 'if now >= at { Decision::Sleep }'
try "the later of two deadlines (code)" 'Some(if x <= y { x } else { y })' 'Some(if x <= y { y } else { x })'
try "settle forgets the order (code)" \
    'let standby = if standby > 0 && sleep > 0 && standby < sleep { sleep } else { standby };' \
    'let standby = if false { sleep } else { standby };'
# The same bugs in the code and its spec alike: only the field-level lemmas can refuse them.
try "both: standby straight from a dark screen" \
    'if sleep_due(s, now, t) { Decision::Sleep } else { Decision::Stay }' 'if sleep_due(s, now, t) { Decision::Standby } else { Decision::Stay }' \
    'Screen::Dark => if sleep_due_spec(s, now, t) { Decision::Sleep }' 'Screen::Dark => if sleep_due_spec(s, now, t) { Decision::Standby }'
try "both: the sleep ignores system holds" \
    '    if s.system_holds > 0 {
        return false;' '    if false {
        return false;' \
    's.system_holds == 0 && t.sleep > 0 && now >= sleep_at_spec(s, t)' 't.sleep > 0 && now >= sleep_at_spec(s, t)'
try "both: the standby ignores system holds" \
    '            if s.system_holds > 0 {
                return Decision::Stay;
            }
            match standby_at(s, t)' '            if false {
                return Decision::Stay;
            }
            match standby_at(s, t)' \
    'Screen::Asleep => if s.system_holds == 0 && t.standby > 0' 'Screen::Asleep => if t.standby > 0'
try "both: a display hold stops nothing" \
    '    if s.display_holds > 0 {
        return Decision::Stay;' '    if false {
        return Decision::Stay;' \
    '    if s.display_holds > 0 {
        Decision::Stay' '    if false {
        Decision::Stay'
try "both: the standby counts from the last input only" \
    'Some(idle_since(s) + (t.standby as i64) * MINUTE_US)' 'Some(s.last_input_us + (t.standby as i64) * MINUTE_US)' \
    'idle_since_spec(s) + t.standby * MINUTE_US' 's.last_input_us + t.standby * MINUTE_US'
try "both: idle forgets the end of a system hold" \
    'later(later(s.last_input_us, s.display_ended_us), s.system_ended_us)' 'later(s.last_input_us, s.display_ended_us)' \
    'max_spec(max_spec(s.last_input_us as int, s.display_ended_us as int), s.system_ended_us as int)' \
    'max_spec(s.last_input_us as int, s.display_ended_us as int)'
try "both: the screen-off forgets the display hold end" \
    'Some(later(s.last_input_us, s.display_ended_us) + (t.off as i64) * MINUTE_US)' 'Some(s.last_input_us + (t.off as i64) * MINUTE_US)' \
    '    max_spec(s.last_input_us as int, s.display_ended_us as int) + t.off * MINUTE_US' '    s.last_input_us + t.off * MINUTE_US'
try "both: a screen-off chosen over a due sleep" \
    '            if sleep_due(s, now, t) {
                return Decision::Sleep;
            }' '            if false {
                return Decision::Sleep;
            }' \
    'Screen::Lit => if sleep_due_spec(s, now, t) {' 'Screen::Lit => if false {'
try "both: input does not restart the timers" \
    'with_input(with_screen(s, Screen::Lit), now)' 'with_screen(s, Screen::Lit)' \
    'with_input_spec(with_screen_spec(s, Screen::Lit), now)' 'with_screen_spec(s, Screen::Lit)'
try "both: a display hold wakes the standby" \
    'Screen::Standby => Screen::Standby,' 'Screen::Standby => Screen::Lit,' \
    'screen: if s.screen == Screen::Dark { Screen::Lit } else { s.screen },' \
    'screen: if s.screen == Screen::Dark || s.screen == Screen::Standby { Screen::Lit } else { s.screen },'
try "both: every give ends the hold" \
    'system_ended_us: if n == 0 { now } else { s.system_ended_us },' 'system_ended_us: now,' \
    'system_ended_us: if s.system_holds == 1 { now } else { s.system_ended_us },' 'system_ended_us: now,'
try "both: settle lets the standby come before the sleep" \
    'let standby = if standby > 0 && sleep > 0 && standby < sleep { sleep } else { standby };' \
    'let standby = if false { sleep } else { standby };' \
    'let standby = if standby0 > 0 && sleep > 0 && standby0 < sleep { sleep } else { standby0 };' 'let standby = standby0;'
try "both: settle lets the sleep past 300" \
    'let sleep = if t.sleep > MAX_MINUTES { MAX_MINUTES } else { t.sleep };' 'let sleep = t.sleep;' \
    'let sleep = if t.sleep > 300 { 300u32 } else { t.sleep };' 'let sleep = t.sleep;'
try "both: the default sleep wrong" 'pub const SLEEP_DEFAULT_MIN: u32 = 15;' 'pub const SLEEP_DEFAULT_MIN: u32 = 10;'
try "both: an asleep deadline at the sleep time" \
    'if s.system_holds > 0 { None } else { standby_at(s, t) }' 'if s.system_holds > 0 { None } else { sleep_at(s, t) }' \
    'Screen::Asleep => if s.system_holds > 0 || t.standby == 0 { None } else { Some(standby_at_spec(s, t) as i64) },' \
    'Screen::Asleep => if s.system_holds > 0 || t.sleep == 0 { None } else { Some(sleep_at_spec(s, t) as i64) },'
echo "$refused of $total refused"
echo "mutants: $refused refused by verification, $weak refused without a verification run"
if [ "${MUTANTS_STRICT:-0}" = "1" ] && [ "$weak" -gt 0 ]; then exit 1; fi
exit $fail
