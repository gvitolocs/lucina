#!/usr/bin/env bash
# Shows that drivers/lucina-uvc/src/rules.rs is what keeps Verus on it: each line plants one bug in a copy of the shipped file,
# turned into what Verus reads exactly as scripts/verify-shipped.sh does, and Verus must refuse every mutant.
# Run on buildhost from the repo root, under verify-gate: verify-gate -- verification/juno-camera/mutants.sh
set -uo pipefail
verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
rules=drivers/lucina-uvc/src/rules.rs
work=$(mktemp -d "${TMPDIR:-/tmp}/juno-camera-mutants.XXXXXX")
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
refused=0
weak=0
try() {
    local from=$1 to=$2
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
            refused=$((refused + 1))
            echo "refused: $from -> $to"
        else
            echo "refused (NO VERIFICATION RUN: compile or other error): $from -> $to"
            weak=$((weak+1))
        fi
    fi
}
# A payload: a one-byte header, or one longer than the packet, is never read.
try "if len < 2 || header < 2 || header as usize > len {" "if len < 2 || header as usize > len {"
try "if len < 2 || header < 2 || header as usize > len {" "if len < 2 || header < 2 || header as usize > len + 1 {"
try "Payload { ok: true, start: header as usize, data: len - header as usize }" "Payload { ok: true, start: header as usize, data: len - 2 }"
# A copy never runs past the frame buffer, and an overflow is always said.
try "    if len > room {" "    if len > room + 1 {"
try "Take { n: room, overflow: true }" "Take { n: len, overflow: true }"
try "Take { n: len, overflow: false }" "Take { n: room, overflow: false }"
# A packet's span stays inside the transfer buffer and is never longer than the packet.
try "    if actual > size {" "    if actual > size + 1 {"
try "if start > buffer || actual > buffer - start {" "if start > buffer {"
# A transfer has 1 to 32 packets, enough for a frame unless capped.
try "    if whole >= MAX_PACKETS {" "    if whole > MAX_PACKETS {"
try "let need = if rest != 0 { whole + 1 } else { whole };" "let need = whole;"
# The alternate setting is big enough.
try "    size >= bandwidth && size < best" "    size + 1 >= bandwidth && size < best"
# The three buffers: the filled one is never the ready or the held one.
try "let fill = if s.ready != NONE { s.ready } else { other(s.fill, s.held) };" "let fill = if s.ready != NONE { s.ready } else { other(s.fill, NONE) };"
try "let fill = if s.ready != NONE { s.ready } else { other(s.fill, s.held) };" "let fill = if s.ready != NONE { s.held } else { other(s.fill, s.held) };"
try "Acquired { slots: Slots { fill: s.fill, ready: NONE, held: s.ready }, got: true }" "Acquired { slots: Slots { fill: s.fill, ready: s.ready, held: s.ready }, got: true }"
try "    if a != 0 && b != 0 {" "    if a != 0 {"
try "Slots { fill: s.fill, ready: NONE, held: s.held }" "Slots { fill: s.held, ready: NONE, held: s.held }"
# Frame boundaries: the same FID with no frame open is dropped, a toggle with data ends the frame, nothing else does.
try "    if !active && last == fid {" "    if last == fid {"
try "    if !active && last == fid {" "    if !active {"
try "    if last != fid && used != 0 {" "    if last != fid {"
try "    if last != fid && used != 0 {" "    if used != 0 {"
# A completed frame is flagged after an error, and an uncompressed one of the wrong size.
try "    error || (!compressed && used != frame_size)" "    error || (compressed && used != frame_size)"
try "    error || (!compressed && used != frame_size)" "    !compressed && used != frame_size"
# The preview never shows a flagged frame.
try "pub fn shown(error: bool) -> bool //@ret
//@ ensures ret == !error,
{
    !error" "pub fn shown(error: bool) -> bool //@ret
//@ ensures ret == !error,
{
    true"
# Frame sizes before a converter or the GPU reads a frame.
try "|| h >= MAX_SIDE || !w.is_multiple_of(2) {" "|| h >= MAX_SIDE {"
try "    w * h * 2
}" "    w * h
}"
try "    if w == 0 || h == 0 || w >= MAX_SIDE || h >= MAX_SIDE {
        return I420Size" "    if w == 0 || h == 0 || w > MAX_SIDE || h >= MAX_SIDE {
        return I420Size"
try "let (cw, ch) = (w / 2 + w % 2, h / 2 + h % 2);" "let (cw, ch) = (w / 2, h / 2);"
# Every plane copy into the GPU buffer stays inside both slices.
try "    if row > pitch || rows >= MAX_SIDE" "    if rows >= MAX_SIDE"
try "    rows * row <= src_len && rows * pitch <= dst_len - offset" "    rows * pitch <= dst_len - offset"
try "    rows * row <= src_len && rows * pitch <= dst_len - offset" "    rows * row <= src_len && (rows - 1) * pitch <= dst_len - offset"
try "|| pitch >= 4 * MAX_SIDE || offset > dst_len {" "|| pitch >= 4 * MAX_SIDE {"
echo "mutants: $refused refused by verification, $weak refused without a verification run"
if [ "${MUTANTS_STRICT:-0}" = "1" ] && [ "$weak" -gt 0 ]; then exit 1; fi
exit "$fail"
