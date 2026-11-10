#!/usr/bin/env bash
# Shows that verification/juno-charge/rules.rs can fail: each line plants one bug in a copy of a shipped file, and Verus must refuse it.
# Run on buildhost from the repo root, under verify-gate. The unchanged copy must verify first, or nothing below counts.
set -uo pipefail
verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
work=$(mktemp -d "${TMPDIR:-/tmp}/juno-charge-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
fail=0
refused=0
files="rules.rs adc.rs pdic_rules.rs"
fresh() {
    rm -rf "$work/crates" "$work/verification"
    mkdir -p "$work/drivers/lucina-charge/src" "$work/verification/juno-charge"
    for f in $files; do cp "drivers/lucina-charge/src/$f" "$work/drivers/lucina-charge/src/"; done
    cp verification/juno-charge/rules.rs "$work/verification/juno-charge/"
}
fresh
if ! "$verus" "$work/verification/juno-charge/rules.rs" > "$work/out" 2>&1; then
    echo "BASE DOES NOT VERIFY: the mutants would prove nothing"; tail -5 "$work/out"; exit 1
fi
echo "base verifies"
# mutate <file> <from> <to>
mutate() {
    local file=$1 from=$2 to=$3
    fresh
    local target=$work/drivers/lucina-charge/src/$file
    local text; text=$(<"$target")
    case "$text" in
        *"$from"*) ;;
        *) echo "NOT FOUND in $file: $from"; fail=1; return ;;
    esac
    printf "%s\n" "${text/"$from"/"$to"}" > "$target"
    if cmp -s "$target" "drivers/lucina-charge/src/$file"; then echo "NOT CHANGED: $from"; fail=1; return; fi
    if "$verus" "$work/verification/juno-charge/rules.rs" > "$work/out" 2>&1; then
        echo "NOT REFUSED ($file): $from -> $to"; fail=1
    else
        refused=$((refused + 1))
        echo "refused ($file): $from -> $to ($(grep -c "^error" "$work/out") errors)"
    fi
}
try() { mutate rules.rs "$1" "$2"; }
tryp() { mutate pdic_rules.rs "$1" "$2"; }
# The numbers.
try "VMAX_MV: u16 = 8760" "VMAX_MV: u16 = 8768"
try "DESIGN_MAX_MA: u16 = 3776" "DESIGN_MAX_MA: u16 = 3840"
try "DEFAULT_CAP_MA: u16 = 2048" "DEFAULT_CAP_MA: u16 = 2112"
try "TEMP_MAX_DK: u16 = 3182" "TEMP_MAX_DK: u16 = 3232"
try "TEMP_MIN_DK: u16 = 2732" "TEMP_MIN_DK: u16 = 2700"
try "BATTERY_MAX_MV: u16 = 8860" "BATTERY_MAX_MV: u16 = 9000"
try "CHARGE_ALARMS: u16 = 0xd000" "CHARGE_ALARMS: u16 = 0x9000"
try "FULLY_CHARGED: u16 = 0x20" "FULLY_CHARGED: u16 = 0x10"
# The stop rules.
try "r.temp_dk < TEMP_MIN_DK || " ""
try " || r.temp_dk > TEMP_MAX_DK" ""
try "if r.status & CHARGE_ALARMS != 0 {" "if r.status & 0x8000 != 0 {"
try "if r.status & FULLY_CHARGED != 0 {
        return Decision" "if false {
        return Decision"
try "r.req_ma == 0 || " ""
try " || r.req_ma == 0xffff" ""
try "r.req_mv < V_REQ_MIN_MV || " ""
try " || r.req_mv > V_REQ_MAX_MV" ""
try "r.battery_mv < BATTERY_MIN_MV || " ""
try " || r.battery_mv > BATTERY_MAX_MV" ""
try "hot_or_cold && r.battery_mv > SWELL_MAX_MV" "false"
# The limits.
try "let mv = smaller(r.req_mv, band_mv);" "let mv = band_mv;"
try "smaller(smaller(r.req_ma, cap_ma), band_ma)" "smaller(r.req_ma, band_ma)"
try "smaller(smaller(r.req_ma, cap_ma), band_ma)" "smaller(cap_ma, band_ma)"
try "Band::Hot => (HOT_MV, HOT_MA)" "Band::Hot => (VMAX_MV, HOT_MA)"
try "Band::Hot => (HOT_MV, HOT_MA)" "Band::Hot => (HOT_MV, COOL_MA + 1024)"
try "Band::Cold => (VMAX_MV, COLD_MA)" "Band::Cold => (VMAX_MV, COOL_MA)"
try "Band::Cool => (VMAX_MV, COOL_MA)" "Band::Cool => (VMAX_MV, HOT_MA)"
try "if r.battery_mv < PRECHARGE_BELOW_MV {" "if false {"
try "mv: voltage_reg(mv)," "mv,"
try "ma: current_reg(ma)," "ma,"
try "mv - mv % 8" "mv"
try "ma - ma % 64" "ma"
# What a round may write.
try "if f.latched {" "if false {"
try "if !f.healthy {" "if false {"
try "f.chip_ma > DEFAULT_CAP_MA || " ""
try " || f.chip_mv > VMAX_MV" ""
try "if !f.watchdog_set {" "if false {"
try "if f.over_current {" "if false {"
try "if !f.fresh {" "if false {"
try "if !f.adapter {" "if false {"
try "if !f.window_open {" "if false {"
try "if !f.timer_left {" "if false {"
try "if !f.ids_known {" "if false {"
try "Decision::Charge { .. } if f.full =>" "Decision::Charge { .. } if false =>"
try "Decision::Charge { mv, ma } if f.due =>" "Decision::Charge { mv, ma } if true =>"
try "let action = if !f.verified {" "let action = if false {"
# The small rules.
try "if temp_dk > 3140 {" "if temp_dk > 3180 {"
try "if temp_dk < 2782 {" "if temp_dk < 2700 {"
try "if status & FULLY_CHARGED != 0 {
        return true;" "if false {
        return true;"
try "Some(c) => c > RECHARGE_AT_PERCENT," "Some(c) => c > 100,"
try "None => true," "None => false,"
try "now_ms - last_change_ms <= FROZEN_LIMIT_MS
}" "true
}"
try "chip_ma as i32 + (chip_ma / 10) as i32 + 64" "chip_ma as i32 + (chip_ma / 5) as i32 + 64"
# The health line.s battery temperature is off by a tenth of a degree (patch 1058).
try "    temp_dk as i32 - 2731" "    temp_dk as i32 - 2730"
# ---- USB-C: the consumer switch.
tryp "!p0.tried && !p1.sink && off1" "!p0.tried && off1"
tryp "!p1.tried && !p0.sink && off0" "!p1.tried && off0"
tryp "if p0.sink && !p0.fault && off0" "if p0.sink && off0"
tryp "pub fn has_fault(byte3: u8, water: u8) -> bool {
    byte3 & 0x60 != 0" "pub fn has_fault(byte3: u8, water: u8) -> bool {
    byte3 & 0x20 != 0"
tryp "(15 <= state && state <= 25) || state == 29
}" "(15 <= state && state <= 26) || state == 29
}"
# ---- USB-C: the PDO request (EmuEC limits, advertised PDOs only, nothing on detach or error).
tryp "(raw % 1024) * 10)" "(raw % 1024) * 11)"
tryp "raw < 0x4000_0000 && (raw / 1024) % 1024 < 0x191" "(raw / 1024) % 1024 < 0x191"
tryp "if b > 2770 { 2770 } else { b }" "if b > 2800 { 2800 } else { b }"
tryp "if b > 2300 { 2300 } else { b }" "if b > 2400 { 2400 } else { b }"
tryp "if b > 1500 { 1500 } else { b }" "if b > 1600 { 1600 } else { b }"
tryp "        452
    };" "        999
    };"
