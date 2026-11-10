/* Derived from Linux drivers/net/wireless/ath/ath10k (SPDX ISC; Atheros Communications, Qualcomm Atheros, The Linux Foundation, Qualcomm
 * Innovation Center, Qualcomm Technologies): the extracted code is compiled unchanged, see oracle/README.md. */
/* WMI-TLV: struct layouts from the real wmi.h/wmi-tlv.h, and the real encoders and event parsers of wmi-tlv.c/wmi.c
 * (cut out by gen.sh, compiled unchanged against a stand-in struct ath10k and sk_buff) run on test inputs. */
#include <linux/types.h>
#include <stdio.h>
#include <stdlib.h>
#include <errno.h>
#include "hwconst.h"
#include "wmi.h"
#include "wmi-tlv.h"
#include "hw.h"

#define ATH10K_DBG_WMI 0
#define ath10k_dbg(...) ((void)0)
#define ath10k_warn(...) ((void)0)
#define ERR_PTR(e) ((void *)(long)(e))
#define PTR_ERR(p) ((long)(p))
#define IS_ERR(p) ((unsigned long)(p) >= (unsigned long)-4095L)
#define kzalloc_objs(P, N, gfp) calloc((N), sizeof(P))
#define kfree(p) free(p)
#define GFP_ATOMIC 0
typedef unsigned gfp_t;
#define WARN_ON(x) (x)
#define ether_addr_copy(d, s) memcpy((d), (s), 6)
#define upper_32_bits(n) ((u32)(((n) >> 16) >> 16))
#define IEEE80211_CCMP_MIC_LEN 8

struct sk_buff { uint8_t *data; unsigned len; };
#define skb_pull(skb, n) ((skb)->len -= (n), (skb)->data += (n))
#define skb_put(skb, n) ((skb)->len += (n))
#define skb_trim(skb, n) ((skb)->len = (n))
struct ieee80211_channel { u32 flags; };
#define IEEE80211_CHAN_RADAR 1
struct ath10k_hw_stub { void *wiphy; };
#define ieee80211_get_channel(w, f) ((struct ieee80211_channel *)0)
struct mem_chunk { u32 req_id; u32 len; u64 paddr; };
struct ath10k {
	struct ath10k_hw_stub *hw;
	struct {
		int num_mem_chunks;
		struct mem_chunk mem_chunks[16];
		unsigned long svc_map[4];
		u32 rx_decap_mode;
	} wmi;
	struct { u32 max_num_pending_tx; } htt;
	struct { int max_num_patterns; } wow;
	struct { int num_peers; int ast_skid_limit; int num_wds_entries; } hw_params;
	const u8 *wmi_key_cipher;
};

struct sk_buff *ath10k_wmi_alloc_skb(struct ath10k *ar, u32 len)
{
	struct sk_buff *skb = malloc(sizeof(*skb));
	u32 round_len = roundup(len, 4);
	skb->data = calloc(1, round_len ? round_len : 4);
	skb->len = round_len;
	return skb;
}

#include "wmi_tlv_cipher.h"
#include "wmi_funcs.inc"

static struct ath10k ar;

#define SZ(t) printf("sizeof.%s %zu\n", #t, sizeof(t))
#define OFF(t, f) printf("offsetof.%s.%s %zu\n", #t, #f, offsetof(t, f))
#define C(x) printf("const.%s %lld\n", #x, (long long)(x))

static void hexline(const char *tag, const void *p, size_t n)
{
	const uint8_t *b = p;
	size_t i;
	printf("%s %zu ", tag, n);
	for (i = 0; i < n; i++) printf("%02x", b[i]);
	printf("\n");
}

static void show_skb(const char *tag, struct sk_buff *skb)
{
	if (IS_ERR(skb)) printf("%s err %ld\n", tag, PTR_ERR(skb));
	else hexline(tag, skb->data, skb->len);
}

/* ---- tlv builders for the event inputs ---- */
static uint8_t evbuf[4096];
static size_t evlen;
static void put_tlv(u16 tag, const void *body, u16 len)
{
	struct wmi_tlv h = {.len = len, .tag = tag};
	memcpy(evbuf + evlen, &h, 4);
	if (body) memcpy(evbuf + evlen + 4, body, len);
	evlen += 4 + len;
}

static int rec_n;
static int rec_cb(struct ath10k *a, u16 tag, u16 len, const void *ptr, void *data)
{
	printf(" tlv(tag=%u,len=%u,off=%ld)", tag, len, (long)((const uint8_t *)ptr - evbuf));
	rec_n++;
	return 0;
}

static void iter_case(int n, const uint8_t *b, size_t len)
{
	int r;
	memcpy(evbuf, b, len);
	rec_n = 0;
	printf("iter.%d", n);
	r = ath10k_wmi_tlv_iter(&ar, evbuf, len, rec_cb, NULL);
	printf(" ret=%d\n", r);
}

static void print_svc_rdy(int r, struct wmi_svc_rdy_ev_arg *a)
{
	int i;
	printf("svc_rdy.ret %d\n", r);
	if (r) return;
	printf("svc_rdy.fields min_tx_power=%u max_tx_power=%u ht_cap=0x%x vht_cap=0x%x vht_supp_mcs=0x%x sw_ver0=0x%x "
	       "sw_ver1=%u fw_build=0x%x phy_capab=0x%x num_rf_chains=%u eeprom_rd=0x%x num_mem_reqs=%u low2=%u high2=%u "
	       "low5=%u high5=%u sys_cap_info=0x%x service_map_len=%zu service_map_off=%ld\n",
	       a->min_tx_power, a->max_tx_power, a->ht_cap, a->vht_cap, a->vht_supp_mcs, a->sw_ver0, a->sw_ver1,
	       a->fw_build, a->phy_capab, a->num_rf_chains, a->eeprom_rd, a->num_mem_reqs, a->low_2ghz_chan,
	       a->high_2ghz_chan, a->low_5ghz_chan, a->high_5ghz_chan, a->sys_cap_info, a->service_map_len,
	       (long)((const uint8_t *)a->service_map - evbuf));
	for (i = 0; i < WMI_MAX_MEM_REQS; i++) {
		if (!a->mem_reqs[i]) break;
		printf("svc_rdy.mem_req.%d req_id=0x%x unit_size=%u num_unit_info=0x%x num_units=%u off=%ld\n", i,
		       a->mem_reqs[i]->req_id, a->mem_reqs[i]->unit_size, a->mem_reqs[i]->num_unit_info,
		       a->mem_reqs[i]->num_units, (long)((const uint8_t *)a->mem_reqs[i] - evbuf));
	}
}

static void svc_rdy_case(const char *name, int abi_bad, int skip_reg, int bad_memreq_tag, int n_memreqs, int short_ev)
{
	struct wmi_tlv_svc_rdy_ev ev;
	struct hal_reg_capabilities reg;
	struct wlan_host_mem_req mr;
	__le32 bmap[3] = {0x00ff00ff, 0x1, 0x80000000};
	struct wmi_svc_rdy_ev_arg arg;
	uint8_t inner[1024];
	size_t inner_len = 0;
	int i, r;
	memset(&ev, 0, sizeof(ev));
	ev.fw_build_vers = 0x12345678;
	ev.abi.abi_ver0 = WMI_TLV_ABI_VER0;
	ev.abi.abi_ver1 = WMI_TLV_ABI_VER1;
	ev.abi.abi_ver_ns0 = WMI_TLV_ABI_VER_NS0;
	ev.abi.abi_ver_ns1 = WMI_TLV_ABI_VER_NS1;
	ev.abi.abi_ver_ns2 = WMI_TLV_ABI_VER_NS2;
	ev.abi.abi_ver_ns3 = WMI_TLV_ABI_VER_NS3;
	if (abi_bad) ev.abi.abi_ver_ns1 ^= 1;
	ev.phy_capability = 0x3; ev.max_frag_entry = 2; ev.num_rf_chains = 2; ev.ht_cap_info = 0x1ef;
	ev.vht_cap_info = 0x339071b2; ev.vht_supp_mcs = 0xfffa; ev.hw_min_tx_power = 2; ev.hw_max_tx_power = 30;
	ev.sys_cap_info = 0x7; ev.num_mem_reqs = n_memreqs; ev.max_num_scan_chans = 38; ev.hw_bd_id = 1;
	ev.hw_bd_info[0].rev = 1; ev.hw_bd_info[1].project_id = 9;
	memset(&reg, 0, sizeof(reg));
	reg.eeprom_rd = 0x6c; reg.eeprom_rd_ext = 1; reg.regcap1 = 0xf; reg.regcap2 = 0xe0; reg.wireless_modes = 0x1fff;
	reg.low_2ghz_chan = 2312; reg.high_2ghz_chan = 2732; reg.low_5ghz_chan = 4915; reg.high_5ghz_chan = 6100;
	evlen = 0;
	put_tlv(WMI_TLV_TAG_STRUCT_SERVICE_READY_EVENT, &ev, short_ev ? sizeof(ev) - 4 : sizeof(ev));
	if (!skip_reg) put_tlv(WMI_TLV_TAG_STRUCT_HAL_REG_CAPABILITIES, &reg, sizeof(reg));
	put_tlv(WMI_TLV_TAG_ARRAY_UINT32, bmap, sizeof(bmap));
	for (i = 0; i < n_memreqs; i++) {
		struct wmi_tlv h = {.len = sizeof(mr), .tag = bad_memreq_tag && i == n_memreqs - 1 ? WMI_TLV_TAG_ARRAY_BYTE : WMI_TLV_TAG_STRUCT_WLAN_HOST_MEM_REQ};
		memset(&mr, 0, sizeof(mr));
		mr.req_id = 0x10 + i; mr.unit_size = 1000 * (i + 1); mr.num_unit_info = 0x1 << i; mr.num_units = 7 * i;
		memcpy(inner + inner_len, &h, 4);
		memcpy(inner + inner_len + 4, &mr, sizeof(mr));
		inner_len += 4 + sizeof(mr);
	}
	put_tlv(WMI_TLV_TAG_ARRAY_STRUCT, inner, inner_len);
	{ __le32 dbs[2] = {1, 2}; put_tlv(WMI_TLV_TAG_ARRAY_UINT32, dbs, sizeof(dbs)); }
	hexline(name, evbuf, evlen);
	{
		struct sk_buff skb = {.data = evbuf, .len = evlen};
		memset(&arg, 0, sizeof(arg));
		r = ath10k_wmi_tlv_op_pull_svc_rdy_ev(&ar, &skb, &arg);
	}
	printf("case %s\n", name);
	print_svc_rdy(r, &arg);
}

