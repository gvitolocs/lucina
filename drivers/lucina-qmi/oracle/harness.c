// SPDX-License-Identifier: GPL-2.0
/*
 * Golden-data generator for drivers/lucina-qmi.
 *
 * Links Linux's own qmi_encdec.c and ath10k's qmi_wlfw_v01.c (see build.sh)
 * and, for every WLFW message the ath10k bring-up uses, fills the C struct
 * with non-trivial values, calls qmi_encode_message(), decodes the bytes
 * back with qmi_decode_message() and aborts unless the struct round-trips.
 * What it prints is what the Rust tests compare against:
 *
 *   encode.txt   "<case> <hex of header + TLVs>"
 *   errors.txt   "<case> <rc>"  encodes the C refuses, with its errno
 *   constants.txt "<NAME> <value>"  the ids, limits and maxima of qmi_wlfw_v01.h
 *   decode.txt   "<case> <hex> rc=<rc> key=value ..."  hand-made inputs the
 *                C decoder accepts or refuses (skipped TLVs, over-long
 *                arrays and strings, duplicates)
 *
 * The values in the Rust tests are typed in separately; the two sides never
 * share a table.
 */
#include <linux/soc/qcom/qmi.h>
#include "qmi_wlfw_v01.h"

#define TXN 0x1234

static FILE *enc, *errs, *dec;

static void hex(FILE *f, const u8 *p, size_t n)
{
	for (size_t i = 0; i < n; i++)
		fprintf(f, "%02x", p[i]);
}

static void emit(const char *name, int type, unsigned int msg_id, unsigned int txn,
		 const struct qmi_elem_info *ei, const void *s, size_t ssz, size_t maxlen)
{
	size_t len = maxlen;
	u8 *msg = qmi_encode_message(type, msg_id, &len, txn, ei, s);
	void *back;
	int rc;

	if (IS_ERR(msg)) {
		fprintf(stderr, "FAIL encode %s: %ld\n", name, PTR_ERR(msg));
		exit(1);
	}
	fprintf(enc, "%s ", name);
	hex(enc, msg, len);
	fputc('\n', enc);

	back = calloc(1, ssz);
	rc = qmi_decode_message(msg, len, ei, back);
	if (rc < 0 || memcmp(back, s, ssz)) {
		fprintf(stderr, "FAIL round trip %s: rc=%d\n", name, rc);
		exit(1);
	}
	free(back);
	free(msg);
}

static void emit_err(const char *name, int type, unsigned int msg_id,
		     const struct qmi_elem_info *ei, const void *s, size_t maxlen)
{
	size_t len = maxlen;
	void *msg = qmi_encode_message(type, msg_id, &len, TXN, ei, s);

	if (!IS_ERR(msg)) {
		fprintf(stderr, "FAIL %s: expected an encode error\n", name);
		exit(1);
	}
	fprintf(errs, "%s %ld\n", name, PTR_ERR(msg));
}

/* ---- hand-made decode inputs ---- */

struct body {
	u8 b[8192];
	size_t n;
};

static void tlv(struct body *x, u8 type, const void *data, size_t len)
{
	x->b[x->n++] = type;
	x->b[x->n++] = len & 0xff;
	x->b[x->n++] = len >> 8;
	memcpy(x->b + x->n, data, len);
	x->n += len;
}

static void tlv_resp(struct body *x)
{
	static const u8 ok[4] = { 0, 0, 0, 0 };

	tlv(x, 0x02, ok, 4);
}

static void show_cap(FILE *f, const void *p)
{
	const struct wlfw_cap_resp_msg_v01 *r = p;

	fprintf(f, " result=%u num_macs_valid=%u num_macs=%u build_id_valid=%u build_id_len=%zu fwv_valid=%u fwv=%u ts_len=%zu",
		r->resp.result, r->num_macs_valid, r->num_macs, r->fw_build_id_valid,
		strlen(r->fw_build_id), r->fw_version_info_valid, r->fw_version_info.fw_version,
		strlen(r->fw_version_info.fw_build_timestamp));
}

static void show_msa(FILE *f, const void *p)
{
	const struct wlfw_msa_info_resp_msg_v01 *r = p;

	fprintf(f, " result=%u len=%u", r->resp.result, r->mem_region_info_len);
}

static void show_bdf(FILE *f, const void *p)
{
	const struct wlfw_bdf_download_req_msg_v01 *r = p;

	fprintf(f, " valid=%u data_valid=%u data_len=%u end_valid=%u end=%u", r->valid,
		r->data_valid, r->data_len, r->end_valid, r->end);
}

static void dcase(const char *name, unsigned int msg_id, const struct qmi_elem_info *ei,
		  size_t ssz, void (*show)(FILE *, const void *), const struct body *x)
{
	u8 msg[8192 + 7];
	void *out = calloc(1, ssz);
	int rc;

	msg[0] = QMI_RESPONSE;
	msg[1] = TXN & 0xff;
	msg[2] = TXN >> 8;
	msg[3] = msg_id & 0xff;
	msg[4] = msg_id >> 8;
	msg[5] = x->n & 0xff;
	msg[6] = x->n >> 8;
	memcpy(msg + 7, x->b, x->n);
	rc = qmi_decode_message(msg, x->n + 7, ei, out);
	fprintf(dec, "%s ", name);
	hex(dec, msg, x->n + 7);
	fprintf(dec, " rc=%d", rc);
	if (rc >= 0)
		show(dec, out);
	fputc('\n', dec);
	free(out);
}

