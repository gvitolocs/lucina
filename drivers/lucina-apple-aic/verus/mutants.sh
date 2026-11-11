#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-2.0-or-later
# Derived from drivers/irqchip/irq-apple-aic.c of AsahiLinux/linux (branch asahi, rev 6ba0637d1ed9):
# Copyright The Asahi Linux Contributors; based on irq-lpc32xx (Copyright 2015-2016 Vladimir Zapolskiy) and
# irq-bcm2836 (Copyright 2015 Broadcom). Behaviour re-expressed in Rust for LucinaOS; see the crate README.
# This is a Rust port of the behaviour of drivers/irqchip/irq-apple-aic.c; the original is the source of truth.
# Shows that drivers/lucina-apple-aic/verus/rules.rs can fail: each line plants one bug in a copy of the shipped file, and Verus must refuse it.
# Run on buildhost from the repo root, under verify-gate (or on buildhost2 with verus-remote).
set -uo pipefail
verus=${VERUS:-verus}
[ -x /home/user/tools/verus/verus-x86-linux/verus ] && verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
work=$(mktemp -d "${TMPDIR:-/tmp}/mac-aic-hwirq-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
fail=0 refused=0 total=0
try() {
    local from=$1 to=$2
    local shown="${from//$'\n'/\\n} -> ${to//$'\n'/\\n}"
    total=$((total + 1))
    mkdir -p "$work/drivers/lucina-apple-aic/src" "$work/drivers/lucina-apple-aic/verus"
    cp drivers/lucina-apple-aic/src/hwirq.rs "$work/drivers/lucina-apple-aic/src/"
    cp drivers/lucina-apple-aic/verus/rules.rs "$work/drivers/lucina-apple-aic/verus/"
    local target=$work/drivers/lucina-apple-aic/src/hwirq.rs
    local text; text=$(<"$target")
    case "$text" in
        *"$from"*) ;;
        *) echo "NOT FOUND: ${shown% -> *}"; fail=1; return ;;
    esac
    printf "%s\n" "${text/"$from"/"$to"}" > "$target"
    if cmp -s "$target" drivers/lucina-apple-aic/src/hwirq.rs; then echo "NOT CHANGED: ${shown% -> *}"; fail=1; return; fi
    if "$verus" "$work/drivers/lucina-apple-aic/verus/rules.rs" > "$work/out" 2>&1; then
        echo "NOT REFUSED: $shown"; fail=1
    else
        refused=$((refused + 1))
        echo "refused: $shown ($(grep -c "^error" "$work/out") errors)"
    fi
}
# The die lands one bit low, half in the type field.
try 'let r = ((die << 24u32)' 'let r = ((die << 23u32)'
# A hardware IRQ is packed with the wrong event type.
try 'let r = ((die << 24u32) & 0xff00_0000u32) | 0x0001_0000u32' 'let r = ((die << 24u32) & 0xff00_0000u32) | 0x0002_0000u32'
# The IRQ number spills into the type field.
try '| 0x0001_0000u32 | (irq & 0xffffu32);' '| 0x0001_0000u32 | (irq & 0x1ffffu32);'
# A FIQ number spills into the type field.
try 'let r = x & 0xffffu32;' 'let r = x & 0x1ffffu32;'
# The die is read one bit off.
try '< 256) by (bit_vector); }
    (h & 0xff00_0000u32) >> 24u32' '< 256) by (bit_vector); }
    (h & 0xff00_0000u32) >> 23u32'
# Repacking swaps die and number.
try 'let r = irq_hwirq(die, num);' 'let r = irq_hwirq(num, die);'
# Repacking loses a hwirq of another shape.
try '    } else {
        h
    }' '    } else {
        0
    }'
echo "mac-aic-hwirq mutants: $refused of $total refused"
exit $fail
