#!/usr/bin/env bash
# Shows that verification/juno-structmap/rules.rs can fail: each line plants one bug in a copy of the shipped file, and Verus must refuse it.
# Run on buildhost from the repo root, under verify-gate.
set -uo pipefail
verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
work=$(mktemp -d "${TMPDIR:-/tmp}/juno-structmap-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
fail=0
try() {
    local from=$1 to=$2
    mkdir -p "$work/kernel/lucina-structmap/src" "$work/verification/juno-structmap"
    cp kernel/lucina-structmap/src/rules.rs "$work/kernel/lucina-structmap/src/"
    cp verification/juno-structmap/rules.rs "$work/verification/juno-structmap/"
    local target=$work/kernel/lucina-structmap/src/rules.rs
    local text; text=$(<"$target")
    case "$text" in
        *"$from"*) ;;
        *) echo "NOT FOUND: $from"; fail=1; return ;;
    esac
    printf "%s\n" "${text/"$from"/"$to"}" > "$target"
    if cmp -s "$target" kernel/lucina-structmap/src/rules.rs; then echo "NOT CHANGED: $from"; fail=1; return; fi
    if "$verus" "$work/verification/juno-structmap/rules.rs" > "$work/out" 2>&1; then
        echo "NOT REFUSED: $from -> $to"; fail=1
    else
        echo "refused: $from -> $to ($(grep -c '^error' "$work/out") errors)"
    fi
}
try 'if len == 0 || len > MAX_FIELD {' 'if len > MAX_FIELD {'
try 'if len == 0 || len > MAX_FIELD {' 'if len == 0 || len > MAX_FIELD + 1 {'
try 'if r.readable && r.start <= addr && end <= r.end {' 'if r.start <= addr && end <= r.end {'
try 'if r.readable && r.start <= addr && end <= r.end {' 'if r.readable && r.start <= addr + 1 && end <= r.end {'
try 'if r.readable && r.start <= addr && end <= r.end {' 'if r.readable && r.start <= addr && end <= r.end + 1 {'
try 'if base > u64::MAX - off {' 'if base >= u64::MAX - off {'
try 'if depth >= MAX_DEPTH {' 'if depth > MAX_DEPTH {'
try 'if at == 0 {' 'if at == 1 {'
try 'if !read_allowed(regions, at, PTR) {' 'if !read_allowed(regions, at, 4) {'
try 'if f < excluded.len() && !excluded[f] {' 'if f < excluded.len() {'
try 'if a.len() != b.len() {' 'if a.len() < b.len() {'
try 'if a[i] != b[i] {' 'if i > 0 && a[i] != b[i] {'
try 'if want.len() < MIN_ID || want.len() > MAX_ID {' 'if want.len() > MAX_ID {'
try $'            if found.is_some() {\n                return Lookup::Duplicate;\n            }\n' ''
# copy_offset: a field read out of an app's own copy of a region (the Game Boy app's WRAM).
try $'    if len == 0 || len > MAX_FIELD {\n        return None;' $'    if len > MAX_FIELD {\n        return None;'
try 'if addr < base {' 'if addr < base - 1 {'
try 'if off > size || len > size - off {' 'if off > size {'
try 'if off > size || len > size - off {' 'if off > size || len > size - off + 1 {'
try 'if off > copy_len || len > copy_len - off {' 'if off > copy_len {'
try 'if off > copy_len || len > copy_len - off {' 'if off > size || len > size - off {'
[ "$fail" = 0 ] && echo "mutants: every planted bug was refused" || echo "mutants: FAILED"
exit $fail
