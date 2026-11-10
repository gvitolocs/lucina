#!/usr/bin/env bash
# Regenerates tests/fixtures/*.txt from the real Linux sources. See oracle/README.md.
# Derived from Linux drivers/net/wireless/ath/ath10k (SPDX ISC; Atheros, Qualcomm Atheros, The Linux Foundation, Qualcomm).
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
K=${ATH10K_SRC:-/home/user/Projects/Lucina/lucinaos-kernel/drivers/net/wireless/ath/ath10k}
DTSI=${SC7180_DTSI:-$K/../../../../../arch/arm64/boot/dts/qcom/sc7180.dtsi}
B=${ORACLE_BUILD:-$HOME/scratch/juno-wifi/oracle-build}
OUT=$here/../tests/fixtures
rm -rf "$B"; mkdir -p "$B" "$OUT"
CC=${CC:-gcc}
CFLAGS="-std=gnu11 -O0 -g -Wall -Wno-unused-function -Wno-unused-variable -Wno-address-of-packed-member -I$here/shim -I$B -I$K"

# --- extraction helpers -------------------------------------------------------------------------------------------
# lines A..B of a file, the markers being literal first-matching lines (inclusive)
range() { awk -v a="$2" -v b="$3" 'index($0,a)==1 && !on {on=1} on {print} on && index($0,b)==1 {exit}' "$1"; }
# range up to but not including the end marker
range_excl() { awk -v a="$2" -v b="$3" 'index($0,a)==1 && !on {on=1; next_ok=1} on && index($0,b)==1 && NR>1 {exit} on {print}' "$1"; }
# the C function called $2 in file $1 (signature may be split over two lines)
fn() {
  awk -v name="$2" '
    { l[NR]=$0 }
    END {
      for (i=1;i<=NR;i++) {
        if (match(l[i], "(^|[ *])" name "\\(") && l[i] !~ /;[ \t]*$/ && l[i] !~ /^[\t ]*(\.|return|if|ret)/ && l[i] !~ /=/) {
          s=i
          while (s>1 && l[s-1] != "" && l[s-1] !~ /[;}\/]$/) s--
          for (j=s;j<=NR;j++) { print l[j]; if (l[j] ~ /^}/) exit }
          exit
        }
      }
    }' "$1"
}

# --- inputs from the Linux tree -----------------------------------------------------------------------------------
cp "$K/ce.c" "$K/ce.h" "$B/"          # copies, so that their "hif.h" resolves to our shim instead of Linux's
range "$K/htc.h" "enum ath10k_htc_svc_gid {" "#undef SVC" > "$B/htc_svc.h"
range "$K/snoc.c" "#define WCN3990_SRC_WR_IDX_OFFSET" "static void ath10k_snoc_write32" | sed '$d' > "$B/snoc_tables.h"
range "$K/qmi.h" "struct ath10k_shadow_reg_cfg {" "};" > "$B/shadow_cfg.h"
awk '/^const struct ath10k_hw_regs wcn3990_regs/ {on=1} on {print} /^const struct ath10k_hw_values wcn3990_values/ {v=1} v && /^};/ {exit}' "$K/hw.c" > "$B/hw_wcn3990.h"
# the WCN3990 hw_params entry of core.c
# the WCN3990 hw_params entry of core.c, as a braced initialiser
awk '/\.id = WCN3990_HW_1_0_DEV_VERSION/ {on=1; print "{"} on {print} on && /^\t},/ {exit}' "$K/core.c" | sed '$s/},/}/' > "$B/core_wcn3990_entry.h"
# wire-format regions of htc.h and htt.h, plus the constants they lean on
range "$K/htc.h" "#define HTC_HOST_MAX_MSG_PER_RX_BUNDLE" "/* Host-side stuff */" | sed '$d' > "$B/htc_wire.h"
range "$K/htt.h" "enum htt_dbg_stats_type {" "/*** host side structures follow ***/" | sed '$d' > "$B/htt_wire.h"
echo '#define roundup(x,y) ((((x)+((y)-1))/(y))*(y))' > "$B/hwconst.h"
range "$K/htt.h" "#define RX_HTT_HDR_STATUS_LEN" "#define RX_HTT_HDR_STATUS_LEN" > "$B/rxlen.h"
range "$K/htt.h" "struct htt_rx_desc {" "/* rx_desc abstraction */" | sed '$d' > "$B/htt_rx_desc.h"
fn "$K/htt.c" ath10k_rx_desc_wcn3990_get_offsets > "$B/rx_desc_offsets.h"
range "$K/htt.h" "struct ath10k_htt_txbuf_64 {" "} __packed __aligned(4);" > "$B/txbuf64.h"

compile_run() { # name
  $CC $CFLAGS -o "$B/$1" "$here/$1.c"
  "$B/$1" > "$OUT/$1.txt"
  echo "wrote $OUT/$1.txt ($(wc -l < "$OUT/$1.txt") lines)"
}
export K B OUT here
{
  awk '/wifi: wifi@18800000/ {on=1} on && /reg = </ {gsub(/[<>;,]/," "); print "membase", $4, $6; } on && /GIC_SPI/ {match($0,/GIC_SPI [0-9]+/); print "irq.ce" n++, substr($0,RSTART+8,RLENGTH-8)} on && /memory-region/ {exit}' "$DTSI"
  sed -n 's/^#define \(CE_POLL_PIPE\|ATH10K_SNOC_WAKE_IRQ\|ATH10K_SNOC_RX_POST_RETRY_MS\) \+\([0-9]*\)$/snoc.\1 \2/p' "$K/snoc.c"
  sed -n 's/.*\.dma_mask = DMA_BIT_MASK(\([0-9]*\)).*/snoc.dma_mask_bits \1/p; s/.*\.msa_size = \(0x[0-9a-f]*\).*/snoc.msa_size \1/p' "$K/snoc.c"
} | awk '!seen[$1]++' > "$OUT/dt_snoc.txt"
for o in ${ORACLES:-snoc_oracle hw_oracle ce_oracle htc_oracle wmi_oracle htt_oracle regd_oracle}; do
  [ -f "$here/$o.c" ] || continue
  if [ -f "$here/prep_$o.sh" ]; then source "$here/prep_$o.sh"; fi
  compile_run "$o"
done
