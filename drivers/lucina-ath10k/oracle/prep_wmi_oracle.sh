# sourced by gen.sh: cut the WMI-TLV code the oracle runs out of wmi-tlv.c and wmi.c
{
  range "$K/wmi-tlv.c" "struct wmi_tlv_tx_bundle_compl_parse {" "};"
  range_excl "$K/wmi-tlv.c" "struct wmi_tlv_policy {" "/**************/"
  range_excl "$K/wmi-tlv.c" "#define WMI_TLV_ABI_VER_NS0" "static int ath10k_wmi_tlv_op_pull_rdy_ev"
  for f in ath10k_wmi_tlv_op_pull_rdy_ev ath10k_wmi_tlv_op_pull_scan_ev ath10k_wmi_tlv_op_pull_vdev_start_ev ath10k_wmi_tlv_svc_avail_parse ath10k_wmi_tlv_op_pull_svc_avail \
           ath10k_wmi_tlv_put_host_mem_chunks ath10k_wmi_tlv_op_gen_init ath10k_wmi_tlv_op_gen_start_scan \
           ath10k_wmi_tlv_op_gen_stop_scan ath10k_wmi_tlv_op_gen_vdev_create ath10k_wmi_tlv_op_gen_vdev_delete \
           ath10k_wmi_tlv_op_gen_vdev_start ath10k_wmi_tlv_op_gen_vdev_up ath10k_wmi_tlv_op_gen_vdev_down \
           ath10k_wmi_tlv_op_gen_peer_create ath10k_wmi_tlv_op_gen_peer_delete ath10k_wmi_tlv_op_gen_pdev_set_rd \
           ath10k_wmi_tlv_op_gen_pdev_set_param ath10k_wmi_tlv_op_gen_vdev_stop ath10k_wmi_tlv_op_gen_vdev_set_param \
           ath10k_wmi_tlv_op_gen_vdev_install_key ath10k_wmi_tlv_op_gen_peer_set_param ath10k_wmi_tlv_op_gen_peer_assoc \
           ath10k_wmi_tlv_op_gen_scan_chan_list ath10k_wmi_tlv_op_gen_set_psmode ath10k_wmi_tlv_op_gen_set_sta_ps \
           ath10k_wmi_tlv_op_gen_sta_keepalive ath10k_wmi_tlv_op_gen_scan_prob_req_oui ath10k_wmi_tlv_op_gen_adaptive_qcs \
           ath10k_wmi_tlv_op_pull_mgmt_rx_ev ath10k_wmi_tlv_op_pull_mgmt_tx_compl_ev ath10k_wmi_tlv_mgmt_tx_bundle_compl_parse \
           ath10k_wmi_tlv_op_pull_mgmt_tx_bundle_compl_ev ath10k_wmi_tlv_op_pull_peer_kick_ev ath10k_wmi_tlv_op_pull_roam_ev; do
    fn "$K/wmi-tlv.c" $f
    echo
  done
  for f in ath10k_wmi_start_scan_verify ath10k_wmi_put_start_scan_common ath10k_wmi_put_wmi_channel; do
    fn "$K/wmi.c" $f
    echo
  done
} > "$B/wmi_funcs.inc"
range "$K/wmi.c" "static const u8 wmi_tlv_key_cipher_suites[] = {" "};" > "$B/wmi_tlv_cipher.h"
