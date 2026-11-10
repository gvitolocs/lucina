/* Derived from Linux drivers/net/wireless/ath/ath10k (SPDX ISC; Atheros Communications, Qualcomm Atheros, The Linux Foundation, Qualcomm
 * Innovation Center, Qualcomm Technologies): the extracted code is compiled unchanged, see oracle/README.md. */
/* HTT wire structs from the real htt.h (the part before the host-side structures), the WCN3990 rx descriptor offsets
 * function from htt.c, and message dumps filled the way htt_tx.c / htt_rx.c fill and read them. */
#include <linux/types.h>
#include <stdio.h>
#include <stdlib.h>
#include <errno.h>
#include "hw.h"
#include "hwconst.h"
#include "rx_desc.h"
#include "rxlen.h"
#include "htc_wire.h"
#include "htt_wire.h"
#include "htt_rx_desc.h"
#include "txbuf64.h"
#include "htt_defaults.h"

#define SM(_v, _f) (((_v) << _f##_LSB) & _f##_MASK)
#define MS(_v, _f) (((_v) & _f##_MASK) >> _f##_LSB)
#include "rx_desc_offsets.h"

#define SZ(t) printf("sizeof.%s %zu\n", #t, sizeof(t))
#define OFF(t, f) printf("offsetof.%s.%s %zu\n", #t, #f, offsetof(t, f))
#define C(x) printf("const.%s %lld\n", #x, (long long)(x))

static void hex(const char *tag, const void *p, size_t n)
{
	const uint8_t *b = p;
	size_t i;
	printf("bytes.%s ", tag);
	for (i = 0; i < n; i++) printf("%02x", b[i]);
	printf("\n");
}

