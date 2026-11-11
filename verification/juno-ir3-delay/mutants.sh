#!/usr/bin/env bash
# Shows that verification/juno-ir3-delay/rules.rs can fail: each line plants one bug in a copy of the shipped
# drivers/lucina-ir3/src/delay.rs, and Verus must refuse it. Run on buildhost from the repo root (or through verus-remote).
set -uo pipefail
verus=${VERUS:-$(command -v verus || echo /home/user/tools/verus/verus-x86-linux/verus)}
src=drivers/lucina-ir3/src/delay.rs
work=$(mktemp -d "${TMPDIR:-/tmp}/juno-ir3-delay-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
fail=0
refused=0
weak=0
try() {
    local from=$1 to=$2
    mkdir -p "$work/drivers/lucina-ir3/src" "$work/verification/juno-ir3-delay"
    cp "$src" "$work/$src"
    cp verification/juno-ir3-delay/rules.rs "$work/verification/juno-ir3-delay/"
    local target=$work/$src
    local text; text=$(<"$target")
    case "$text" in
        *"$from"*) ;;
        *) echo "NOT FOUND: $from"; fail=1; return ;;
    esac
    printf "%s\n" "${text/"$from"/"$to"}" > "$target"
    if cmp -s "$target" "$src"; then echo "NOT CHANGED: $from"; fail=1; return; fi
    if "$verus" "$work/verification/juno-ir3-delay/rules.rs" > "$work/out" 2>&1; then
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
if "$verus" verification/juno-ir3-delay/rules.rs > "$work/baseline" 2>&1; then
    echo "baseline: verifies"
else
    cat "$work/baseline"; echo "mutants: the unchanged files do not verify"; exit 1
fi
try "pub const ALU_TO_ALU: u32 = 3;" "pub const ALU_TO_ALU: u32 = 2;"
try "pub const NON_ALU: u32 = 6;" "pub const NON_ALU: u32 = 5;"
try "pub const CAT3_SRC2_READ: u32 = 2;" "pub const CAT3_SRC2_READ: u32 = 1;"
try "if writes_addr {
        return Need::Slots(NON_ALU);" "if writes_addr {
        return Need::Slots(ALU_TO_ALU);"
try "Kind::Sfu => return Need::Ss," "Kind::Sfu => return Need::Slots(3),"
try "Kind::Tex => return Need::Sy," "Kind::Tex => return Need::Ss,"
try "Kind::End => Need::Slots(0)," "Kind::End => Need::Slots(1),"
try "Kind::Flow | Kind::Sfu | Kind::Tex | Kind::Mem => Need::Slots(NON_ALU)," "Kind::Flow | Kind::Sfu | Kind::Tex | Kind::Mem => Need::Slots(ALU_TO_ALU),"
try "Kind::Mad if src_n == 2 => Need::Slots(ALU_TO_ALU - CAT3_SRC2_READ)," "Kind::Mad if src_n == 1 => Need::Slots(ALU_TO_ALU - CAT3_SRC2_READ),"
try "if gap > need as u64 { 0 } else { need as u64 + 1 - gap }" "if gap > need as u64 { 0 } else { need as u64 - gap }"
try "if gap > need as u64 { 0 }" "if gap >= need as u64 { 0 }"
echo "mutants: $refused refused by verification, $weak refused without a verification run"
if [ "${MUTANTS_STRICT:-0}" = "1" ] && [ "$weak" -gt 0 ]; then exit 1; fi
exit $fail
