#!/bin/sh
# Build the QRTR oracle from Linux's own struct definitions and regenerate the golden files in
# ../tests/fixtures. Run on buildhost; see README.md.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
linux=${LINUX:-/home/user/Projects/Lucina/lucinaos-kernel}
work=${WORK:-$HOME/scratch/juno-wifi/qrtr-oracle}
out=${OUT:-$here/../tests/fixtures}
af=$linux/net/qrtr/af_qrtr.c

mkdir -p "$work"

# The header layouts and constants come out of af_qrtr.c, ns.c and qrtr.h by text, so that what is compared
# against is what Linux has, not a copy typed here.
{
    echo "/* extracted from $af, ns.c and qrtr.h: do not edit */"
    grep -E '^#define (QRTR_PROTO_VER_[12]|QRTR_PORT_CTRL_LEGACY|QRTR_MIN_EPH_SOCKET|QRTR_MAX_EPH_SOCKET|QRTR_FLAGS_CONFIRM_RX|QRTR_TX_FLOW_(HIGH|LOW))\b' "$af"
    grep -E '^#define QRTR_NS_MAX_(NODES|SERVERS|LOOKUPS)\b' "$linux/net/qrtr/ns.c"
    grep -E '^#define QRTR_EP_NID_AUTO\b' "$linux/net/qrtr/qrtr.h"
    sed -n '/^struct qrtr_hdr_v1 {/,/^} __packed;/p' "$af"
    sed -n '/^struct qrtr_hdr_v2 {/,/^};/p' "$af"
} > "$work/extracted.h"

gcc -O1 -g -Wall \
    -I"$here/shim" -I"$work" -I"$linux/include" \
    -o "$work/qrtr-oracle" "$here/harness.c"

"$work/qrtr-oracle" "$out"
echo "wrote packets.txt constants.txt in $out"
