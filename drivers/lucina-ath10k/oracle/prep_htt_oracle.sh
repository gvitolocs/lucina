# sourced by gen.sh: constants from the host side part of htt.h that the oracle prints
grep -E '^#define ATH10K_HTT_MAX_NUM_AM(S|P)DU_DEFAULT' "$K/htt.h" > "$B/htt_defaults.h"
