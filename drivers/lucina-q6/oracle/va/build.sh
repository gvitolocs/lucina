#!/usr/bin/env bash
# Builds and runs the VA oracle: Linux's VA macro functions (lpass-va-macro.c), its LPI pin mux (pinctrl-lpass-lpi.c) and the SM8250 pin
# table, cut out of the files unchanged, with fake-va.h in place of the kernel and regmap. main-va.c calls them as ALSA and DAPM do for
# two captures. Run on buildhost.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
LINUX=${LINUX:-/home/user/Projects/Lucina/lucinaos-kernel}
WORK=${WORK:-$HOME/scratch/juno-sound/va-oracle}
OUT=${OUT:-$here/../../tests/fixtures/va-oracle.txt}
va=$LINUX/sound/soc/codecs/lpass-va-macro.c
lpi=$LINUX/drivers/pinctrl/qcom/pinctrl-lpass-lpi.c
lpih=$LINUX/drivers/pinctrl/qcom/pinctrl-lpass-lpi.h
sm8250=$LINUX/drivers/pinctrl/qcom/pinctrl-sm8250-lpass-lpi.c
rm -rf "$WORK"
mkdir -p "$WORK"
# A definition from its first line to the first line that is only a closing brace.
cut_out() { # <file> <regex of the first line>
    RE="$2" awk '!on && $0 ~ ENVIRON["RE"] { on = 1 } on { print } on && /^}/ { exit }' "$1"
}
cut_table() { # <file> <regex of the first line>: to its `};`
    RE="$2" awk '!on && $0 ~ ENVIRON["RE"] { on = 1 } on { print } on && /^};/ { exit }' "$1"
}
{
    echo '#include "fake-va.h"'
    grep -v '^#include' "$LINUX/sound/soc/codecs/lpass-macro-common.h"
    # The register names, the enums, struct va_macro and the variants: everything before the first function, but the gain scale.
    awk '/^static bool va_is_volatile_register/ { exit } !/^#include|DECLARE_TLV_DB_SCALE/ { print }' "$va"
    cut_out "$va" '^static bool va_is_volatile_register\('
    cut_table "$va" '^static const struct reg_default va_defaults\[\]'
    cut_out "$va" '^static bool va_is_rw_register\('
    cut_out "$va" '^static bool va_is_readable_register\('
    for f in va_clk_rsc_fs_gen_request va_macro_mclk_enable va_macro_put_dec_enum va_dmic_clk_enable va_macro_enable_dmic \
        va_macro_enable_dec va_macro_hw_params va_macro_digital_mute fsgen_gate_enable fsgen_gate_disable va_macro_runtime_suspend \
        va_macro_runtime_resume va_macro_validate_dmic_sample_rate; do
        cut_out "$va" "^static (int|void) $f\\("
    done
    grep -E "^#define LPI_(SLEW|TLMM|GPIO|NO_SLEW|FLAG)" "$lpih"
    awk '/^#define LPI_PINGROUP\(/ { on = 1 } on { print } on && !/\\$/ { exit }' "$lpih"
    cut_table "$lpih" '^struct lpi_pingroup \{'
    cut_table "$lpih" '^struct lpi_function \{'
    cut_table "$lpih" '^struct lpi_pinctrl_variant_data \{'
    echo '#define GPIO_FUNC 0'
    echo '#define MAX_LPI_NUM_CLKS 2'
    echo 'struct gpio_chip { int unused; }; struct pinctrl_desc { int unused; }; struct clk_bulk_data { int unused; };'
    cut_table "$lpi" '^struct lpi_pinctrl \{'
    for f in lpi_gpio_read lpi_gpio_write lpi_gpio_set_mux lpi_config_set; do
        cut_out "$lpi" "^static int $f\\("
    done
    cut_table "$sm8250" '^enum lpass_lpi_functions \{'
    cut_table "$sm8250" '^static const struct lpi_pingroup sm8250_groups\[\]'
} > "$WORK/va-src.c"
gcc -w -std=gnu11 -I "$here" -I "$WORK" -o "$WORK/va-oracle" "$here/main-va.c"
"$WORK/va-oracle" > "$WORK/out.txt"
cp "$WORK/out.txt" "$OUT"
# Linux's SM8250 and SC7280 boards: qcom,dmic-sample-rate = 600000 and kodiak's DMIC pin states.
"$WORK/va-oracle" linux-boards > "${OUT%.txt}-linux-boards.txt"
echo "${OUT%.txt}-linux-boards.txt: $(wc -l < "${OUT%.txt}-linux-boards.txt") lines"
# This laptop's internal mic as Windows sets it up: DMIC5, VA decimator 2, LPI gpio12 and 13 with PEP component 10's states.
"$WORK/va-oracle" laptop > "${OUT%.txt}-laptop.txt"
echo "${OUT%.txt}-laptop.txt: $(wc -l < "${OUT%.txt}-laptop.txt") lines"
echo "$OUT: $(wc -l < "$OUT") lines, from Linux at $(git -C "$LINUX" rev-parse --short HEAD)"