int main(void)
{
	int i;
	SZ(struct htt_cmd_hdr); SZ(struct htt_ver_req); SZ(struct htt_resp_hdr); SZ(struct htt_ver_resp);
	OFF(struct htt_ver_resp, minor); OFF(struct htt_ver_resp, major);
	SZ(struct htt_rx_ring_rx_desc_offsets); OFF(struct htt_rx_ring_rx_desc_offsets, frag_info_offset);
	SZ(struct htt_rx_ring_setup_ring32); SZ(struct htt_rx_ring_setup_ring64);
	OFF(struct htt_rx_ring_setup_ring32, rx_ring_base_paddr); OFF(struct htt_rx_ring_setup_ring32, rx_ring_len);
	OFF(struct htt_rx_ring_setup_ring32, rx_ring_bufsize); OFF(struct htt_rx_ring_setup_ring32, flags);
	OFF(struct htt_rx_ring_setup_ring32, fw_idx_init_val); OFF(struct htt_rx_ring_setup_ring32, offsets);
	OFF(struct htt_rx_ring_setup_ring64, rx_ring_base_paddr); OFF(struct htt_rx_ring_setup_ring64, rx_ring_len);
	OFF(struct htt_rx_ring_setup_ring64, rx_ring_bufsize); OFF(struct htt_rx_ring_setup_ring64, flags);
	OFF(struct htt_rx_ring_setup_ring64, fw_idx_init_val); OFF(struct htt_rx_ring_setup_ring64, offsets);
	SZ(struct htt_rx_ring_setup_hdr); OFF(struct htt_rx_ring_setup_hdr, num_rings); OFF(struct htt_rx_ring_setup_hdr, rsvd0);
	OFF(struct htt_rx_ring_setup_32, rings); OFF(struct htt_rx_ring_setup_64, rings);
	SZ(struct htt_data_tx_desc); SZ(struct htt_data_tx_desc_64);
	OFF(struct htt_data_tx_desc, flags0); OFF(struct htt_data_tx_desc, flags1); OFF(struct htt_data_tx_desc, len);
	OFF(struct htt_data_tx_desc, id); OFF(struct htt_data_tx_desc, frags_paddr); OFF(struct htt_data_tx_desc, peerid);
	OFF(struct htt_data_tx_desc_64, flags0); OFF(struct htt_data_tx_desc_64, flags1); OFF(struct htt_data_tx_desc_64, len);
	OFF(struct htt_data_tx_desc_64, id); OFF(struct htt_data_tx_desc_64, frags_paddr); OFF(struct htt_data_tx_desc_64, peerid);
	OFF(struct htt_data_tx_desc_64, offchan_tx.freq); OFF(struct htt_data_tx_desc_64, prefetch);
	SZ(struct htt_data_tx_desc_frag); OFF(struct htt_data_tx_desc_frag, tword_addr.paddr_lo);
	OFF(struct htt_data_tx_desc_frag, tword_addr.paddr_hi); OFF(struct htt_data_tx_desc_frag, tword_addr.len_16);
	SZ(struct htt_msdu_ext_desc_64); OFF(struct htt_msdu_ext_desc_64, tso_flag); OFF(struct htt_msdu_ext_desc_64, ip_identification);
	OFF(struct htt_msdu_ext_desc_64, flags); OFF(struct htt_msdu_ext_desc_64, frags);
	SZ(struct ath10k_htt_txbuf_64); OFF(struct ath10k_htt_txbuf_64, frags); OFF(struct ath10k_htt_txbuf_64, htc_hdr);
	OFF(struct ath10k_htt_txbuf_64, cmd_hdr); OFF(struct ath10k_htt_txbuf_64, cmd_tx);
	SZ(struct htt_data_tx_completion); OFF(struct htt_data_tx_completion, num_msdus); OFF(struct htt_data_tx_completion, flags2);
	OFF(struct htt_data_tx_completion, msdus);
	SZ(struct htt_rx_offload_msdu); OFF(struct htt_rx_offload_msdu, msdu_len); OFF(struct htt_rx_offload_msdu, peer_id);
	OFF(struct htt_rx_offload_msdu, vdev_id); OFF(struct htt_rx_offload_msdu, tid); OFF(struct htt_rx_offload_msdu, fw_desc);
	OFF(struct htt_rx_offload_msdu, payload);
	SZ(struct htt_rx_offload_ind); OFF(struct htt_rx_offload_ind, msdu_count);
	SZ(struct htt_rx_in_ord_msdu_desc); SZ(struct htt_rx_in_ord_msdu_desc_ext);
	OFF(struct htt_rx_in_ord_msdu_desc_ext, msdu_len); OFF(struct htt_rx_in_ord_msdu_desc_ext, fw_desc);
	SZ(struct htt_rx_in_ord_ind); OFF(struct htt_rx_in_ord_ind, info); OFF(struct htt_rx_in_ord_ind, peer_id);
	OFF(struct htt_rx_in_ord_ind, vdev_id); OFF(struct htt_rx_in_ord_ind, msdu_count); OFF(struct htt_rx_in_ord_ind, msdu_descs64);
	OFF(struct htt_resp, rx_in_ord_ind); OFF(struct htt_resp, data_tx_completion); OFF(struct htt_resp, ver_resp);
	SZ(struct htt_data_tx_ppdu_dur);

	C(HTT_H2T_MSG_TYPE_VERSION_REQ); C(HTT_H2T_MSG_TYPE_TX_FRM); C(HTT_H2T_MSG_TYPE_RX_RING_CFG);
	C(HTT_H2T_MSG_TYPE_STATS_REQ); C(HTT_H2T_MSG_TYPE_SYNC); C(HTT_H2T_MSG_TYPE_AGGR_CFG);
	C(HTT_H2T_MSG_TYPE_FRAG_DESC_BANK_CFG); C(HTT_H2T_MSG_TYPE_MGMT_TX); C(HTT_H2T_MSG_TYPE_TX_FETCH_RESP);
	C(HTT_TLV_T2H_MSG_TYPE_VERSION_CONF); C(HTT_TLV_T2H_MSG_TYPE_RX_IND); C(HTT_TLV_T2H_MSG_TYPE_RX_FLUSH);
	C(HTT_TLV_T2H_MSG_TYPE_PEER_MAP); C(HTT_TLV_T2H_MSG_TYPE_PEER_UNMAP); C(HTT_TLV_T2H_MSG_TYPE_RX_ADDBA);
	C(HTT_TLV_T2H_MSG_TYPE_RX_DELBA); C(HTT_TLV_T2H_MSG_TYPE_TX_COMPL_IND); C(HTT_TLV_T2H_MSG_TYPE_PKTLOG);
	C(HTT_TLV_T2H_MSG_TYPE_STATS_CONF); C(HTT_TLV_T2H_MSG_TYPE_RX_FRAG_IND); C(HTT_TLV_T2H_MSG_TYPE_SEC_IND);
	C(HTT_TLV_T2H_MSG_TYPE_RC_UPDATE_IND); C(HTT_TLV_T2H_MSG_TYPE_TX_INSPECT_IND);
	C(HTT_TLV_T2H_MSG_TYPE_MGMT_TX_COMPL_IND); C(HTT_TLV_T2H_MSG_TYPE_TX_CREDIT_UPDATE_IND);
	C(HTT_TLV_T2H_MSG_TYPE_RX_PN_IND); C(HTT_TLV_T2H_MSG_TYPE_RX_OFFLOAD_DELIVER_IND);
	C(HTT_TLV_T2H_MSG_TYPE_RX_IN_ORD_PADDR_IND); C(HTT_TLV_T2H_MSG_TYPE_WDI_IPA_OP_RESPONSE);
	C(HTT_TLV_T2H_MSG_TYPE_CHAN_CHANGE); C(HTT_TLV_T2H_MSG_TYPE_RX_OFLD_PKT_ERR); C(HTT_TLV_T2H_MSG_TYPE_TEST);
	C(HTT_TLV_T2H_NUM_MSGS);
	C(HTT_DATA_TX_DESC_FLAGS0_MAC_HDR_PRESENT); C(HTT_DATA_TX_DESC_FLAGS0_NO_AGGR); C(HTT_DATA_TX_DESC_FLAGS0_NO_ENCRYPT);
	C(HTT_DATA_TX_DESC_FLAGS0_NO_CLASSIFY); C(HTT_DATA_TX_DESC_FLAGS0_PKT_TYPE_MASK); C(HTT_DATA_TX_DESC_FLAGS0_PKT_TYPE_LSB);
	C(HTT_DATA_TX_DESC_FLAGS1_VDEV_ID_MASK); C(HTT_DATA_TX_DESC_FLAGS1_VDEV_ID_LSB); C(HTT_DATA_TX_DESC_FLAGS1_EXT_TID_MASK);
	C(HTT_DATA_TX_DESC_FLAGS1_EXT_TID_LSB); C(HTT_DATA_TX_DESC_FLAGS1_POSTPONED); C(HTT_DATA_TX_DESC_FLAGS1_MORE_IN_BATCH);
	C(HTT_DATA_TX_DESC_FLAGS1_CKSUM_L3_OFFLOAD); C(HTT_DATA_TX_DESC_FLAGS1_CKSUM_L4_OFFLOAD);
	C(HTT_DATA_TX_DESC_FLAGS1_TX_COMPLETE);
	C(HTT_DATA_TX_EXT_TID_NON_QOS_MCAST_BCAST); C(HTT_DATA_TX_EXT_TID_MGMT); C(HTT_DATA_TX_EXT_TID_INVALID);
	C(HTT_INVALID_PEERID); C(HTT_MSDU_CHECKSUM_ENABLE_64); C(HTT_MSDU_CHECKSUM_ENABLE);
	C(HTT_RX_RING_FLAGS_MAC80211_HDR); C(HTT_RX_RING_FLAGS_PHY_DATA_RX); C(HTT_RX_RING_SIZE_MIN); C(HTT_RX_RING_SIZE_MAX);
	C(HTT_RX_RING_SIZE); C(HTT_RX_RING_FILL_LEVEL); C(HTT_RX_RING_FILL_LEVEL_DUAL_MAC);
	C(HTT_DATA_TX_STATUS_OK); C(HTT_DATA_TX_STATUS_DISCARD); C(HTT_DATA_TX_STATUS_NO_ACK); C(HTT_DATA_TX_STATUS_POSTPONE);
	C(HTT_DATA_TX_STATUS_MASK); C(HTT_DATA_TX_STATUS_LSB); C(HTT_DATA_TX_TID_MASK); C(HTT_DATA_TX_TID_LSB);
	C(HTT_DATA_TX_TID_INVALID); C(HTT_TX_COMPL_INV_MSDU_ID);
	C(HTT_TX_CMPL_FLAG_DATA_RSSI); C(HTT_TX_CMPL_FLAG_PPID_PRESENT); C(HTT_TX_CMPL_FLAG_PA_PRESENT);
	C(HTT_TX_CMPL_FLAG_PPDU_DURATION_PRESENT); C(HTT_TX_DATA_RSSI_ENABLE_WCN3990); C(HTT_TX_DATA_APPEND_RETRIES);
	C(HTT_TX_DATA_APPEND_TIMESTAMP);
	C(HTT_RX_IN_ORD_IND_INFO_TID_MASK); C(HTT_RX_IN_ORD_IND_INFO_OFFLOAD_MASK); C(HTT_RX_IN_ORD_IND_INFO_FRAG_MASK);
	C(HTT_TX_CREDIT_DELTA_ABS_M); C(HTT_TX_CREDIT_SIGN_BIT_M);
	C(RX_HTT_HDR_STATUS_LEN);
	C(ATH10K_HW_TXRX_RAW); C(ATH10K_HW_TXRX_NATIVE_WIFI); C(ATH10K_HW_TXRX_ETHERNET); C(ATH10K_HW_TXRX_MGMT);

	/* rx descriptor of the WCN3990 */
	{
		struct htt_rx_ring_rx_desc_offsets off;
		memset(&off, 0, sizeof(off));
		ath10k_rx_desc_wcn3990_get_offsets(&off);
		hex("wcn3990_rx_desc_offsets", &off, sizeof(off));
		printf("wcn3990_rx_desc.size %zu\nwcn3990_rx_desc.msdu_payload_offset %zu\n", sizeof(struct htt_rx_desc_v2),
		       offsetof(struct htt_rx_desc_v2, msdu_payload));
		printf("wcn3990_rx_desc.attention %zu\nwcn3990_rx_desc.frag_info %zu\nwcn3990_rx_desc.mpdu_start %zu\n"
		       "wcn3990_rx_desc.msdu_start %zu\nwcn3990_rx_desc.msdu_end %zu\nwcn3990_rx_desc.mpdu_end %zu\n"
		       "wcn3990_rx_desc.ppdu_start %zu\nwcn3990_rx_desc.ppdu_end %zu\nwcn3990_rx_desc.rx_hdr_status %zu\n",
		       offsetof(struct htt_rx_desc_v2, attention), offsetof(struct htt_rx_desc_v2, frag_info),
		       offsetof(struct htt_rx_desc_v2, mpdu_start), offsetof(struct htt_rx_desc_v2, msdu_start),
		       offsetof(struct htt_rx_desc_v2, msdu_end), offsetof(struct htt_rx_desc_v2, mpdu_end),
		       offsetof(struct htt_rx_desc_v2, ppdu_start), offsetof(struct htt_rx_desc_v2, ppdu_end),
		       offsetof(struct htt_rx_desc_v2, rx_hdr_status));
	}

	/* ath10k_htt_h2t_ver_req_msg */
	{
		struct htt_cmd cmd;
		memset(&cmd, 0, sizeof(cmd));
		cmd.hdr.msg_type = HTT_H2T_MSG_TYPE_VERSION_REQ;
		hex("ver_req", &cmd, sizeof(cmd.hdr) + sizeof(cmd.ver_req));
	}
	/* ath10k_htt_send_rx_ring_cfg_64 with example addresses */
	{
		uint8_t buf[64] = {0};
		struct htt_cmd *cmd = (struct htt_cmd *)buf;
		struct htt_rx_ring_setup_ring64 *ring = &cmd->rx_setup_64.rings[0];
		u16 flags = 0;
		int len = sizeof(cmd->hdr) + sizeof(cmd->rx_setup_64.hdr) + sizeof(*ring);
		cmd->hdr.msg_type = HTT_H2T_MSG_TYPE_RX_RING_CFG;
		cmd->rx_setup_64.hdr.num_rings = 1;
		flags |= HTT_RX_RING_FLAGS_MAC80211_HDR; flags |= HTT_RX_RING_FLAGS_MSDU_PAYLOAD;
		flags |= HTT_RX_RING_FLAGS_PPDU_START; flags |= HTT_RX_RING_FLAGS_PPDU_END;
		flags |= HTT_RX_RING_FLAGS_MPDU_START; flags |= HTT_RX_RING_FLAGS_MPDU_END;
		flags |= HTT_RX_RING_FLAGS_MSDU_START; flags |= HTT_RX_RING_FLAGS_MSDU_END;
		flags |= HTT_RX_RING_FLAGS_RX_ATTENTION; flags |= HTT_RX_RING_FLAGS_FRAG_INFO;
		flags |= HTT_RX_RING_FLAGS_UNICAST_RX; flags |= HTT_RX_RING_FLAGS_MULTICAST_RX;
		flags |= HTT_RX_RING_FLAGS_CTRL_RX; flags |= HTT_RX_RING_FLAGS_MGMT_RX;
		flags |= HTT_RX_RING_FLAGS_NULL_RX; flags |= HTT_RX_RING_FLAGS_PHY_DATA_RX;
		printf("rx_ring_cfg.flags_all 0x%x\n", flags);
		ring->fw_idx_shadow_reg_paddr = 0x123456780ULL;
		ring->rx_ring_base_paddr = 0x1fedcba00ULL;
		ring->rx_ring_len = HTT_RX_RING_SIZE;
		ring->rx_ring_bufsize = 2048;
		ring->flags = flags;
		ring->fw_idx_init_val = 0x1234;
		ath10k_rx_desc_wcn3990_get_offsets(&ring->offsets);
		hex("rx_ring_cfg_64", buf, len);
	}
	{ /* the 32-bit variant: same fields, 4-byte addresses, offsets of the QCA6174 style descriptor not available here */
		uint8_t buf[64] = {0};
		struct htt_cmd *cmd = (struct htt_cmd *)buf;
		struct htt_rx_ring_setup_ring32 *ring = &cmd->rx_setup_32.rings[0];
		int len = sizeof(cmd->hdr) + sizeof(cmd->rx_setup_32.hdr) + sizeof(*ring);
		cmd->hdr.msg_type = HTT_H2T_MSG_TYPE_RX_RING_CFG;
		cmd->rx_setup_32.hdr.num_rings = 1;
		ring->fw_idx_shadow_reg_paddr = 0x23456780;
		ring->rx_ring_base_paddr = 0xfedcba00;
		ring->rx_ring_len = 0x400;
		ring->rx_ring_bufsize = 2048;
		ring->flags = 0xa5a5;
		ring->fw_idx_init_val = 0x1234;
		ring->offsets.mac80211_hdr_offset = 1; ring->offsets.msdu_payload_offset = 2; ring->offsets.ppdu_start_offset = 3;
		ring->offsets.ppdu_end_offset = 4; ring->offsets.mpdu_start_offset = 5; ring->offsets.mpdu_end_offset = 6;
		ring->offsets.msdu_start_offset = 7; ring->offsets.msdu_end_offset = 8; ring->offsets.rx_attention_offset = 9;
		ring->offsets.frag_info_offset = 10;
		hex("rx_ring_cfg_32", buf, len);
	}
	/* ath10k_htt_tx_64: txbuf header words for a native-wifi frame, no freq (peerid word) and with the offchannel union */
	for (i = 0; i < 3; i++) {
		struct ath10k_htt_txbuf_64 tb;
		u8 flags0 = 0;
		u16 flags1 = 0;
		int prefetch_len = i == 2 ? 36 : 32;
		memset(&tb, 0, sizeof(tb));
		if (i < 2) {
			flags0 |= HTT_DATA_TX_DESC_FLAGS0_MAC_HDR_PRESENT;
			flags0 |= SM(ATH10K_HW_TXRX_NATIVE_WIFI, HTT_DATA_TX_DESC_FLAGS0_PKT_TYPE);
		} else {
			flags0 |= SM(ATH10K_HW_TXRX_MGMT, HTT_DATA_TX_DESC_FLAGS0_PKT_TYPE);
			flags0 |= HTT_DATA_TX_DESC_FLAGS0_MAC_HDR_PRESENT;
			flags0 |= HTT_DATA_TX_DESC_FLAGS0_NO_ENCRYPT;
		}
		flags1 |= SM((u16)(i + 1), HTT_DATA_TX_DESC_FLAGS1_VDEV_ID);
		flags1 |= SM((u16)(i == 2 ? HTT_DATA_TX_EXT_TID_MGMT : 5), HTT_DATA_TX_DESC_FLAGS1_EXT_TID);
		if (i == 1) {
			flags1 |= HTT_DATA_TX_DESC_FLAGS1_CKSUM_L3_OFFLOAD;
			flags1 |= HTT_DATA_TX_DESC_FLAGS1_CKSUM_L4_OFFLOAD;
		}
		flags1 |= HTT_DATA_TX_DESC_FLAGS1_POSTPONED;
		tb.htc_hdr.eid = 1;
		tb.htc_hdr.len = sizeof(tb.cmd_hdr) + sizeof(tb.cmd_tx) + prefetch_len;
		tb.htc_hdr.flags = 0;
		tb.cmd_hdr.msg_type = HTT_H2T_MSG_TYPE_TX_FRM;
		tb.cmd_tx.flags0 = flags0;
		tb.cmd_tx.flags1 = flags1;
		tb.cmd_tx.len = 100 + i;
		tb.cmd_tx.id = 0x2a + i;
		tb.cmd_tx.frags_paddr = 0x7f0000000ULL + 0x100 * i;
		if (i == 0) tb.cmd_tx.peerid = HTT_INVALID_PEERID;
		else { tb.cmd_tx.offchan_tx.peerid = HTT_INVALID_PEERID; tb.cmd_tx.offchan_tx.freq = i == 1 ? 0 : 5180; }
		{
			char name[32];
			snprintf(name, sizeof(name), "tx_frm_64_%d", i);
			hex(name, &tb.htc_hdr, sizeof(tb.htc_hdr) + sizeof(tb.cmd_hdr) + sizeof(tb.cmd_tx));
		}
	}
	{ /* continuous frag descriptor of ath10k_htt_tx_64 */
		struct htt_msdu_ext_desc_64 d;
		struct htt_data_tx_desc_frag *frags = (struct htt_data_tx_desc_frag *)&d.frags;
		memset(&d, 0, sizeof(d));
		frags[0].tword_addr.paddr_lo = 0x23456780;
		frags[0].tword_addr.paddr_hi = 0x1;
		frags[0].tword_addr.len_16 = 1500;
		hex("msdu_ext_desc_64", &d, sizeof(d));
		d.tso_flag[3] |= HTT_MSDU_CHECKSUM_ENABLE_64;
		hex("msdu_ext_desc_64_csum", &d, sizeof(d));
	}

	/* ver conf, credit update */
	{
		uint8_t b[4] = {HTT_TLV_T2H_MSG_TYPE_VERSION_CONF, 3, 2, 0};
		struct htt_resp *r = (struct htt_resp *)b;
		hex("ver_conf", b, 4);
		printf("ver_conf.major %u\nver_conf.minor %u\n", r->ver_resp.major, r->ver_resp.minor);
	}
	for (i = 0; i < 4; i++) {
		u32 word = HTT_TLV_T2H_MSG_TYPE_TX_CREDIT_UPDATE_IND | ((u32)(100 + i) << 16) | (i & 1 ? 0x100 : 0);
		int delta = HTT_TX_CREDIT_DELTA_ABS_GET(word);
		if (HTT_TX_CREDIT_SIGN_BIT_GET(word)) delta = -delta;
		printf("credit_update.%d word=0x%08x delta=%d\n", i, word, delta);
	}

	/* tx completion indications */
	{
		struct { const char *name; int n; int flags2; } cases[] = {
			{"tx_compl_even", 4, 0}, {"tx_compl_odd", 3, 0}, {"tx_compl_rssi_even", 4, HTT_TX_DATA_RSSI_ENABLE_WCN3990},
			{"tx_compl_rssi_odd", 3, HTT_TX_DATA_RSSI_ENABLE_WCN3990},
			{"tx_compl_rssi_retries_ts", 2, HTT_TX_DATA_RSSI_ENABLE_WCN3990 | HTT_TX_DATA_APPEND_RETRIES | HTT_TX_DATA_APPEND_TIMESTAMP},
		};
		int c;
		for (c = 0; c < 5; c++) {
			uint8_t b[128] = {0};
			struct htt_resp *r = (struct htt_resp *)b;
			int n = cases[c].n, k, words;
			int rssi = !!(cases[c].flags2 & HTT_TX_DATA_RSSI_ENABLE_WCN3990);
			int pad = 0;
			__le16 *m = r->data_tx_completion.msdus;
			r->hdr.msg_type = HTT_TLV_T2H_MSG_TYPE_TX_COMPL_IND;
			r->data_tx_completion.flags = (c == 1 ? HTT_DATA_TX_STATUS_NO_ACK : HTT_DATA_TX_STATUS_OK) | (5 << HTT_DATA_TX_TID_LSB);
			r->data_tx_completion.num_msdus = n;
			r->data_tx_completion.flags2 = cases[c].flags2;
			for (k = 0; k < n; k++) m[k] = 0x100 + k;
			if (n & 1) m[n] = 0xffff;
			if (cases[c].flags2 & HTT_TX_DATA_APPEND_RETRIES) pad += 2;
			if (cases[c].flags2 & HTT_TX_DATA_APPEND_TIMESTAMP) pad += 2;
			words = n + (n & 1);
			if (rssi) {
				/* layout implied by ath10k_htt_rx_tx_compl_ind: ids, (retries, timestamp,) rssi per msdu */
				for (k = 0; k < pad; k++) m[words + k] = 0xe000 + k;
				for (k = 0; k < n; k++) m[words + pad + k] = 0x50 + k;
				words += pad + n;
			}
			hex(cases[c].name, b, 1 + 3 + 2 * words);
			/* the reader's index arithmetic, copied from the loop in ath10k_htt_rx_tx_compl_ind */
			for (k = 0; k < n; k++) {
				u16 id = m[k], ack_rssi = 0;
				if (rssi) {
					if (n & 0x01) ack_rssi = m[n + k + 1 + pad];
					else ack_rssi = m[n + k + pad];
				}
				printf("%s.msdu.%d id=0x%x ack_rssi=0x%x\n", cases[c].name, k, id, ack_rssi);
			}
		}
	}

	/* rx in-order indication, 64-bit descriptors, two msdus */
	{
		uint8_t b[128] = {0};
		struct htt_resp *r = (struct htt_resp *)b;
		struct htt_rx_in_ord_msdu_desc_ext *d = r->rx_in_ord_ind.msdu_descs64;
		r->hdr.msg_type = HTT_TLV_T2H_MSG_TYPE_RX_IN_ORD_PADDR_IND;
		r->rx_in_ord_ind.info = 5 | HTT_RX_IN_ORD_IND_INFO_FRAG_MASK;
		r->rx_in_ord_ind.peer_id = 0x1234;
		r->rx_in_ord_ind.vdev_id = 2;
		r->rx_in_ord_ind.msdu_count = 2;
		d[0].msdu_paddr = 0x1deadbe00ULL; d[0].msdu_len = 1500; d[0].fw_desc = 0x11;
		d[1].msdu_paddr = 0x2cafef000ULL; d[1].msdu_len = 60; d[1].fw_desc = 0x22; d[1].reserved = 0x33;
		hex("rx_in_ord_ind_2", b, 1 + sizeof(r->rx_in_ord_ind) + 2 * sizeof(*d));
		printf("rx_in_ord_ind_2.header_len %zu\n", 1 + sizeof(r->rx_in_ord_ind));
		r->rx_in_ord_ind.info = 3 | HTT_RX_IN_ORD_IND_INFO_OFFLOAD_MASK;
		hex("rx_in_ord_ind_2_offload", b, 1 + sizeof(r->rx_in_ord_ind) + 2 * sizeof(*d));
	}
	/* rx offload msdu in a host buffer */
	{
		uint8_t b[64] = {0};
		struct htt_rx_offload_msdu *m = (struct htt_rx_offload_msdu *)b;
		m->msdu_len = 12; m->peer_id = 0x77; m->vdev_id = 1; m->tid = 6; m->fw_desc = 0x40;
		memcpy(m->payload, "0123456789ab", 12);
		hex("rx_offload_msdu", b, sizeof(*m) + 12);
	}
	/* ---- for the driver: frag bank, aggregation, peer map, mgmt completion, rx indication, rx descriptor ---- */
	SZ(struct htt_frag_desc_bank_cfg64); SZ(struct htt_q_state_conf); SZ(struct htt_aggr_conf_v2);
	OFF(struct htt_frag_desc_bank_cfg64, bank_base_addrs); OFF(struct htt_frag_desc_bank_cfg64, bank_id); OFF(struct htt_frag_desc_bank_cfg64, q_state);
	C(HTT_FRAG_DESC_BANK_MAX); C(ATH10K_HTT_MAX_NUM_AMSDU_DEFAULT); C(ATH10K_HTT_MAX_NUM_AMPDU_DEFAULT); C(HTT_H2T_MSG_TYPE_FRAG_DESC_BANK_CFG); C(HTT_H2T_MSG_TYPE_AGGR_CFG);
	{
		struct htt_cmd cmd;
		struct htt_frag_desc_bank_cfg64 *cfg = &cmd.frag_desc_bank_cfg64;
		memset(&cmd, 0, sizeof(cmd));
		cmd.hdr.msg_type = HTT_H2T_MSG_TYPE_FRAG_DESC_BANK_CFG;
		cfg->info = 0; cfg->num_banks = 1; cfg->desc_size = sizeof(struct htt_msdu_ext_desc_64);
		cfg->bank_base_addrs[0] = 0x123456700ULL;
		cfg->bank_id[0].bank_min_id = 0; cfg->bank_id[0].bank_max_id = 127;
		cfg->q_state.paddr = 0; cfg->q_state.num_peers = 0; cfg->q_state.num_tids = 0;
		cfg->q_state.record_size = HTT_TX_Q_STATE_ENTRY_SIZE; cfg->q_state.record_multiplier = HTT_TX_Q_STATE_ENTRY_MULTIPLIER;
		hex("frag_desc_bank_cfg_64", &cmd, sizeof(cmd.hdr) + sizeof(cmd.frag_desc_bank_cfg64));
		memset(&cmd, 0, sizeof(cmd));
		cmd.hdr.msg_type = HTT_H2T_MSG_TYPE_AGGR_CFG;
		cmd.aggr_conf_v2.max_num_ampdu_subframes = 64; cmd.aggr_conf_v2.max_num_amsdu_subframes = 3;
		hex("aggr_cfg_v2", &cmd, sizeof(cmd.hdr) + sizeof(cmd.aggr_conf_v2));
	}
	OFF(struct htt_rx_peer_map, vdev_id); OFF(struct htt_rx_peer_map, peer_id); OFF(struct htt_rx_peer_map, addr); SZ(struct htt_rx_peer_map);
	OFF(struct htt_rx_peer_unmap, peer_id); SZ(struct htt_rx_peer_unmap);
	OFF(struct htt_mgmt_tx_completion, flags); OFF(struct htt_mgmt_tx_completion, desc_id); OFF(struct htt_mgmt_tx_completion, status);
	OFF(struct htt_mgmt_tx_completion, info); SZ(struct htt_mgmt_tx_completion);
	OFF(struct htt_security_indication, flags); OFF(struct htt_security_indication, peer_id); SZ(struct htt_security_indication);
	C(HTT_SECURITY_IS_UNICAST); C(HTT_SECURITY_TYPE_MASK); C(HTT_MGMT_TX_STATUS_OK); C(HTT_MGMT_TX_STATUS_RETRY); C(HTT_MGMT_TX_STATUS_DROP);
	SZ(struct htt_rx_indication_hdr); SZ(struct htt_rx_indication_ppdu); SZ(struct htt_rx_indication_prefix); SZ(struct htt_rx_indication_mpdu_range);
	OFF(struct htt_rx_indication_hdr, info0); OFF(struct htt_rx_indication_hdr, peer_id); OFF(struct htt_rx_indication_hdr, info1);
	OFF(struct htt_rx_indication_prefix, fw_rx_desc_bytes); OFF(struct htt_rx_indication_mpdu_range, mpdu_count); OFF(struct htt_rx_indication_mpdu_range, mpdu_range_status);
	C(HTT_RX_INDICATION_INFO0_EXT_TID_MASK); C(HTT_RX_INDICATION_INFO1_NUM_MPDU_RANGES_MASK); C(HTT_RX_INDICATION_INFO1_NUM_MPDU_RANGES_LSB);
	{
		uint8_t b[128] = {0};
		struct htt_resp *r = (struct htt_resp *)b;
		struct htt_rx_indication_mpdu_range *rg;
		int off;
		r->hdr.msg_type = HTT_TLV_T2H_MSG_TYPE_PEER_MAP;
		r->peer_map.vdev_id = 2; r->peer_map.peer_id = 0x123;
		memcpy(r->peer_map.addr, "\xde\xad\xbe\xef\x00\x01", 6);
		hex("peer_map", b, 1 + sizeof(r->peer_map));
		memset(b, 0, sizeof(b));
		r->hdr.msg_type = HTT_TLV_T2H_MSG_TYPE_PEER_UNMAP; r->peer_unmap.peer_id = 0x123;
		hex("peer_unmap", b, 1 + sizeof(r->peer_unmap));
		memset(b, 0, sizeof(b));
		r->hdr.msg_type = HTT_TLV_T2H_MSG_TYPE_MGMT_TX_COMPL_IND;
		r->mgmt_tx_completion.flags = HTT_MGMT_TX_CMPL_FLAG_ACK_RSSI; r->mgmt_tx_completion.desc_id = 9; r->mgmt_tx_completion.status = HTT_MGMT_TX_STATUS_RETRY;
		r->mgmt_tx_completion.ppdu_id = 5; r->mgmt_tx_completion.info = 0xaa;
		hex("mgmt_tx_compl", b, 1 + sizeof(r->mgmt_tx_completion));
		memset(b, 0, sizeof(b));
		r->hdr.msg_type = HTT_TLV_T2H_MSG_TYPE_SEC_IND;
		r->security_indication.flags = HTT_SECURITY_AES_CCMP | HTT_SECURITY_IS_UNICAST; r->security_indication.peer_id = 0x45;
		hex("sec_ind", b, 1 + sizeof(r->security_indication));
		memset(b, 0, sizeof(b));
		r->hdr.msg_type = HTT_TLV_T2H_MSG_TYPE_RX_IND;
		r->rx_ind.hdr.info0 = 5; r->rx_ind.hdr.peer_id = 0x77;
		r->rx_ind.hdr.info1 = (2 << HTT_RX_INDICATION_INFO1_NUM_MPDU_RANGES_LSB);
		r->rx_ind.prefix.fw_rx_desc_bytes = 6;
		rg = htt_rx_ind_get_mpdu_ranges(&r->rx_ind);
		off = (uint8_t *)rg - b;
		printf("rx_ind.ranges_off %d\n", off);
		rg[0].mpdu_count = 3; rg[0].mpdu_range_status = HTT_RX_IND_MPDU_STATUS_OK;
		rg[1].mpdu_count = 250; rg[1].mpdu_range_status = HTT_RX_IND_MPDU_STATUS_ERR_FCS;
		hex("rx_ind_2ranges", b, off + 2 * sizeof(*rg));
	}
	{
		struct htt_rx_desc_v2 v;
		#define RD(f) printf("rxd.%s %zu\n", #f, offsetof(struct htt_rx_desc_v2, f))
		RD(attention); RD(frag_info); RD(mpdu_start); RD(msdu_start); RD(msdu_end); RD(mpdu_end); RD(ppdu_start); RD(ppdu_end); RD(rx_hdr_status); RD(msdu_payload);
		RD(frag_info.common.ring2_more_count); RD(mpdu_start.info0); RD(mpdu_start.pn31_0); RD(mpdu_start.info1); RD(mpdu_start.pn);
		RD(msdu_start.common.info0); RD(msdu_start.common.info1); RD(msdu_end.common.info0); RD(ppdu_start.rssi_chains); RD(ppdu_start.rssi_comb); RD(ppdu_start.info1); RD(ppdu_start.info2); RD(ppdu_start.info3);
		(void)v;
		C(RX_ATTENTION_FLAGS_FIRST_MPDU); C(RX_ATTENTION_FLAGS_LAST_MPDU); C(RX_ATTENTION_FLAGS_MCAST_BCAST); C(RX_ATTENTION_FLAGS_PEER_IDX_INVALID); C(RX_ATTENTION_FLAGS_MGMT_TYPE);
		C(RX_ATTENTION_FLAGS_MSDU_LENGTH_ERR); C(RX_ATTENTION_FLAGS_MPDU_LENGTH_ERR); C(RX_ATTENTION_FLAGS_TKIP_MIC_ERR); C(RX_ATTENTION_FLAGS_DECRYPT_ERR); C(RX_ATTENTION_FLAGS_FCS_ERR); C(RX_ATTENTION_FLAGS_MSDU_DONE);
		C(RX_MPDU_START_INFO0_ENCRYPT_TYPE_MASK); C(RX_MPDU_START_INFO0_ENCRYPT_TYPE_LSB); C(RX_MPDU_START_INFO0_PEER_IDX_MASK); C(RX_MPDU_START_INFO1_TID_MASK); C(RX_MPDU_START_INFO1_TID_LSB);
		C(RX_MSDU_START_INFO0_MSDU_LENGTH_MASK); C(RX_MSDU_START_INFO1_DECAP_FORMAT_MASK); C(RX_MSDU_START_INFO1_DECAP_FORMAT_LSB);
		C(RX_MSDU_END_INFO0_FIRST_MSDU); C(RX_MSDU_END_INFO0_LAST_MSDU);
		C(RX_PPDU_START_INFO1_PREAMBLE_TYPE_MASK); C(RX_PPDU_START_INFO1_PREAMBLE_TYPE_LSB); C(RX_PPDU_START_INFO1_L_SIG_RATE_MASK); C(RX_PPDU_START_INFO1_L_SIG_RATE_SELECT); C(RX_PPDU_START_RATE_FLAG);
		C(HTT_RX_LEGACY); C(HTT_RX_HT); C(HTT_RX_HT_WITH_TXBF); C(HTT_RX_VHT); C(HTT_RX_VHT_WITH_TXBF);
		C(RX_MSDU_DECAP_RAW); C(RX_MSDU_DECAP_NATIVE_WIFI); C(RX_MSDU_DECAP_ETHERNET2_DIX); C(RX_MSDU_DECAP_8023_SNAP_LLC);
		C(HTT_RX_MPDU_ENCRYPT_NONE); C(HTT_RX_MPDU_ENCRYPT_AES_CCM_WPA2); C(HTT_RX_MPDU_ENCRYPT_TKIP_WPA);
	}
	return 0;
}
