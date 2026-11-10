/* Derived from Linux drivers/net/wireless/ath/ath10k (SPDX ISC; Atheros Communications, Qualcomm Atheros, The Linux Foundation, Qualcomm
 * Innovation Center, Qualcomm Technologies): the extracted code is compiled unchanged, see oracle/README.md. */
/* HTC wire structs from the real htc.h, and the real trailer/credit functions cut out of htc.c, run on test vectors. */
#include <linux/types.h>
#include <stdio.h>
#include <stdlib.h>
#include <errno.h>
#include "htc.h"

#define spin_lock_bh(x) ((void)(x))
#define spin_unlock_bh(x) ((void)(x))
#define ath10k_warn(...) ((void)0)
#define ath10k_dbg(...) ((void)0)
#define ath10k_dbg_dump(...) ((void)0)
#define ATH10K_DBG_HTC 0
#define DIV_ROUND_UP(n, d) (((n) + (d) - 1) / (d))
#define EXPORT_SYMBOL(x)
#define SM(_v, _f) (((_v) << _f##_LSB) & _f##_MASK)
#define MS(_v, _f) (((_v) & _f##_MASK) >> _f##_LSB)

struct ath10k {
	struct { int use_fw_tx_credits; } hw_params;
};

#include "htc_funcs.inc"

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

static struct ath10k ar;
static struct ath10k_htc htc;
static int credits_cb_calls;
static void tx_credits_cb(struct ath10k *a) { credits_cb_calls++; }

static void trailer_case(int n, const uint8_t *buf, int len, int bundle_max)
{
	uint8_t padded[256] = {0};
	uint8_t la[4 * 32] = {0};
	int la_len = 0, status, i;
	memset(&htc, 0, sizeof(htc));
	htc.ar = &ar;
	htc.max_msgs_per_htc_bundle = bundle_max;
	for (i = 0; i < ATH10K_HTC_EP_COUNT; i++) htc.endpoint[i].ep_ops.ep_tx_credits = tx_credits_cb;
	credits_cb_calls = 0;
	memcpy(padded, buf, len);
	status = ath10k_htc_process_trailer(&htc, padded, len, 1, la, &la_len);
	printf("trailer.%d status=%d la_len=%d la=", n, status, la_len);
	for (i = 0; i < la_len * 4; i++) printf("%02x", la[i]);
	printf(" credits=");
	for (i = 0; i < ATH10K_HTC_EP_COUNT; i++) printf("%d%s", htc.endpoint[i].tx_credits, i < 8 ? "," : "");
	printf(" cb=%d\n", credits_cb_calls);
}

