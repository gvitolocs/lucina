/* Derived from Linux drivers/net/wireless/ath/ath10k (SPDX ISC; Atheros Communications, Qualcomm Atheros, The Linux Foundation, Qualcomm
 * Innovation Center, Qualcomm Technologies): the extracted code is compiled unchanged, see oracle/README.md. */
/* Prints the WCN3990 SNOC tables as the C compiler sees them. The tables are not retyped here: gen.sh cuts them out
 * of snoc.c, hw.c and the device tree, so every number comes from the Linux source. */
#include <linux/types.h>
#include "ce.h"
#include "htc_svc.h"
#include "shadow_cfg.h"

static void ath10k_snoc_htc_tx_cb(struct ath10k_ce_pipe *p) {}
static void ath10k_snoc_htt_tx_cb(struct ath10k_ce_pipe *p) {}
static void ath10k_snoc_htc_rx_cb(struct ath10k_ce_pipe *p) {}
static void ath10k_snoc_htt_rx_cb(struct ath10k_ce_pipe *p) {}
static void ath10k_snoc_htt_htc_rx_cb(struct ath10k_ce_pipe *p) {}
static void ath10k_snoc_pktlog_rx_cb(struct ath10k_ce_pipe *p) {}

#include "snoc_tables.h"

static const char *cbname(void (*f)(struct ath10k_ce_pipe *))
{
	if (!f) return "none";
	if (f == ath10k_snoc_htc_tx_cb) return "htc_tx";
	if (f == ath10k_snoc_htt_tx_cb) return "htt_tx";
	if (f == ath10k_snoc_htc_rx_cb) return "htc_rx";
	if (f == ath10k_snoc_htt_rx_cb) return "htt_rx";
	if (f == ath10k_snoc_htt_htc_rx_cb) return "htt_htc_rx";
	if (f == ath10k_snoc_pktlog_rx_cb) return "pktlog_rx";
	return "unknown";
}

