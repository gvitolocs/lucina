#!/usr/bin/env bash
# Shows that verification/juno-gpu-vm/rules.rs can fail: each line plants one bug in a copy of the shipped
# drivers/lucina-adreno/src/gpu_rules.rs, and Verus must refuse it. Run on buildhost from the repo root (or through verus-remote).
set -uo pipefail
verus=${VERUS:-$(command -v verus || echo /home/user/tools/verus/verus-x86-linux/verus)}
src=drivers/lucina-adreno/src/gpu_rules.rs
work=$(mktemp -d "${TMPDIR:-/tmp}/juno-gpu-vm-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
fail=0
refused=0
weak=0
try() {
    local from=$1 to=$2
    mkdir -p "$work/drivers/lucina-adreno/src" "$work/verification/juno-gpu-vm"
    cp "$src" "$work/$src"
    cp verification/juno-gpu-vm/rules.rs "$work/verification/juno-gpu-vm/"
    local target=$work/$src
    local text; text=$(<"$target")
    case "$text" in
        *"$from"*) ;;
        *) echo "NOT FOUND: $from"; fail=1; return ;;
    esac
    printf "%s\n" "${text/"$from"/"$to"}" > "$target"
    if cmp -s "$target" "$src"; then echo "NOT CHANGED: $from"; fail=1; return; fi
    if "$verus" "$work/verification/juno-gpu-vm/rules.rs" > "$work/out" 2>&1; then
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
if "$verus" verification/juno-gpu-vm/rules.rs > "$work/baseline" 2>&1; then
    echo "baseline: verifies"
else
    cat "$work/baseline"; echo "mutants: the unchanged files do not verify"; exit 1
fi
# A program's object over the kernel's first 2 MiB (its fence page) or the stub's slots.
try "&& iova >= BO_BASE && len <= VA_END" "&& iova >= FENCE_IOVA && len <= VA_END"
try "&& iova >= BO_BASE && len <= VA_END" "&& len <= VA_END"
# Past the end of the space, or a wrap.
try "len <= VA_END && iova <= VA_END - len" "len <= VA_END && iova < VA_END"
# Unaligned ranges.
try "iova % PAGE == 0 && len % PAGE == 0 && len > 0 && iova >= BO_BASE" "len % PAGE == 0 && len > 0 && iova >= BO_BASE"
# A read-only page written as read-write, or the wrong address bits.
try "let flags: u64 = if read_only { 0xec3 } else { 0xe43 };" "let flags: u64 = if read_only { 0xe43 } else { 0xe43 };"
try "let flags: u64 = if read_only { 0xec3 } else { 0xe43 };" "let flags: u64 = if read_only { 0xec3 } else { 0xa43 };"
try "    phys | flags
}

/// A level 1 block" "    (phys + 0x1000) | flags
}

/// A level 1 block"
# A table entry that is a block.
try "let flags: u64 = 3;" "let flags: u64 = 1;"
try "let flags: u64 = 0xe41;" "let flags: u64 = 0xe43;"
# A command buffer past its object's end, unaligned, or too long for IB_SIZE.
try "|| offset > bo_size || 4 * dwords > bo_size - offset {" "|| offset > bo_size || 4 * dwords > bo_size {"
try "if offset % 4 != 0 || dwords == 0" "if dwords == 0"
try "|| dwords > IB_MAX_DWORDS ||" "|| dwords > 0x10_0000 ||"
# Budgets.
try "&& size <= CONTEXT_BYTES_LIMIT - ctx_bytes && size <= TOTAL_BYTES_LIMIT - total_bytes" "&& size <= CONTEXT_BYTES_LIMIT - ctx_bytes"
try "if size <= held { held - size } else { 0 }" "if size <= held { held - size } else { held }"
# A fence that moves past what was queued, or forward when it is behind.
try "if ahead == 0 || ahead > submitted - retired {" "if ahead == 0 {"
try "if ahead == 0 || ahead > submitted - retired {" "if ahead > submitted - retired + 1 {"
try "    last_use <= retired
}" "    last_use <= retired + 1
}"
# Slots.
try "        0x20_0000
    } else {" "        0x40_0000
    } else {"
echo "mutants: $refused refused by verification, $weak refused without a verification run"
if [ "${MUTANTS_STRICT:-0}" = "1" ] && [ "$weak" -gt 0 ]; then exit 1; fi
exit $fail
