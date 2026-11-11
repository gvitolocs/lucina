#!/usr/bin/env bash
# Shows that Verus can fail on drivers/lucina-venus/src/rules.rs: each line plants one bug in a copy of the shipped file, and Verus must refuse it.
# Run on buildhost from the repo root, under verify-gate.
set -uo pipefail
verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
file=drivers/lucina-venus/src/rules.rs
work=$(mktemp -d "${TMPDIR:-/tmp}/juno-venus-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
fail=0
refused=0
weak=0
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
        if grep -qE 'verification results|simplifies to|not satisfied|assertion failed|possible arithmetic|may not terminate|cannot show|recommendation not met' "$work/out" && ! grep -q 'error\[E' "$work/out"; then
            refused=$((refused + 1))
            echo "refused: $from -> $to"
        else
            echo "refused (NO VERIFICATION RUN: compile or other error): $from -> $to"
            weak=$((weak+1))
        fi
    fi
}
# The unchanged files must verify, or every "refused" below would mean nothing.
base_body=$(sed -E -e '/\/\/@omit$/d' -e 's/-> (.*[^ ]) \/\/@ret$/-> (ret: \1)/' -e 's/^( *)\/\/@ ?/\1\/*proof*\/ /' "$file")
printf 'use vstd::prelude::*;\nverus! {\n%s\n}\nfn main() {}\n' "$base_body" > "$work/base.rs"
if "$verus" "$work/base.rs" > "$work/baseline" 2>&1; then
    echo "baseline: verifies"
else
    cat "$work/baseline"; echo "mutants: the unchanged files do not verify"; exit 1
fi
# The queues: a write that fills the last word, a wrong wrap, an index past the end let through, a packet longer than what was written.
try 'if free_words(rd, wr) <= words {' 'if free_words(rd, wr) < words {'
try 'if words < room {
        Ok(Span { at: wr' 'if words <= room {
        Ok(Span { at: wr'
try 'next: words - room })
    }
}

/// What reading' 'next: words - room + 1 })
    }
}

/// What reading'
try 'if rd >= QUEUE_WORDS || wr >= QUEUE_WORDS {
        return Err(QueueError::Corrupt);
    }
    let words = bytes / 4;' 'if rd > QUEUE_WORDS || wr > QUEUE_WORDS {
        return Err(QueueError::Corrupt);
    }
    let words = bytes / 4;'
try ' || words > written {' ' {'
try 'words > MAX_PACKET_WORDS || words > written' 'words > MAX_PACKET_WORDS + 1 || words > written'
try 'if head % 4 != 0 || words < MIN_PACKET_WORDS' 'if words < MIN_PACKET_WORDS'
try 'if wr >= rd { QUEUE_WORDS - (wr - rd) } else { rd - wr }' 'if wr >= rd { QUEUE_WORDS - (wr - rd) } else { rd - wr + 1 }'
# Addresses: memory at or above 0xe0000000, below the protected range, or running over the end.
try 'pub const DEVICE_END: u64 = 0xe000_0000;' 'pub const DEVICE_END: u64 = 0x1_0000_0000;'
try 'bytes == 0 || phys < DEVICE_LOW || phys' 'bytes == 0 || phys'
try ' || bytes > DEVICE_END - phys {' ' {'
# A frame said to lie past the end of its buffer.
try 'offset <= alloc && filled <= alloc - offset' 'filled <= alloc'
# Sizes from a file: a side past the decoder's limit, too many macroblocks, a chroma plane that does not fit, a count of 0 or 33.
try 'if w < MIN_SIDE || w > MAX_SIDE' 'if w < MIN_SIDE || w > MAX_SIDE + 1'
try '(w + 15) / 16 * ((h + 15) / 16) <= MAX_MACROBLOCKS' '(w + 15) / 16 * ((h + 15) / 16) <= MAX_MACROBLOCKS + 1'
try 'let uv_lines = ((h + 1) / 2 + 15) / 16 * 16;' 'let uv_lines = (h / 2) / 16 * 16;'
try 'let bytes = uv_offset + stride * uv_lines;' 'let bytes = uv_offset;'
try 'count >= 1 && count <= MAX_BUFFERS' 'count <= MAX_BUFFERS'
try 'count >= 1 && count <= MAX_BUFFERS' 'count >= 1 && count <= MAX_BUFFERS + 1'
try '    if size > MAX_INPUT_BYTES {
        Some(MAX_INPUT_BYTES)' '    if size > MAX_INPUT_BYTES + 4096 {
        Some(MAX_INPUT_BYTES)'
# The UBWC reference picture: the whole not rounded to 4096, the picture check gone, a tail that runs past the 64 MiB ceiling.
try 'Some((y_meta + y_plane + uv_meta + uv_plane + extra + 4095) / 4096 * 4096)' 'Some(y_meta + y_plane + uv_meta + uv_plane + extra)'
try '    if !picture_ok(w, h) {
        return None;
    }
    let y_meta_stride' '    let y_meta_stride'
try 'let extra = if y_stride * 48 > 16384 { y_stride * 48 } else { 16384 };' 'let extra = if y_stride * 48 > 16384 { y_stride * 48 } else { 16384 * 4096 };'
# An input buffer: no 4096 rounding, a zero size let through, a ceiling one page past the 8 MiB.
try '    let size = (size + 4095) / 4096 * 4096;' '    let size = size;'
try '    } else if size == 0 {
        Some(4096)' '    } else if size == 0 {
        Some(0)'
try '        Some(MAX_INPUT_BYTES)
    } else if size == 0 {' '        Some(MAX_INPUT_BYTES + 4096)
    } else if size == 0 {'
[ "$fail" = 0 ] && echo "mutants: every planted bug was refused" || echo "mutants: FAILED"
echo "mutants: $refused refused by verification, $weak refused without a verification run"
if [ "${MUTANTS_STRICT:-0}" = "1" ] && [ "$weak" -gt 0 ]; then exit 1; fi
exit $fail