int main(void)
{
	int i;
	printf("host_ce.count %zu\n", ARRAY_SIZE(host_ce_config_wlan));
	for (i = 0; i < (int)ARRAY_SIZE(host_ce_config_wlan); i++) {
		const struct ce_attr *a = &host_ce_config_wlan[i];
		printf("host_ce.%d flags=%u src_nentries=%u src_sz_max=%u dest_nentries=%u send_cb=%s recv_cb=%s\n", i,
		       a->flags, a->src_nentries, a->src_sz_max, a->dest_nentries, cbname(a->send_cb), cbname(a->recv_cb));
	}
	printf("target_ce.count %zu\n", ARRAY_SIZE(target_ce_config_wlan));
	for (i = 0; i < (int)ARRAY_SIZE(target_ce_config_wlan); i++) {
		const struct ce_pipe_config *p = &target_ce_config_wlan[i];
		printf("target_ce.%d pipenum=%u pipedir=%u nentries=%u nbytes_max=%u flags=%u reserved=%u\n", i,
		       p->pipenum, p->pipedir, p->nentries, p->nbytes_max, p->flags, p->reserved);
	}
	printf("svc_map.count %zu\n", ARRAY_SIZE(target_service_to_ce_map_wlan));
	for (i = 0; i < (int)ARRAY_SIZE(target_service_to_ce_map_wlan); i++) {
		const struct ce_service_to_pipe *s = &target_service_to_ce_map_wlan[i];
		printf("svc_map.%d service_id=0x%x pipedir=%u pipenum=%u\n", i, s->service_id, s->pipedir, s->pipenum);
	}
	printf("shadow.count %zu\n", ARRAY_SIZE(target_shadow_reg_cfg_map));
	for (i = 0; i < (int)ARRAY_SIZE(target_shadow_reg_cfg_map); i++)
		printf("shadow.%d ce_id=%u reg_offset=0x%x\n", i, target_shadow_reg_cfg_map[i].ce_id,
		       target_shadow_reg_cfg_map[i].reg_offset);
	printf("sizeof.ce_pipe_config %zu\nsizeof.ce_service_to_pipe %zu\nsizeof.ath10k_shadow_reg_cfg %zu\n",
	       sizeof(struct ce_pipe_config), sizeof(struct ce_service_to_pipe), sizeof(struct ath10k_shadow_reg_cfg));
	printf("const.PIPEDIR_NONE %d\nconst.PIPEDIR_IN %d\nconst.PIPEDIR_OUT %d\nconst.PIPEDIR_INOUT %d\n", PIPEDIR_NONE,
	       PIPEDIR_IN, PIPEDIR_OUT, PIPEDIR_INOUT);
	printf("const.CE_ATTR_NO_SNOOP %lu\nconst.CE_ATTR_BYTE_SWAP_DATA %lu\nconst.CE_ATTR_SWIZZLE_DESCRIPTORS %lu\n"
	       "const.CE_ATTR_DIS_INTR %lu\nconst.CE_ATTR_POLL %lu\nconst.CE_ATTR_FLAGS %d\n",
	       CE_ATTR_NO_SNOOP, CE_ATTR_BYTE_SWAP_DATA, CE_ATTR_SWIZZLE_DESCRIPTORS, CE_ATTR_DIS_INTR, CE_ATTR_POLL,
	       CE_ATTR_FLAGS);
	printf("const.WCN3990_SRC_WR_IDX_OFFSET 0x%x\nconst.WCN3990_DST_WR_IDX_OFFSET 0x%x\n", WCN3990_SRC_WR_IDX_OFFSET,
	       WCN3990_DST_WR_IDX_OFFSET);
	printf("svc.RSVD_CTRL 0x%x\nsvc.WMI_CONTROL 0x%x\nsvc.WMI_DATA_BE 0x%x\nsvc.WMI_DATA_BK 0x%x\nsvc.WMI_DATA_VI 0x%x\n"
	       "svc.WMI_DATA_VO 0x%x\nsvc.NMI_CONTROL 0x%x\nsvc.NMI_DATA 0x%x\nsvc.HTT_DATA_MSG 0x%x\nsvc.HTT_DATA2_MSG 0x%x\n"
	       "svc.HTT_DATA3_MSG 0x%x\nsvc.HTT_LOG_MSG 0x%x\nsvc.TEST_RAW_STREAMS 0x%x\n",
	       ATH10K_HTC_SVC_ID_RSVD_CTRL, ATH10K_HTC_SVC_ID_WMI_CONTROL, ATH10K_HTC_SVC_ID_WMI_DATA_BE,
	       ATH10K_HTC_SVC_ID_WMI_DATA_BK, ATH10K_HTC_SVC_ID_WMI_DATA_VI, ATH10K_HTC_SVC_ID_WMI_DATA_VO,
	       ATH10K_HTC_SVC_ID_NMI_CONTROL, ATH10K_HTC_SVC_ID_NMI_DATA, ATH10K_HTC_SVC_ID_HTT_DATA_MSG,
	       ATH10K_HTC_SVC_ID_HTT_DATA2_MSG, ATH10K_HTC_SVC_ID_HTT_DATA3_MSG, ATH10K_HTC_SVC_ID_HTT_LOG_MSG,
	       ATH10K_HTC_SVC_ID_TEST_RAW_STREAMS);
	/* the same lookup as ath10k_snoc_hif_map_service_to_pipe, over the real table, for every service id seen */
	{
		static const unsigned ids[] = {0, ATH10K_HTC_SVC_ID_RSVD_CTRL, ATH10K_HTC_SVC_ID_WMI_CONTROL,
			ATH10K_HTC_SVC_ID_WMI_DATA_BE, ATH10K_HTC_SVC_ID_WMI_DATA_BK, ATH10K_HTC_SVC_ID_WMI_DATA_VI,
			ATH10K_HTC_SVC_ID_WMI_DATA_VO, ATH10K_HTC_SVC_ID_NMI_CONTROL, ATH10K_HTC_SVC_ID_NMI_DATA,
			ATH10K_HTC_SVC_ID_HTT_DATA_MSG, ATH10K_HTC_SVC_ID_HTT_DATA2_MSG, ATH10K_HTC_SVC_ID_HTT_DATA3_MSG,
			ATH10K_HTC_SVC_ID_HTT_LOG_MSG, ATH10K_HTC_SVC_ID_TEST_RAW_STREAMS, 0xffff};
		for (i = 0; i < (int)ARRAY_SIZE(ids); i++) {
			bool ul_set = false, dl_set = false;
			int ul = -1, dl = -1, k;
			for (k = 0; k < (int)ARRAY_SIZE(target_service_to_ce_map_wlan); k++) {
				const struct ce_service_to_pipe *e = &target_service_to_ce_map_wlan[k];
				if (e->service_id != ids[i]) continue;
				switch (e->pipedir) {
				case PIPEDIR_NONE: break;
				case PIPEDIR_IN: dl = e->pipenum; dl_set = true; break;
				case PIPEDIR_OUT: ul = e->pipenum; ul_set = true; break;
				case PIPEDIR_INOUT: dl = ul = e->pipenum; dl_set = ul_set = true; break;
				}
			}
			if (!ul_set || !dl_set) printf("map.0x%x enoent\n", ids[i]);
			else printf("map.0x%x ul=%d dl=%d\n", ids[i], ul, dl);
		}
	}
	return 0;
}
