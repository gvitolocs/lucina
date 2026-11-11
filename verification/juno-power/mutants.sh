#!/usr/bin/env bash
# Shows that verification/juno-power/rules.rs can fail: each line plants one bug in a copy of the shipped file, and Verus must refuse it.
# Run on buildhost from the repo root, under verify-gate.
set -uo pipefail
verus=${VERUS:-verus}
[ -x /home/user/tools/verus/verus-x86-linux/verus ] && verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
work=$(mktemp -d "${TMPDIR:-/tmp}/juno-power-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
fail=0
weak=0
# try_in <shipped file> <from> <to>; try <from> <to> is level.rs.
try() { try_in kernel/lucina-power/src/level.rs "$@"; }
refused=0 total=0
try_in() {
    local file=$1 from=$2 to=$3
    total=$((total + 1))
    mkdir -p "$work/kernel/lucina-power/src" "$work/kernel/lucina-spmi/src" "$work/verification/juno-power"
    cp kernel/lucina-power/src/level.rs kernel/lucina-power/src/wake_rules.rs "$work/kernel/lucina-power/src/"
    cp kernel/lucina-spmi/src/allow.rs "$work/kernel/lucina-spmi/src/"
    cp verification/juno-power/rules.rs "$work/verification/juno-power/"
    local target=$work/$file
    local text; text=$(<"$target")
    case "$text" in
        *"$from"*) ;;
        *) echo "NOT FOUND: $from"; fail=1; return ;;
    esac
    printf "%s\n" "${text/"$from"/"$to"}" > "$target"
    if cmp -s "$target" "$file"; then echo "NOT CHANGED: $from"; fail=1; return; fi
    if "$verus" "$work/verification/juno-power/rules.rs" > "$work/out" 2>&1; then
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
if "$verus" verification/juno-power/rules.rs > "$work/baseline" 2>&1; then
    echo "baseline: verifies"
else
    cat "$work/baseline"; echo "mutants: the unchanged files do not verify"; exit 1
fi
# The 9-bit size bit.
try 'if size_clk & 4 != 0 { 511 } else { 63 }' 'if size_clk & 2 != 0 { 511 } else { 63 }'
# Never above the PWM's top value.
try 'if value > max as u64 { max } else { value as u16 }' 'value as u16'
try 'let level = if level > STEPS { STEPS } else { level };' 'let level = level;'
# The firmware's point: level x (max + 1) / 100.
try 'level as u64 * (max as u64 + 1) / STEPS as u64' 'level as u64 * max as u64 / STEPS as u64'
# Level back from a value never above 100.
try '    if value >= max {
        return STEPS;
    }' '    if value > max {
        return STEPS;
    }'
# The wake interrupts: the PDC words (wake_rules.rs).
try_in kernel/lucina-power/src/wake_rules.rs "(cfg & !7) | type_bits(t)" "(cfg & !3) | type_bits(t)"
try_in kernel/lucina-power/src/wake_rules.rs "PdcType::EdgeRising => 0b110," "PdcType::EdgeRising => 0b100,"
try_in kernel/lucina-power/src/wake_rules.rs "{ IRQ_ENABLE_BANK + 4 * (pin / 32) }" "{ IRQ_ENABLE_BANK + 4 * (pin / 16) }"
try_in kernel/lucina-power/src/wake_rules.rs "1u32 << (pin % 32)" "1u32 << (pin % 16)"
try_in kernel/lucina-power/src/wake_rules.rs "if on { old | bit } else { old & !bit }" "if on { old | bit | 1 } else { old & !bit }"
try_in kernel/lucina-power/src/wake_rules.rs "pub const CFG_ENABLE: u32 = 0x8;" "pub const CFG_ENABLE: u32 = 0x4;"
try_in kernel/lucina-power/src/wake_rules.rs "if level_high { PdcType::EdgeFalling } else { PdcType::EdgeRising }" "if level_high { PdcType::EdgeRising } else { PdcType::EdgeFalling }"
try_in kernel/lucina-power/src/wake_rules.rs "if diff < 0 || diff > u32::MAX as i128" "if diff < 0 || diff > u64::MAX as i128"
try_in kernel/lucina-power/src/wake_rules.rs "let diff = at as i128 - offset as i128;" "let diff = at as i128 + offset as i128;"
# The PMIC writes (allow.rs): no register outside the PON's and the alarm's.
try_in kernel/lucina-spmi/src/allow.rs "let interrupt = 0x11 <= reg && reg <= 0x16;" "let interrupt = 0x10 <= reg && reg <= 0x16;"
try_in kernel/lucina-spmi/src/allow.rs "|| reg == 0x46 || reg == 0x48;" "|| reg == 0x46 || reg == 0x48 || reg == 0x70;"
try_in kernel/lucina-spmi/src/allow.rs "pub const PON_PPID: u16 = 0x008;" "pub const PON_PPID: u16 = 0x108;"
try_in kernel/lucina-spmi/src/allow.rs "if len == 0 || len > 4 ||" "if len == 0 || len > 8 ||"
try_in kernel/lucina-spmi/src/allow.rs "        && (len < 4 || reg_allowed(ppid, (first + 3) as u16))" "        && (len < 4 || true)"
try_in kernel/lucina-spmi/src/allow.rs "(ppid == PON_PPID && interrupt) || (ppid == ALARM_PPID && (interrupt || alarm))" "(ppid == PON_PPID && (interrupt || alarm)) || (ppid == ALARM_PPID && (interrupt || alarm))"
# Patch 1193: the power thread's wait.
try_in kernel/lucina-power/src/wake_rules.rs "pub const QUIET_POLLS: u32 = 10;" "pub const QUIET_POLLS: u32 = 1;"
try_in kernel/lucina-power/src/wake_rules.rs "pub fn may_wait(armed: bool, quiet: u32) -> bool {
    armed && quiet >= QUIET_POLLS" "pub fn may_wait(armed: bool, quiet: u32) -> bool {
    quiet >= QUIET_POLLS"
try_in kernel/lucina-power/src/wake_rules.rs "pub fn may_wait(armed: bool, quiet: u32) -> bool {
    armed && quiet >= QUIET_POLLS" "pub fn may_wait(armed: bool, quiet: u32) -> bool {
    armed && quiet > QUIET_POLLS"
try_in kernel/lucina-power/src/wake_rules.rs "pub open spec fn may_wait_spec(armed: bool, quiet: u32) -> bool {
    armed && quiet >= QUIET_POLLS" "pub open spec fn may_wait_spec(armed: bool, quiet: u32) -> bool {
    quiet >= QUIET_POLLS"
echo "$refused of $total refused"
echo "mutants: $refused refused by verification, $weak refused without a verification run"
if [ "${MUTANTS_STRICT:-0}" = "1" ] && [ "$weak" -gt 0 ]; then exit 1; fi
exit $fail
