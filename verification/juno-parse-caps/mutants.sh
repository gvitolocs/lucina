#!/usr/bin/env bash
# Shows that verification/juno-parse-caps/rules.rs can fail: each line plants one bug in a copy of the shipped file (or of
# the proof file, for the models), and Verus must refuse it. Run on buildhost from the repo root, under verify-gate.
set -uo pipefail
verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
work=$(mktemp -d "${TMPDIR:-/tmp}/juno-parse-caps-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
fail=0
weak=0
killed=0
total=0
try() {
    local file=$1 from=$2 to=$3
    local shown="${from//$'\n'/\\n} -> ${to//$'\n'/\\n}"
    total=$((total + 1))
    mkdir -p "$work/kernel/lucina-parse-caps/src" "$work/verification/juno-parse-caps"
    cp kernel/lucina-parse-caps/src/lib.rs "$work/kernel/lucina-parse-caps/src/"
    cp verification/juno-parse-caps/rules.rs "$work/verification/juno-parse-caps/"
    local target=$work/$file
    local text; text=$(<"$target")
    case "$text" in
        *"$from"*) ;;
        *) echo "NOT FOUND in $file: ${shown% -> *}"; fail=1; return ;;
    esac
    printf "%s\n" "${text/"$from"/"$to"}" > "$target"
    if cmp -s "$target" "$file"; then echo "NOT CHANGED in $file: ${shown% -> *}"; fail=1; return; fi
    if "$verus" "$work/verification/juno-parse-caps/rules.rs" > "$work/out" 2>&1; then
        echo "NOT REFUSED in $file: $shown"; fail=1
    else
        if grep -qE 'verification results|simplifies to|not satisfied|assertion failed|possible arithmetic|may not terminate|cannot show|recommendation not met' "$work/out" && ! grep -q 'error\[E' "$work/out"; then
            killed=$((killed + 1))
            echo "refused in $file: $shown ($(grep -c "^error" "$work/out") errors)"
        else
            echo "refused (NO VERIFICATION RUN: compile or other error) in $file: $shown ($(grep -c "^error" "$work/out") errors)"
            weak=$((weak+1))
        fi
    fi
}
# The unchanged files must verify, or every "refused" below would mean nothing.
if "$verus" verification/juno-parse-caps/rules.rs > "$work/baseline" 2>&1; then
    echo "baseline: verifies"
else
    cat "$work/baseline"; echo "mutants: the unchanged files do not verify"; exit 1
fi
lib=kernel/lucina-parse-caps/src/lib.rs
rules=verification/juno-parse-caps/rules.rs
# take grants without lowering the budget.
try "$lib" 'if n <= left { Some(left - n) } else { None }' 'if n <= left { Some(left) } else { None }'
# take grants what is not there.
try "$lib" 'if n <= left { Some(left - n) } else { None }' 'if n <= left { Some(left - n) } else { Some(0) }'
# product's fit test is off by one.
try "$lib" '    if b > cap / a {' '    if b > cap / a + 1 {'
# product refuses an empty picture.
try "$lib" '    if a == 0 || b == 0 {
        return Some(0);' '    if a == 0 || b == 0 {
        return None;'
# area forgets the bytes per element.
try "$lib" '        Some(wh) => product(wh, per, cap),' '        Some(wh) => Some(wh),'
# range checks the length against the whole buffer instead of what is left after the offset.
try "$lib" 'if off <= total && len <= total - off { Some(off + len) } else { None }' 'if off <= total && len <= total { Some(off + len) } else { None }'
# count_next lets one item past the cap.
try "$lib" 'if count < cap { Some(count + 1) } else { None }' 'if count <= cap { Some(count + 1) } else { None }'
# enter lets one level past the limit.
try "$lib" 'if depth < max { Some(depth + 1) } else { None }' 'if depth <= max { Some(depth + 1) } else { None }'
# zip_entry drops the entry cap.
try "$lib" '    if declared > max_entry {
        return None;
    }' '    if declared > max_entry && false {
        return None;
    }'
# zip_entry drops the ratio test.
try "$lib" '    if declared > allowed {' '    if declared > allowed && false {'
# zip_entry does not charge the archive budget.
try "$lib" '    take(total_left, declared)' '    Some(total_left)'
# inflate_more stops counting what was produced.
try "$lib" '    range(produced, chunk, declared)' '    range(produced, 0, declared)'
# read_next lets two bytes past the cap.
try "$lib" '    let room = cap + 1 - have;' '    let room = cap + 2 - have;'
# over refuses a read of exactly the cap.
try "$lib" '    total > cap
}' '    total >= cap
}'
# The descent model goes around enter.
try "$rules" '    match enter(depth, max) {' '    match enter(depth, u32::MAX) {'
# The archive model does not count entries.
try "$rules" '        match count_next(count, max_entries) {' '        match count_next(count, u64::MAX) {'
echo "mutants: $killed of $total refused"
echo "mutants: $killed refused by verification, $weak refused without a verification run"
if [ "${MUTANTS_STRICT:-0}" = "1" ] && [ "$weak" -gt 0 ]; then exit 1; fi
exit $fail