static void decode_cases(void)
{
	struct body x;
	static char s129[129], s128[128], s33[33], s32[32];
	u8 nested[64];
	u8 one = 2;

	memset(s129, 'a', sizeof(s129));
	memset(s128, 'a', sizeof(s128));
	memset(s33, 'b', sizeof(s33));
	memset(s32, 'b', sizeof(s32));

	/* An unknown optional TLV is skipped. */
	memset(&x, 0, sizeof(x));
	tlv_resp(&x);
	tlv(&x, 0x7f, "xyz", 3);
	tlv(&x, 0x15, &one, 1);
	dcase("cap_unknown_optional_skipped", 0x24, wlfw_cap_resp_msg_v01_ei,
	      sizeof(struct wlfw_cap_resp_msg_v01), show_cap, &x);

	/* An unknown mandatory TLV is refused. */
	memset(&x, 0, sizeof(x));
	tlv_resp(&x);
	tlv(&x, 0x05, "xyz", 3);
	dcase("cap_unknown_mandatory", 0x24, wlfw_cap_resp_msg_v01_ei,
	      sizeof(struct wlfw_cap_resp_msg_v01), show_cap, &x);

	/* A struct that ends at a field boundary, before its last field: the C takes it (error = 0). */
	memset(&x, 0, sizeof(x));
	{
		static const u8 result_only[2] = { 0, 0 };

		tlv(&x, 0x02, result_only, 2);
	}
	dcase("cap_response_without_error_field", 0x24, wlfw_cap_resp_msg_v01_ei,
	      sizeof(struct wlfw_cap_resp_msg_v01), show_cap, &x);

	/* Duplicates: the later one wins. */
	memset(&x, 0, sizeof(x));
	tlv_resp(&x);
	one = 1;
	tlv(&x, 0x15, &one, 1);
	one = 2;
	tlv(&x, 0x15, &one, 1);
	dcase("cap_duplicate_last_wins", 0x24, wlfw_cap_resp_msg_v01_ei,
	      sizeof(struct wlfw_cap_resp_msg_v01), show_cap, &x);

	/* Strings: decode refuses len >= elem_len, one less than the C buffer. */
	memset(&x, 0, sizeof(x));
	tlv_resp(&x);
	tlv(&x, 0x14, s128, sizeof(s128));
	dcase("cap_build_id_128_ok", 0x24, wlfw_cap_resp_msg_v01_ei,
	      sizeof(struct wlfw_cap_resp_msg_v01), show_cap, &x);

	memset(&x, 0, sizeof(x));
	tlv_resp(&x);
	tlv(&x, 0x14, s129, sizeof(s129));
	dcase("cap_build_id_129_refused", 0x24, wlfw_cap_resp_msg_v01_ei,
	      sizeof(struct wlfw_cap_resp_msg_v01), show_cap, &x);

	/* The nested string (fw_version_info.fw_build_timestamp) has a 1-byte length. */
	memset(&x, 0, sizeof(x));
	tlv_resp(&x);
	nested[0] = 0x78; nested[1] = 0x56; nested[2] = 0x34; nested[3] = 0x12;
	nested[4] = sizeof(s32);
	memcpy(nested + 5, s32, sizeof(s32));
	tlv(&x, 0x13, nested, 5 + sizeof(s32));
	dcase("cap_timestamp_32_ok", 0x24, wlfw_cap_resp_msg_v01_ei,
	      sizeof(struct wlfw_cap_resp_msg_v01), show_cap, &x);

	memset(&x, 0, sizeof(x));
	tlv_resp(&x);
	nested[4] = sizeof(s33);
	memcpy(nested + 5, s33, sizeof(s33));
	tlv(&x, 0x13, nested, 5 + sizeof(s33));
	dcase("cap_timestamp_33_refused", 0x24, wlfw_cap_resp_msg_v01_ei,
	      sizeof(struct wlfw_cap_resp_msg_v01), show_cap, &x);

	/* Var-len arrays: count above the maximum is refused; zero is fine. */
	{
		u8 v[3 + 13 * 13];
		u8 r[1 + 2 * 13] = { 0 };

		memset(&x, 0, sizeof(x));
		tlv_resp(&x);
		memset(v, 0, sizeof(v));
		v[0] = 3;
		tlv(&x, 0x03, v, 1 + 3 * 13);
		dcase("msa_info_resp_len_3_refused", 0x2d, wlfw_msa_info_resp_msg_v01_ei,
		      sizeof(struct wlfw_msa_info_resp_msg_v01), show_msa, &x);

		memset(&x, 0, sizeof(x));
		tlv_resp(&x);
		r[0] = 0;
		tlv(&x, 0x03, r, 1);
		dcase("msa_info_resp_len_0_ok", 0x2d, wlfw_msa_info_resp_msg_v01_ei,
		      sizeof(struct wlfw_msa_info_resp_msg_v01), show_msa, &x);
	}

	/* bdf: data_len is a 16-bit prefix, maximum 6144. */
	{
		static u8 v[2 + 6145];

		memset(v, 0x5a, sizeof(v));
		memset(&x, 0, sizeof(x));
		v[0] = 6144 & 0xff;
		v[1] = 6144 >> 8;
		tlv(&x, 0x01, "\x01", 1);
		tlv(&x, 0x13, v, 2 + 6144);
		dcase("bdf_req_data_6144_ok", 0x25, wlfw_bdf_download_req_msg_v01_ei,
		      sizeof(struct wlfw_bdf_download_req_msg_v01), show_bdf, &x);

		memset(&x, 0, sizeof(x));
		v[0] = 6145 & 0xff;
		v[1] = 6145 >> 8;
		tlv(&x, 0x01, "\x01", 1);
		tlv(&x, 0x13, v, 2 + 6145);
		dcase("bdf_req_data_6145_refused", 0x25, wlfw_bdf_download_req_msg_v01_ei,
		      sizeof(struct wlfw_bdf_download_req_msg_v01), show_bdf, &x);
	}
}

/* ---- constants ---- */

#define C(n) fprintf(cst, "%s %llu\n", #n, (unsigned long long)(n))

