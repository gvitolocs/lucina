#!/usr/bin/env bash
# Shows that verification/juno-ir3-pack/rules.rs can fail: each line plants one bug in a copy of the shipped
# drivers/lucina-ir3/src/pack.rs, and Verus must refuse it. Run on buildhost from the repo root (or through verus-remote).
set -uo pipefail
verus=${VERUS:-$(command -v verus || echo /home/user/tools/verus/verus-x86-linux/verus)}
src=drivers/lucina-ir3/src/pack.rs
work=$(mktemp -d "${TMPDIR:-/tmp}/juno-ir3-pack-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
fail=0
refused=0
weak=0
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
        if grep -qE 'verification results|simplifies to|not satisfied|assertion failed|possible arithmetic|may not terminate|cannot show|recommendation not met' "$work/out" && ! grep -q 'error\[E' "$work/out"; then
            refused=$((refused + 1))
            echo "refused: $from -> $to ($(grep -c "^error" "$work/out") errors)"
        else
            echo "refused (NO VERIFICATION RUN: compile or other error): $from -> $to ($(grep -c "^error" "$work/out") errors)"
            weak=$((weak+1))
        fi
    fi
}
# The unchanged files must verify, or every "refused" below would mean nothing.
if "$verus" verification/juno-ir3-pack/rules.rs > "$work/baseline" 2>&1; then
    echo "baseline: verifies"
else
    cat "$work/baseline"; echo "mutants: the unchanged files do not verify"; exit 1
fi
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
echo "mutants: $refused refused by verification, $weak refused without a verification run"
if [ "${MUTANTS_STRICT:-0}" = "1" ] && [ "$weak" -gt 0 ]; then exit 1; fi
exit $fail