int main(void)
{
	int i;
	SZ(struct wmi_tlv); OFF(struct wmi_tlv, len); OFF(struct wmi_tlv, tag); OFF(struct wmi_tlv, value);
	SZ(struct wmi_cmd_hdr); C(WMI_CMD_HDR_CMD_ID_MASK); C(WMI_CMD_HDR_PLT_PRIV_MASK); C(WMI_CMD_HDR_PLT_PRIV_LSB);
	SZ(struct wmi_mac_addr); SZ(struct wmi_ssid); OFF(struct wmi_ssid, ssid);
	SZ(struct wmi_tlv_abi_version); SZ(struct wmi_tlv_hw_bd_info);
	SZ(struct wmi_tlv_svc_rdy_ev);
#define F(t, f) OFF(struct t, f)
	F(wmi_tlv_svc_rdy_ev, fw_build_vers); F(wmi_tlv_svc_rdy_ev, abi); F(wmi_tlv_svc_rdy_ev, phy_capability);
	F(wmi_tlv_svc_rdy_ev, max_frag_entry); F(wmi_tlv_svc_rdy_ev, num_rf_chains); F(wmi_tlv_svc_rdy_ev, ht_cap_info);
	F(wmi_tlv_svc_rdy_ev, vht_cap_info); F(wmi_tlv_svc_rdy_ev, vht_supp_mcs); F(wmi_tlv_svc_rdy_ev, hw_min_tx_power);
	F(wmi_tlv_svc_rdy_ev, hw_max_tx_power); F(wmi_tlv_svc_rdy_ev, sys_cap_info); F(wmi_tlv_svc_rdy_ev, min_pkt_size_enable);
	F(wmi_tlv_svc_rdy_ev, max_bcn_ie_size); F(wmi_tlv_svc_rdy_ev, num_mem_reqs); F(wmi_tlv_svc_rdy_ev, max_num_scan_chans);
	F(wmi_tlv_svc_rdy_ev, hw_bd_id); F(wmi_tlv_svc_rdy_ev, hw_bd_info);
	SZ(struct wmi_tlv_rdy_ev); F(wmi_tlv_rdy_ev, abi); F(wmi_tlv_rdy_ev, mac_addr); F(wmi_tlv_rdy_ev, status);
	SZ(struct hal_reg_capabilities); SZ(struct wlan_host_mem_req); SZ(struct wmi_scan_event);
	SZ(struct wmi_tlv_resource_config);
	F(wmi_tlv_resource_config, num_vdevs); F(wmi_tlv_resource_config, num_peers); F(wmi_tlv_resource_config, num_offload_peers);
	F(wmi_tlv_resource_config, num_offload_reorder_bufs); F(wmi_tlv_resource_config, num_peer_keys);
	F(wmi_tlv_resource_config, num_tids); F(wmi_tlv_resource_config, ast_skid_limit); F(wmi_tlv_resource_config, tx_chain_mask);
	F(wmi_tlv_resource_config, rx_chain_mask); F(wmi_tlv_resource_config, rx_timeout_pri); F(wmi_tlv_resource_config, rx_decap_mode);
	F(wmi_tlv_resource_config, scan_max_pending_reqs); F(wmi_tlv_resource_config, bmiss_offload_max_vdev);
	F(wmi_tlv_resource_config, roam_offload_max_vdev); F(wmi_tlv_resource_config, roam_offload_max_ap_profiles);
	F(wmi_tlv_resource_config, num_mcast_groups); F(wmi_tlv_resource_config, num_mcast_table_elems);
	F(wmi_tlv_resource_config, mcast2ucast_mode); F(wmi_tlv_resource_config, tx_dbg_log_size);
	F(wmi_tlv_resource_config, num_wds_entries); F(wmi_tlv_resource_config, dma_burst_size);
	F(wmi_tlv_resource_config, mac_aggr_delim); F(wmi_tlv_resource_config, rx_skip_defrag_timeout_dup_detection_check);
	F(wmi_tlv_resource_config, vow_config); F(wmi_tlv_resource_config, gtk_offload_max_vdev);
	F(wmi_tlv_resource_config, num_msdu_desc); F(wmi_tlv_resource_config, max_frag_entries);
	F(wmi_tlv_resource_config, num_tdls_vdevs); F(wmi_tlv_resource_config, num_tdls_conn_table_entries);
	F(wmi_tlv_resource_config, beacon_tx_offload_max_vdev); F(wmi_tlv_resource_config, num_multicast_filter_entries);
	F(wmi_tlv_resource_config, num_wow_filters); F(wmi_tlv_resource_config, num_keep_alive_pattern);
	F(wmi_tlv_resource_config, keep_alive_pattern_size); F(wmi_tlv_resource_config, max_tdls_concurrent_sleep_sta);
	F(wmi_tlv_resource_config, max_tdls_concurrent_buffer_sta); F(wmi_tlv_resource_config, wmi_send_separate);
	F(wmi_tlv_resource_config, num_ocb_vdevs); F(wmi_tlv_resource_config, num_ocb_channels);
	F(wmi_tlv_resource_config, num_ocb_schedules); F(wmi_tlv_resource_config, host_capab);
	SZ(struct host_memory_chunk_tlv); F(host_memory_chunk_tlv, req_id); F(host_memory_chunk_tlv, ptr);
	F(host_memory_chunk_tlv, size); F(host_memory_chunk_tlv, ptr_high);
	SZ(struct wmi_tlv_init_cmd); F(wmi_tlv_init_cmd, num_host_mem_chunks);
	SZ(struct wmi_start_scan_common); SZ(struct wmi_tlv_start_scan_cmd);
	F(wmi_start_scan_common, scan_id); F(wmi_start_scan_common, scan_req_id); F(wmi_start_scan_common, vdev_id);
	F(wmi_start_scan_common, scan_priority); F(wmi_start_scan_common, notify_scan_events);
	F(wmi_start_scan_common, dwell_time_active); F(wmi_start_scan_common, dwell_time_passive);
	F(wmi_start_scan_common, min_rest_time); F(wmi_start_scan_common, max_rest_time);
	F(wmi_start_scan_common, repeat_probe_time); F(wmi_start_scan_common, probe_spacing_time);
	F(wmi_start_scan_common, idle_time); F(wmi_start_scan_common, max_scan_time); F(wmi_start_scan_common, probe_delay);
	F(wmi_start_scan_common, scan_ctrl_flags);
	F(wmi_tlv_start_scan_cmd, common); F(wmi_tlv_start_scan_cmd, burst_duration_ms); F(wmi_tlv_start_scan_cmd, num_channels);
	F(wmi_tlv_start_scan_cmd, num_bssids); F(wmi_tlv_start_scan_cmd, num_ssids); F(wmi_tlv_start_scan_cmd, ie_len);
	F(wmi_tlv_start_scan_cmd, num_probes); F(wmi_tlv_start_scan_cmd, mac_addr); F(wmi_tlv_start_scan_cmd, mac_mask);
	SZ(struct wmi_stop_scan_cmd); F(wmi_stop_scan_cmd, scan_req_id); F(wmi_stop_scan_cmd, scan_id);
	F(wmi_stop_scan_cmd, req_type); F(wmi_stop_scan_cmd, vdev_id);
	SZ(struct wmi_vdev_create_cmd); F(wmi_vdev_create_cmd, vdev_macaddr); SZ(struct wmi_vdev_delete_cmd);
	SZ(struct wmi_vdev_up_cmd); F(wmi_vdev_up_cmd, vdev_bssid); SZ(struct wmi_vdev_down_cmd);
	SZ(struct wmi_tlv_peer_create_cmd); F(wmi_tlv_peer_create_cmd, peer_addr); F(wmi_tlv_peer_create_cmd, peer_type);
	SZ(struct wmi_peer_delete_cmd); F(wmi_peer_delete_cmd, peer_macaddr);
	SZ(struct wmi_tlv_pdev_set_rd_cmd); F(wmi_tlv_pdev_set_rd_cmd, regd); F(wmi_tlv_pdev_set_rd_cmd, regd_2ghz);
	F(wmi_tlv_pdev_set_rd_cmd, regd_5ghz); F(wmi_tlv_pdev_set_rd_cmd, conform_limit_2ghz);
	F(wmi_tlv_pdev_set_rd_cmd, conform_limit_5ghz);
	SZ(struct wmi_tlv_vdev_start_cmd); F(wmi_tlv_vdev_start_cmd, vdev_id); F(wmi_tlv_vdev_start_cmd, requestor_id);
	F(wmi_tlv_vdev_start_cmd, bcn_intval); F(wmi_tlv_vdev_start_cmd, dtim_period); F(wmi_tlv_vdev_start_cmd, flags);
	F(wmi_tlv_vdev_start_cmd, ssid); F(wmi_tlv_vdev_start_cmd, bcn_tx_rate); F(wmi_tlv_vdev_start_cmd, bcn_tx_power);
	F(wmi_tlv_vdev_start_cmd, num_noa_descr); F(wmi_tlv_vdev_start_cmd, disable_hw_ack);
	SZ(struct wmi_channel); F(wmi_channel, mhz); F(wmi_channel, band_center_freq1); F(wmi_channel, band_center_freq2);
	F(wmi_channel, flags); F(wmi_channel, reginfo0); F(wmi_channel, reginfo1);
	SZ(struct wmi_tlv_mgmt_tx_cmd); F(wmi_tlv_mgmt_tx_cmd, vdev_id); F(wmi_tlv_mgmt_tx_cmd, desc_id);
	F(wmi_tlv_mgmt_tx_cmd, chanfreq); F(wmi_tlv_mgmt_tx_cmd, paddr); F(wmi_tlv_mgmt_tx_cmd, frame_len);
	F(wmi_tlv_mgmt_tx_cmd, buf_len);
	SZ(struct wmi_tlv_peer_assoc_cmd); SZ(struct wmi_tlv_scan_chan_list_cmd);
	C(WMI_TLV_MGMT_TX_FRAME_MAX_LEN); C(WMI_RSRC_CFG_FLAG_TX_ACK_RSSI); C(WMI_TLV_FLAG_MGMT_BUNDLE_TX_COMPL);
	C(WMI_TLV_ABI_VER0); C(WMI_TLV_ABI_VER1); C(WMI_TLV_ABI_VER_NS0); C(WMI_TLV_ABI_VER_NS1);
	C(WMI_TLV_ABI_VER_NS2); C(WMI_TLV_ABI_VER_NS3);
	C(WLAN_SCAN_PARAMS_MAX_SSID); C(WLAN_SCAN_PARAMS_MAX_BSSID); C(WLAN_SCAN_PARAMS_MAX_IE_LEN);
	C(WMI_HOST_SCAN_REQ_ID_PREFIX); C(WMI_HOST_SCAN_REQUESTOR_ID_PREFIX); C(WMI_SCAN_FILTER_PROBE_REQ);
	C(WMI_SCAN_STOP_ONE); C(WMI_SCAN_STOP_VDEV_ALL); C(WMI_SCAN_STOP_ALL);
	C(WMI_VDEV_START_HIDDEN_SSID); C(WMI_VDEV_START_PMF_ENABLED);
	C(WMI_CHAN_FLAG_HT40_PLUS); C(WMI_CHAN_FLAG_PASSIVE); C(WMI_CHAN_FLAG_ADHOC_ALLOWED); C(WMI_CHAN_FLAG_DFS);
	C(WMI_CHAN_FLAG_ALLOW_HT); C(WMI_CHAN_FLAG_ALLOW_VHT); C(WMI_CHAN_FLAG_DFS_CFREQ2);
	C(MODE_11GONLY); C(MODE_11NG_HT40); C(MODE_11NA_HT40); C(MODE_11AC_VHT40); C(MODE_11AC_VHT80); C(MODE_11AC_VHT20_2G);
	C(MODE_11AC_VHT40_2G); C(MODE_11AC_VHT80_2G); C(MODE_11AC_VHT80_80); C(MODE_11AC_VHT160); C(MODE_UNKNOWN);
	C(WMI_VDEV_TYPE_AP); C(WMI_VDEV_TYPE_STA); C(WMI_VDEV_TYPE_IBSS); C(WMI_VDEV_TYPE_MONITOR);
	C(WMI_TLV_VDEV_SUBTYPE_NONE); C(WMI_PEER_TYPE_DEFAULT); C(WMI_PEER_TYPE_BSS); C(WMI_PEER_TYPE_TDLS);
	C(MODE_11A); C(MODE_11G); C(MODE_11B); C(MODE_11NG_HT20); C(MODE_11NA_HT20); C(MODE_11AC_VHT20);
	C(WMI_SCAN_EVENT_STARTED); C(WMI_SCAN_EVENT_COMPLETED); C(WMI_SCAN_EVENT_BSS_CHANNEL);
	C(WMI_SCAN_EVENT_FOREIGN_CHANNEL); C(WMI_SCAN_EVENT_DEQUEUED); C(WMI_SCAN_EVENT_PREEMPTED);
	C(WMI_SCAN_EVENT_START_FAILED); C(WMI_SCAN_EVENT_RESTARTED); C(WMI_SCAN_EVENT_FOREIGN_CHANNEL_EXIT);
	C(WMI_SCAN_REASON_COMPLETED); C(WMI_SCAN_REASON_CANCELLED); C(WMI_SCAN_REASON_PREEMPTED);
	C(WMI_SCAN_REASON_TIMEDOUT); C(WMI_SCAN_REASON_INTERNAL_FAILURE);
	C(WMI_MAX_MEM_REQS); C(WMI_TLV_TAG_MAX);
	C(WMI_SERVICE_RX_FULL_REORDER); C(WMI_SERVICE_TX_DATA_ACK_RSSI); C(WMI_SERVICE_SUPPORT_EXTEND_ADDRESS);
	C(TARGET_TLV_NUM_VDEVS); C(TARGET_TLV_NUM_PEERS); C(TARGET_TLV_NUM_TIDS); C(TARGET_TLV_NUM_TDLS_VDEVS);
	C(TARGET_HL_TLV_NUM_PEERS); C(TARGET_HL_TLV_AST_SKID_LIMIT); C(TARGET_HL_TLV_NUM_WDS_ENTRIES);
#define ID(x) printf("id.%s %lld\n", #x, (long long)(x))
	ID(WMI_TLV_INIT_CMDID); ID(WMI_TLV_PDEV_SET_REGDOMAIN_CMDID); ID(WMI_TLV_PDEV_SET_PARAM_CMDID);
	ID(WMI_TLV_START_SCAN_CMDID); ID(WMI_TLV_STOP_SCAN_CMDID); ID(WMI_TLV_VDEV_CREATE_CMDID);
	ID(WMI_TLV_VDEV_DELETE_CMDID); ID(WMI_TLV_VDEV_START_REQUEST_CMDID); ID(WMI_TLV_VDEV_UP_CMDID);
	ID(WMI_TLV_VDEV_STOP_CMDID); ID(WMI_TLV_VDEV_DOWN_CMDID); ID(WMI_TLV_VDEV_INSTALL_KEY_CMDID);
	ID(WMI_TLV_PEER_CREATE_CMDID); ID(WMI_TLV_PEER_DELETE_CMDID); ID(WMI_TLV_PEER_ASSOC_CMDID);
	ID(WMI_TLV_MGMT_TX_CMDID);
	ID(WMI_TLV_SERVICE_READY_EVENTID); ID(WMI_TLV_READY_EVENTID); ID(WMI_TLV_SERVICE_AVAILABLE_EVENTID);
	ID(WMI_TLV_SCAN_EVENTID); ID(WMI_TLV_TX_PAUSE_EVENTID); ID(WMI_TLV_VDEV_START_RESP_EVENTID);
	ID(WMI_TLV_VDEV_STOPPED_EVENTID); ID(WMI_TLV_VDEV_DELETE_RESP_EVENTID); ID(WMI_TLV_PEER_STA_KICKOUT_EVENTID);
	ID(WMI_TLV_PEER_DELETE_RESP_EVENTID); ID(WMI_TLV_MGMT_RX_EVENTID); ID(WMI_TLV_MGMT_TX_COMPLETION_EVENTID);
	ID(WMI_TLV_CHAN_INFO_EVENTID);
#define TG(x) printf("tag.%s %lld\n", #x, (long long)(x))
	TG(WMI_TLV_TAG_ARRAY_UINT32); TG(WMI_TLV_TAG_ARRAY_BYTE); TG(WMI_TLV_TAG_ARRAY_STRUCT);
	TG(WMI_TLV_TAG_ARRAY_FIXED_STRUCT); TG(WMI_TLV_TAG_STRUCT_INIT_CMD); TG(WMI_TLV_TAG_STRUCT_RESOURCE_CONFIG);
	TG(WMI_TLV_TAG_STRUCT_WLAN_HOST_MEMORY_CHUNK); TG(WMI_TLV_TAG_STRUCT_START_SCAN_CMD);
	TG(WMI_TLV_TAG_STRUCT_STOP_SCAN_CMD); TG(WMI_TLV_TAG_STRUCT_VDEV_CREATE_CMD); TG(WMI_TLV_TAG_STRUCT_VDEV_DELETE_CMD);
	TG(WMI_TLV_TAG_STRUCT_VDEV_START_REQUEST_CMD); TG(WMI_TLV_TAG_STRUCT_VDEV_UP_CMD); TG(WMI_TLV_TAG_STRUCT_VDEV_DOWN_CMD);
	TG(WMI_TLV_TAG_STRUCT_PEER_CREATE_CMD); TG(WMI_TLV_TAG_STRUCT_PEER_DELETE_CMD);
	TG(WMI_TLV_TAG_STRUCT_PDEV_SET_REGDOMAIN_CMD); TG(WMI_TLV_TAG_STRUCT_CHANNEL); TG(WMI_TLV_TAG_STRUCT_MGMT_TX_CMD);
	TG(WMI_TLV_TAG_STRUCT_SERVICE_READY_EVENT); TG(WMI_TLV_TAG_STRUCT_HAL_REG_CAPABILITIES);
	TG(WMI_TLV_TAG_STRUCT_WLAN_HOST_MEM_REQ); TG(WMI_TLV_TAG_STRUCT_READY_EVENT);
	TG(WMI_TLV_TAG_STRUCT_SERVICE_AVAILABLE_EVENT); TG(WMI_TLV_TAG_STRUCT_SCAN_EVENT);
	TG(WMI_TLV_TAG_STRUCT_MGMT_RX_HDR); TG(WMI_TLV_TAG_STRUCT_CHAN_INFO_EVENT);
	TG(WMI_TLV_TAG_STRUCT_VDEV_START_RESPONSE_EVENT); TG(WMI_TLV_TAG_STRUCT_PEER_STA_KICKOUT_EVENT);
	TG(WMI_TLV_TAG_STRUCT_HOST_SWBA_EVENT); TG(WMI_TLV_TAG_STRUCT_TIM_INFO); TG(WMI_TLV_TAG_STRUCT_P2P_NOA_INFO);
	TG(WMI_TLV_TAG_STRUCT_OFFLOAD_BCN_TX_STATUS_EVENT); TG(WMI_TLV_TAG_STRUCT_DIAG_DATA_CONTAINER_EVENT);
	TG(WMI_TLV_TAG_STRUCT_P2P_NOA_EVENT); TG(WMI_TLV_TAG_STRUCT_ROAM_EVENT); TG(WMI_TLV_TAG_STRUCT_WOW_EVENT_INFO);
	TG(WMI_TLV_TAG_STRUCT_TX_PAUSE_EVENT);
	for (i = 0; i < (int)ARRAY_SIZE(wmi_tlv_policies); i++)
		if (wmi_tlv_policies[i].min_len) printf("policy.%d %zu\n", i, wmi_tlv_policies[i].min_len);

	/* ---- iteration ---- */
	{
		static const uint8_t ok[] = {4, 0, 1, 0, 1, 2, 3, 4, 0, 0, 2, 0};          /* two tlvs, second empty */
		static const uint8_t trailing[] = {4, 0, 1, 0, 1, 2, 3, 4, 7, 7};          /* 2 bytes left */
		static const uint8_t toolong[] = {9, 0, 1, 0, 1, 2, 3, 4};                 /* len beyond the buffer */
		static const uint8_t unaligned[] = {3, 0, 1, 0, 1, 2, 3, 0, 0, 0, 2, 0};   /* len 3: no alignment demanded */
		uint8_t pol[4 + 8] = {8, 0};
		uint8_t pol_ok[4 + 24] = {24, 0};
		uint8_t arr[4] = {0, 0, 0, 0};
		pol[2] = WMI_TLV_TAG_STRUCT_SCAN_EVENT & 0xff; pol[3] = WMI_TLV_TAG_STRUCT_SCAN_EVENT >> 8;
		pol_ok[2] = pol[2]; pol_ok[3] = pol[3];
		arr[2] = WMI_TLV_TAG_ARRAY_UINT32 & 0xff; arr[3] = WMI_TLV_TAG_ARRAY_UINT32 >> 8;
		iter_case(1, ok, sizeof(ok)); iter_case(2, trailing, sizeof(trailing)); iter_case(3, toolong, sizeof(toolong));
		iter_case(4, unaligned, sizeof(unaligned)); iter_case(5, pol, sizeof(pol)); iter_case(6, pol_ok, sizeof(pol_ok));
		iter_case(7, arr, sizeof(arr)); iter_case(8, ok, 0);
	}

	/* ---- service ready ---- */
	svc_rdy_case("svc_rdy_ok", 0, 0, 0, 2, 0);
	svc_rdy_case("svc_rdy_no_memreq", 0, 0, 0, 0, 0);
	svc_rdy_case("svc_rdy_abi", 1, 0, 0, 2, 0);
	svc_rdy_case("svc_rdy_no_reg", 0, 1, 0, 2, 0);
	svc_rdy_case("svc_rdy_bad_memreq_tag", 0, 0, 1, 2, 0);
	svc_rdy_case("svc_rdy_short_ev", 0, 0, 0, 2, 1);
	svc_rdy_case("svc_rdy_many_memreqs", 0, 0, 0, 17, 0);

	/* ---- ready ---- */
	{
		struct wmi_tlv_rdy_ev ev;
		struct wmi_rdy_ev_arg arg;
		struct sk_buff skb;
		int r;
		memset(&ev, 0, sizeof(ev));
		ev.abi.abi_ver0 = 0x01000000; ev.abi.abi_ver1 = 53; ev.status = 0;
		ev.mac_addr.addr[0] = 0x02; ev.mac_addr.addr[1] = 0x11; ev.mac_addr.addr[2] = 0x22;
		ev.mac_addr.addr[3] = 0x33; ev.mac_addr.addr[4] = 0x44; ev.mac_addr.addr[5] = 0x55;
		evlen = 0;
		put_tlv(WMI_TLV_TAG_STRUCT_READY_EVENT, &ev, sizeof(ev));
		hexline("ready_ok", evbuf, evlen);
		skb.data = evbuf; skb.len = evlen;
		memset(&arg, 0, sizeof(arg));
		r = ath10k_wmi_tlv_op_pull_rdy_ev(&ar, &skb, &arg);
		printf("ready.ret %d\n", r);
		if (!r) printf("ready.fields sw_version=0x%x abi_version=%u status=%u mac=%02x%02x%02x%02x%02x%02x\n",
			       arg.sw_version, arg.abi_version, arg.status, arg.mac_addr[0], arg.mac_addr[1], arg.mac_addr[2],
			       arg.mac_addr[3], arg.mac_addr[4], arg.mac_addr[5]);
		evlen = 0;
		put_tlv(WMI_TLV_TAG_ARRAY_BYTE, "abcd", 4);
		skb.len = evlen;
		r = ath10k_wmi_tlv_op_pull_rdy_ev(&ar, &skb, &arg);
		printf("ready_missing.ret %d\n", r);
		evlen = 0;
		put_tlv(WMI_TLV_TAG_STRUCT_READY_EVENT, &ev, sizeof(ev) - 1);
		skb.len = evlen;
		r = ath10k_wmi_tlv_op_pull_rdy_ev(&ar, &skb, &arg);
		printf("ready_short.ret %d\n", r);
	}

	/* ---- scan event, vdev start response ---- */
	{
		struct wmi_scan_event ev = {.event_type = WMI_SCAN_EVENT_COMPLETED, .reason = WMI_SCAN_REASON_CANCELLED,
					    .channel_freq = 2437, .scan_req_id = 0xa007, .scan_id = 0xa123, .vdev_id = 1};
		struct wmi_vdev_start_response_event vs = {.vdev_id = 1, .req_id = 2, .resp_type = 3, .status = 4};
		struct wmi_scan_ev_arg sa;
		struct wmi_vdev_start_ev_arg va;
		struct sk_buff skb;
		int r;
		evlen = 0;
		put_tlv(WMI_TLV_TAG_STRUCT_SCAN_EVENT, &ev, sizeof(ev));
		hexline("scan_ev", evbuf, evlen);
		skb.data = evbuf; skb.len = evlen;
		r = ath10k_wmi_tlv_op_pull_scan_ev(&ar, &skb, &sa);
		printf("scan_ev.ret %d\n", r);
		if (!r) printf("scan_ev.fields event_type=0x%x reason=%u channel_freq=%u scan_req_id=0x%x scan_id=0x%x vdev_id=%u\n",
			       sa.event_type, sa.reason, sa.channel_freq, sa.scan_req_id, sa.scan_id, sa.vdev_id);
		evlen = 0;
		put_tlv(WMI_TLV_TAG_ARRAY_BYTE, "xx", 2);
		skb.len = evlen;
		printf("scan_ev_missing.ret %d\n", ath10k_wmi_tlv_op_pull_scan_ev(&ar, &skb, &sa));
		evlen = 0;
		put_tlv(WMI_TLV_TAG_STRUCT_VDEV_START_RESPONSE_EVENT, &vs, sizeof(vs));
		hexline("vdev_start_resp", evbuf, evlen);
		skb.data = evbuf; skb.len = evlen;
		r = ath10k_wmi_tlv_op_pull_vdev_start_ev(&ar, &skb, &va);
		printf("vdev_start_resp.ret %d\n", r);
		if (!r) printf("vdev_start_resp.fields vdev_id=%u req_id=%u resp_type=%u status=%u\n", va.vdev_id, va.req_id, va.resp_type, va.status);
	}

	/* ---- service available ---- */
	{
		struct wmi_svc_avail_ev_arg arg;
		struct sk_buff skb;
		uint8_t body[4 + 8];
		int r;
		__le32 n = 8;
		memcpy(body, &n, 4);
		memset(body + 4, 0xa5, 8);
		evlen = 0;
		put_tlv(WMI_TLV_TAG_STRUCT_SERVICE_AVAILABLE_EVENT, body, sizeof(body));
		hexline("svc_avail", evbuf, evlen);
		skb.data = evbuf; skb.len = evlen;
		memset(&arg, 0, sizeof(arg));
		r = ath10k_wmi_tlv_op_pull_svc_avail(&ar, &skb, &arg);
		printf("svc_avail.ret %d valid=%d len=%u off=%ld\n", r, arg.service_map_ext_valid, arg.service_map_ext_len,
		       arg.service_map_ext ? (long)((const uint8_t *)arg.service_map_ext - evbuf) : -1L);
	}

	/* ---- init ---- */
	{
		struct sk_buff *skb;
		memset(&ar, 0, sizeof(ar));
		ar.hw_params.num_peers = TARGET_HL_TLV_NUM_PEERS;
		ar.hw_params.ast_skid_limit = TARGET_HL_TLV_AST_SKID_LIMIT;
		ar.hw_params.num_wds_entries = TARGET_HL_TLV_NUM_WDS_ENTRIES;
		ar.wmi.rx_decap_mode = 1;
		ar.htt.max_num_pending_tx = TARGET_TLV_NUM_MSDU_DESC;
		ar.wow.max_num_patterns = 0;
		skb = ath10k_wmi_tlv_op_gen_init(&ar);
		show_skb("cmd.init_no_chunks", skb);
		ar.wmi.num_mem_chunks = 3;
		ar.wmi.mem_chunks[0] = (struct mem_chunk){1, 0x20000, 0x120000000ULL};
		ar.wmi.mem_chunks[1] = (struct mem_chunk){2, 0x4000, 0x130000000ULL};
		ar.wmi.mem_chunks[2] = (struct mem_chunk){0x13, 0x800, 0xfff00000ULL};
		skb = ath10k_wmi_tlv_op_gen_init(&ar);
		show_skb("cmd.init_chunks_32", skb);
		__set_bit(WMI_SERVICE_RX_FULL_REORDER, ar.wmi.svc_map);
		__set_bit(WMI_SERVICE_TX_DATA_ACK_RSSI, ar.wmi.svc_map);
		__set_bit(WMI_SERVICE_SUPPORT_EXTEND_ADDRESS, ar.wmi.svc_map);
		ar.wow.max_num_patterns = 22;
		ar.wmi.rx_decap_mode = 2;
		skb = ath10k_wmi_tlv_op_gen_init(&ar);
		show_skb("cmd.init_all_services", skb);
	}

	/* ---- start / stop scan ---- */
	{
		struct wmi_start_scan_arg a;
		struct sk_buff *skb;
		static const u8 ie[10] = {0xdd, 8, 0, 0x50, 0xf2, 4, 1, 2, 3, 4};
		static const u8 ssid_a[3] = {'a', 'b', 'c'};
		static const u8 ssid_b[32] = "0123456789abcdef0123456789abcdef";
		static const u8 bssid[6] = {2, 3, 4, 5, 6, 7};
		memset(&a, 0, sizeof(a));
		a.scan_id = 0x123; a.scan_req_id = 0x45; a.vdev_id = 1; a.scan_priority = 2; a.notify_scan_events = 0x1ff;
		a.dwell_time_active = 50; a.dwell_time_passive = 110; a.min_rest_time = 50; a.max_rest_time = 500;
		a.repeat_probe_time = 0; a.probe_spacing_time = 0; a.idle_time = 25; a.max_scan_time = 20000; a.probe_delay = 5;
		a.scan_ctrl_flags = 0x2 | 0x4; a.burst_duration_ms = 0;
		a.ie_len = 10; memcpy(a.ie, ie, 10);
		a.n_channels = 3; a.channels[0] = 2412; a.channels[1] = 2437; a.channels[2] = 5180;
		a.n_ssids = 2; a.ssids[0].len = 3; a.ssids[0].ssid = ssid_a; a.ssids[1].len = 32; a.ssids[1].ssid = ssid_b;
		a.n_bssids = 1; a.bssids[0].bssid = bssid;
		memcpy(a.mac_addr.addr, "\x0a\x0b\x0c\x0d\x0e\x0f", 6);
		memcpy(a.mac_mask.addr, "\xff\xff\xff\xff\xff\xff", 6);
		skb = ath10k_wmi_tlv_op_gen_start_scan(&ar, &a);
		show_skb("cmd.start_scan_full", skb);
		a.scan_ctrl_flags = WMI_SCAN_FILTER_PROBE_REQ | 0x1;
		a.ie_len = 0; a.n_channels = 0; a.n_ssids = 0; a.n_bssids = 0;
		skb = ath10k_wmi_tlv_op_gen_start_scan(&ar, &a);
		show_skb("cmd.start_scan_empty", skb);
		a.ie_len = 5; a.n_channels = 1; a.n_ssids = 1; a.ssids[0].len = 0; a.n_bssids = 0;
		skb = ath10k_wmi_tlv_op_gen_start_scan(&ar, &a);
		show_skb("cmd.start_scan_odd_ie", skb);
		a.ie_len = 257;
		show_skb("cmd.start_scan_ie_too_long", ath10k_wmi_tlv_op_gen_start_scan(&ar, &a));
		a.ie_len = 0; a.n_channels = 65;
		show_skb("cmd.start_scan_too_many_channels", ath10k_wmi_tlv_op_gen_start_scan(&ar, &a));
		a.n_channels = 0; a.n_ssids = 17;
		show_skb("cmd.start_scan_too_many_ssids", ath10k_wmi_tlv_op_gen_start_scan(&ar, &a));
		a.n_ssids = 0; a.n_bssids = 5;
		show_skb("cmd.start_scan_too_many_bssids", ath10k_wmi_tlv_op_gen_start_scan(&ar, &a));
	}
	{
		struct wmi_stop_scan_arg a;
		memset(&a, 0, sizeof(a));
		a.req_id = 0x7; a.req_type = WMI_SCAN_STOP_ONE; a.u.scan_id = 0x123;
		show_skb("cmd.stop_scan_one", ath10k_wmi_tlv_op_gen_stop_scan(&ar, &a));
		a.req_type = WMI_SCAN_STOP_VDEV_ALL; a.u.vdev_id = 3;
		show_skb("cmd.stop_scan_vdev", ath10k_wmi_tlv_op_gen_stop_scan(&ar, &a));
		a.req_type = WMI_SCAN_STOP_ALL; a.u.scan_id = 0;
		show_skb("cmd.stop_scan_all", ath10k_wmi_tlv_op_gen_stop_scan(&ar, &a));
		a.req_id = 0x1000;
		show_skb("cmd.stop_scan_bad_req", ath10k_wmi_tlv_op_gen_stop_scan(&ar, &a));
		a.req_id = 1; a.req_type = WMI_SCAN_STOP_ONE; a.u.scan_id = 0x1000;
		show_skb("cmd.stop_scan_bad_id", ath10k_wmi_tlv_op_gen_stop_scan(&ar, &a));
	}

	/* ---- vdev / peer / regdomain ---- */
	{
		static const u8 mac[6] = {0x02, 0x11, 0x22, 0x33, 0x44, 0x55};
		static const u8 bss[6] = {0xde, 0xad, 0xbe, 0xef, 0x00, 0x01};
		show_skb("cmd.vdev_create", ath10k_wmi_tlv_op_gen_vdev_create(&ar, 1, WMI_VDEV_TYPE_STA, WMI_VDEV_SUBTYPE_NONE, mac));
		show_skb("cmd.vdev_delete", ath10k_wmi_tlv_op_gen_vdev_delete(&ar, 1));
		show_skb("cmd.vdev_up", ath10k_wmi_tlv_op_gen_vdev_up(&ar, 1, 0x4001, bss));
		show_skb("cmd.vdev_down", ath10k_wmi_tlv_op_gen_vdev_down(&ar, 1));
		show_skb("cmd.peer_create", ath10k_wmi_tlv_op_gen_peer_create(&ar, 1, bss, WMI_PEER_TYPE_BSS));
		show_skb("cmd.peer_delete", ath10k_wmi_tlv_op_gen_peer_delete(&ar, 1, bss));
		show_skb("cmd.pdev_set_rd", ath10k_wmi_tlv_op_gen_pdev_set_rd(&ar, 0x1111, 0x2222, 0x3333, 0x44, 0x55, WMI_FCC_DFS_DOMAIN));
	}
	{
		struct wmi_vdev_start_request_arg a;
		static u8 ssid[6] = "lucina";
		memset(&a, 0, sizeof(a));
		a.vdev_id = 1; a.bcn_intval = 100; a.dtim_period = 2; a.ssid = ssid; a.ssid_len = 6; a.bcn_tx_rate = 0;
		a.bcn_tx_power = 0; a.disable_hw_ack = false; a.hidden_ssid = false; a.pmf_enabled = true;
		a.channel.freq = 5180; a.channel.band_center_freq1 = 5190; a.channel.band_center_freq2 = 0;
		a.channel.passive = false; a.channel.allow_ht = true; a.channel.allow_vht = true; a.channel.ht40plus = true;
		a.channel.chan_radar = true; a.channel.min_power = 0; a.channel.max_power = 60; a.channel.max_reg_power = 54;
		a.channel.max_antenna_gain = 3; a.channel.reg_class_id = 0x80; a.channel.mode = MODE_11AC_VHT40;
		show_skb("cmd.vdev_start", ath10k_wmi_tlv_op_gen_vdev_start(&ar, &a, false));
		a.ssid = NULL; a.ssid_len = 0; a.hidden_ssid = false; a.pmf_enabled = false; a.disable_hw_ack = true;
		a.channel.freq = 2437; a.channel.band_center_freq1 = 2437; a.channel.mode = MODE_11G;
		a.channel.allow_ht = false; a.channel.allow_vht = false; a.channel.ht40plus = false; a.channel.chan_radar = false;
		a.channel.passive = true;
		show_skb("cmd.vdev_start_open", ath10k_wmi_tlv_op_gen_vdev_start(&ar, &a, true));
		a.ssid = ssid; a.ssid_len = 33;
		show_skb("cmd.vdev_start_ssid_too_long", ath10k_wmi_tlv_op_gen_vdev_start(&ar, &a, false));
		a.ssid = NULL; a.ssid_len = 0; a.hidden_ssid = true;
		show_skb("cmd.vdev_start_hidden_no_ssid", ath10k_wmi_tlv_op_gen_vdev_start(&ar, &a, false));
	}
	{ /* ath10k_wmi_tlv_op_gen_mgmt_tx_send, minus the mac80211 plumbing: same TLV layout, same field assignments */
		struct wmi_tlv_mgmt_tx_cmd *cmd;
		struct wmi_tlv *tlv;
		uint8_t frame[100];
		uint8_t out[256];
		u32 buf_len, len, vdev_id = 1, desc_id = 7;
		u64 paddr = 0x123456780ULL;
		void *ptr = out;
		int k;
		for (k = 0; k < 100; k++) frame[k] = k;
		memset(out, 0, sizeof(out));
		buf_len = 100;
		len = sizeof(*cmd) + 2 * sizeof(*tlv);
		buf_len = buf_len < WMI_TLV_MGMT_TX_FRAME_MAX_LEN ? buf_len : WMI_TLV_MGMT_TX_FRAME_MAX_LEN;
		buf_len = (buf_len + 3) & ~3u;
		len += buf_len;
		len = (len + 3) & ~3u;
		tlv = ptr;
		tlv->tag = WMI_TLV_TAG_STRUCT_MGMT_TX_CMD; tlv->len = sizeof(*cmd);
		cmd = (void *)tlv->value;
		cmd->vdev_id = vdev_id; cmd->desc_id = desc_id; cmd->chanfreq = 0; cmd->buf_len = buf_len;
		cmd->frame_len = 100; cmd->paddr = paddr;
		ptr += sizeof(*tlv); ptr += sizeof(*cmd);
		tlv = ptr;
		tlv->tag = WMI_TLV_TAG_ARRAY_BYTE; tlv->len = buf_len;
		ptr += sizeof(*tlv);
		memcpy(ptr, frame, buf_len);
		hexline("cmd.mgmt_tx_100", out, len);
	}
	/* ---- station commands and events added for the driver (lucina-ath10k src/driver) ---- */
	ar.wmi_key_cipher = wmi_tlv_key_cipher_suites;
	SZ(struct wmi_tlv_pdev_set_param_cmd); SZ(struct wmi_vdev_stop_cmd); SZ(struct wmi_vdev_set_param_cmd);
	SZ(struct wmi_vdev_install_key_cmd); F(wmi_vdev_install_key_cmd, peer_macaddr); F(wmi_vdev_install_key_cmd, key_idx);
	F(wmi_vdev_install_key_cmd, key_flags); F(wmi_vdev_install_key_cmd, key_cipher); F(wmi_vdev_install_key_cmd, key_len);
	F(wmi_vdev_install_key_cmd, key_txmic_len); F(wmi_vdev_install_key_cmd, key_rxmic_len);
	SZ(struct wmi_peer_set_param_cmd); SZ(struct wmi_vht_rate_set);
	F(wmi_tlv_peer_assoc_cmd, mac_addr); F(wmi_tlv_peer_assoc_cmd, vdev_id); F(wmi_tlv_peer_assoc_cmd, phy_mode); F(wmi_tlv_peer_assoc_cmd, num_ht_rates);
	SZ(struct wmi_sta_powersave_mode_cmd); SZ(struct wmi_sta_powersave_param_cmd); SZ(struct wmi_tlv_sta_keepalive_cmd);
	SZ(struct wmi_sta_keepalive_arp_resp);
	SZ(struct wmi_tlv_mgmt_rx_ev); F(wmi_tlv_mgmt_rx_ev, channel); F(wmi_tlv_mgmt_rx_ev, snr); F(wmi_tlv_mgmt_rx_ev, rate);
	F(wmi_tlv_mgmt_rx_ev, phy_mode); F(wmi_tlv_mgmt_rx_ev, buf_len); F(wmi_tlv_mgmt_rx_ev, status); F(wmi_tlv_mgmt_rx_ev, rssi);
	SZ(struct wmi_tlv_mgmt_tx_compl_ev); F(wmi_tlv_mgmt_tx_compl_ev, status); F(wmi_tlv_mgmt_tx_compl_ev, ack_rssi);
	SZ(struct wmi_tlv_roam_ev); SZ(struct wmi_peer_sta_kickout_event);
	C(WMI_KEY_PAIRWISE); C(WMI_KEY_GROUP); C(WMI_TLV_CIPHER_NONE); C(WMI_TLV_CIPHER_AES_CCM); C(WMI_TLV_CIPHER_TKIP); C(WMI_MAX_KEY_INDEX);
	C(MAX_SUPPORTED_RATES); C(WMI_TLV_MGMT_RX_NUM_RSSI); C(WMI_RX_STATUS_ERR_CRC); C(WMI_RX_STATUS_ERR_DECRYPT); C(WMI_RX_STATUS_ERR_MIC);
	C(WMI_RX_STATUS_ERR_KEY_CACHE_MISS); C(WMI_RX_STATUS_EXT_INFO);
	C(WMI_TLV_PDEV_PARAM_TX_CHAIN_MASK); C(WMI_TLV_PDEV_PARAM_RX_CHAIN_MASK); C(WMI_TLV_PDEV_PARAM_TXPOWER_LIMIT2G);
	C(WMI_TLV_PDEV_PARAM_TXPOWER_LIMIT5G); C(WMI_TLV_PDEV_PARAM_DYNAMIC_BW); C(WMI_TLV_PDEV_PARAM_PMF_QOS);
	C(WMI_TLV_PDEV_PARAM_ARP_AC_OVERRIDE); C(WMI_TLV_PDEV_PARAM_ANI_ENABLE); C(WMI_TLV_PDEV_PARAM_IDLE_PS_CONFIG);
	C(WMI_TLV_PDEV_PARAM_BURST_ENABLE); C(WMI_TLV_PDEV_PARAM_PEER_STATS_INFO_ENABLE);
	C(WMI_TLV_VDEV_PARAM_RTS_THRESHOLD); C(WMI_TLV_VDEV_PARAM_BEACON_INTERVAL); C(WMI_TLV_VDEV_PARAM_DTIM_PERIOD);
	C(WMI_TLV_VDEV_PARAM_SLOT_TIME); C(WMI_TLV_VDEV_PARAM_PREAMBLE); C(WMI_TLV_VDEV_PARAM_NSS); C(WMI_TLV_VDEV_PARAM_TX_ENCAP_TYPE);
	C(WMI_TLV_VDEV_PARAM_TXBF);
	C(WMI_TLV_PEER_SMPS_STATE); C(WMI_TLV_PEER_AUTHORIZE); C(WMI_TLV_PEER_DUMMY_VAR);
	C(WMI_TLV_PEER_AUTH); C(WMI_TLV_PEER_QOS); C(WMI_TLV_PEER_NEED_PTK_4_WAY); C(WMI_TLV_PEER_NEED_GTK_2_WAY); C(WMI_TLV_PEER_HT);
	C(WMI_TLV_PEER_40MHZ); C(WMI_TLV_PEER_STBC); C(WMI_TLV_PEER_LDPC); C(WMI_TLV_PEER_VHT); C(WMI_TLV_PEER_80MHZ); C(WMI_TLV_PEER_160MHZ); C(WMI_TLV_PEER_PMF);
	C(WMI_RC_DS_FLAG); C(WMI_RC_CW40_FLAG); C(WMI_RC_SGI_FLAG); C(WMI_RC_HT_FLAG); C(WMI_RC_TX_STBC_FLAG); C(WMI_RC_RX_STBC_FLAG_S); C(WMI_RC_TS_FLAG);
	C(WMI_STA_PS_PARAM_RX_WAKE_POLICY); C(WMI_STA_PS_PARAM_TX_WAKE_THRESHOLD); C(WMI_STA_PS_PARAM_PSPOLL_COUNT);
	C(WMI_STA_PS_RX_WAKE_POLICY_WAKE); C(WMI_STA_PS_TX_WAKE_THRESHOLD_ALWAYS); C(WMI_STA_PS_PSPOLL_COUNT_NO_MAX);
	C(WMI_STA_PS_MODE_DISABLED); C(WMI_STA_PS_MODE_ENABLED); C(WMI_STA_KEEPALIVE_METHOD_NULL_FRAME); C(WMI_STA_KEEPALIVE_INTERVAL_DISABLE);
	C(WMI_VDEV_PARAM_TXBF_SU_TX_BFEE); C(WMI_VDEV_PARAM_TXBF_MU_TX_BFEE); C(WMI_VDEV_PARAM_TXBF_SU_TX_BFER); C(WMI_VDEV_PARAM_TXBF_MU_TX_BFER);
	C(WMI_ROAM_REASON_BEACON_MISS); C(WMI_VDEV_SLOT_TIME_SHORT); C(WMI_VDEV_SLOT_TIME_LONG); C(WMI_VDEV_PREAMBLE_SHORT); C(WMI_VDEV_PREAMBLE_LONG);
#define SVC(x) printf("svc.%s %lld\n", #x, (long long)(WMI_TLV_SERVICE_##x))
	SVC(BEACON_OFFLOAD); SVC(STA_KEEP_ALIVE); SVC(BURST); SVC(ADAPTIVE_OCS); SVC(RX_FULL_REORDER); SVC(MGMT_TX_WMI);
	SVC(SPOOF_MAC_SUPPORT); SVC(TX_DATA_MGMT_ACK_RSSI); SVC(SUPPORT_EXTEND_ADDRESS); C(WMI_TLV_MAX_SERVICE);
	ID(WMI_TLV_SCAN_CHAN_LIST_CMDID); ID(WMI_TLV_SCAN_PROB_REQ_OUI_CMDID); ID(WMI_TLV_VDEV_SET_PARAM_CMDID);
	ID(WMI_TLV_PEER_SET_PARAM_CMDID); ID(WMI_TLV_STA_POWERSAVE_MODE_CMDID); ID(WMI_TLV_STA_POWERSAVE_PARAM_CMDID);
	ID(WMI_TLV_STA_KEEPALIVE_CMDID); ID(WMI_TLV_RESMGR_ADAPTIVE_OCS_CMDID); ID(WMI_TLV_MGMT_TX_SEND_CMD);
	ID(WMI_TLV_ROAM_EVENTID); ID(WMI_TLV_VDEV_INSTALL_KEY_COMPLETE_EVENTID); ID(WMI_TLV_MGMT_TX_BUNDLE_COMPLETION_EVENTID);
	ID(WMI_TLV_OFFLOAD_BCN_TX_STATUS_EVENTID); ID(WMI_TLV_DIAG_EVENTID); ID(WMI_TLV_DIAG_DATA_CONTAINER_EVENTID); ID(WMI_TLV_DEBUG_MESG_EVENTID);
	TG(WMI_TLV_TAG_STRUCT_PDEV_SET_PARAM_CMD); TG(WMI_TLV_TAG_STRUCT_VDEV_STOP_CMD); TG(WMI_TLV_TAG_STRUCT_VDEV_SET_PARAM_CMD);
	TG(WMI_TLV_TAG_STRUCT_VDEV_INSTALL_KEY_CMD); TG(WMI_TLV_TAG_STRUCT_PEER_SET_PARAM_CMD); TG(WMI_TLV_TAG_STRUCT_PEER_ASSOC_COMPLETE_CMD);
	TG(WMI_TLV_TAG_STRUCT_VHT_RATE_SET); TG(WMI_TLV_TAG_STRUCT_STA_POWERSAVE_MODE_CMD); TG(WMI_TLV_TAG_STRUCT_STA_POWERSAVE_PARAM_CMD);
	TG(WMI_TLV_TAG_STRUCT_STA_KEEPALIVE_CMD); TG(WMI_TLV_TAG_STRUCT_STA_KEEPALVE_ARP_RESPONSE); TG(WMI_TLV_TAG_STRUCT_SCAN_CHAN_LIST_CMD);
	TG(WMI_TLV_TAG_STRUCT_SCAN_PROB_REQ_OUI_CMD); TG(WMI_TLV_TAG_STRUCT_RESMGR_ADAPTIVE_OCS_CMD); TG(WMI_TLV_TAG_STRUCT_MGMT_TX_COMPL_EVENT);
	TG(WMI_TLV_TAG_STRUCT_MGMT_TX_COMPL_BUNDLE_EVENT);
	{
		static const u8 mac[6] = {0x02, 0x11, 0x22, 0x33, 0x44, 0x55};
		static const u8 bss[6] = {0xde, 0xad, 0xbe, 0xef, 0x00, 0x01};
		static const u8 ptk[16] = {1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16};
		static const u8 tk[32] = {0xa0,0xa1,0xa2,0xa3,0xa4,0xa5,0xa6,0xa7,0xa8,0xa9,0xaa,0xab,0xac,0xad,0xae,0xaf,
			0xb0,0xb1,0xb2,0xb3,0xb4,0xb5,0xb6,0xb7,0xb8,0xb9,0xba,0xbb,0xbc,0xbd,0xbe,0xbf};
		struct wmi_vdev_install_key_arg k;
		struct wmi_peer_assoc_complete_arg pa;
		struct wmi_scan_chan_list_arg cl;
		struct wmi_channel_arg chans[3];
		struct wmi_sta_keepalive_arg ka;
		int n;
		show_skb("cmd.pdev_set_param", ath10k_wmi_tlv_op_gen_pdev_set_param(&ar, WMI_TLV_PDEV_PARAM_PMF_QOS, 1));
		show_skb("cmd.vdev_stop", ath10k_wmi_tlv_op_gen_vdev_stop(&ar, 1));
		show_skb("cmd.vdev_set_param", ath10k_wmi_tlv_op_gen_vdev_set_param(&ar, 1, WMI_TLV_VDEV_PARAM_TX_ENCAP_TYPE, 1));
		show_skb("cmd.peer_set_param", ath10k_wmi_tlv_op_gen_peer_set_param(&ar, 1, bss, WMI_TLV_PEER_AUTHORIZE, 1));
		show_skb("cmd.set_psmode", ath10k_wmi_tlv_op_gen_set_psmode(&ar, 1, WMI_STA_PS_MODE_DISABLED));
		show_skb("cmd.set_sta_ps", ath10k_wmi_tlv_op_gen_set_sta_ps(&ar, 1, WMI_STA_PS_PARAM_TX_WAKE_THRESHOLD, WMI_STA_PS_TX_WAKE_THRESHOLD_ALWAYS));
		show_skb("cmd.scan_prob_req_oui", ath10k_wmi_tlv_op_gen_scan_prob_req_oui(&ar, 0x021122));
		show_skb("cmd.adaptive_qcs", ath10k_wmi_tlv_op_gen_adaptive_qcs(&ar, true));
		memset(&ka, 0, sizeof(ka));
		ka.vdev_id = 1; ka.enabled = 1; ka.method = WMI_STA_KEEPALIVE_METHOD_NULL_FRAME; ka.interval = WMI_STA_KEEPALIVE_INTERVAL_DISABLE;
		show_skb("cmd.sta_keepalive", ath10k_wmi_tlv_op_gen_sta_keepalive(&ar, &ka));
		memset(&k, 0, sizeof(k));
		k.vdev_id = 1; k.key_idx = 0; k.key_len = 16; k.key_data = ptk; k.key_flags = WMI_KEY_PAIRWISE; k.macaddr = bss;
		k.key_cipher = wmi_tlv_key_cipher_suites[WMI_CIPHER_AES_CCM];
		show_skb("cmd.install_key_ptk", ath10k_wmi_tlv_op_gen_vdev_install_key(&ar, &k));
		k.key_idx = 2; k.key_flags = WMI_KEY_GROUP;
		show_skb("cmd.install_key_gtk", ath10k_wmi_tlv_op_gen_vdev_install_key(&ar, &k));
		k.key_idx = 1; k.key_len = 32; k.key_data = tk; k.key_txmic_len = 8; k.key_rxmic_len = 8;
		k.key_cipher = wmi_tlv_key_cipher_suites[WMI_CIPHER_TKIP];
		show_skb("cmd.install_key_tkip", ath10k_wmi_tlv_op_gen_vdev_install_key(&ar, &k));
		k.key_idx = 0; k.key_len = 16; k.key_data = NULL; k.key_txmic_len = 0; k.key_rxmic_len = 0;
		k.key_flags = WMI_KEY_PAIRWISE; k.key_cipher = wmi_tlv_key_cipher_suites[WMI_CIPHER_NONE];
		show_skb("cmd.install_key_delete", ath10k_wmi_tlv_op_gen_vdev_install_key(&ar, &k));
		k.key_data = ptk;
		show_skb("cmd.install_key_none_with_data", ath10k_wmi_tlv_op_gen_vdev_install_key(&ar, &k));
		k.key_data = NULL; k.key_cipher = wmi_tlv_key_cipher_suites[WMI_CIPHER_AES_CCM];
		show_skb("cmd.install_key_ccm_no_data", ath10k_wmi_tlv_op_gen_vdev_install_key(&ar, &k));

		memset(&pa, 0, sizeof(pa));
		memcpy(pa.addr, bss, 6);
		pa.vdev_id = 1; pa.peer_reassoc = false; pa.peer_aid = 5; pa.peer_flags = WMI_TLV_PEER_AUTH | WMI_TLV_PEER_QOS | WMI_TLV_PEER_NEED_PTK_4_WAY | WMI_TLV_PEER_HT | WMI_TLV_PEER_VHT;
		pa.peer_caps = 0x431; pa.peer_listen_intval = 10; pa.peer_ht_caps = 0x1ef; pa.peer_max_mpdu = 0xffff; pa.peer_mpdu_density = 8;
		pa.peer_rate_caps = WMI_RC_HT_FLAG | WMI_RC_CW40_FLAG; pa.peer_num_spatial_streams = 2; pa.peer_vht_caps = 0x339071b2; pa.peer_phymode = MODE_11AC_VHT80;
		pa.peer_legacy_rates.num_rates = 5; for (n = 0; n < 5; n++) pa.peer_legacy_rates.rates[n] = 12 + 12 * n;
		pa.peer_ht_rates.num_rates = 16; for (n = 0; n < 16; n++) pa.peer_ht_rates.rates[n] = n;
		pa.peer_vht_rates.rx_max_rate = 867; pa.peer_vht_rates.rx_mcs_set = 0xfffa; pa.peer_vht_rates.tx_max_rate = 867; pa.peer_vht_rates.tx_mcs_set = 0xfffa;
		show_skb("cmd.peer_assoc", ath10k_wmi_tlv_op_gen_peer_assoc(&ar, &pa));
		pa.peer_reassoc = true; pa.peer_legacy_rates.num_rates = 0; pa.peer_ht_rates.num_rates = 0;
		show_skb("cmd.peer_assoc_reassoc_empty", ath10k_wmi_tlv_op_gen_peer_assoc(&ar, &pa));
		pa.peer_mpdu_density = 17;
		show_skb("cmd.peer_assoc_bad_density", ath10k_wmi_tlv_op_gen_peer_assoc(&ar, &pa));
		pa.peer_mpdu_density = 0; pa.peer_legacy_rates.num_rates = MAX_SUPPORTED_RATES + 1;
		show_skb("cmd.peer_assoc_too_many_rates", ath10k_wmi_tlv_op_gen_peer_assoc(&ar, &pa));

		memset(chans, 0, sizeof(chans));
		chans[0].freq = 2412; chans[0].band_center_freq1 = 2412; chans[0].allow_ht = true; chans[0].allow_vht = true; chans[0].allow_ibss = true;
		chans[0].ht40plus = true; chans[0].max_power = 40; chans[0].max_reg_power = 40; chans[0].mode = MODE_11G;
		chans[1].freq = 2467; chans[1].band_center_freq1 = 2467; chans[1].allow_ht = true; chans[1].allow_vht = true; chans[1].passive = true;
		chans[1].max_power = 40; chans[1].max_reg_power = 40; chans[1].mode = MODE_11G;
		chans[2].freq = 5260; chans[2].band_center_freq1 = 5260; chans[2].allow_ht = true; chans[2].allow_vht = true; chans[2].passive = true;
		chans[2].chan_radar = true; chans[2].ht40plus = true; chans[2].max_power = 60; chans[2].max_reg_power = 60; chans[2].max_antenna_gain = 3; chans[2].mode = MODE_11A;
		memset(&cl, 0, sizeof(cl)); cl.n_channels = 3; cl.channels = chans;
		show_skb("cmd.scan_chan_list", ath10k_wmi_tlv_op_gen_scan_chan_list(&ar, &cl));
		cl.n_channels = 0;
		show_skb("cmd.scan_chan_list_empty", ath10k_wmi_tlv_op_gen_scan_chan_list(&ar, &cl));
	}
	{ /* the service bitmaps: which of 0..255 the SVCMAP test finds, for a main map (len 16 and 48) and an extended one */
		static const __le32 m[12] = {0x00000005, 0x0000000f, 0xfffffff0, 0x00000008, 0x80000001, 0, 0, 0, 0, 0, 0, 0x2};
		int x, ml;
		for (ml = 16; ml <= 48; ml += 32) {
			printf("svcmap.main.%d", ml);
			for (x = 0; x < 256; x++) if (WMI_SERVICE_IS_ENABLED(m, x, ml) || (x >= ml && (x - ml) / 32 < 12 && WMI_TLV_EXT_SERVICE_IS_ENABLED(m, x, ml))) printf(" %d", x);
			printf("\n");
		}
		printf("svcmap.ext");
		for (x = 128; x < 256; x++) if (((x - 128) / 32 < 12) && WMI_TLV_EXT_SERVICE_IS_ENABLED(m, x, WMI_TLV_MAX_SERVICE)) printf(" %d", x);
		printf("\n");
		hexline("svcmap.map", m, sizeof(m));
	}
	{ /* events */
		struct wmi_tlv_mgmt_rx_ev rx;
		struct wmi_mgmt_rx_ev_arg ra;
		struct wmi_tlv_mgmt_tx_compl_ev tc;
		struct wmi_tlv_mgmt_tx_compl_ev_arg ta;
		struct wmi_tlv_mgmt_tx_bundle_compl_ev_arg ba;
		struct wmi_peer_sta_kickout_event kick;
		struct wmi_peer_kick_ev_arg ka;
		struct wmi_tlv_roam_ev roam;
		struct wmi_roam_ev_arg rma;
		struct sk_buff skb;
		static const u8 frame[10] = {0x80, 0, 0, 0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff};
		int r, n;
		__le32 reports = 2, ids[2] = {3, 4}, sts[2] = {0, 2}, ppdu[2] = {9, 8}, rssi[2] = {40, 41};
		memset(&rx, 0, sizeof(rx));
		rx.channel = 6; rx.snr = 45; rx.rate = 1000; rx.phy_mode = MODE_11G; rx.buf_len = 10; rx.status = WMI_RX_STATUS_ERR_MIC;
		rx.rssi[0] = 44; rx.rssi[1] = 128; rx.rssi[2] = 0; rx.rssi[3] = 42;
		evlen = 0;
		put_tlv(WMI_TLV_TAG_STRUCT_MGMT_RX_HDR, &rx, sizeof(rx));
		put_tlv(WMI_TLV_TAG_ARRAY_BYTE, frame, 12 > sizeof(frame) ? sizeof(frame) : 12);
		hexline("mgmt_rx", evbuf, evlen);
		skb.data = evbuf; skb.len = evlen; memset(&ra, 0, sizeof(ra));
		r = ath10k_wmi_tlv_op_pull_mgmt_rx_ev(&ar, &skb, &ra);
		printf("mgmt_rx.ret %d channel=%u snr=%u rate=%u phy_mode=%u buf_len=%u status=%u rssi=%u,%u,%u,%u frame_off=%ld frame_len=%u\n", r, ra.channel, ra.snr, ra.rate, ra.phy_mode, ra.buf_len,
		       ra.status, ra.rssi[0], ra.rssi[1], ra.rssi[2], ra.rssi[3], (long)(skb.data - evbuf), skb.len);
		rx.buf_len = 11; evlen = 0;
		put_tlv(WMI_TLV_TAG_STRUCT_MGMT_RX_HDR, &rx, sizeof(rx));
		put_tlv(WMI_TLV_TAG_ARRAY_BYTE, frame, sizeof(frame));
		hexline("mgmt_rx_long", evbuf, evlen);
		skb.data = evbuf; skb.len = evlen;
		printf("mgmt_rx_long.ret %d\n", ath10k_wmi_tlv_op_pull_mgmt_rx_ev(&ar, &skb, &ra));
		evlen = 0;
		put_tlv(WMI_TLV_TAG_STRUCT_MGMT_RX_HDR, &rx, sizeof(rx));
		hexline("mgmt_rx_noframe", evbuf, evlen);
		skb.data = evbuf; skb.len = evlen;
		printf("mgmt_rx_noframe.ret %d\n", ath10k_wmi_tlv_op_pull_mgmt_rx_ev(&ar, &skb, &ra));

		memset(&tc, 0, sizeof(tc));
		tc.desc_id = 7; tc.status = 1; tc.pdev_id = 2; tc.ppdu_id = 3; tc.ack_rssi = 44;
		evlen = 0;
		put_tlv(WMI_TLV_TAG_STRUCT_MGMT_TX_COMPL_EVENT, &tc, sizeof(tc));
		hexline("mgmt_tx_compl", evbuf, evlen);
		skb.data = evbuf; skb.len = evlen; memset(&ta, 0, sizeof(ta));
		r = ath10k_wmi_tlv_op_pull_mgmt_tx_compl_ev(&ar, &skb, &ta);
		printf("mgmt_tx_compl.ret %d desc_id=%u status=%u pdev_id=%u ppdu_id=%u ack_rssi=%u\n", r, ta.desc_id, ta.status, ta.pdev_id, ta.ppdu_id, ta.ack_rssi);

		evlen = 0;
		put_tlv(WMI_TLV_TAG_STRUCT_MGMT_TX_COMPL_BUNDLE_EVENT, &reports, 4);
		put_tlv(WMI_TLV_TAG_ARRAY_UINT32, ids, sizeof(ids));
		put_tlv(WMI_TLV_TAG_ARRAY_UINT32, sts, sizeof(sts));
		put_tlv(WMI_TLV_TAG_ARRAY_UINT32, ppdu, sizeof(ppdu));
		put_tlv(WMI_TLV_TAG_ARRAY_UINT32, rssi, sizeof(rssi));
		hexline("mgmt_tx_bundle", evbuf, evlen);
		skb.data = evbuf; skb.len = evlen; memset(&ba, 0, sizeof(ba));
		r = ath10k_wmi_tlv_op_pull_mgmt_tx_bundle_compl_ev(&ar, &skb, &ba);
		printf("mgmt_tx_bundle.ret %d num_reports=%u", r, ba.num_reports);
		for (n = 0; n < 2; n++) printf(" id=%u status=%u ack=%u", ba.desc_ids[n], ba.status[n], ba.ack_rssi[n]);
		printf("\n");

		memset(&kick, 0, sizeof(kick));
		memcpy(kick.peer_macaddr.addr, "\xde\xad\xbe\xef\x00\x01", 6);
		evlen = 0;
		put_tlv(WMI_TLV_TAG_STRUCT_PEER_STA_KICKOUT_EVENT, &kick, sizeof(kick));
		hexline("peer_kick", evbuf, evlen);
		skb.data = evbuf; skb.len = evlen;
		r = ath10k_wmi_tlv_op_pull_peer_kick_ev(&ar, &skb, &ka);
		printf("peer_kick.ret %d off=%ld\n", r, (long)((const uint8_t *)ka.mac_addr - evbuf));

		roam.vdev_id = 1; roam.reason = WMI_ROAM_REASON_BEACON_MISS; roam.rssi = 22;
		evlen = 0;
		put_tlv(WMI_TLV_TAG_STRUCT_ROAM_EVENT, &roam, sizeof(roam));
		hexline("roam", evbuf, evlen);
		skb.data = evbuf; skb.len = evlen;
		r = ath10k_wmi_tlv_op_pull_roam_ev(&ar, &skb, &rma);
		printf("roam.ret %d vdev_id=%u reason=%u rssi=%u\n", r, rma.vdev_id, rma.reason, rma.rssi);
	}
	return 0;
}