int main(void)
{
	struct ath10k_htc_hdr h;
	struct ath10k_htc_msg m;
	int i;

	SZ(struct ath10k_htc_hdr); OFF(struct ath10k_htc_hdr, eid); OFF(struct ath10k_htc_hdr, flags);
	OFF(struct ath10k_htc_hdr, len); OFF(struct ath10k_htc_hdr, trailer_len); OFF(struct ath10k_htc_hdr, seq_no);
	OFF(struct ath10k_htc_hdr, pad_len);
	SZ(struct ath10k_ath10k_htc_msg_hdr); SZ(struct ath10k_htc_ready); OFF(struct ath10k_htc_ready, credit_count);
	OFF(struct ath10k_htc_ready, credit_size); OFF(struct ath10k_htc_ready, max_endpoints);
	SZ(struct ath10k_htc_ready_extended); OFF(struct ath10k_htc_ready_extended, htc_version);
	OFF(struct ath10k_htc_ready_extended, max_msgs_per_htc_bundle); OFF(struct ath10k_htc_ready_extended, reserved);
	SZ(struct ath10k_htc_conn_svc); OFF(struct ath10k_htc_conn_svc, service_id); OFF(struct ath10k_htc_conn_svc, flags);
	SZ(struct ath10k_htc_conn_svc_response); OFF(struct ath10k_htc_conn_svc_response, service_id);
	OFF(struct ath10k_htc_conn_svc_response, status); OFF(struct ath10k_htc_conn_svc_response, eid);
	OFF(struct ath10k_htc_conn_svc_response, max_msg_size);
	SZ(struct ath10k_htc_setup_complete_extended); OFF(struct ath10k_htc_setup_complete_extended, flags);
	OFF(struct ath10k_htc_setup_complete_extended, max_msgs_per_bundled_recv);
	SZ(struct ath10k_htc_msg); OFF(struct ath10k_htc_msg, ready); OFF(struct ath10k_htc_msg, connect_service);
	SZ(struct ath10k_ath10k_htc_record_hdr); SZ(struct ath10k_htc_credit_report);
	SZ(struct ath10k_htc_lookahead_report); OFF(struct ath10k_htc_lookahead_report, lookahead);
	OFF(struct ath10k_htc_lookahead_report, post_valid); SZ(struct ath10k_htc_lookahead_bundle);
	C(ATH10K_HTC_FLAG_NEED_CREDIT_UPDATE); C(ATH10K_HTC_FLAG_SEND_BUNDLE); C(ATH10K_HTC_FLAGS_RECV_1MORE_BLOCK);
	C(ATH10K_HTC_FLAG_TRAILER_PRESENT); C(ATH10K_HTC_FLAG_BUNDLE_MASK); C(ATH10K_HTC_BUNDLE_EXTRA_MASK);
	C(ATH10K_HTC_BUNDLE_EXTRA_SHIFT); C(HTC_HOST_MAX_MSG_PER_RX_BUNDLE);
	C(ATH10K_HTC_MSG_READY_ID); C(ATH10K_HTC_MSG_CONNECT_SERVICE_ID); C(ATH10K_HTC_MSG_CONNECT_SERVICE_RESP_ID);
	C(ATH10K_HTC_MSG_SETUP_COMPLETE_ID); C(ATH10K_HTC_MSG_SETUP_COMPLETE_EX_ID); C(ATH10K_HTC_MSG_SEND_SUSPEND_COMPLETE);
	C(ATH10K_HTC_VERSION_2P0); C(ATH10K_HTC_VERSION_2P1);
	C(ATH10K_HTC_CONN_FLAGS_THRESHOLD_LEVEL_MASK); C(ATH10K_HTC_CONN_FLAGS_REDUCE_CREDIT_DRIBBLE);
	C(ATH10K_HTC_CONN_FLAGS_DISABLE_CREDIT_FLOW_CTRL); C(ATH10K_HTC_CONN_FLAGS_RECV_ALLOC_MASK);
	C(ATH10K_HTC_CONN_FLAGS_RECV_ALLOC_LSB); C(ATH10K_HTC_MSG_READY_EXT_ALT_DATA_MASK);
	C(ATH10K_HTC_CONN_SVC_STATUS_SUCCESS); C(ATH10K_HTC_CONN_SVC_STATUS_NOT_FOUND); C(ATH10K_HTC_CONN_SVC_STATUS_FAILED);
	C(ATH10K_HTC_CONN_SVC_STATUS_NO_RESOURCES); C(ATH10K_HTC_CONN_SVC_STATUS_NO_MORE_EP);
	C(ATH10K_HTC_SETUP_COMPLETE_FLAGS_RX_BNDL_EN);
	C(ATH10K_MAX_MSG_PER_HTC_TX_BUNDLE); C(ATH10K_MIN_MSG_PER_HTC_TX_BUNDLE); C(ATH10K_MIN_CREDIT_PER_HTC_TX_BUNDLE);
	C(ATH10K_HTC_RECORD_NULL); C(ATH10K_HTC_RECORD_CREDITS); C(ATH10K_HTC_RECORD_LOOKAHEAD);
	C(ATH10K_HTC_RECORD_LOOKAHEAD_BUNDLE);
	C(ATH10K_HTC_EP_COUNT); C(ATH10K_HTC_MAX_LEN); C(ATH10K_HTC_MAX_CTRL_MSG_LEN); C(ATH10K_NUM_CONTROL_TX_BUFFERS);
	C(ATH10K_HTC_CONTROL_BUFFER_SIZE);
	C(ATH10K_HTC_SVC_ID_RSVD_CTRL); C(ATH10K_HTC_SVC_ID_WMI_CONTROL); C(ATH10K_HTC_SVC_ID_HTT_DATA_MSG);
	C(ATH10K_HTC_SVC_ID_HTT_LOG_MSG);
	for (i = 0; i < 40; i++)
		printf("bundle_count.%d.%d %u\n", (i & 1) ? 32 : 16, i * 6, ath10k_htc_get_bundle_count((i & 1) ? 32 : 16, i * 6));

	/* wire dumps: the same field assignments the C makes */
	memset(&h, 0, sizeof(h));
	h.eid = 1; h.flags = ATH10K_HTC_FLAG_NEED_CREDIT_UPDATE; h.len = 0x0123; h.seq_no = 7;
	hex("hdr_tx", &h, sizeof(h));
	memset(&h, 0, sizeof(h));
	h.eid = 2; h.flags = ATH10K_HTC_FLAG_TRAILER_PRESENT; h.len = 0x0040; h.trailer_len = 8; h.pad_len = 0x0102;
	hex("hdr_rx", &h, sizeof(h));
	{ /* ath10k_htc_connect_service, WMI control: flags |= SM(tx_alloc, RECV_ALLOC) */
		u16 flags = 0; u8 tx_alloc = 1;
		memset(&m, 0, sizeof(m));
		m.hdr.message_id = ATH10K_HTC_MSG_CONNECT_SERVICE_ID;
		flags |= SM(tx_alloc, ATH10K_HTC_CONN_FLAGS_RECV_ALLOC);
		m.connect_service.flags = flags;
		m.connect_service.service_id = ATH10K_HTC_SVC_ID_WMI_CONTROL;
		hex("connect_wmi_control", &m, sizeof(m.hdr) + sizeof(m.connect_service));
		memset(&m, 0, sizeof(m));
		flags = 0; tx_alloc = 0;
		m.hdr.message_id = ATH10K_HTC_MSG_CONNECT_SERVICE_ID;
		flags |= SM(tx_alloc, ATH10K_HTC_CONN_FLAGS_RECV_ALLOC);
		flags |= ATH10K_HTC_CONN_FLAGS_DISABLE_CREDIT_FLOW_CTRL;
		m.connect_service.flags = flags;
		m.connect_service.service_id = ATH10K_HTC_SVC_ID_HTT_DATA_MSG;
		hex("connect_htt_data", &m, sizeof(m.hdr) + sizeof(m.connect_service));
	}
	{ /* ath10k_htc_start on a non-SDIO bus */
		memset(&m, 0, sizeof(m));
		m.hdr.message_id = ATH10K_HTC_MSG_SETUP_COMPLETE_EX_ID;
		hex("setup_complete_ex", &m, sizeof(m.hdr) + sizeof(m.setup_complete_ext));
	}
	{ /* ath10k_htc_wait_target on a ready_ext message (credit_count 0x20, credit_size 0x800, 8 endpoints) */
		memset(&m, 0, sizeof(m));
		m.hdr.message_id = ATH10K_HTC_MSG_READY_ID;
		m.ready_ext.base.credit_count = 0x20;
		m.ready_ext.base.credit_size = 0x800;
		m.ready_ext.base.max_endpoints = 8;
		m.ready_ext.htc_version = ATH10K_HTC_VERSION_2P1;
		m.ready_ext.max_msgs_per_htc_bundle = 40;
		m.ready_ext.reserved = 0xf123;
		hex("ready_ext", &m, sizeof(m.hdr) + sizeof(m.ready_ext));
		/* the values wait_target derives (use_fw_tx_credits = false on WCN3990) */
		printf("wait_target.total_transmit_credits 1\nwait_target.target_credit_size %d\n", m.ready_ext.base.credit_size);
		printf("wait_target.alt_data_credit_size %d\n", m.ready_ext.reserved & ATH10K_HTC_MSG_READY_EXT_ALT_DATA_MASK);
		printf("wait_target.max_msgs_per_htc_bundle %d\n",
		       m.ready_ext.max_msgs_per_htc_bundle < HTC_HOST_MAX_MSG_PER_RX_BUNDLE ? m.ready_ext.max_msgs_per_htc_bundle : HTC_HOST_MAX_MSG_PER_RX_BUNDLE);
	}
	{ /* connect_service_response */
		memset(&m, 0, sizeof(m));
		m.hdr.message_id = ATH10K_HTC_MSG_CONNECT_SERVICE_RESP_ID;
		m.connect_service_response.service_id = ATH10K_HTC_SVC_ID_WMI_CONTROL;
		m.connect_service_response.status = 0;
		m.connect_service_response.eid = 2;
		m.connect_service_response.max_msg_size = 0x0800;
		hex("connect_resp", &m, sizeof(m.hdr) + sizeof(m.connect_service_response));
	}

	/* trailer vectors */
	{
		static const uint8_t v1[] = {1, 8, 0, 0, 1, 2, 0, 0, 3, 7, 0, 0};            /* two credit reports */
		static const uint8_t v2[] = {1, 8, 0, 0, 9, 5, 0, 0, 3, 7, 0, 0};            /* eid 9 stops the record */
		static const uint8_t v3[] = {2, 12, 0, 0, 0x55, 0, 0, 0, 1, 2, 3, 4, 0xaa, 0, 0, 0}; /* valid lookahead */
		static const uint8_t v4[] = {2, 12, 0, 0, 0x55, 0, 0, 0, 1, 2, 3, 4, 0xab, 0, 0, 0}; /* bad flags: ignored */
		static const uint8_t v5[] = {3, 12, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12};   /* bundle of 3 */
		static const uint8_t v6[] = {3, 20, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20}; /* 5 > 4 */
		static const uint8_t v7[] = {3, 3, 0, 0, 1, 2, 3};                            /* bundle_cnt 0 */
		static const uint8_t v8[] = {1, 40, 0, 0, 1, 2};                              /* record longer than the trailer */
		static const uint8_t v9[] = {9, 2, 0, 0, 7, 7, 1, 4, 0, 0, 1, 1, 0, 0};       /* unknown id skipped, then credits */
		static const uint8_t v10[] = {1, 2, 0, 0, 1, 2};                              /* credit report shorter than 4 */
		static const uint8_t v11[] = {1, 4, 0, 0};                                    /* header only, len == length: C reads 4 past */
		static const uint8_t v12[] = {1, 6, 0, 0, 1, 2, 0, 0, 5, 5};                  /* uneven credit report */
		static const uint8_t v13[] = {1, 0, 0};                                       /* shorter than a record header */
		trailer_case(1, v1, sizeof(v1), 4); trailer_case(2, v2, sizeof(v2), 4); trailer_case(3, v3, sizeof(v3), 4);
		trailer_case(4, v4, sizeof(v4), 4); trailer_case(5, v5, sizeof(v5), 4); trailer_case(6, v6, sizeof(v6), 4);
		trailer_case(7, v7, sizeof(v7), 4); trailer_case(8, v8, sizeof(v8), 4); trailer_case(9, v9, sizeof(v9), 4);
		trailer_case(10, v10, sizeof(v10), 4); trailer_case(11, v11, sizeof(v11), 4); trailer_case(12, v12, sizeof(v12), 4);
		trailer_case(13, v13, sizeof(v13), 4);
	}
	return 0;
}