static void constants(FILE *cst)
{
	C(WLFW_SERVICE_ID_V01);
	C(WLFW_SERVICE_VERS_V01);

	C(QMI_WLFW_IND_REGISTER_REQ_V01);
	C(QMI_WLFW_FW_READY_IND_V01);
	C(QMI_WLFW_WLAN_MODE_REQ_V01);
	C(QMI_WLFW_WLAN_CFG_REQ_V01);
	C(QMI_WLFW_CAP_REQ_V01);
	C(QMI_WLFW_BDF_DOWNLOAD_REQ_V01);
	C(QMI_WLFW_CAL_REPORT_REQ_V01);
	C(QMI_WLFW_CAL_DOWNLOAD_REQ_V01);
	C(QMI_WLFW_INITIATE_CAL_DOWNLOAD_IND_V01);
	C(QMI_WLFW_CAL_UPDATE_REQ_V01);
	C(QMI_WLFW_INITIATE_CAL_UPDATE_IND_V01);
	C(QMI_WLFW_MSA_READY_IND_V01);
	C(QMI_WLFW_PIN_CONNECT_RESULT_IND_V01);
	C(QMI_WLFW_MSA_INFO_REQ_V01);
	C(QMI_WLFW_MSA_READY_REQ_V01);
	C(QMI_WLFW_INI_REQ_V01);
	C(QMI_WLFW_ATHDIAG_READ_REQ_V01);
	C(QMI_WLFW_ATHDIAG_WRITE_REQ_V01);
	C(QMI_WLFW_VBATT_REQ_V01);
	C(QMI_WLFW_MAC_ADDR_REQ_V01);
	C(QMI_WLFW_HOST_CAP_REQ_V01);
	C(QMI_WLFW_REQUEST_MEM_IND_V01);
	C(QMI_WLFW_RESPOND_MEM_REQ_V01);
	C(QMI_WLFW_MEM_READY_IND_V01);
	C(QMI_WLFW_FW_INIT_DONE_IND_V01);
	C(QMI_WLFW_REJUVENATE_IND_V01);
	C(QMI_WLFW_REJUVENATE_ACK_REQ_V01);
	C(QMI_WLFW_DYNAMIC_FEATURE_MASK_REQ_V01);
	C(QMI_WLFW_M3_INFO_REQ_V01);
	C(QMI_WLFW_XO_CAL_IND_V01);

	C(QMI_WLFW_MAX_MEM_REG_V01);
	C(QMI_WLFW_MAX_NUM_CAL_V01);
	C(QMI_WLFW_MAX_DATA_SIZE_V01);
	C(QMI_WLFW_MAX_NUM_CE_V01);
	C(QMI_WLFW_MAX_TIMESTAMP_LEN_V01);
	C(QMI_WLFW_MAX_NUM_GPIO_V01);
	C(QMI_WLFW_MAX_BUILD_ID_LEN_V01);
	C(QMI_WLFW_MAX_STR_LEN_V01);
	C(QMI_WLFW_MAX_NUM_SHADOW_REG_V01);
	C(QMI_WLFW_MAX_SHADOW_REG_V2);
	C(QMI_WLFW_MAX_NUM_SVC_V01);

	C(WLFW_IND_REGISTER_REQ_MSG_V01_MAX_MSG_LEN);
	C(WLFW_IND_REGISTER_RESP_MSG_V01_MAX_MSG_LEN);
	C(WLFW_WLAN_MODE_REQ_MSG_V01_MAX_MSG_LEN);
	C(WLFW_WLAN_MODE_RESP_MSG_V01_MAX_MSG_LEN);
	C(WLFW_WLAN_CFG_REQ_MSG_V01_MAX_MSG_LEN);
	C(WLFW_WLAN_CFG_RESP_MSG_V01_MAX_MSG_LEN);
	C(WLFW_CAP_RESP_MSG_V01_MAX_MSG_LEN);
	C(WLFW_BDF_DOWNLOAD_REQ_MSG_V01_MAX_MSG_LEN);
	C(WLFW_BDF_DOWNLOAD_RESP_MSG_V01_MAX_MSG_LEN);
	C(WLFW_CAL_REPORT_REQ_MSG_V01_MAX_MSG_LEN);
	C(WLFW_CAL_REPORT_RESP_MSG_V01_MAX_MSG_LEN);
	C(WLFW_MSA_INFO_REQ_MSG_V01_MAX_MSG_LEN);
	C(WLFW_MSA_INFO_RESP_MSG_V01_MAX_MSG_LEN);
	C(WLFW_MSA_READY_RESP_MSG_V01_MAX_MSG_LEN);
	C(WLFW_INI_REQ_MSG_V01_MAX_MSG_LEN);
	C(WLFW_INI_RESP_MSG_V01_MAX_MSG_LEN);
	C(WLFW_HOST_CAP_REQ_MSG_V01_MAX_MSG_LEN);
	C(WLFW_HOST_CAP_RESP_MSG_V01_MAX_MSG_LEN);

	C(QMI_WLFW_MISSION_V01);
	C(QMI_WLFW_FTM_V01);
	C(QMI_WLFW_EPPING_V01);
	C(QMI_WLFW_WALTEST_V01);
	C(QMI_WLFW_OFF_V01);
	C(QMI_WLFW_CCPM_V01);
	C(QMI_WLFW_QVIT_V01);
	C(QMI_WLFW_CALIBRATION_V01);
	C(QMI_WLFW_PIPEDIR_NONE_V01);
	C(QMI_WLFW_PIPEDIR_IN_V01);
	C(QMI_WLFW_PIPEDIR_OUT_V01);
	C(QMI_WLFW_PIPEDIR_INOUT_V01);
	C(QMI_WLFW_MEM_TYPE_MSA_V01);
	C(QMI_WLFW_MEM_TYPE_DDR_V01);
	C(QMI_WLFW_CE_ATTR_NO_SNOOP_V01);
	C(QMI_WLFW_CE_ATTR_BYTE_SWAP_DATA_V01);
	C(QMI_WLFW_CE_ATTR_SWIZZLE_DESCRIPTORS_V01);
	C(QMI_WLFW_CE_ATTR_DISABLE_INTR_V01);
	C(QMI_WLFW_CE_ATTR_ENABLE_POLL_V01);
	C(QMI_WLFW_ALREADY_REGISTERED_V01);
	C(QMI_WLFW_FW_READY_V01);
	C(QMI_WLFW_MSA_READY_V01);
	C(QMI_WLFW_MEM_READY_V01);
	C(QMI_WLFW_FW_INIT_DONE_V01);

	C(QMI_REQUEST);
	C(QMI_RESPONSE);
	C(QMI_INDICATION);
	C(QMI_RESULT_SUCCESS_V01);
	C(QMI_RESULT_FAILURE_V01);
	C(QMI_ERR_NONE_V01);
	C(QMI_ERR_MALFORMED_MSG_V01);
	C(QMI_ERR_NO_MEMORY_V01);
	C(QMI_ERR_INTERNAL_V01);
	C(QMI_ERR_CLIENT_IDS_EXHAUSTED_V01);
	C(QMI_ERR_INVALID_ID_V01);
	C(QMI_ERR_ENCODING_V01);
	C(QMI_ERR_DISABLED_V01);
	C(QMI_ERR_INCOMPATIBLE_STATE_V01);
	C(QMI_ERR_NOT_SUPPORTED_V01);
	C(sizeof(struct qmi_header));
}

/* ---- encode cases ---- */

static void fill_pattern(u8 *p, size_t n)
{
	for (size_t i = 0; i < n; i++)
		p[i] = (u8)(i * 7 + 3);
}

