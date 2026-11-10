#!/usr/bin/env bash
# Shows that verification/juno-tcpbuf/rules.rs can fail: each line plants one bug in a copy of the shipped file
# (or of the proof file, for the model's steps), and Verus must refuse it.
# Run on buildhost from the repo root, under verify-gate.
set -uo pipefail
verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
work=$(mktemp -d "${TMPDIR:-/tmp}/juno-tcpbuf-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
fail=0
try() {
    local file=$1 from=$2 to=$3
    # Shown on one line: a planted bug may span lines.
    local shown="${from//$'\n'/\\n} -> ${to//$'\n'/\\n}"
    mkdir -p "$work/third_party/smoltcp/src/storage" "$work/verification/juno-tcpbuf"
    cp third_party/smoltcp/src/storage/paged_math.rs "$work/third_party/smoltcp/src/storage/"
    cp verification/juno-tcpbuf/rules.rs "$work/verification/juno-tcpbuf/"
    local target=$work/$file
    local text; text=$(<"$target")
    case "$text" in
        *"$from"*) ;;
        *) echo "NOT FOUND in $file: ${shown% -> *}"; fail=1; return ;;
    esac
    printf "%s\n" "${text/"$from"/"$to"}" > "$target"
    if cmp -s "$target" "$file"; then echo "NOT CHANGED in $file: ${shown% -> *}"; fail=1; return; fi
    if "$verus" "$work/verification/juno-tcpbuf/rules.rs" > "$work/out" 2>&1; then
        echo "NOT REFUSED in $file: $shown"; fail=1
    else
        echo "refused in $file: $shown ($(grep -c "^error" "$work/out") errors)"
    fi
}
lib=third_party/smoltcp/src/storage/paged_math.rs
rules=verification/juno-tcpbuf/rules.rs
# A piece runs one element into the next page.
try "$lib" '    let to_page_end = page - offset;' '    let to_page_end = page - offset + 1;'
# A piece ignores the end of the storage.
try "$lib" '    let len = if to_page_end < to_end { to_page_end } else { to_end };' '    let len = to_page_end;'
# enqueue_slice ignores the room left.
try "$rules" '        let size = if window < rest { window } else { rest };' '        let size = rest;'
# dequeue_slice ignores how much is held.
try "$rules" '        let available = if piece < r.length { piece } else { r.length };' '        let available = piece;'
# dequeue forgets to wrap read_at.
try "$rules" '        r.read_at = ((r.read_at as u128 + size as u128) % r.cap as u128) as usize;' '        r.read_at = r.read_at + size;'
# enqueue counts one element too many.
try "$rules" '    r.length = r.length + size;' '    r.length = r.length + size + 1;'
# chunk_plan says the last of whole pages is empty.
try "$lib" '        (whole, page)' '        (whole, 0)'
# chunk_plan drops the piece that holds the rest.
try "$lib" '        (whole + 1, rest)' '        (whole, rest)'
# capped ignores the cap (syslog's buffer as large as the caller asks).
try "$lib" '    if len < cap { len } else { cap }' '    len'
[ "$fail" = 0 ] && echo "all mutants refused" || echo "SOME MUTANTS SURVIVED"
exit $fail
