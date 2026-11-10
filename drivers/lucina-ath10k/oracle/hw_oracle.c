/* Derived from Linux drivers/net/wireless/ath/ath10k (SPDX ISC; Atheros Communications, Qualcomm Atheros, The Linux Foundation, Qualcomm
 * Innovation Center, Qualcomm Technologies): the extracted code is compiled unchanged, see oracle/README.md. */
/* WCN3990's register description (hw.c) and hw_params entry (core.c), compiled and printed. */
#include <linux/types.h>
#include <stdio.h>
#include "hw.h"
#include "hw_wcn3990.h"
#include "hwconst.h"
#include "rx_desc.h"
#include "rxlen.h"
#include "htc_wire.h"
#include "htt_wire.h"

struct ath10k_htt_rx_desc_ops { int dummy; };
const struct ath10k_htt_rx_desc_ops wcn3990_rx_desc_ops;
const struct ath10k_hw_ops wcn3990_ops;

static const struct ath10k_hw_params p =
#include "core_wcn3990_entry.h"
;

#define P(x) printf(#x " 0x%x\n", (unsigned)(x))
int main(void)
{
	const struct ath10k_hw_ce_regs *c = &wcn3990_ce_regs;
	const struct ath10k_hw_regs *r = &wcn3990_regs;
	const struct ath10k_hw_values *v = &wcn3990_values;
	P(c->sr_base_addr_lo); P(c->sr_base_addr_hi); P(c->sr_size_addr); P(c->dr_base_addr_lo);
	P(c->dr_base_addr_hi); P(c->dr_size_addr); P(c->misc_ie_addr); P(c->sr_wr_index_addr);
	P(c->dst_wr_index_addr); P(c->current_srri_addr); P(c->current_drri_addr); P(c->ce_rri_low);
	P(c->ce_rri_high); P(c->host_ie_addr);
	P(c->ctrl1_regs->addr); P(c->ctrl1_regs->src_ring->msb); P(c->ctrl1_regs->src_ring->lsb);
	P(c->ctrl1_regs->src_ring->mask); P(c->ctrl1_regs->dst_ring->msb); P(c->ctrl1_regs->dst_ring->lsb);
	P(c->ctrl1_regs->dst_ring->mask); P(c->ctrl1_regs->dmax->msb); P(c->ctrl1_regs->dmax->lsb);
	P(c->ctrl1_regs->dmax->mask);
	P(c->host_ie->copy_complete->mask);
	P(c->wm_regs->dstr_lmask); P(c->wm_regs->dstr_hmask); P(c->wm_regs->srcr_lmask); P(c->wm_regs->srcr_hmask);
	P(c->wm_regs->cc_mask); P(c->wm_regs->wm_mask); P(c->wm_regs->addr);
	P(c->misc_regs->axi_err); P(c->misc_regs->dstr_add_err); P(c->misc_regs->srcr_len_err);
	P(c->misc_regs->dstr_mlen_vio); P(c->misc_regs->dstr_overflow); P(c->misc_regs->srcr_overflow);
	P(c->misc_regs->err_mask); P(c->misc_regs->addr);
	P(c->wm_srcr->addr); P(c->wm_srcr->low_rst); P(c->wm_srcr->high_rst); P(c->wm_srcr->wm_low->lsb);
	P(c->wm_srcr->wm_low->mask); P(c->wm_srcr->wm_high->lsb); P(c->wm_srcr->wm_high->mask);
	P(c->wm_dstr->addr); P(c->wm_dstr->low_rst); P(c->wm_dstr->high_rst); P(c->wm_dstr->wm_low->lsb);
	P(c->wm_dstr->wm_low->mask); P(c->wm_dstr->wm_high->lsb); P(c->wm_dstr->wm_high->mask);
	P(c->upd->shift); P(c->upd->mask); P(c->upd->enable);
	P(r->ce_wrapper_base_address); P(r->ce0_base_address); P(r->ce1_base_address); P(r->ce2_base_address);
	P(r->ce3_base_address); P(r->ce4_base_address); P(r->ce5_base_address); P(r->ce6_base_address);
	P(r->ce7_base_address); P(r->ce8_base_address); P(r->ce9_base_address); P(r->ce10_base_address);
	P(r->ce11_base_address); P(r->ce_wrap_intr_sum_host_msi_lsb); P(r->ce_wrap_intr_sum_host_msi_mask);
	P(r->soc_chip_id_address);
	P(v->ce_count); P(v->msi_assign_ce_max); P(v->num_target_ce_config_wlan); P(v->ce_desc_meta_data_mask);
	P(v->ce_desc_meta_data_lsb);
	/* struct ce_desc_64 etc. are in ce.h */
	/* hw_params entry */
	P(p.id); P(p.bus); P(p.continuous_frag_desc); P(p.tx_chain_mask); P(p.rx_chain_mask); P(p.max_spatial_stream);
	P(p.sw_decrypt_mcast_mgmt); P(p.decap_align_bytes); P(p.num_peers); P(p.n_cipher_suites); P(p.ast_skid_limit);
	P(p.num_wds_entries); P(p.target_64bit); P(p.rx_ring_fill_level); P(p.shadow_reg_support); P(p.rri_on_ddr);
	P(p.hw_filter_reset_required); P(p.fw_diag_ce_download); P(p.credit_size_workaround);
	P(p.tx_stats_over_pktlog); P(p.dynamic_sar_support); P(p.hw_restart_disconnect); P(p.use_fw_tx_credits);
	P(p.delay_unmap_buffer); P(p.mcast_frame_registration); P(p.led_pin); P(p.dev_id);
	P(TARGET_HL_TLV_NUM_PEERS); P(TARGET_HL_TLV_AST_SKID_LIMIT); P(TARGET_HL_TLV_NUM_WDS_ENTRIES);
	P(TARGET_TLV_NUM_VDEVS); P(TARGET_TLV_NUM_PEERS); P(TARGET_TLV_NUM_TIDS); P(TARGET_TLV_NUM_TDLS_VDEVS);
	P(TARGET_TLV_NUM_MSDU_DESC); P(TARGET_NUM_MSDU_DESC);
	P(HTT_RX_RING_SIZE); P(HTT_RX_RING_FILL_LEVEL_DUAL_MAC);
	return 0;
}
