# sourced by gen.sh: the regulatory pair table of ath/regd_common.h
R="$K/.."
range "$R/regd_common.h" "enum EnumRd {" "};" > "$B/regd_enum.h"
range "$R/regd_common.h" "static struct reg_dmn_pair_mapping regDomainPairs[] = {" "};" > "$B/regd_pairs.h"
range "$R/regd.h" "enum ctl_group {" "};" > "$B/regd_ctl.h"
{ echo "struct reg_dmn_pair_mapping { u16 reg_domain; u16 reg_5ghz_ctl; u16 reg_2ghz_ctl; };"
  grep -hE "^#define[ \t]+(NO_CTL|CTL_FCC|CTL_MKK|CTL_ETSI|COUNTRY_ERD_FLAG|WORLDWIDE_ROAMING_FLAG|DEBUG_REG_DMN|WORLD_SKU_PREFIX|WORLD_SKU_MASK)\b" "$R/regd.h" "$R/regd_common.h" "$R/ath.h" || true
} > "$B/regd_defs.h"
