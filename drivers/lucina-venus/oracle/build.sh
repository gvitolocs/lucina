#!/usr/bin/env bash
# Builds and runs the oracle: Linux's own hfi_cmds.c, hfi_venus.c, hfi_msgs.c, hfi_parser.c and hfi_platform*.c, unchanged, with fake.h in place of
# the kernel, driven by main.c as core.c, vdec.c and helpers.c drive them for one H.264 decode, and by power.c for a suspend and a
# resume after it. Run on buildhost.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
LINUX=${LINUX:-/home/user/Projects/Lucina/lucinaos-kernel}
WORK=${WORK:-$HOME/scratch/juno-venus/oracle}
OUT=${OUT:-$here/../tests/fixtures/oracle.txt}
POWER_OUT=${POWER_OUT:-$here/../tests/fixtures/power.txt}
v=$LINUX/drivers/media/platform/qcom/venus
files=(hfi_cmds.c hfi_venus.c hfi_msgs.c hfi_parser.c hfi_platform.c hfi_platform_v4.c hfi_platform_v6.c)
rm -rf "$WORK"
mkdir -p "$WORK/shim"
# Every kernel header these files name is an empty file here: fake.h and fake-types.h have what they need.
for f in "${files[@]}" core.h dbgfs.h hfi.h hfi_cmds.h hfi_msgs.h hfi_venus.h hfi_venus_io.h hfi_helper.h hfi_parser.h hfi_platform.h firmware.h helpers.h pm_helpers.h; do
    grep -h '^#include <' "$v/$f" || true
done | sed -E 's/#include <(.*)>.*/\1/' | sort -u | while read -r h; do
    mkdir -p "$WORK/shim/$(dirname "$h")"
    : > "$WORK/shim/$h"
done
cflags=(-w -std=gnu11 -include "$here/fake.h" -include "$here/fake-types.h" -I "$WORK/shim" -I "$v")
for f in "${files[@]}"; do
    gcc "${cflags[@]}" -c "$v/$f" -o "$WORK/${f%.c}.o"
done
gcc "${cflags[@]}" -c "$here/main.c" -o "$WORK/main.o"
gcc -o "$WORK/oracle" "$WORK"/*.o
"$WORK/oracle" > "$WORK/out.txt"
cp "$WORK/out.txt" "$OUT"
echo "$OUT: $(wc -l < "$OUT") lines, from Linux at $(git -C "$LINUX" rev-parse --short HEAD)"
# power.c: the same model and session, then Linux's suspend and resume; only what follows its "power" line is kept.
gcc "${cflags[@]}" -I "$here" -c "$here/power.c" -o "$WORK/power.obj"
gcc -o "$WORK/power" $(ls "$WORK"/*.o | grep -v "/main.o$") "$WORK/power.obj"
"$WORK/power" | sed -n '/^power$/,$p' > "$POWER_OUT"
echo "$POWER_OUT: $(wc -l < "$POWER_OUT") lines"