tryp "if band < ma { band } else { ma }" "band"
tryp "while i <= num {" "while i <= num + 1 {"
tryp "if mv < PDO_KEEP_BELOW_MV && caps[i] < 0x4000_0000 {" "if caps[i] < 0x4000_0000 {"
tryp "if mv < PDO_KEEP_BELOW_MV && caps[i] < 0x4000_0000 {" "if mv < PDO_KEEP_BELOW_MV {"
tryp "if fault && num >= 1 && caps[1] < 0x4000_0000 {" "if fault {"
tryp "if fault && num >= 1 && caps[1] < 0x4000_0000 {" "if false {"
tryp "        None => None,
        Some(v) => {" "        None => Some(PE_SELECT_V0),
        Some(v) => {"
tryp "} else if v < 5 {" "} else if v < 6 {"
tryp "    if !sink {
        return None;
    }
    let f" "    let f"
tryp "if f & SRC_CAP_FLAG == 0 {" "if false {"
tryp "        None => return None,
    };
    let (sel" "        None => [0x1000, 0, 0, 0, 0, 0, 0, 0],
    };
    let (sel"
tryp "if sel == selected {" "if false {"
# ---- USB-C: the charger input limit EmuEC derives from the PDO (computed and logged; the write follows a laptop run).
tryp "Some(((lim / 50) * 256) as u16)" "Some(((lim / 40) * 256) as u16)"
tryp "    if mv == 0 {
        return None;
    }
    let lim" "    let lim"
tryp "    if !online {
        return None;
    }
    match wanted {" "    match wanted {"
tryp "if w != now && w != 0 && w <= 0x3c00 && w % 256 == 0 {" "if w != now && w != 0 && w <= 0x4100 && w % 256 == 0 {"
tryp "if w != now && w != 0 && w <= 0x3c00 && w % 256 == 0 {" "if w != 0 && w <= 0x3c00 && w % 256 == 0 {"
# ---- What the battery shows: it follows the cable at once.
try "    if !online {
        Shown::Discharging" "    if false {
        Shown::Discharging"
try "    } else if full {
        Shown::Full" "    } else if false {
        Shown::Full"
try "} else if current_ma > 0 || dry_ms < DRY_LIMIT_MS {" "} else if current_ma >= 0 || dry_ms < DRY_LIMIT_MS {"
try "} else if current_ma > 0 || dry_ms < DRY_LIMIT_MS {" "} else if current_ma > 0 || dry_ms < 6000 {"
tryp "pub fn rearm(flags: u32) -> bool {
    flags & SRC_CAP_FLAG != 0" "pub fn rearm(flags: u32) -> bool {
    flags != 0"
# ---- The cable before AC_STAT.
try "        Some(c) => Some(c),
        None => ac," "        Some(_) => ac,
        None => ac,"
echo "$refused refused"
[ "$fail" = 0 ] && echo "all refused"
exit "$fail"
