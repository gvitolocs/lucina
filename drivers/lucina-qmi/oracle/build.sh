#!/bin/sh
# Build the QMI oracle against Linux's own sources and regenerate the golden
# files in ../tests/fixtures. Run on buildhost; see README.md.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
linux=${LINUX:-/home/user/Projects/Lucina/lucinaos-kernel}
work=${WORK:-$HOME/scratch/juno-wifi/qmi-oracle}
out=${OUT:-$here/../tests/fixtures}

mkdir -p "$work"
# -fno-strict-aliasing: qmi_encdec.c reads and writes through casted pointers.
gcc -O1 -g -fno-strict-aliasing -Wall -Wno-unused-function \
    -I"$here/shim" \
    -I"$linux/include" \
    -I"$linux/drivers/net/wireless/ath/ath10k" \
    -o "$work/qmi-oracle" \
    "$here/harness.c" \
    "$linux/drivers/soc/qcom/qmi_encdec.c" \
    "$linux/drivers/net/wireless/ath/ath10k/qmi_wlfw_v01.c"

"$work/qmi-oracle" "$out"
echo "wrote encode.txt errors.txt decode.txt constants.txt in $out"