int main(int argc, char **argv)
{
	const char *dir = argc > 1 ? argv[1] : ".";
	char path[512];

	snprintf(path, sizeof(path), "%s/encode.txt", dir);
	enc = fopen(path, "w");
	snprintf(path, sizeof(path), "%s/errors.txt", dir);
	errs = fopen(path, "w");
	snprintf(path, sizeof(path), "%s/decode.txt", dir);
	dec = fopen(path, "w");
	if (!enc || !errs || !dec) {
		perror("open");
		return 1;
	}
	fprintf(enc, "# generated by drivers/lucina-qmi/oracle (Linux qmi_encdec.c); do not edit\n");
	fprintf(errs, "# generated by drivers/lucina-qmi/oracle (Linux qmi_encdec.c); do not edit\n");
	fprintf(dec, "# generated by drivers/lucina-qmi/oracle (Linux qmi_encdec.c); do not edit\n");

	/* indication register */
	{
		struct wlfw_ind_register_req_msg_v01 r = {};

		r.client_id_valid = 1;
		r.client_id = 0x4b4e454c;
		r.fw_ready_enable_valid = 1;
		r.fw_ready_enable = 1;
		r.msa_ready_enable_valid = 1;
		r.msa_ready_enable = 1;
		emit("ind_register_req_ath10k", QMI_REQUEST, QMI_WLFW_IND_REGISTER_REQ_V01, 0, wlfw_ind_register_req_msg_v01_ei,
		     &r, sizeof(r), WLFW_IND_REGISTER_REQ_MSG_V01_MAX_MSG_LEN);
		r.xo_cal_enable_valid = 1;
		r.xo_cal_enable = 1;
		emit("ind_register_req_xo_cal", QMI_REQUEST, QMI_WLFW_IND_REGISTER_REQ_V01, 1, wlfw_ind_register_req_msg_v01_ei,
		     &r, sizeof(r), WLFW_IND_REGISTER_REQ_MSG_V01_MAX_MSG_LEN);

		memset(&r, 0, sizeof(r));
		r.fw_ready_enable_valid = 1; r.fw_ready_enable = 0x11;
		r.initiate_cal_download_enable_valid = 1; r.initiate_cal_download_enable = 0x22;
		r.initiate_cal_update_enable_valid = 1; r.initiate_cal_update_enable = 0x33;
		r.msa_ready_enable_valid = 1; r.msa_ready_enable = 0x44;
		r.pin_connect_result_enable_valid = 1; r.pin_connect_result_enable = 0x55;
		r.client_id_valid = 1; r.client_id = 0xa1b2c3d4;
		r.request_mem_enable_valid = 1; r.request_mem_enable = 0x66;
		r.mem_ready_enable_valid = 1; r.mem_ready_enable = 0x77;
		r.fw_init_done_enable_valid = 1; r.fw_init_done_enable = 0x88;
		r.rejuvenate_enable_valid = 1; r.rejuvenate_enable = 0x01020304;
		r.xo_cal_enable_valid = 1; r.xo_cal_enable = 0x99;
		emit("ind_register_req_all", QMI_REQUEST, QMI_WLFW_IND_REGISTER_REQ_V01, 2, wlfw_ind_register_req_msg_v01_ei,
		     &r, sizeof(r), WLFW_IND_REGISTER_REQ_MSG_V01_MAX_MSG_LEN);
		/* the C refuses to write past the length it was given */
		emit_err("ind_register_req_all_len_10", QMI_REQUEST, QMI_WLFW_IND_REGISTER_REQ_V01, wlfw_ind_register_req_msg_v01_ei, &r, 10);
	}
	{
		struct wlfw_ind_register_resp_msg_v01 r = {};

		r.resp.result = QMI_RESULT_SUCCESS_V01;
		r.fw_status_valid = 1;
		r.fw_status = QMI_WLFW_FW_READY_V01 | QMI_WLFW_ALREADY_REGISTERED_V01 | 0x0123456700000000ULL;
		emit("ind_register_resp_fw_status", QMI_RESPONSE, QMI_WLFW_IND_REGISTER_RESP_V01, 0x0102, wlfw_ind_register_resp_msg_v01_ei,
		     &r, sizeof(r), WLFW_IND_REGISTER_RESP_MSG_V01_MAX_MSG_LEN);
		memset(&r, 0, sizeof(r));
		emit("ind_register_resp_ok", QMI_RESPONSE, QMI_WLFW_IND_REGISTER_RESP_V01, 0x0103, wlfw_ind_register_resp_msg_v01_ei,
		     &r, sizeof(r), WLFW_IND_REGISTER_RESP_MSG_V01_MAX_MSG_LEN);
		r.resp.result = QMI_RESULT_FAILURE_V01;
		r.resp.error = QMI_ERR_INCOMPATIBLE_STATE_V01;
		emit("ind_register_resp_failure", QMI_RESPONSE, QMI_WLFW_IND_REGISTER_RESP_V01, 0x0104, wlfw_ind_register_resp_msg_v01_ei,
		     &r, sizeof(r), WLFW_IND_REGISTER_RESP_MSG_V01_MAX_MSG_LEN);
	}

	/* indications without a body */
	{
		struct wlfw_fw_ready_ind_msg_v01 f = {};
		struct wlfw_msa_ready_ind_msg_v01 m = {};

		emit("fw_ready_ind", QMI_INDICATION, QMI_WLFW_FW_READY_IND_V01, 0, wlfw_fw_ready_ind_msg_v01_ei, &f, sizeof(f), 0);
		emit("msa_ready_ind", QMI_INDICATION, QMI_WLFW_MSA_READY_IND_V01, 0x7777, wlfw_msa_ready_ind_msg_v01_ei, &m, sizeof(m), 0);
	}

	/* host capabilities */
	{
		struct wlfw_host_cap_req_msg_v01 r = {};

		r.daemon_support_valid = 1;
		r.daemon_support = 0;
		emit("host_cap_req_ath10k", QMI_REQUEST, QMI_WLFW_HOST_CAP_REQ_V01, 3, wlfw_host_cap_req_msg_v01_ei,
		     &r, sizeof(r), WLFW_HOST_CAP_REQ_MSG_V01_MAX_MSG_LEN);

		memset(&r, 0, sizeof(r));
		r.daemon_support_valid = 1; r.daemon_support = 1;
		emit("host_cap_req_8bit", QMI_REQUEST, QMI_WLFW_HOST_CAP_REQ_V01, 4, wlfw_host_cap_8bit_req_msg_v01_ei,
		     &r, sizeof(r), WLFW_HOST_CAP_REQ_MSG_V01_MAX_MSG_LEN);

		memset(&r, 0, sizeof(r));
		r.daemon_support_valid = 1; r.daemon_support = 0x11223344;
		r.wake_msi_valid = 1; r.wake_msi = 0x55667788;
		r.gpios_valid = 1; r.gpios_len = 3;
		r.gpios[0] = 0xa0a1a2a3; r.gpios[1] = 5; r.gpios[2] = 0xfffffffe;
		r.nm_modem_valid = 1; r.nm_modem = 0x10;
		r.bdf_support_valid = 1; r.bdf_support = 0x11;
		r.bdf_cache_support_valid = 1; r.bdf_cache_support = 0x12;
		r.m3_support_valid = 1; r.m3_support = 0x13;
		r.m3_cache_support_valid = 1; r.m3_cache_support = 0x14;
		r.cal_filesys_support_valid = 1; r.cal_filesys_support = 0x15;
		r.cal_cache_support_valid = 1; r.cal_cache_support = 0x16;
		r.cal_done_valid = 1; r.cal_done = 0x17;
		r.mem_bucket_valid = 1; r.mem_bucket = 0x01234567;
		r.mem_cfg_mode_valid = 1; r.mem_cfg_mode = 0x18;
		emit("host_cap_req_all", QMI_REQUEST, QMI_WLFW_HOST_CAP_REQ_V01, 5, wlfw_host_cap_req_msg_v01_ei,
		     &r, sizeof(r), WLFW_HOST_CAP_REQ_MSG_V01_MAX_MSG_LEN);

		memset(&r, 0, sizeof(r));
		r.gpios_valid = 1; r.gpios_len = 0;
		emit("host_cap_req_gpios_empty", QMI_REQUEST, QMI_WLFW_HOST_CAP_REQ_V01, 6, wlfw_host_cap_req_msg_v01_ei,
		     &r, sizeof(r), WLFW_HOST_CAP_REQ_MSG_V01_MAX_MSG_LEN);

		memset(&r, 0, sizeof(r));
		r.gpios_valid = 1; r.gpios_len = 32;
		for (int i = 0; i < 32; i++)
			r.gpios[i] = 0x100 + i;
		emit("host_cap_req_gpios_32", QMI_REQUEST, QMI_WLFW_HOST_CAP_REQ_V01, 7, wlfw_host_cap_req_msg_v01_ei,
		     &r, sizeof(r), WLFW_HOST_CAP_REQ_MSG_V01_MAX_MSG_LEN);
		r.gpios_len = 33;
		emit_err("host_cap_req_gpios_33", QMI_REQUEST, QMI_WLFW_HOST_CAP_REQ_V01, wlfw_host_cap_req_msg_v01_ei, &r,
			 WLFW_HOST_CAP_REQ_MSG_V01_MAX_MSG_LEN);
	}
	{
		struct wlfw_host_cap_resp_msg_v01 r = {};

		emit("host_cap_resp_ok", QMI_RESPONSE, QMI_WLFW_HOST_CAP_RESP_V01, 3, wlfw_host_cap_resp_msg_v01_ei, &r, sizeof(r),
		     WLFW_HOST_CAP_RESP_MSG_V01_MAX_MSG_LEN);
		r.resp.result = QMI_RESULT_FAILURE_V01;
		r.resp.error = QMI_ERR_NOT_SUPPORTED_V01;
		emit("host_cap_resp_not_supported", QMI_RESPONSE, QMI_WLFW_HOST_CAP_RESP_V01, 4, wlfw_host_cap_resp_msg_v01_ei, &r,
		     sizeof(r), WLFW_HOST_CAP_RESP_MSG_V01_MAX_MSG_LEN);
	}

	/* MSA */
	{
		struct wlfw_msa_info_req_msg_v01 r = {};

		r.msa_addr = 0x93900000ULL;
		r.size = 0x200000;
		emit("msa_info_req", QMI_REQUEST, QMI_WLFW_MSA_INFO_REQ_V01, 8, wlfw_msa_info_req_msg_v01_ei, &r, sizeof(r),
		     WLFW_MSA_INFO_REQ_MSG_V01_MAX_MSG_LEN);
		r.msa_addr = 0xfedcba9876543210ULL;
		r.size = 0xdeadbeef;
		emit("msa_info_req_wide", QMI_REQUEST, QMI_WLFW_MSA_INFO_REQ_V01, 9, wlfw_msa_info_req_msg_v01_ei, &r, sizeof(r),
		     WLFW_MSA_INFO_REQ_MSG_V01_MAX_MSG_LEN);
	}
	{
		struct wlfw_msa_info_resp_msg_v01 r = {};

		r.mem_region_info_len = 2;
		r.mem_region_info[0].region_addr = 0x93900000ULL;
		r.mem_region_info[0].size = 0x100000;
		r.mem_region_info[0].secure_flag = 1;
		r.mem_region_info[1].region_addr = 0x93a00000ULL;
		r.mem_region_info[1].size = 0x80000;
		r.mem_region_info[1].secure_flag = 0;
		emit("msa_info_resp_2", QMI_RESPONSE, QMI_WLFW_MSA_INFO_RESP_V01, 8, wlfw_msa_info_resp_msg_v01_ei, &r, sizeof(r),
		     WLFW_MSA_INFO_RESP_MSG_V01_MAX_MSG_LEN);
		r.mem_region_info_len = 1;
		memset(&r.mem_region_info[1], 0, sizeof(r.mem_region_info[1]));
		emit("msa_info_resp_1", QMI_RESPONSE, QMI_WLFW_MSA_INFO_RESP_V01, 9, wlfw_msa_info_resp_msg_v01_ei, &r, sizeof(r),
		     WLFW_MSA_INFO_RESP_MSG_V01_MAX_MSG_LEN);
		memset(&r, 0, sizeof(r));
		emit("msa_info_resp_0", QMI_RESPONSE, QMI_WLFW_MSA_INFO_RESP_V01, 10, wlfw_msa_info_resp_msg_v01_ei, &r, sizeof(r),
		     WLFW_MSA_INFO_RESP_MSG_V01_MAX_MSG_LEN);
		r.mem_region_info_len = 3;
		emit_err("msa_info_resp_3", QMI_RESPONSE, QMI_WLFW_MSA_INFO_RESP_V01, wlfw_msa_info_resp_msg_v01_ei, &r,
			 WLFW_MSA_INFO_RESP_MSG_V01_MAX_MSG_LEN);
	}
	{
		struct wlfw_msa_ready_req_msg_v01 q = {};
		struct wlfw_msa_ready_resp_msg_v01 r = {};

		emit("msa_ready_req", QMI_REQUEST, QMI_WLFW_MSA_READY_REQ_V01, 11, wlfw_msa_ready_req_msg_v01_ei, &q, sizeof(q),
		     WLFW_MSA_READY_REQ_MSG_V01_MAX_MSG_LEN);
		emit("msa_ready_resp_ok", QMI_RESPONSE, QMI_WLFW_MSA_READY_RESP_V01, 11, wlfw_msa_ready_resp_msg_v01_ei, &r, sizeof(r),
		     WLFW_MSA_READY_RESP_MSG_V01_MAX_MSG_LEN);
		r.resp.result = QMI_RESULT_FAILURE_V01;
		r.resp.error = QMI_ERR_INTERNAL_V01;
		emit("msa_ready_resp_failure", QMI_RESPONSE, QMI_WLFW_MSA_READY_RESP_V01, 12, wlfw_msa_ready_resp_msg_v01_ei, &r,
		     sizeof(r), WLFW_MSA_READY_RESP_MSG_V01_MAX_MSG_LEN);
	}

	/* capabilities */
	{
		struct wlfw_cap_req_msg_v01 q = {};
		struct wlfw_cap_resp_msg_v01 *r = calloc(1, sizeof(*r));

		emit("cap_req", QMI_REQUEST, QMI_WLFW_CAP_REQ_V01, 13, wlfw_cap_req_msg_v01_ei, &q, sizeof(q),
		     WLFW_CAP_REQ_MSG_V01_MAX_MSG_LEN);

		r->chip_info_valid = 1;
		r->chip_info.chip_id = 0x30224;
		r->chip_info.chip_family = 0x4001;
		r->board_info_valid = 1;
		r->board_info.board_id = 0x55;
		r->soc_info_valid = 1;
		r->soc_info.soc_id = 0x40060000;
		r->fw_version_info_valid = 1;
		r->fw_version_info.fw_version = 0x32c8c9bc;
		strcpy(r->fw_version_info.fw_build_timestamp, "2019-03-26 13:54:20");
		r->fw_build_id_valid = 1;
		strcpy(r->fw_build_id, "QC_IMAGE_VERSION_STRING=WLAN.HL.3.1-00959-QCAHLSWMTPLZ-1");
		r->num_macs_valid = 1;
		r->num_macs = 2;
		emit("cap_resp_full", QMI_RESPONSE, QMI_WLFW_CAP_RESP_V01, 13, wlfw_cap_resp_msg_v01_ei, r, sizeof(*r),
		     WLFW_CAP_RESP_MSG_V01_MAX_MSG_LEN);

		/* longest strings the decoder accepts: 32 and 128 characters */
		memset(r->fw_version_info.fw_build_timestamp, 't', 32);
		r->fw_version_info.fw_build_timestamp[32] = 0;
		memset(r->fw_build_id, 'i', 128);
		r->fw_build_id[128] = 0;
		emit("cap_resp_long_strings", QMI_RESPONSE, QMI_WLFW_CAP_RESP_V01, 14, wlfw_cap_resp_msg_v01_ei, r, sizeof(*r),
		     WLFW_CAP_RESP_MSG_V01_MAX_MSG_LEN);

		memset(r, 0, sizeof(*r));
		emit("cap_resp_minimal", QMI_RESPONSE, QMI_WLFW_CAP_RESP_V01, 15, wlfw_cap_resp_msg_v01_ei, r, sizeof(*r),
		     WLFW_CAP_RESP_MSG_V01_MAX_MSG_LEN);

		r->chip_info_valid = 1;
		r->chip_info.chip_id = 0x1;
		r->resp.result = QMI_RESULT_FAILURE_V01;
		r->resp.error = QMI_ERR_MALFORMED_MSG_V01;
		emit("cap_resp_failure", QMI_RESPONSE, QMI_WLFW_CAP_RESP_V01, 16, wlfw_cap_resp_msg_v01_ei, r, sizeof(*r),
		     WLFW_CAP_RESP_MSG_V01_MAX_MSG_LEN);
		free(r);
	}

	/* board data */
	{
		struct wlfw_bdf_download_req_msg_v01 *r = calloc(1, sizeof(*r));
		struct wlfw_bdf_download_resp_msg_v01 resp = {};

		r->valid = 1;
		r->file_id_valid = 1;
		r->file_id = 0;
		r->total_size_valid = 1;
		r->total_size = 12011;
		r->seg_id_valid = 1;
		r->seg_id = 0;
		r->data_valid = 1;
		r->data_len = 6000;
		fill_pattern(r->data, 6000);
		r->end_valid = 1;
		r->end = 0;
		emit("bdf_req_seg0_6000", QMI_REQUEST, QMI_WLFW_BDF_DOWNLOAD_REQ_V01, 17, wlfw_bdf_download_req_msg_v01_ei, r, sizeof(*r),
		     WLFW_BDF_DOWNLOAD_REQ_MSG_V01_MAX_MSG_LEN);

		r->seg_id = 2;
		r->data_len = 11;
		memset(r->data, 0, sizeof(r->data));
		fill_pattern(r->data, 11);
		r->end = 1;
		emit("bdf_req_seg2_last_11", QMI_REQUEST, QMI_WLFW_BDF_DOWNLOAD_REQ_V01, 18, wlfw_bdf_download_req_msg_v01_ei, r, sizeof(*r),
		     WLFW_BDF_DOWNLOAD_REQ_MSG_V01_MAX_MSG_LEN);

		memset(r, 0, sizeof(*r));
		r->valid = 1;
		r->file_id_valid = 1;
		r->file_id = QMI_WLFW_CAL_TEMP_IDX_3_V01;
		r->total_size_valid = 1;
		r->total_size = 0xcafe;
		r->seg_id_valid = 1;
		r->seg_id = 0x01020304;
		r->data_valid = 1;
		r->data_len = 6144;
		fill_pattern(r->data, 6144);
		r->end_valid = 1;
		r->end = 1;
		r->bdf_type_valid = 1;
		r->bdf_type = 0x7e;
		emit("bdf_req_max_6144_bdf_type", QMI_REQUEST, QMI_WLFW_BDF_DOWNLOAD_REQ_V01, 19, wlfw_bdf_download_req_msg_v01_ei, r,
		     sizeof(*r), WLFW_BDF_DOWNLOAD_REQ_MSG_V01_MAX_MSG_LEN);

		memset(r, 0, sizeof(*r));
		r->valid = 1;
		r->data_valid = 1;
		r->data_len = 0;
		emit("bdf_req_data_empty", QMI_REQUEST, QMI_WLFW_BDF_DOWNLOAD_REQ_V01, 20, wlfw_bdf_download_req_msg_v01_ei, r, sizeof(*r),
		     WLFW_BDF_DOWNLOAD_REQ_MSG_V01_MAX_MSG_LEN);

		r->data_len = 6145;
		emit_err("bdf_req_data_6145", QMI_REQUEST, QMI_WLFW_BDF_DOWNLOAD_REQ_V01, wlfw_bdf_download_req_msg_v01_ei, r,
			 WLFW_BDF_DOWNLOAD_REQ_MSG_V01_MAX_MSG_LEN);
		free(r);

		emit("bdf_resp_ok", QMI_RESPONSE, QMI_WLFW_BDF_DOWNLOAD_RESP_V01, 17, wlfw_bdf_download_resp_msg_v01_ei, &resp, sizeof(resp),
		     WLFW_BDF_DOWNLOAD_RESP_MSG_V01_MAX_MSG_LEN);
		resp.resp.result = QMI_RESULT_FAILURE_V01;
		resp.resp.error = QMI_ERR_MALFORMED_MSG_V01;
		emit("bdf_resp_malformed", QMI_RESPONSE, QMI_WLFW_BDF_DOWNLOAD_RESP_V01, 18, wlfw_bdf_download_resp_msg_v01_ei, &resp,
		     sizeof(resp), WLFW_BDF_DOWNLOAD_RESP_MSG_V01_MAX_MSG_LEN);
	}

	/* calibration report (ath10k sends it after the board data) */
	{
		struct wlfw_cal_report_req_msg_v01 r = {};
		struct wlfw_cal_report_resp_msg_v01 resp = {};

		emit("cal_report_req_empty", QMI_REQUEST, QMI_WLFW_CAL_REPORT_REQ_V01, 21, wlfw_cal_report_req_msg_v01_ei, &r, sizeof(r),
		     WLFW_CAL_REPORT_REQ_MSG_V01_MAX_MSG_LEN);
		r.xo_cal_data_valid = 1;
		r.xo_cal_data = 0xa5;
		emit("cal_report_req_xo", QMI_REQUEST, QMI_WLFW_CAL_REPORT_REQ_V01, 22, wlfw_cal_report_req_msg_v01_ei, &r, sizeof(r),
		     WLFW_CAL_REPORT_REQ_MSG_V01_MAX_MSG_LEN);
		r.meta_data_len = 3;
		r.meta_data[0] = QMI_WLFW_CAL_TEMP_IDX_0_V01;
		r.meta_data[1] = QMI_WLFW_CAL_TEMP_IDX_2_V01;
		r.meta_data[2] = QMI_WLFW_CAL_TEMP_IDX_4_V01;
		emit("cal_report_req_3", QMI_REQUEST, QMI_WLFW_CAL_REPORT_REQ_V01, 23, wlfw_cal_report_req_msg_v01_ei, &r, sizeof(r),
		     WLFW_CAL_REPORT_REQ_MSG_V01_MAX_MSG_LEN);
		r.meta_data_len = 5;
		for (int i = 0; i < 5; i++)
			r.meta_data[i] = i;
		emit("cal_report_req_5", QMI_REQUEST, QMI_WLFW_CAL_REPORT_REQ_V01, 24, wlfw_cal_report_req_msg_v01_ei, &r, sizeof(r),
		     WLFW_CAL_REPORT_REQ_MSG_V01_MAX_MSG_LEN);
		r.meta_data_len = 6;
		emit_err("cal_report_req_6", QMI_REQUEST, QMI_WLFW_CAL_REPORT_REQ_V01, wlfw_cal_report_req_msg_v01_ei, &r,
			 WLFW_CAL_REPORT_REQ_MSG_V01_MAX_MSG_LEN);
		emit("cal_report_resp_ok", QMI_RESPONSE, QMI_WLFW_CAL_REPORT_RESP_V01, 21, wlfw_cal_report_resp_msg_v01_ei, &resp,
		     sizeof(resp), WLFW_CAL_REPORT_RESP_MSG_V01_MAX_MSG_LEN);
		resp.resp.result = QMI_RESULT_FAILURE_V01;
		resp.resp.error = QMI_ERR_INTERNAL_V01;
		emit("cal_report_resp_failure", QMI_RESPONSE, QMI_WLFW_CAL_REPORT_RESP_V01, 22, wlfw_cal_report_resp_msg_v01_ei, &resp,
		     sizeof(resp), WLFW_CAL_REPORT_RESP_MSG_V01_MAX_MSG_LEN);
	}

	/* wlan mode */
	{
		struct wlfw_wlan_mode_req_msg_v01 r = {};
		struct wlfw_wlan_mode_resp_msg_v01 resp = {};

		r.mode = QMI_WLFW_MISSION_V01;
		r.hw_debug_valid = 1;
		r.hw_debug = 0;
		emit("wlan_mode_req_mission", QMI_REQUEST, QMI_WLFW_WLAN_MODE_REQ_V01, 25, wlfw_wlan_mode_req_msg_v01_ei, &r, sizeof(r),
		     WLFW_WLAN_MODE_REQ_MSG_V01_MAX_MSG_LEN);
		r.mode = QMI_WLFW_OFF_V01;
		emit("wlan_mode_req_off", QMI_REQUEST, QMI_WLFW_WLAN_MODE_REQ_V01, 26, wlfw_wlan_mode_req_msg_v01_ei, &r, sizeof(r),
		     WLFW_WLAN_MODE_REQ_MSG_V01_MAX_MSG_LEN);
		r.mode = QMI_WLFW_CALIBRATION_V01;
		r.hw_debug = 9;
		emit("wlan_mode_req_cal_hwdebug", QMI_REQUEST, QMI_WLFW_WLAN_MODE_REQ_V01, 27, wlfw_wlan_mode_req_msg_v01_ei, &r,
		     sizeof(r), WLFW_WLAN_MODE_REQ_MSG_V01_MAX_MSG_LEN);
		r.hw_debug_valid = 0;
		r.hw_debug = 0;
		emit("wlan_mode_req_no_hwdebug", QMI_REQUEST, QMI_WLFW_WLAN_MODE_REQ_V01, 28, wlfw_wlan_mode_req_msg_v01_ei, &r,
		     sizeof(r), WLFW_WLAN_MODE_REQ_MSG_V01_MAX_MSG_LEN);
		emit("wlan_mode_resp_ok", QMI_RESPONSE, QMI_WLFW_WLAN_MODE_RESP_V01, 25, wlfw_wlan_mode_resp_msg_v01_ei, &resp,
		     sizeof(resp), WLFW_WLAN_MODE_RESP_MSG_V01_MAX_MSG_LEN);
		resp.resp.result = QMI_RESULT_FAILURE_V01;
		resp.resp.error = QMI_ERR_DISABLED_V01;
		emit("wlan_mode_resp_failure", QMI_RESPONSE, QMI_WLFW_WLAN_MODE_RESP_V01, 26, wlfw_wlan_mode_resp_msg_v01_ei, &resp,
		     sizeof(resp), WLFW_WLAN_MODE_RESP_MSG_V01_MAX_MSG_LEN);
	}

	/* wlan config */
	{
		struct wlfw_wlan_cfg_req_msg_v01 *r = calloc(1, sizeof(*r));
		struct wlfw_wlan_cfg_resp_msg_v01 resp = {};

		/* a WCN3990-style configuration: ten copy engines, six service
		 * pipes, three shadow registers */
		static const u32 tgt[10][5] = {
			{ 0, QMI_WLFW_PIPEDIR_OUT_V01, 32, 2048, 0 },
			{ 1, QMI_WLFW_PIPEDIR_IN_V01, 32, 2048, 0 },
			{ 2, QMI_WLFW_PIPEDIR_IN_V01, 64, 2048, 0 },
			{ 3, QMI_WLFW_PIPEDIR_OUT_V01, 32, 2048, 0 },
			{ 4, QMI_WLFW_PIPEDIR_OUT_V01, 4096, 256, QMI_WLFW_CE_ATTR_DISABLE_INTR_V01 },
			{ 5, QMI_WLFW_PIPEDIR_OUT_V01, 32, 2048, 0 },
			{ 6, QMI_WLFW_PIPEDIR_INOUT_V01, 32, 4096, 0 },
			{ 7, QMI_WLFW_PIPEDIR_INOUT_V01, 32, 2048, QMI_WLFW_CE_ATTR_BYTE_SWAP_DATA_V01 },
			{ 8, QMI_WLFW_PIPEDIR_NONE_V01, 0, 0, 0 },
			{ 9, QMI_WLFW_PIPEDIR_OUT_V01, 32, 2048, 0x1f },
		};
		static const u32 svc[6][3] = {
			{ 0x0000, QMI_WLFW_PIPEDIR_OUT_V01, 3 },
			{ 0x0000, QMI_WLFW_PIPEDIR_IN_V01, 2 },
			{ 0x0001, QMI_WLFW_PIPEDIR_OUT_V01, 3 },
			{ 0x0001, QMI_WLFW_PIPEDIR_IN_V01, 2 },
			{ 0x0100, QMI_WLFW_PIPEDIR_OUT_V01, 4 },
			{ 0xdeadbeef, QMI_WLFW_PIPEDIR_INOUT_V01, 7 },
		};

		r->host_version_valid = 0;
		r->tgt_cfg_valid = 1;
		r->tgt_cfg_len = 10;
		for (int i = 0; i < 10; i++) {
			r->tgt_cfg[i].pipe_num = tgt[i][0];
			r->tgt_cfg[i].pipe_dir = tgt[i][1];
			r->tgt_cfg[i].nentries = tgt[i][2];
			r->tgt_cfg[i].nbytes_max = tgt[i][3];
			r->tgt_cfg[i].flags = tgt[i][4];
		}
		r->svc_cfg_valid = 1;
		r->svc_cfg_len = 6;
		for (int i = 0; i < 6; i++) {
			r->svc_cfg[i].service_id = svc[i][0];
			r->svc_cfg[i].pipe_dir = svc[i][1];
			r->svc_cfg[i].pipe_num = svc[i][2];
		}
		r->shadow_reg_valid = 1;
		r->shadow_reg_len = 3;
		r->shadow_reg[0].id = 0x0102; r->shadow_reg[0].offset = 0x0304;
		r->shadow_reg[1].id = 0xfffe; r->shadow_reg[1].offset = 0x0001;
		r->shadow_reg[2].id = 7; r->shadow_reg[2].offset = 0x8000;
		emit("wlan_cfg_req_ath10k", QMI_REQUEST, QMI_WLFW_WLAN_CFG_REQ_V01, 29, wlfw_wlan_cfg_req_msg_v01_ei, r, sizeof(*r),
		     WLFW_WLAN_CFG_REQ_MSG_V01_MAX_MSG_LEN);

		r->host_version_valid = 1;
		strcpy(r->host_version, "lucinaos-1.2.3");
		r->shadow_reg_v2_valid = 1;
		r->shadow_reg_v2_len = 2;
		r->shadow_reg_v2[0].addr = 0x0a0b0c0d;
		r->shadow_reg_v2[1].addr = 0xf0e0d0c0;
		emit("wlan_cfg_req_version_v2", QMI_REQUEST, QMI_WLFW_WLAN_CFG_REQ_V01, 30, wlfw_wlan_cfg_req_msg_v01_ei, r, sizeof(*r),
		     WLFW_WLAN_CFG_REQ_MSG_V01_MAX_MSG_LEN);

		/* every array at its maximum, longest host version */
		memset(r, 0, sizeof(*r));
		r->host_version_valid = 1;
		memset(r->host_version, 'v', 16);
		r->tgt_cfg_valid = 1;
		r->tgt_cfg_len = QMI_WLFW_MAX_NUM_CE_V01;
		for (int i = 0; i < QMI_WLFW_MAX_NUM_CE_V01; i++) {
			r->tgt_cfg[i].pipe_num = i;
			r->tgt_cfg[i].pipe_dir = i % 4;
			r->tgt_cfg[i].nentries = 100 + i;
			r->tgt_cfg[i].nbytes_max = 200 + i;
			r->tgt_cfg[i].flags = 1u << (i % 5);
		}
		r->svc_cfg_valid = 1;
		r->svc_cfg_len = QMI_WLFW_MAX_NUM_SVC_V01;
		for (int i = 0; i < QMI_WLFW_MAX_NUM_SVC_V01; i++) {
			r->svc_cfg[i].service_id = 0x1000 + i;
			r->svc_cfg[i].pipe_dir = i % 4;
			r->svc_cfg[i].pipe_num = i;
		}
		r->shadow_reg_valid = 1;
		r->shadow_reg_len = QMI_WLFW_MAX_NUM_SHADOW_REG_V01;
		for (int i = 0; i < QMI_WLFW_MAX_NUM_SHADOW_REG_V01; i++) {
			r->shadow_reg[i].id = i;
			r->shadow_reg[i].offset = 0x4000 + i;
		}
		r->shadow_reg_v2_valid = 1;
		r->shadow_reg_v2_len = QMI_WLFW_MAX_SHADOW_REG_V2;
		for (int i = 0; i < QMI_WLFW_MAX_SHADOW_REG_V2; i++)
			r->shadow_reg_v2[i].addr = 0x90000 + 4 * i;
		emit("wlan_cfg_req_max", QMI_REQUEST, QMI_WLFW_WLAN_CFG_REQ_V01, 31, wlfw_wlan_cfg_req_msg_v01_ei, r, sizeof(*r),
		     WLFW_WLAN_CFG_REQ_MSG_V01_MAX_MSG_LEN);

		memset(r, 0, sizeof(*r));
		r->tgt_cfg_valid = 1;
		r->tgt_cfg_len = 0;
		r->svc_cfg_valid = 1;
		r->svc_cfg_len = 0;
		emit("wlan_cfg_req_empty_arrays", QMI_REQUEST, QMI_WLFW_WLAN_CFG_REQ_V01, 32, wlfw_wlan_cfg_req_msg_v01_ei, r, sizeof(*r),
		     WLFW_WLAN_CFG_REQ_MSG_V01_MAX_MSG_LEN);

		r->tgt_cfg_len = QMI_WLFW_MAX_NUM_CE_V01 + 1;
		emit_err("wlan_cfg_req_tgt_13", QMI_REQUEST, QMI_WLFW_WLAN_CFG_REQ_V01, wlfw_wlan_cfg_req_msg_v01_ei, r,
			 WLFW_WLAN_CFG_REQ_MSG_V01_MAX_MSG_LEN);
		r->tgt_cfg_len = 12;
		/* the declared maximum length, one byte short of what the message needs */
		emit_err("wlan_cfg_req_tgt_12_len_240", QMI_REQUEST, QMI_WLFW_WLAN_CFG_REQ_V01, wlfw_wlan_cfg_req_msg_v01_ei, r, 240);
		free(r);

		emit("wlan_cfg_resp_ok", QMI_RESPONSE, QMI_WLFW_WLAN_CFG_RESP_V01, 29, wlfw_wlan_cfg_resp_msg_v01_ei, &resp, sizeof(resp),
		     WLFW_WLAN_CFG_RESP_MSG_V01_MAX_MSG_LEN);
		resp.resp.result = QMI_RESULT_FAILURE_V01;
		resp.resp.error = QMI_ERR_INVALID_ID_V01;
		emit("wlan_cfg_resp_failure", QMI_RESPONSE, QMI_WLFW_WLAN_CFG_RESP_V01, 30, wlfw_wlan_cfg_resp_msg_v01_ei, &resp,
		     sizeof(resp), WLFW_WLAN_CFG_RESP_MSG_V01_MAX_MSG_LEN);
	}

	/* firmware log mode (ath10k_qmi_set_fw_log_mode) */
	{
		struct wlfw_ini_req_msg_v01 r = {};
		struct wlfw_ini_resp_msg_v01 resp = {};

		r.enablefwlog_valid = 1;
		r.enablefwlog = 1;
		emit("ini_req", QMI_REQUEST, QMI_WLFW_INI_REQ_V01, 33, wlfw_ini_req_msg_v01_ei, &r, sizeof(r),
		     WLFW_INI_REQ_MSG_V01_MAX_MSG_LEN);
		memset(&r, 0, sizeof(r));
		emit("ini_req_empty", QMI_REQUEST, QMI_WLFW_INI_REQ_V01, 34, wlfw_ini_req_msg_v01_ei, &r, sizeof(r),
		     WLFW_INI_REQ_MSG_V01_MAX_MSG_LEN);
		emit("ini_resp_ok", QMI_RESPONSE, QMI_WLFW_INI_RESP_V01, 33, wlfw_ini_resp_msg_v01_ei, &resp, sizeof(resp),
		     WLFW_INI_RESP_MSG_V01_MAX_MSG_LEN);
	}

	{
		FILE *cst;

		snprintf(path, sizeof(path), "%s/constants.txt", dir);
		cst = fopen(path, "w");
		if (!cst) {
			perror("open");
			return 1;
		}
		fprintf(cst, "# generated by drivers/lucina-qmi/oracle (ath10k qmi_wlfw_v01.h); do not edit\n");
		constants(cst);
		fclose(cst);
	}

	decode_cases();

	fclose(enc);
	fclose(errs);
	fclose(dec);
	return 0;
}
