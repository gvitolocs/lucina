#!/usr/bin/env bash
# Shows that Verus can fail on drivers/lucina-video/src/mse_rules.rs: each line plants one bug in a copy of the shipped file, and Verus must refuse it.
# Run on buildhost from the repo root, under verify-gate.
set -uo pipefail
verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
file=drivers/lucina-video/src/mse_rules.rs
work=$(mktemp -d "${TMPDIR:-/tmp}/lucinanet-mse-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
fail=0
try() {
    local from=$1 to=$2
    local text; text=$(<"$file")
    case "$text" in
        *"$from"*) ;;
        *) echo "NOT FOUND: $from"; fail=1; return ;;
    esac
    printf '%s\n' "${text/"$from"/"$to"}" > "$work/mutant.rs"
    local body; body=$(sed -E -e '/\/\/@omit$/d' -e 's/-> (.*[^ ]) \/\/@ret$/-> (ret: \1)/' -e 's/^( *)\/\/@ ?/\1\/*proof*\/ /' "$work/mutant.rs")
    printf 'use vstd::prelude::*;\nverus! {\n%s\n}\nfn main() {}\n' "$body" > "$work/rules.rs"
    if "$verus" "$work/rules.rs" > "$work/out" 2>&1; then
        echo "NOT REFUSED: $from -> $to"; fail=1
    else
        echo "refused: $from -> $to"
    fi
}
# Box headers: a size over the limit let through, a size smaller than its header, a 64-bit size read as a 32-bit box's, "to the end" taken.
try '} else if small as u64 > limit {' '} else if small as u64 > limit + 1 {'
try '} else if small < 8 {' '} else if small < 4 {'
try 'if size < 16 {' 'if size < 8 {'
try 'BoxSize::Box { head: 16, size }' 'BoxSize::Box { head: 8, size }'
try 'if small == 0 {
        return BoxSize::Bad;' 'if small == 0 {
        return BoxSize::Box { head: 8, size: held };'
try 'if held < 16 {
            return BoxSize::NeedMore;
        }' ''
# Stream positions: an overflow check gone, a data offset before the stream's start, the next sample's offset one off.
try 'if head > u64::MAX - at {' 'if false {'
try 'if d > base { None } else { Some(base - d) }' 'if d > base + 1 { None } else { Some(base - d) }'
try 'if size as u64 > u64::MAX - offset {
        None
    } else {
        Some(offset + size as u64)
    }' 'if size as u64 > u64::MAX - offset { None } else { Some(offset + size as u64 + 1) }'
# A sample past its mdat's end, an mdat not all held, a sample before the mdat's start.
try ' || size > end - offset {' ' {'
try 'end - held_at > held_len {' 'end - held_at > held_len + 1 {'
try 'if offset < start || offset > end' 'if offset > end'
# Limits: more held than 128 MiB, a fragment past its sample count, an element past its limit.
try 'n <= MAX_PENDING - held' 'n <= MAX_PENDING - held + 1'
try 'pub const MAX_PENDING: u64 = 134217728;' 'pub const MAX_PENDING: u64 = 268435456;'
try 'count <= max && n <= max - count' 'n <= max'
try '    size <= limit
}' '    size <= limit + 1
}'
[ "$fail" = 0 ] && echo "mutants: every planted bug was refused" || echo "mutants: FAILED"
exit $fail
