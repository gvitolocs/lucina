#!/usr/bin/env bash
# Builds and runs the oracle: Linux's own q6afe.c, q6adm.c, q6asm.c, q6core.c and q6dsp-common.c, unchanged, with fake.h in place of the
# kernel, called by main.c as Linux's sound stack calls them. Run on buildhost.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
LINUX=${LINUX:-/home/user/Projects/Lucina/lucinaos-kernel}
WORK=${WORK:-$HOME/scratch/juno-sound/q6-oracle}
OUT=${OUT:-$here/../tests/fixtures/oracle.txt}
q6=$LINUX/sound/soc/qcom/qdsp6
rm -rf "$WORK"
mkdir -p "$WORK/shim"
# Every kernel header these files name is an empty file here, as fake.h has what they need. Bindings and two plain headers stay real.
for f in q6afe.c q6adm.c q6asm.c q6core.c q6dsp-common.c q6afe.h q6adm.h q6asm.h q6core.h q6dsp-common.h q6routing.h; do
    grep -h '^#include <' "$q6/$f" || true
done | sed -E 's/#include <(.*)>.*/\1/' | sort -u > "$WORK/headers"
grep -h '^#include <' "$LINUX/include/linux/soc/qcom/apr.h" | sed -E 's/#include <(.*)>.*/\1/' >> "$WORK/headers"
sort -u "$WORK/headers" | grep -v -E '^(dt-bindings/|uapi/sound/compress_params.h|linux/soc/qcom/apr.h)' | while read -r h; do
    mkdir -p "$WORK/shim/$(dirname "$h")"
    : > "$WORK/shim/$h"
done
mkdir -p "$WORK/shim/linux"; : > "$WORK/shim/linux/types.h"
: > "$WORK/shim/linux/mod_devicetable.h"
cflags=(-w -std=gnu11 -include "$here/fake.h" -I "$WORK/shim" -I "$q6" -I "$LINUX/include" -I "$LINUX/include/uapi")
tu() { # <file> <driver> <export> [cflags]
    gcc "${cflags[@]}" ${4:-} -DORACLE_FILE="$q6/$1" -DORACLE_DRIVER="$2" -DORACLE_EXPORT="$3" -c "$here/tu.c" -o "$WORK/$3.o"
}
tu q6afe.c qcom_q6afe_driver oracle_afe -DORACLE_AFE_LANES
tu q6adm.c qcom_q6adm_driver oracle_adm
tu q6asm.c qcom_q6asm_driver oracle_asm
tu q6core.c qcom_q6core_driver oracle_core
gcc "${cflags[@]}" -c "$q6/q6dsp-common.c" -o "$WORK/common.o"
gcc "${cflags[@]}" -c "$here/main.c" -o "$WORK/main.o"
gcc -o "$WORK/oracle" "$WORK"/*.o
"$WORK/oracle" > "$WORK/out.txt"
cp "$WORK/out.txt" "$OUT"
echo "$OUT: $(wc -l < "$OUT") lines, from Linux at $(git -C "$LINUX" rev-parse --short HEAD)"
