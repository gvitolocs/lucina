#!/usr/bin/env bash
# Builds and runs the teo oracle: Linux's teo idle-governor decision functions (drivers/cpuidle/governors/teo.c), cut out of the
# file unchanged, with fake-teo.h in place of the kernel. main-teo.c drives them through 18000 steps of two idle tables (Juno's
# silver and gold cores) in three wake-up patterns. Run on buildhost.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
LINUX=${LINUX:-/home/user/Projects/.worktrees/LucinaOS/minerva-linux}
WORK=${WORK:-$HOME/scratch/juno-cpuidle/teo-oracle}
OUT=${OUT:-$here/../../tests/fixtures/teo-oracle.txt}
teo=$LINUX/drivers/cpuidle/governors/teo.c
govh=$LINUX/drivers/cpuidle/governors/gov.h
rm -rf "$WORK"
mkdir -p "$WORK"
mkdir -p "$(dirname "$OUT")"
# A definition from its first line to the first line that is only a closing brace.
cut_out() { # <file> <regex of the first line>
    RE="$2" awk '!on && $0 ~ ENVIRON["RE"] { on = 1 } on { print } on && /^}/ { exit }' "$1"
}
cut_table() { # <file> <regex of the first line>: to its `};`
    RE="$2" awk '!on && $0 ~ ENVIRON["RE"] { on = 1 } on { print } on && /^};/ { exit }' "$1"
}
{
    echo '#include "fake-teo.h"'
    # The governor's own thresholds, then the shared ones they are built from (gov.h).
    grep -E '^#define (LATENCY_THRESHOLD_NS|PULSE|DECAY_SHIFT)\b' "$teo"
    grep -E '^#define (RESIDENCY_THRESHOLD_NS|SAFE_TIMER_RANGE_NS)\b' "$govh"
    cut_table "$teo" '^struct teo_bin \{'
    cut_table "$teo" '^struct teo_cpu \{'
    grep '^static DEFINE_PER_CPU(struct teo_cpu, teo_cpus);' "$teo" | sed 's/^static //'
    for f in teo_decay teo_update teo_find_shallower_state teo_select teo_reflect teo_enable_device; do
        cut_out "$teo" "^static (int|void) $f\\("
    done
} > "$WORK/teo-cut.c"
gcc -O1 -Wall -std=gnu11 -I "$here" -I "$WORK" -o "$WORK/teo-oracle" "$here/main-teo.c"
"$WORK/teo-oracle" > "$OUT"
echo "$OUT: $(wc -l < "$OUT") lines, from Linux at $(git -C "$LINUX" rev-parse --short HEAD)"
