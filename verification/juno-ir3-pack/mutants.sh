#!/usr/bin/env bash
# Shows that verification/juno-ir3-pack/rules.rs can fail: each line plants one bug in a copy of the shipped
# drivers/lucina-ir3/src/pack.rs, and Verus must refuse it. Run on buildhost from the repo root (or through verus-remote).
set -uo pipefail
verus=${VERUS:-$(command -v verus || echo /home/user/tools/verus/verus-x86-linux/verus)}
src=drivers/lucina-ir3/src/pack.rs
work=$(mktemp -d "${TMPDIR:-/tmp}/juno-ir3-pack-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
fail=0
try() {
    local from=$1 to=$2
    mkdir -p "$work/drivers/lucina-ir3/src" "$work/verification/juno-ir3-pack"
    cp "$src" "$work/$src"
    cp verification/juno-ir3-pack/rules.rs "$work/verification/juno-ir3-pack/"
    local target=$work/$src
    local text; text=$(<"$target")
    case "$text" in
        *"$from"*) ;;
        *) echo "NOT FOUND: $from"; fail=1; return ;;
    esac
    printf "%s\n" "${text/"$from"/"$to"}" > "$target"
    if cmp -s "$target" "$src"; then echo "NOT CHANGED: $from"; fail=1; return; fi
    if "$verus" "$work/verification/juno-ir3-pack/rules.rs" > "$work/out" 2>&1; then
        echo "NOT REFUSED: $from -> $to"; fail=1
    else
        echo "refused: $from -> $to ($(grep -c "^error" "$work/out") errors)"
    fi
}
# A value wider than its field let through.
try "if width < 64 && raw >> width as u64 != 0 {" "if width < 63 && raw >> width as u64 != 0 {"
try "if width < 64 && raw >> width as u64 != 0 {" "if false {"
# Disagreeing bits accepted.
try "if covered & m != 0 && word & m != at & m {" "if false {"
# Other bits of the word lost, or the old field bits kept.
try "Some(((word & !m) | at, covered | m))" "Some((at, covered | m))"
try "Some(((word & !m) | at, covered | m))" "Some((word | at, covered | m))"
try "Some(((word & !m) | at, covered | m))" "Some(((word & !m) | at, covered))"
# The mask one bit short.
try "let ones = if high - low == 63 { u64::MAX } else { (1u64 << (high - low + 1) as u64) - 1 };" "let ones = if high - low == 63 { u64::MAX } else { (1u64 << (high - low) as u64) - 1 };"
exit $fail
