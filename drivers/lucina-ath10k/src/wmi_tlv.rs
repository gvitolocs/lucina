//! WMI-TLV framing and the bring-up commands, for the WCN3990 firmware that Linux drives with `wmi-tlv.c`.
//!
//! Derived from Linux `drivers/net/wireless/ath/ath10k/wmi-tlv.c`, `wmi-tlv.h`, `wmi.h` and `wmi.c` (SPDX ISC;
//! Copyright (c) 2005-2011 Atheros Communications Inc., 2011-2017 Qualcomm Atheros, Inc., 2018-2019 The Linux
//! Foundation, 2022, 2024 Qualcomm Innovation Center, Inc., Qualcomm Technologies, Inc. and/or its subsidiaries).
//! `tests/wmi_tlv.rs` compares every encoder's bytes and every parser's result with the unchanged Linux functions
//! (cut out of the C files and compiled by `oracle/wmi_oracle.c`), and every struct size, field offset, id and tag
//! with the C compiler's view of the headers (`tests/fixtures/wmi_oracle.txt`).
//!
//! * Framing: a TLV is `len: u16`, `tag: u16`, then `len` bytes ([`TLV_HDR_LEN`]); [`iter`] walks a buffer with the
//!   checks of `ath10k_wmi_tlv_iter` (header fits, value fits, per-tag minimum length from the policy table
//!   [`POLICY`]). It does not look at alignment, and neither does Linux: the length field already includes whatever
//!   padding the sender added. A WMI message is a 4-byte command header ([`CMD_HDR_LEN`], `struct wmi_cmd_hdr`: the
//!   command id in the low 24 bits, the top byte `WMI_CMD_HDR_PLT_PRIV` which ath10k leaves 0; there is no
//!   sequence number) followed by TLVs. [`frame`] builds one, [`rx`] splits an event.
//! * Ids: [`cmd`], [`event`], [`tag`]. [`EventId`] classifies events as `ath10k_wmi_tlv_op_rx` dispatches them.
//! * Event parsers: [`parse_service_ready`] (`ath10k_wmi_tlv_op_pull_svc_rdy_ev`, with its ABI check),
//!   [`parse_ready`] (`..._pull_rdy_ev`, with the MAC address), [`parse_service_available`],
//!   [`parse_scan_event`], [`parse_vdev_start_response`].
//! * Command encoders return the message body (what `ath10k_wmi_tlv_op_gen_*` leaves in the skb, zero padded to a
//!   multiple of 4); wrap it with [`frame`]. [`gen_init`] covers `wmi_tlv_op_gen_init` with the WCN3990 resource
//!   configuration ([`ResourceConfig`]) and host memory chunks, [`gen_start_scan`], [`gen_stop_scan`],
//!   [`gen_pdev_set_regdomain`], [`gen_vdev_create`], [`gen_vdev_start`], [`gen_vdev_up`], [`gen_vdev_down`],
//!   [`gen_vdev_delete`], [`gen_peer_create`], [`gen_peer_delete`] and [`gen_mgmt_tx_send`].
//!
//! * For a station ([`crate::driver`]): [`gen_pdev_set_param`], [`gen_vdev_stop`], [`gen_vdev_set_param`],
//!   [`gen_vdev_install_key`], [`gen_peer_set_param`], [`gen_peer_assoc`], [`gen_scan_chan_list`], [`gen_set_psmode`],
//!   [`gen_set_sta_ps`], [`gen_sta_keepalive_disable`], [`gen_scan_prob_req_oui`], [`gen_adaptive_qcs`] and the events
//!   [`parse_mgmt_rx`], [`parse_mgmt_tx_compl`], [`parse_mgmt_tx_bundle_compl`], [`parse_peer_kickout`],
//!   [`parse_roam`]. Service numbers are [`svc`]; [`svc_is_enabled`] is the bitmap test, four bits per word.
//!
//! Not covered: the other WMI-TLV commands and events, and the regulatory lookup `ath10k_wmi_put_wmi_channel` does for 80+80/160 MHz channels (the caller says whether the
//! second segment is a radar channel).

use alloc::vec::Vec;

/// `sizeof(struct wmi_tlv)`.
pub const TLV_HDR_LEN: usize = 4;
/// `sizeof(struct wmi_cmd_hdr)`.
pub const CMD_HDR_LEN: usize = 4;
/// `WMI_CMD_HDR_CMD_ID_MASK`.
pub const CMD_ID_MASK: u32 = 0x00ff_ffff;
/// `WMI_MAX_MEM_REQS`.
pub const MAX_MEM_REQS: usize = 16;
/// `WLAN_SCAN_PARAMS_MAX_*`.
pub const SCAN_MAX_SSID: usize = 16;
pub const SCAN_MAX_BSSID: usize = 4;
pub const SCAN_MAX_IE_LEN: usize = 256;
/// `ARRAY_SIZE(wmi_start_scan_arg.channels)`.
pub const SCAN_MAX_CHANNELS: usize = 64;
/// `WMI_HOST_SCAN_REQ_ID_PREFIX` and `WMI_HOST_SCAN_REQUESTOR_ID_PREFIX`.
pub const SCAN_ID_PREFIX: u32 = 0xa000;
/// `WMI_SCAN_FILTER_PROBE_REQ`: the TLV firmware reads this flag inverted, so the encoder flips it.
pub const SCAN_FILTER_PROBE_REQ: u32 = 0x20;
/// `WMI_TLV_MGMT_TX_FRAME_MAX_LEN`: how much of a management frame travels inline.
pub const MGMT_TX_FRAME_MAX_LEN: usize = 64;
/// `WMI_TLV_FLAG_MGMT_BUNDLE_TX_COMPL` and `WMI_RSRC_CFG_FLAG_TX_ACK_RSSI` (bits of `host_capab`).
pub const FLAG_MGMT_BUNDLE_TX_COMPL: u32 = 1 << 9;
pub const RSRC_CFG_FLAG_TX_ACK_RSSI: u32 = 1 << 18;

/// The ABI the host speaks (`WMI_TLV_ABI_VER*`); the service ready event must carry the same.
pub const ABI_VER0: u32 = 0x0100_0000;
pub const ABI_VER1: u32 = 53;
pub const ABI_VER_NS0: u32 = 0x5f41_4351;
pub const ABI_VER_NS1: u32 = 0x0000_4c4d;
pub const ABI_VER_NS2: u32 = 0;
pub const ABI_VER_NS3: u32 = 0;

/// WCN3990 values from `hw_params` (`core.c`) and `hw.h` that `gen_init` uses.
pub mod wcn3990 {
    /// `TARGET_HL_TLV_NUM_PEERS`.
    pub const NUM_PEERS: u32 = 33;
    /// `TARGET_HL_TLV_AST_SKID_LIMIT`.
    pub const AST_SKID_LIMIT: u32 = 16;
    /// `TARGET_HL_TLV_NUM_WDS_ENTRIES`.
    pub const NUM_WDS_ENTRIES: u32 = 2;
    /// `TARGET_TLV_NUM_VDEVS`.
    pub const NUM_VDEVS: u32 = 4;
    /// `TARGET_TLV_NUM_TDLS_VDEVS`.
    pub const NUM_TDLS_VDEVS: u32 = 1;
    /// `TARGET_TLV_NUM_MSDU_DESC`, the usual `htt.max_num_pending_tx`.
    pub const NUM_MSDU_DESC: u32 = 0x420;
}

/// WMI-TLV tags (`enum wmi_tlv_tag`). Only the ones used here.
pub mod tag {
    pub const ARRAY_UINT32: u16 = 16;
    pub const ARRAY_BYTE: u16 = 17;
    pub const ARRAY_STRUCT: u16 = 18;
    pub const ARRAY_FIXED_STRUCT: u16 = 19;
    pub const SERVICE_READY_EVENT: u16 = 32;
    pub const HAL_REG_CAPABILITIES: u16 = 33;
    pub const WLAN_HOST_MEM_REQ: u16 = 34;
    pub const READY_EVENT: u16 = 35;
    pub const SCAN_EVENT: u16 = 36;
    pub const CHAN_INFO_EVENT: u16 = 38;
    pub const VDEV_START_RESPONSE_EVENT: u16 = 40;
    pub const PEER_STA_KICKOUT_EVENT: u16 = 43;
    pub const MGMT_RX_HDR: u16 = 44;
    pub const ROAM_EVENT: u16 = 48;
    pub const WOW_EVENT_INFO: u16 = 49;
    pub const HOST_SWBA_EVENT: u16 = 67;
    pub const TIM_INFO: u16 = 68;
    pub const P2P_NOA_INFO: u16 = 69;
    pub const SCAN_CHAN_LIST_CMD: u16 = 79;
    pub const PDEV_SET_PARAM_CMD: u16 = 82;
    pub const VDEV_STOP_CMD: u16 = 93;
    pub const VDEV_SET_PARAM_CMD: u16 = 95;
    pub const VDEV_SET_WMM_PARAMS_CMD: u16 = 199;
    pub const VDEV_INSTALL_KEY_CMD: u16 = 96;
    pub const PEER_SET_PARAM_CMD: u16 = 100;
    pub const PEER_ASSOC_COMPLETE_CMD: u16 = 101;
    pub const VHT_RATE_SET: u16 = 102;
    pub const STA_POWERSAVE_MODE_CMD: u16 = 108;
    pub const STA_POWERSAVE_PARAM_CMD: u16 = 109;
    pub const STA_KEEPALIVE_CMD: u16 = 127;
    pub const STA_KEEPALVE_ARP_RESPONSE: u16 = 128;
    pub const STA_UAPSD_AUTO_TRIG_CMD: u16 = 0x79;
    pub const STA_UAPSD_AUTO_TRIG_PARAM: u16 = 0x7a;
    pub const RESMGR_ADAPTIVE_OCS_CMD: u16 = 207;
    pub const SCAN_PROB_REQ_OUI_CMD: u16 = 349;
    pub const MGMT_TX_COMPL_EVENT: u16 = 423;
    pub const MGMT_TX_COMPL_BUNDLE_EVENT: u16 = 552;
    pub const INIT_CMD: u16 = 74;
    pub const RESOURCE_CONFIG: u16 = 75;
    pub const WLAN_HOST_MEMORY_CHUNK: u16 = 76;
    pub const START_SCAN_CMD: u16 = 77;
    pub const STOP_SCAN_CMD: u16 = 78;
    pub const CHANNEL: u16 = 80;
    pub const PDEV_SET_REGDOMAIN_CMD: u16 = 81;
    pub const VDEV_CREATE_CMD: u16 = 86;
    pub const VDEV_DELETE_CMD: u16 = 87;
    pub const VDEV_START_REQUEST_CMD: u16 = 88;
    pub const VDEV_UP_CMD: u16 = 92;
    pub const VDEV_DOWN_CMD: u16 = 94;
    pub const PEER_CREATE_CMD: u16 = 97;
    pub const PEER_DELETE_CMD: u16 = 98;
    pub const OFFLOAD_BCN_TX_STATUS_EVENT: u16 = 222;
    pub const P2P_NOA_EVENT: u16 = 223;
    pub const TX_PAUSE_EVENT: u16 = 230;
    pub const SERVICE_AVAILABLE_EVENT: u16 = 559;
    pub const DIAG_DATA_CONTAINER_EVENT: u16 = 260;
    pub const MGMT_TX_CMD: u16 = 422;
}

/// `wmi_tlv_cmd_id` values (`WMI_TLV_CMD(group) = group << 12 | 1`, groups counted from 3).
pub mod cmd {
    pub const INIT: u32 = 0x0001;
    pub const START_SCAN: u32 = 0x3001;
    pub const STOP_SCAN: u32 = 0x3002;
    pub const PDEV_SET_REGDOMAIN: u32 = 0x4001;
    pub const PDEV_SET_PARAM: u32 = 0x4003;
    pub const VDEV_CREATE: u32 = 0x5001;
    pub const VDEV_DELETE: u32 = 0x5002;
    pub const VDEV_START_REQUEST: u32 = 0x5003;
    pub const VDEV_UP: u32 = 0x5005;
    pub const VDEV_STOP: u32 = 0x5006;
    pub const VDEV_DOWN: u32 = 0x5007;
    pub const VDEV_INSTALL_KEY: u32 = 0x5009;
    pub const PEER_CREATE: u32 = 0x6001;
    pub const PEER_DELETE: u32 = 0x6002;
    pub const PEER_ASSOC: u32 = 0x6005;
    /// `WMI_TLV_MGMT_TX_CMDID`: the inline form, absent from Linux's `wmi_tlv_ops`.
    pub const MGMT_TX: u32 = 0x7006;
    /// `WMI_TLV_MGMT_TX_SEND_CMD` (`cmd_map.mgmt_tx_send_cmdid`): by reference, what [`super::gen_mgmt_tx_send`] goes with.
    pub const MGMT_TX_SEND: u32 = 0x7008;
    pub const SCAN_CHAN_LIST: u32 = 0x3003;
    pub const SCAN_PROB_REQ_OUI: u32 = 0x3006;
    pub const VDEV_SET_PARAM: u32 = 0x5008;
    pub const VDEV_SET_WMM_PARAMS: u32 = 0x500d;
    pub const PEER_SET_PARAM: u32 = 0x6004;
    pub const STA_POWERSAVE_MODE: u32 = 0x9001;
    pub const STA_POWERSAVE_PARAM: u32 = 0x9002;
    /// `WMI_TLV_STA_UAPSD_AUTO_TRIG_CMDID`.
    pub const STA_UAPSD_AUTO_TRIG: u32 = 0x1c004;
    pub const STA_KEEPALIVE: u32 = 0x1c005;
    pub const RESMGR_ADAPTIVE_OCS: u32 = 0x21001;
}

/// `enum wmi_tlv_service` values the driver looks at. Numbers from 128 up live in the extended map of the service
/// available event.
pub mod svc {
    pub const BEACON_OFFLOAD: u32 = 0;
    pub const STA_KEEP_ALIVE: u32 = 29;
    pub const BURST: u32 = 37;
    pub const ADAPTIVE_OCS: u32 = 39;
    pub const RX_FULL_REORDER: u32 = 65;
    pub const MGMT_TX_WMI: u32 = 76;
    pub const STA_UAPSD_BASIC_AUTO_TRIG: u32 = 27;
    pub const STA_UAPSD_VAR_AUTO_TRIG: u32 = 28;
    pub const SPOOF_MAC_SUPPORT: u32 = 155;
    pub const TX_DATA_MGMT_ACK_RSSI: u32 = 174;
    pub const SUPPORT_EXTEND_ADDRESS: u32 = 179;
    /// `WMI_TLV_MAX_SERVICE`: what the extended numbers are counted from.
    pub const MAX_SERVICE: u32 = 128;
    /// `WMI_TLV_MAX_EXT_SERVICE`.
    pub const MAX_EXT_SERVICE: u32 = 256;
}

/// `enum wmi_tlv_pdev_param`, the ones a station sets (`ath10k_start`).
pub mod pdev_param {
    pub const TX_CHAIN_MASK: u32 = 1;
    pub const RX_CHAIN_MASK: u32 = 2;
    pub const TXPOWER_LIMIT2G: u32 = 3;
    pub const TXPOWER_LIMIT5G: u32 = 4;
    pub const DYNAMIC_BW: u32 = 10;
    pub const PMF_QOS: u32 = 34;
    pub const ARP_AC_OVERRIDE: u32 = 35;
    pub const ANI_ENABLE: u32 = 37;
    pub const IDLE_PS_CONFIG: u32 = 44;
    pub const BURST_ENABLE: u32 = 48;
    pub const PEER_STATS_INFO_ENABLE: u32 = 139;
}

/// `enum wmi_tlv_vdev_param`.
pub mod vdev_param {
    pub const RTS_THRESHOLD: u32 = 1;
    pub const BEACON_INTERVAL: u32 = 3;
    pub const SLOT_TIME: u32 = 7;
    pub const PREAMBLE: u32 = 8;
    pub const DTIM_PERIOD: u32 = 13;
    pub const NSS: u32 = 34;
    pub const TXBF: u32 = 45;
    pub const TX_ENCAP_TYPE: u32 = 48;
    /// `WMI_VDEV_PARAM_TXBF_*`, the value bits of [`TXBF`].
    pub const TXBF_SU_TX_BFEE: u32 = 1 << 0;
    pub const TXBF_MU_TX_BFEE: u32 = 1 << 1;
    pub const TXBF_SU_TX_BFER: u32 = 1 << 2;
    pub const TXBF_MU_TX_BFER: u32 = 1 << 3;
}

/// `enum wmi_tlv_peer_param`.
pub mod peer_param {
    pub const SMPS_STATE: u32 = 1;
    pub const AUTHORIZE: u32 = 3;
    pub const DUMMY_VAR: u32 = 0xff;
}

/// `WMI_TLV_PEER_*`: the values of `wmi_tlv_peer_flags_map`, what goes in `peer_flags`.
pub mod peer_flags {
    pub const AUTH: u32 = 0x0000_0001;
    pub const QOS: u32 = 0x0000_0002;
    pub const NEED_PTK_4_WAY: u32 = 0x0000_0004;
    pub const NEED_GTK_2_WAY: u32 = 0x0000_0010;
    pub const HT: u32 = 0x0000_1000;
    pub const BW40: u32 = 0x0000_2000;
    pub const STBC: u32 = 0x0000_8000;
    pub const LDPC: u32 = 0x0001_0000;
    pub const VHT: u32 = 0x0200_0000;
    pub const BW80: u32 = 0x0400_0000;
    pub const PMF: u32 = 0x0800_0000;
    pub const BW160: u32 = 0x2000_0000;
}

/// `WMI_RC_*`: what goes in `peer_rate_caps`.
pub mod rate_caps {
    pub const DS: u32 = 0x01;
    pub const CW40: u32 = 0x02;
    pub const SGI: u32 = 0x04;
    pub const HT: u32 = 0x08;
    pub const TX_STBC: u32 = 0x20;
    pub const RX_STBC_SHIFT: u32 = 6;
    pub const TS: u32 = 0x200;
}

/// `WMI_KEY_*`.
pub mod key_flags {
    pub const PAIRWISE: u32 = 0;
    pub const GROUP: u32 = 1;
}

/// `WMI_TLV_CIPHER_*`, through `wmi_tlv_key_cipher_suites`.
pub mod cipher {
    pub const NONE: u32 = 0;
    pub const TKIP: u32 = 2;
    pub const AES_CCM: u32 = 4;
}

/// `enum wmi_sta_powersave_param` and the values `ath10k_add_interface` writes.
/// `WMI_STA_PS_PARAM_INACTIVITY_TIME` is the value of `PARAM_INACTIVITY_TIME`.
pub mod sta_ps {
    pub const PARAM_RX_WAKE_POLICY: u32 = 0;
    pub const PARAM_TX_WAKE_THRESHOLD: u32 = 1;
    pub const PARAM_PSPOLL_COUNT: u32 = 2;
    pub const PARAM_INACTIVITY_TIME: u32 = 3;
    pub const RX_WAKE_POLICY_WAKE: u32 = 0;
    pub const TX_WAKE_THRESHOLD_ALWAYS: u32 = 1;
    pub const PSPOLL_COUNT_NO_MAX: u32 = 0;
    pub const PARAM_UAPSD: u32 = 4;
    pub const MODE_DISABLED: u32 = 0;
    pub const MODE_ENABLED: u32 = 1;
}

/// `WMI_RX_STATUS_*`, the status word of a management rx event.
pub mod rx_status {
    pub const ERR_CRC: u32 = 0x01;
    pub const ERR_DECRYPT: u32 = 0x08;
    pub const ERR_MIC: u32 = 0x10;
    pub const ERR_KEY_CACHE_MISS: u32 = 0x20;
    pub const EXT_INFO: u32 = 0x40;
}

/// `WMI_ROAM_REASON_BEACON_MISS`.
pub const ROAM_REASON_BEACON_MISS: u32 = 2;
/// `WMI_VDEV_SLOT_TIME_*` and `WMI_VDEV_PREAMBLE_*`.
pub const VDEV_SLOT_TIME_LONG: u32 = 1;
pub const VDEV_SLOT_TIME_SHORT: u32 = 2;
pub const VDEV_PREAMBLE_LONG: u32 = 1;
pub const VDEV_PREAMBLE_SHORT: u32 = 2;
/// `WMI_MAX_KEY_INDEX`.
pub const MAX_KEY_INDEX: u32 = 3;
/// `MAX_SUPPORTED_RATES`.
pub const MAX_SUPPORTED_RATES: usize = 128;

/// One `le32` word of a bitmap, `None` past its end (the C reads on).
fn map_word(map: &[u8], word: usize) -> Option<u32> {
    let w = map.get(word.checked_mul(4)?..word.checked_mul(4)?.checked_add(4)?)?;
    Some(u32::from_le_bytes([w[0], w[1], w[2], w[3]]))
}

/// `SVCMAP(x, y, len)` of `wmi-tlv.h`: `WMI_SERVICE_IS_ENABLED` (below `len`: the target uses only the low four bits
/// of each word, so `x` is bit `x % 4` of word `x / 4`) or `WMI_TLV_EXT_SERVICE_IS_ENABLED` (from `len` up to 256: bit
/// `(x - len) % 32` of word `(x - len) / 32`). For the service ready map `len` is the map's length in bytes; for the
/// service available map it is [`svc::MAX_SERVICE`]. A word past the end of the map reads as clear.
pub fn svc_is_enabled(map: &[u8], x: u32, len: u32) -> bool {
    if x < len {
        return map_word(map, (x / 4) as usize).is_some_and(|w| w & (1 << (x % 4)) != 0);
    }
    if x < svc::MAX_EXT_SERVICE {
        let n = x - len;
        return map_word(map, (n / 32) as usize).is_some_and(|w| w & (1 << (n % 32)) != 0);
    }
    false
}

/// `wmi_tlv_event_id` values.
pub mod event {
    pub const SERVICE_READY: u32 = 0x0001;
    pub const READY: u32 = 0x0002;
    pub const SERVICE_AVAILABLE: u32 = 0x0003;
    pub const SCAN: u32 = 0x3001;
    pub const CHAN_INFO: u32 = 0x4002;
    pub const TX_PAUSE: u32 = 0x4005;
    pub const VDEV_START_RESP: u32 = 0x5001;
    pub const VDEV_STOPPED: u32 = 0x5002;
    pub const VDEV_DELETE_RESP: u32 = 0x5006;
    pub const PEER_STA_KICKOUT: u32 = 0x6001;
    pub const PEER_DELETE_RESP: u32 = 0x6007;
    pub const MGMT_RX: u32 = 0x7001;
    pub const MGMT_TX_COMPLETION: u32 = 0x7006;
    pub const MGMT_TX_BUNDLE_COMPLETION: u32 = 0x7007;
    pub const VDEV_INSTALL_KEY_COMPLETE: u32 = 0x5003;
    pub const ROAM: u32 = 0xb001;
}

/// Event classes of `ath10k_wmi_tlv_op_rx` that this crate knows by name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventId {
    ServiceReady,
    Ready,
    ServiceAvailable,
    Scan,
    ChanInfo,
    TxPause,
    VdevStartResp,
    VdevStopped,
    VdevDeleteResp,
    PeerStaKickout,
    PeerDeleteResp,
    MgmtRx,
    MgmtTxCompletion,
    MgmtTxBundleCompletion,
    VdevInstallKeyComplete,
    Roam,
    Other(u32),
}

impl EventId {
    pub fn from_id(id: u32) -> EventId {
        match id {
            event::SERVICE_READY => EventId::ServiceReady,
            event::READY => EventId::Ready,
            event::SERVICE_AVAILABLE => EventId::ServiceAvailable,
            event::SCAN => EventId::Scan,
            event::CHAN_INFO => EventId::ChanInfo,
            event::TX_PAUSE => EventId::TxPause,
            event::VDEV_START_RESP => EventId::VdevStartResp,
            event::VDEV_STOPPED => EventId::VdevStopped,
            event::VDEV_DELETE_RESP => EventId::VdevDeleteResp,
            event::PEER_STA_KICKOUT => EventId::PeerStaKickout,
            event::PEER_DELETE_RESP => EventId::PeerDeleteResp,
            event::MGMT_RX => EventId::MgmtRx,
            event::MGMT_TX_COMPLETION => EventId::MgmtTxCompletion,
            event::MGMT_TX_BUNDLE_COMPLETION => EventId::MgmtTxBundleCompletion,
            event::VDEV_INSTALL_KEY_COMPLETE => EventId::VdevInstallKeyComplete,
            event::ROAM => EventId::Roam,
            other => EventId::Other(other),
        }
    }
}

/// Errors, with the errno the C returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WmiError {
    /// Malformed TLV stream or argument (`-EINVAL`).
    Invalid,
    /// A required TLV is missing or has the wrong tag (`-EPROTO`).
    Protocol,
    /// The firmware speaks another ABI (`-EOPNOTSUPP`).
    Unsupported,
    /// More memory requests than the host keeps (`-ENOMEM`).
    TooMany,
}

impl WmiError {
    pub const fn errno(self) -> i32 {
        match self {
            WmiError::Invalid => -22,
            WmiError::Protocol => -71,
            WmiError::Unsupported => -95,
            WmiError::TooMany => -12,
        }
    }
}

/// `wmi_tlv_policies[]`: minimum value length per tag (the sizes of the structs the tags carry). Tags not listed
/// have no minimum.
pub static POLICY: [(u16, usize); 18] = [
    (tag::SERVICE_READY_EVENT, 104),
    (tag::HAL_REG_CAPABILITIES, 36),
    (tag::WLAN_HOST_MEM_REQ, 16),
    (tag::READY_EVENT, 36),
    (tag::SCAN_EVENT, 24),
    (tag::CHAN_INFO_EVENT, 24),
    (tag::VDEV_START_RESPONSE_EVENT, 16),
    (tag::PEER_STA_KICKOUT_EVENT, 8),
    (tag::MGMT_RX_HDR, 40),
    (tag::ROAM_EVENT, 12),
    (tag::WOW_EVENT_INFO, 16),
    (tag::HOST_SWBA_EVENT, 4),
    (tag::TIM_INFO, 32),
    (tag::P2P_NOA_INFO, 68),
    (tag::OFFLOAD_BCN_TX_STATUS_EVENT, 8),
    (tag::P2P_NOA_EVENT, 4),
    (tag::TX_PAUSE_EVENT, 20),
    (tag::DIAG_DATA_CONTAINER_EVENT, 4),
];

fn policy_min_len(t: u16) -> usize {
    POLICY.iter().find(|(p, _)| *p == t).map_or(0, |(_, m)| *m)
}

/// One TLV: tag, value bytes, and where the value starts in the buffer that was walked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tlv<'a> {
    pub tag: u16,
    pub value: &'a [u8],
    pub offset: usize,
}

/// Iterator of `ath10k_wmi_tlv_iter`. After the first error it ends.
pub struct TlvIter<'a> {
    buf: &'a [u8],
    pos: usize,
    failed: bool,
}

/// Walk the TLVs of `buf`. Errors are [`WmiError::Invalid`] for a header or value that does not fit and for a
/// value below the tag's policy minimum.
pub fn iter(buf: &[u8]) -> TlvIter<'_> {
    TlvIter {
        buf,
        pos: 0,
        failed: false,
    }
}

impl<'a> Iterator for TlvIter<'a> {
    type Item = Result<Tlv<'a>, WmiError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed || self.pos >= self.buf.len() {
            return None;
        }
        let rest = &self.buf[self.pos..];
        if rest.len() < TLV_HDR_LEN {
            self.failed = true;
            return Some(Err(WmiError::Invalid));
        }
        let len = usize::from(u16::from_le_bytes([rest[0], rest[1]]));
        let t = u16::from_le_bytes([rest[2], rest[3]]);
        if len > rest.len() - TLV_HDR_LEN {
            self.failed = true;
            return Some(Err(WmiError::Invalid));
        }
        let min = policy_min_len(t);
        if min != 0 && min > len {
            self.failed = true;
            return Some(Err(WmiError::Invalid));
        }
        let off = self.pos + TLV_HDR_LEN;
        self.pos = off + len;
        Some(Ok(Tlv {
            tag: t,
            value: &self.buf[off..off + len],
            offset: off,
        }))
    }
}

fn le32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

/// Prefix `payload` with the 4-byte WMI command header, as `ath10k_wmi_cmd_send_nowait` does.
pub fn frame(cmd_id: u32, payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(CMD_HDR_LEN + payload.len());
    v.extend_from_slice(&cmd_id.to_le_bytes());
    v.extend_from_slice(payload);
    v
}

/// `ath10k_wmi_tlv_op_rx` up to the dispatch: the event id (low 24 bits of the header) and the TLVs after it.
pub fn rx(buf: &[u8]) -> Result<(EventId, &[u8]), WmiError> {
    if buf.len() < CMD_HDR_LEN {
        return Err(WmiError::Invalid);
    }
    let id = le32(buf, 0) & CMD_ID_MASK;
    Ok((EventId::from_id(id), &buf[CMD_HDR_LEN..]))
}

/// The last TLV with `tag` (the C stores into a table indexed by tag, so a later one replaces an earlier one).
fn find_last(buf: &[u8], t: u16) -> Result<Option<&[u8]>, WmiError> {
    let mut found = None;
    for tlv in iter(buf) {
        let tlv = tlv?;
        if tlv.tag == t {
            found = Some(tlv.value);
        }
    }
    Ok(found)
}

// ---- event parsers --------------------------------------------------------------------------------------------

/// `struct wlan_host_mem_req`: a memory area the firmware asks the host to provide.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemReq {
    pub req_id: u32,
    pub unit_size: u32,
    pub num_unit_info: u32,
    pub num_units: u32,
}

/// The fields `ath10k_wmi_tlv_op_pull_svc_rdy_ev` hands to the generic code (`struct wmi_svc_rdy_ev_arg`), with
/// the raw service bitmap and the memory requests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceReady<'a> {
    pub min_tx_power: u32,
    pub max_tx_power: u32,
    pub ht_cap: u32,
    pub vht_cap: u32,
    pub vht_supp_mcs: u32,
    /// `abi.abi_ver0` and `abi.abi_ver1`: the firmware's software version.
    pub sw_ver0: u32,
    pub sw_ver1: u32,
    pub fw_build: u32,
    pub phy_capab: u32,
    pub num_rf_chains: u32,
    pub eeprom_rd: u32,
    pub num_mem_reqs: u32,
    pub low_2ghz_chan: u32,
    pub high_2ghz_chan: u32,
    pub low_5ghz_chan: u32,
    pub high_5ghz_chan: u32,
    pub sys_cap_info: u32,
    /// The service bitmap as the firmware sent it (little-endian words, byte length not necessarily a multiple of 4).
    pub service_map: &'a [u8],
    pub mem_reqs: Vec<MemReq>,
}

impl ServiceReady<'_> {
    /// Whether the firmware advertises service number `x` (`wmi_tlv_svc_map`, see [`svc_is_enabled`]).
    pub fn service_enabled(&self, x: u32) -> bool {
        svc_is_enabled(self.service_map, x, self.service_map.len() as u32)
    }
}

/// `ath10k_wmi_tlv_op_pull_svc_rdy_ev`. Order of checks as in the C: the TLV walk, the presence of the service
/// ready struct, the register capabilities, the service bitmap (first `ARRAY_UINT32`) and the memory request array
/// (`ARRAY_STRUCT`), then the ABI, then the nested walk of the memory requests (each must be a
/// `WLAN_HOST_MEM_REQ`, at most 16).
pub fn parse_service_ready(buf: &[u8]) -> Result<ServiceReady<'_>, WmiError> {
    let mut ev: Option<&[u8]> = None;
    let mut reg: Option<&[u8]> = None;
    let mut svc_bmap: Option<&[u8]> = None;
    let mut mem_reqs: Option<&[u8]> = None;
    let mut svc_bmap_done = false;
    for tlv in iter(buf) {
        let tlv = tlv?;
        match tlv.tag {
            tag::SERVICE_READY_EVENT => ev = Some(tlv.value),
            tag::HAL_REG_CAPABILITIES => reg = Some(tlv.value),
            tag::ARRAY_STRUCT => mem_reqs = Some(tlv.value),
            // the first array is the service bitmap; a second one is the DBS hardware mode list, which we ignore
            tag::ARRAY_UINT32 if !svc_bmap_done => {
                svc_bmap_done = true;
                svc_bmap = Some(tlv.value);
            }
            _ => {}
        }
    }
    let (ev, reg, svc_bmap, mem_reqs) = match (ev, reg, svc_bmap, mem_reqs) {
        (Some(a), Some(b), Some(c), Some(d)) => (a, b, c, d),
        _ => return Err(WmiError::Protocol),
    };
    // struct wmi_tlv_svc_rdy_ev: fw_build_vers@0, abi@4 (6 words), phy_capability@28, max_frag_entry@32,
    // num_rf_chains@36, ht_cap_info@40, vht_cap_info@44, vht_supp_mcs@48, hw_min_tx_power@52, hw_max_tx_power@56,
    // sys_cap_info@60, min_pkt_size_enable@64, max_bcn_ie_size@68, num_mem_reqs@72
    if le32(ev, 4) != ABI_VER0
        || le32(ev, 12) != ABI_VER_NS0
        || le32(ev, 16) != ABI_VER_NS1
        || le32(ev, 20) != ABI_VER_NS2
        || le32(ev, 24) != ABI_VER_NS3
    {
        return Err(WmiError::Unsupported);
    }
    let mut reqs = Vec::new();
    for tlv in iter(mem_reqs) {
        let tlv = tlv?;
        if tlv.tag != tag::WLAN_HOST_MEM_REQ {
            return Err(WmiError::Protocol);
        }
        if reqs.len() >= MAX_MEM_REQS {
            return Err(WmiError::TooMany);
        }
        reqs.push(MemReq {
            req_id: le32(tlv.value, 0),
            unit_size: le32(tlv.value, 4),
            num_unit_info: le32(tlv.value, 8),
            num_units: le32(tlv.value, 12),
        });
    }
    Ok(ServiceReady {
        min_tx_power: le32(ev, 52),
        max_tx_power: le32(ev, 56),
        ht_cap: le32(ev, 40),
        vht_cap: le32(ev, 44),
        vht_supp_mcs: le32(ev, 48),
        sw_ver0: le32(ev, 4),
        sw_ver1: le32(ev, 8),
        fw_build: le32(ev, 0),
        phy_capab: le32(ev, 28),
        num_rf_chains: le32(ev, 36),
        // hal_reg_capabilities: eeprom_rd@0, eeprom_rd_ext@4, regcap1@8, regcap2@12, wireless_modes@16,
        // low_2ghz@20, high_2ghz@24, low_5ghz@28, high_5ghz@32
        eeprom_rd: le32(reg, 0),
        num_mem_reqs: le32(ev, 72),
        low_2ghz_chan: le32(reg, 20),
        high_2ghz_chan: le32(reg, 24),
        low_5ghz_chan: le32(reg, 28),
        high_5ghz_chan: le32(reg, 32),
        sys_cap_info: le32(ev, 60),
        service_map: svc_bmap,
        mem_reqs: reqs,
    })
}

/// `struct wmi_rdy_ev_arg`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ready {
    pub sw_version: u32,
    pub abi_version: u32,
    pub status: u32,
    pub mac_addr: [u8; 6],
}

/// `ath10k_wmi_tlv_op_pull_rdy_ev`: the `READY_EVENT` struct is `abi` (24 bytes), `mac_addr` (a `wmi_mac_addr`,
/// 8 bytes of which the first 6 are the address) and `status`.
pub fn parse_ready(buf: &[u8]) -> Result<Ready, WmiError> {
    let ev = find_last(buf, tag::READY_EVENT)?.ok_or(WmiError::Protocol)?;
    let mut mac_addr = [0u8; 6];
    mac_addr.copy_from_slice(&ev[24..30]);
    Ok(Ready {
        sw_version: le32(ev, 0),
        abi_version: le32(ev, 4),
        status: le32(ev, 32),
        mac_addr,
    })
}

/// `struct wmi_svc_avail_ev_arg`: the extended service bitmap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServiceAvailable<'a> {
    /// The length word the firmware put in front of the bitmap (not checked against the TLV, as in the C).
    pub service_map_ext_len: u32,
    /// The bytes after that word, up to the end of the TLV.
    pub service_map_ext: &'a [u8],
}

/// `ath10k_wmi_tlv_op_pull_svc_avail`. `Ok(None)` when there is no such TLV (the C leaves `valid` false). A
/// TLV shorter than the length word is refused here; the C would read past it.
pub fn parse_service_available(buf: &[u8]) -> Result<Option<ServiceAvailable<'_>>, WmiError> {
    let mut out = None;
    for tlv in iter(buf) {
        let tlv = tlv?;
        if tlv.tag == tag::SERVICE_AVAILABLE_EVENT {
            if tlv.value.len() < 4 {
                return Err(WmiError::Invalid);
            }
            out = Some(ServiceAvailable {
                service_map_ext_len: le32(tlv.value, 0),
                service_map_ext: &tlv.value[4..],
            });
        }
    }
    Ok(out)
}

/// `struct wmi_scan_event` (`WMI_SCAN_EVENT_*`, `WMI_SCAN_REASON_*`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScanEvent {
    pub event_type: u32,
    pub reason: u32,
    pub channel_freq: u32,
    pub scan_req_id: u32,
    pub scan_id: u32,
    pub vdev_id: u32,
}

/// `WMI_SCAN_EVENT_*` bits.
pub mod scan_event {
    pub const STARTED: u32 = 1 << 0;
    pub const COMPLETED: u32 = 1 << 1;
    pub const BSS_CHANNEL: u32 = 1 << 2;
    pub const FOREIGN_CHANNEL: u32 = 1 << 3;
    pub const DEQUEUED: u32 = 1 << 4;
    pub const PREEMPTED: u32 = 1 << 5;
    pub const START_FAILED: u32 = 1 << 6;
    pub const RESTARTED: u32 = 1 << 7;
    pub const FOREIGN_CHANNEL_EXIT: u32 = 1 << 8;
}

/// `ath10k_wmi_tlv_op_pull_scan_ev`.
pub fn parse_scan_event(buf: &[u8]) -> Result<ScanEvent, WmiError> {
    let ev = find_last(buf, tag::SCAN_EVENT)?.ok_or(WmiError::Protocol)?;
    Ok(ScanEvent {
        event_type: le32(ev, 0),
        reason: le32(ev, 4),
        channel_freq: le32(ev, 8),
        scan_req_id: le32(ev, 12),
        scan_id: le32(ev, 16),
        vdev_id: le32(ev, 20),
    })
}

/// `struct wmi_vdev_start_response_event`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VdevStartResponse {
    pub vdev_id: u32,
    pub req_id: u32,
    pub resp_type: u32,
    pub status: u32,
}

/// `ath10k_wmi_tlv_op_pull_vdev_start_ev`.
pub fn parse_vdev_start_response(buf: &[u8]) -> Result<VdevStartResponse, WmiError> {
    let ev = find_last(buf, tag::VDEV_START_RESPONSE_EVENT)?.ok_or(WmiError::Protocol)?;
    Ok(VdevStartResponse {
        vdev_id: le32(ev, 0),
        req_id: le32(ev, 4),
        resp_type: le32(ev, 8),
        status: le32(ev, 12),
    })
}

// ---- command encoders -----------------------------------------------------------------------------------------

struct Out(Vec<u8>);

impl Out {
    fn new() -> Out {
        Out(Vec::new())
    }
    fn tlv(&mut self, t: u16, len: usize) {
        self.0.extend_from_slice(&(len as u16).to_le_bytes());
        self.0.extend_from_slice(&t.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn bytes(&mut self, b: &[u8]) {
        self.0.extend_from_slice(b);
    }
    fn zeros(&mut self, n: usize) {
        self.0.resize(self.0.len() + n, 0);
    }
    /// A `struct wmi_mac_addr`: the six address bytes in an 8-byte union.
    fn mac(&mut self, a: &[u8; 6]) {
        self.bytes(a);
        self.zeros(2);
    }
    /// `ath10k_wmi_alloc_skb` rounds the message up to a multiple of 4 and zero fills.
    fn finish(mut self) -> Vec<u8> {
        while !self.0.len().is_multiple_of(4) {
            self.0.push(0);
        }
        self.0
    }
}

/// `struct wmi_tlv_resource_config`, in wire order. Build the WCN3990 one with [`ResourceConfig::wcn3990`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResourceConfig {
    pub num_vdevs: u32,
    pub num_peers: u32,
    pub num_offload_peers: u32,
    pub num_offload_reorder_bufs: u32,
    pub num_peer_keys: u32,
    pub num_tids: u32,
    pub ast_skid_limit: u32,
    pub tx_chain_mask: u32,
    pub rx_chain_mask: u32,
    pub rx_timeout_pri: [u32; 4],
    pub rx_decap_mode: u32,
    pub scan_max_pending_reqs: u32,
    pub bmiss_offload_max_vdev: u32,
    pub roam_offload_max_vdev: u32,
    pub roam_offload_max_ap_profiles: u32,
    pub num_mcast_groups: u32,
    pub num_mcast_table_elems: u32,
    pub mcast2ucast_mode: u32,
    pub tx_dbg_log_size: u32,
    pub num_wds_entries: u32,
    pub dma_burst_size: u32,
    pub mac_aggr_delim: u32,
    pub rx_skip_defrag_timeout_dup_detection_check: u32,
    pub vow_config: u32,
    pub gtk_offload_max_vdev: u32,
    pub num_msdu_desc: u32,
    pub max_frag_entries: u32,
    pub num_tdls_vdevs: u32,
    pub num_tdls_conn_table_entries: u32,
    pub beacon_tx_offload_max_vdev: u32,
    pub num_multicast_filter_entries: u32,
    pub num_wow_filters: u32,
    pub num_keep_alive_pattern: u32,
    pub keep_alive_pattern_size: u32,
    pub max_tdls_concurrent_sleep_sta: u32,
    pub max_tdls_concurrent_buffer_sta: u32,
    pub wmi_send_separate: u32,
    pub num_ocb_vdevs: u32,
    pub num_ocb_channels: u32,
    pub num_ocb_schedules: u32,
    pub host_capab: u32,
}

/// Field names in wire order, as in `struct wmi_tlv_resource_config` (for the offset test).
pub const RESOURCE_CONFIG_FIELDS: [&str; 41] = [
    "num_vdevs",
    "num_peers",
    "num_offload_peers",
    "num_offload_reorder_bufs",
    "num_peer_keys",
    "num_tids",
    "ast_skid_limit",
    "tx_chain_mask",
    "rx_chain_mask",
    "rx_timeout_pri",
    "rx_decap_mode",
    "scan_max_pending_reqs",
    "bmiss_offload_max_vdev",
    "roam_offload_max_vdev",
    "roam_offload_max_ap_profiles",
    "num_mcast_groups",
    "num_mcast_table_elems",
    "mcast2ucast_mode",
    "tx_dbg_log_size",
    "num_wds_entries",
    "dma_burst_size",
    "mac_aggr_delim",
    "rx_skip_defrag_timeout_dup_detection_check",
    "vow_config",
    "gtk_offload_max_vdev",
    "num_msdu_desc",
    "max_frag_entries",
    "num_tdls_vdevs",
    "num_tdls_conn_table_entries",
    "beacon_tx_offload_max_vdev",
    "num_multicast_filter_entries",
    "num_wow_filters",
    "num_keep_alive_pattern",
    "keep_alive_pattern_size",
    "max_tdls_concurrent_sleep_sta",
    "max_tdls_concurrent_buffer_sta",
    "wmi_send_separate",
    "num_ocb_vdevs",
    "num_ocb_channels",
    "num_ocb_schedules",
    "host_capab",
];

/// `sizeof(struct wmi_tlv_resource_config)`.
pub const RESOURCE_CONFIG_LEN: usize = 176;

impl ResourceConfig {
    /// The words in wire order (`rx_timeout_pri` expands to four).
    pub fn words(&self) -> [u32; 44] {
        [
            self.num_vdevs,
            self.num_peers,
            self.num_offload_peers,
            self.num_offload_reorder_bufs,
            self.num_peer_keys,
            self.num_tids,
            self.ast_skid_limit,
            self.tx_chain_mask,
            self.rx_chain_mask,
            self.rx_timeout_pri[0],
            self.rx_timeout_pri[1],
            self.rx_timeout_pri[2],
            self.rx_timeout_pri[3],
            self.rx_decap_mode,
            self.scan_max_pending_reqs,
            self.bmiss_offload_max_vdev,
            self.roam_offload_max_vdev,
            self.roam_offload_max_ap_profiles,
            self.num_mcast_groups,
            self.num_mcast_table_elems,
            self.mcast2ucast_mode,
            self.tx_dbg_log_size,
            self.num_wds_entries,
            self.dma_burst_size,
            self.mac_aggr_delim,
            self.rx_skip_defrag_timeout_dup_detection_check,
            self.vow_config,
            self.gtk_offload_max_vdev,
            self.num_msdu_desc,
            self.max_frag_entries,
            self.num_tdls_vdevs,
            self.num_tdls_conn_table_entries,
            self.beacon_tx_offload_max_vdev,
            self.num_multicast_filter_entries,
            self.num_wow_filters,
            self.num_keep_alive_pattern,
            self.keep_alive_pattern_size,
            self.max_tdls_concurrent_sleep_sta,
            self.max_tdls_concurrent_buffer_sta,
            self.wmi_send_separate,
            self.num_ocb_vdevs,
            self.num_ocb_channels,
            self.num_ocb_schedules,
            self.host_capab,
        ]
    }

    pub fn to_le_bytes(&self) -> [u8; RESOURCE_CONFIG_LEN] {
        let mut out = [0u8; RESOURCE_CONFIG_LEN];
        for (i, w) in self.words().iter().enumerate() {
            out[4 * i..4 * i + 4].copy_from_slice(&w.to_le_bytes());
        }
        out
    }

    /// The values `ath10k_wmi_tlv_op_gen_init` fills in for the WCN3990 (`hw_params`: 33 peers, AST skid limit 16, 2
    /// WDS entries), from the runtime inputs in `p`.
    pub fn wcn3990(p: &InitParams<'_>) -> ResourceConfig {
        let mut host_capab = FLAG_MGMT_BUNDLE_TX_COMPL;
        if p.svc_tx_data_ack_rssi {
            host_capab |= RSRC_CFG_FLAG_TX_ACK_RSSI;
        }
        let offload = if p.svc_rx_full_reorder {
            wcn3990::NUM_VDEVS
        } else {
            0
        };
        ResourceConfig {
            num_vdevs: wcn3990::NUM_VDEVS,
            num_peers: wcn3990::NUM_PEERS,
            num_offload_peers: offload,
            num_offload_reorder_bufs: offload,
            num_peer_keys: 2,
            num_tids: wcn3990::NUM_PEERS * 2,
            ast_skid_limit: wcn3990::AST_SKID_LIMIT,
            tx_chain_mask: 0x7,
            rx_chain_mask: 0x7,
            rx_timeout_pri: [0x64, 0x64, 0x64, 0x28],
            rx_decap_mode: p.rx_decap_mode,
            scan_max_pending_reqs: 4,
            bmiss_offload_max_vdev: wcn3990::NUM_VDEVS,
            roam_offload_max_vdev: wcn3990::NUM_VDEVS,
            roam_offload_max_ap_profiles: 8,
            num_mcast_groups: 0,
            num_mcast_table_elems: 0,
            mcast2ucast_mode: 0,
            tx_dbg_log_size: 0x400,
            num_wds_entries: wcn3990::NUM_WDS_ENTRIES,
            dma_burst_size: 0,
            mac_aggr_delim: 0,
            rx_skip_defrag_timeout_dup_detection_check: 0,
            vow_config: 0,
            gtk_offload_max_vdev: 2,
            num_msdu_desc: p.max_num_pending_tx,
            max_frag_entries: 2,
            num_tdls_vdevs: wcn3990::NUM_TDLS_VDEVS,
            num_tdls_conn_table_entries: 0x20,
            beacon_tx_offload_max_vdev: 2,
            num_multicast_filter_entries: 5,
            num_wow_filters: p.max_num_wow_patterns,
            num_keep_alive_pattern: 6,
            keep_alive_pattern_size: 0,
            max_tdls_concurrent_sleep_sta: 1,
            max_tdls_concurrent_buffer_sta: 1,
            wmi_send_separate: 0,
            num_ocb_vdevs: 0,
            num_ocb_channels: 0,
            num_ocb_schedules: 0,
            host_capab,
        }
    }
}

/// One host memory chunk given to the firmware (`ar->wmi.mem_chunks[i]`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemChunk {
    pub req_id: u32,
    pub len: u32,
    pub paddr: u64,
}

/// What `ath10k_wmi_tlv_op_gen_init` reads from the driver state.
#[derive(Clone, Copy, Debug)]
pub struct InitParams<'a> {
    pub chunks: &'a [MemChunk],
    /// `ar->wmi.rx_decap_mode` (`ATH10K_HW_TXRX_*`).
    pub rx_decap_mode: u32,
    /// `ar->htt.max_num_pending_tx`.
    pub max_num_pending_tx: u32,
    /// `ar->wow.max_num_patterns`.
    pub max_num_wow_patterns: u32,
    /// `WMI_SERVICE_RX_FULL_REORDER` is set in the firmware's service map.
    pub svc_rx_full_reorder: bool,
    /// `WMI_SERVICE_TX_DATA_ACK_RSSI`.
    pub svc_tx_data_ack_rssi: bool,
    /// `WMI_SERVICE_SUPPORT_EXTEND_ADDRESS`: chunk addresses carry their upper 32 bits.
    pub svc_support_extend_address: bool,
}

/// `ath10k_wmi_tlv_op_gen_init`: ABI + chunk count, resource configuration, host memory chunks.
pub fn gen_init(p: &InitParams<'_>) -> Result<Vec<u8>, WmiError> {
    let chunk_len = 16 + TLV_HDR_LEN;
    let chunks_len = p.chunks.len() * chunk_len;
    if chunks_len > usize::from(u16::MAX) {
        return Err(WmiError::Invalid);
    }
    let mut o = Out::new();
    o.tlv(tag::INIT_CMD, 28);
    for w in [
        ABI_VER0,
        ABI_VER1,
        ABI_VER_NS0,
        ABI_VER_NS1,
        ABI_VER_NS2,
        ABI_VER_NS3,
        p.chunks.len() as u32,
    ] {
        o.u32(w);
    }
    o.tlv(tag::RESOURCE_CONFIG, RESOURCE_CONFIG_LEN);
    o.bytes(&ResourceConfig::wcn3990(p).to_le_bytes());
    o.tlv(tag::ARRAY_STRUCT, chunks_len);
    for c in p.chunks {
        o.tlv(tag::WLAN_HOST_MEMORY_CHUNK, 16);
        o.u32(c.req_id);
        o.u32(c.paddr as u32);
        o.u32(c.len);
        o.u32(if p.svc_support_extend_address {
            (c.paddr >> 32) as u32
        } else {
            0
        });
    }
    Ok(o.finish())
}

/// `struct wmi_start_scan_arg`, with the variable parts as slices.
#[derive(Clone, Copy, Debug, Default)]
pub struct StartScanArg<'a> {
    pub scan_id: u32,
    pub scan_req_id: u32,
    pub vdev_id: u32,
    pub scan_priority: u32,
    pub notify_scan_events: u32,
    pub dwell_time_active: u32,
    pub dwell_time_passive: u32,
    pub min_rest_time: u32,
    pub max_rest_time: u32,
    pub repeat_probe_time: u32,
    pub probe_spacing_time: u32,
    pub idle_time: u32,
    pub max_scan_time: u32,
    pub probe_delay: u32,
    pub scan_ctrl_flags: u32,
    pub burst_duration_ms: u32,
    pub ie: &'a [u8],
    pub channels: &'a [u16],
    pub ssids: &'a [&'a [u8]],
    pub bssids: &'a [[u8; 6]],
    pub mac_addr: [u8; 6],
    pub mac_mask: [u8; 6],
}

/// `ath10k_wmi_tlv_op_gen_start_scan` (after `ath10k_wmi_start_scan_verify`). Limits: IE 256 bytes, 64 channels, 16
/// SSIDs of at most 32 bytes, 4 BSSIDs. The IDs get `WMI_HOST_SCAN_*_PREFIX` ORed in (without masking the
/// caller's value), the probe count is fixed at 3, and `WMI_SCAN_FILTER_PROBE_REQ` is flipped.
pub fn gen_start_scan(a: &StartScanArg<'_>) -> Result<Vec<u8>, WmiError> {
    if a.ie.len() > SCAN_MAX_IE_LEN
        || a.channels.len() > SCAN_MAX_CHANNELS
        || a.ssids.len() > SCAN_MAX_SSID
        || a.bssids.len() > SCAN_MAX_BSSID
        || a.ssids.iter().any(|s| s.len() > 32)
    {
        return Err(WmiError::Invalid);
    }
    let mut o = Out::new();
    o.tlv(tag::START_SCAN_CMD, 100);
    for w in [
        SCAN_ID_PREFIX | a.scan_id,
        SCAN_ID_PREFIX | a.scan_req_id,
        a.vdev_id,
        a.scan_priority,
        a.notify_scan_events,
        a.dwell_time_active,
        a.dwell_time_passive,
        a.min_rest_time,
        a.max_rest_time,
        a.repeat_probe_time,
        a.probe_spacing_time,
        a.idle_time,
        a.max_scan_time,
        a.probe_delay,
        a.scan_ctrl_flags ^ SCAN_FILTER_PROBE_REQ,
        a.burst_duration_ms,
        a.channels.len() as u32,
        a.bssids.len() as u32,
        a.ssids.len() as u32,
        a.ie.len() as u32,
        3, // num_probes
    ] {
        o.u32(w);
    }
    o.mac(&a.mac_addr);
    o.mac(&a.mac_mask);

    o.tlv(tag::ARRAY_UINT32, a.channels.len() * 4);
    for c in a.channels {
        o.u32(u32::from(*c));
    }
    o.tlv(tag::ARRAY_FIXED_STRUCT, a.ssids.len() * 36);
    for s in a.ssids {
        o.u32(s.len() as u32);
        o.bytes(s);
        o.zeros(32 - s.len());
    }
    o.tlv(tag::ARRAY_FIXED_STRUCT, a.bssids.len() * 8);
    for b in a.bssids {
        o.mac(b);
    }
    let ie_len = a.ie.len().div_ceil(4) * 4;
    o.tlv(tag::ARRAY_BYTE, ie_len);
    o.bytes(a.ie);
    o.zeros(ie_len - a.ie.len());
    Ok(o.finish())
}

/// What a stop-scan request addresses (`WMI_SCAN_STOP_*`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopScan {
    /// One scan, by id (`WMI_SCAN_STOP_ONE`, 0).
    One { scan_id: u32 },
    /// Every scan of a vdev (`WMI_SCAN_STOP_VDEV_ALL`, 0x01000000).
    VdevAll { vdev_id: u32 },
    /// All scans (`WMI_SCAN_STOP_ALL`, 0x04000000).
    All,
}

/// `ath10k_wmi_tlv_op_gen_stop_scan`. `req_id` and a one-scan id must fit 12 bits. As in the C, the scan id word is
/// the prefixed value of the union both `scan_id` and `vdev_id` live in, so a vdev stop also carries the vdev id
/// there.
pub fn gen_stop_scan(req_id: u32, what: StopScan) -> Result<Vec<u8>, WmiError> {
    if req_id > 0xfff {
        return Err(WmiError::Invalid);
    }
    let (req_type, union_word) = match what {
        StopScan::One { scan_id } => {
            if scan_id > 0xfff {
                return Err(WmiError::Invalid);
            }
            (0u32, scan_id)
        }
        StopScan::VdevAll { vdev_id } => (0x0100_0000, vdev_id),
        StopScan::All => (0x0400_0000, 0),
    };
    let mut o = Out::new();
    o.tlv(tag::STOP_SCAN_CMD, 16);
    o.u32(SCAN_ID_PREFIX | req_id);
    o.u32(SCAN_ID_PREFIX | union_word);
    o.u32(req_type);
    o.u32(union_word);
    Ok(o.finish())
}

/// `ath10k_wmi_tlv_op_gen_pdev_set_rd` (the DFS region argument is ignored by the TLV encoder, and so is the pdev
/// id, which stays 0).
pub fn gen_pdev_set_regdomain(
    rd: u16,
    rd_2g: u16,
    rd_5g: u16,
    ctl_2g: u16,
    ctl_5g: u16,
) -> Vec<u8> {
    let mut o = Out::new();
    o.tlv(tag::PDEV_SET_REGDOMAIN_CMD, 24);
    for w in [
        0,
        u32::from(rd),
        u32::from(rd_2g),
        u32::from(rd_5g),
        u32::from(ctl_2g),
        u32::from(ctl_5g),
    ] {
        o.u32(w);
    }
    o.finish()
}

/// `WMI_VDEV_TYPE_*`.
pub mod vdev_type {
    pub const AP: u32 = 1;
    pub const STA: u32 = 2;
    pub const IBSS: u32 = 3;
    pub const MONITOR: u32 = 4;
}

/// `WMI_TLV_VDEV_SUBTYPE_*` (the TLV firmware's numbering, not `enum wmi_vdev_subtype`'s).
pub mod vdev_subtype {
    pub const NONE: u32 = 0;
    pub const P2P_DEV: u32 = 1;
    pub const P2P_CLI: u32 = 2;
    pub const P2P_GO: u32 = 3;
    pub const PROXY_STA: u32 = 4;
    pub const MESH: u32 = 5;
    pub const MESH_11S: u32 = 6;
}

/// `WMI_PEER_TYPE_*`.
pub mod peer_type {
    pub const DEFAULT: u32 = 0;
    pub const BSS: u32 = 1;
    pub const TDLS: u32 = 2;
}

/// `ath10k_wmi_tlv_op_gen_vdev_create`. `subtype` is written as given (a `WMI_VDEV_SUBTYPE_*` of the generic code
/// that `mac.c` has already mapped through `get_vdev_subtype`).
pub fn gen_vdev_create(vdev_id: u32, vdev_type: u32, subtype: u32, mac: &[u8; 6]) -> Vec<u8> {
    let mut o = Out::new();
    o.tlv(tag::VDEV_CREATE_CMD, 20);
    o.u32(vdev_id);
    o.u32(vdev_type);
    o.u32(subtype);
    o.mac(mac);
    o.finish()
}

fn one_vdev_cmd(t: u16, vdev_id: u32) -> Vec<u8> {
    let mut o = Out::new();
    o.tlv(t, 4);
    o.u32(vdev_id);
    o.finish()
}

/// `ath10k_wmi_tlv_op_gen_vdev_delete`.
pub fn gen_vdev_delete(vdev_id: u32) -> Vec<u8> {
    one_vdev_cmd(tag::VDEV_DELETE_CMD, vdev_id)
}

/// `ath10k_wmi_tlv_op_gen_vdev_down`.
pub fn gen_vdev_down(vdev_id: u32) -> Vec<u8> {
    one_vdev_cmd(tag::VDEV_DOWN_CMD, vdev_id)
}

/// `ath10k_wmi_tlv_op_gen_vdev_up`. `aid` is written as given.
pub fn gen_vdev_up(vdev_id: u32, aid: u32, bssid: &[u8; 6]) -> Vec<u8> {
    let mut o = Out::new();
    o.tlv(tag::VDEV_UP_CMD, 16);
    o.u32(vdev_id);
    o.u32(aid);
    o.mac(bssid);
    o.finish()
}

/// `ath10k_wmi_tlv_op_gen_peer_create`.
pub fn gen_peer_create(vdev_id: u32, addr: &[u8; 6], peer_type: u32) -> Vec<u8> {
    let mut o = Out::new();
    o.tlv(tag::PEER_CREATE_CMD, 16);
    o.u32(vdev_id);
    o.mac(addr);
    o.u32(peer_type);
    o.finish()
}

/// `ath10k_wmi_tlv_op_gen_peer_delete`.
pub fn gen_peer_delete(vdev_id: u32, addr: &[u8; 6]) -> Vec<u8> {
    let mut o = Out::new();
    o.tlv(tag::PEER_DELETE_CMD, 12);
    o.u32(vdev_id);
    o.mac(addr);
    o.finish()
}

/// `WMI_CHAN_FLAG_*`.
pub mod chan_flag {
    pub const HT40_PLUS: u32 = 1 << 6;
    pub const PASSIVE: u32 = 1 << 7;
    pub const ADHOC_ALLOWED: u32 = 1 << 8;
    pub const DFS: u32 = 1 << 10;
    pub const ALLOW_HT: u32 = 1 << 11;
    pub const ALLOW_VHT: u32 = 1 << 12;
    pub const DFS_CFREQ2: u32 = 1 << 15;
}

/// `enum wmi_phy_mode`.
pub mod phy_mode {
    pub const MODE_11A: u8 = 0;
    pub const MODE_11G: u8 = 1;
    pub const MODE_11B: u8 = 2;
    pub const MODE_11GONLY: u8 = 3;
    pub const MODE_11NA_HT20: u8 = 4;
    pub const MODE_11NG_HT20: u8 = 5;
    pub const MODE_11NA_HT40: u8 = 6;
    pub const MODE_11NG_HT40: u8 = 7;
    pub const MODE_11AC_VHT20: u8 = 8;
    pub const MODE_11AC_VHT40: u8 = 9;
    pub const MODE_11AC_VHT80: u8 = 10;
    pub const MODE_11AC_VHT20_2G: u8 = 11;
    pub const MODE_11AC_VHT40_2G: u8 = 12;
    pub const MODE_11AC_VHT80_2G: u8 = 13;
    pub const MODE_11AC_VHT80_80: u8 = 14;
    pub const MODE_11AC_VHT160: u8 = 15;
    pub const MODE_UNKNOWN: u8 = 16;
}

/// `struct wmi_channel_arg`.
#[derive(Clone, Copy, Debug, Default)]
pub struct ChannelArg {
    pub freq: u32,
    pub band_center_freq1: u32,
    pub band_center_freq2: u32,
    pub passive: bool,
    pub allow_ibss: bool,
    pub allow_ht: bool,
    pub allow_vht: bool,
    pub ht40plus: bool,
    pub chan_radar: bool,
    pub min_power: u32,
    pub max_power: u32,
    pub max_reg_power: u32,
    pub max_antenna_gain: u32,
    pub reg_class_id: u32,
    pub mode: u8,
    /// For 80+80 and 160 MHz: the second segment is a radar channel (the C asks mac80211's channel table; this
    /// crate has none). Ignored for other modes.
    pub cfreq2_radar: bool,
}

/// `ath10k_wmi_put_wmi_channel`: the 24-byte `struct wmi_channel`. `reg_class_id` is not transmitted (the C never
/// copies it), and `mode` shares the low byte of the flags word.
pub fn put_channel(o: &mut Vec<u8>, c: &ChannelArg) {
    let mut flags = 0u32;
    if c.passive {
        flags |= chan_flag::PASSIVE;
    }
    if c.allow_ibss {
        flags |= chan_flag::ADHOC_ALLOWED;
    }
    if c.allow_ht {
        flags |= chan_flag::ALLOW_HT;
    }
    if c.allow_vht {
        flags |= chan_flag::ALLOW_VHT;
    }
    if c.ht40plus {
        flags |= chan_flag::HT40_PLUS;
    }
    if c.chan_radar {
        flags |= chan_flag::DFS;
    }
    let mut bcf1 = c.band_center_freq1;
    let mut bcf2 = 0u32;
    if c.mode == phy_mode::MODE_11AC_VHT80_80 {
        bcf2 = c.band_center_freq2;
        if c.cfreq2_radar {
            flags |= chan_flag::DFS_CFREQ2;
        }
    }
    if c.mode == phy_mode::MODE_11AC_VHT160 {
        let (f1, f2) = if c.freq > c.band_center_freq1 {
            (
                c.band_center_freq1.wrapping_add(40),
                c.band_center_freq1.wrapping_sub(40),
            )
        } else {
            (
                c.band_center_freq1.wrapping_sub(40),
                c.band_center_freq1.wrapping_add(40),
            )
        };
        let _ = f2; // only used for the regulatory lookup in the C
        bcf1 = f1;
        // the centre of the entire VHT160
        bcf2 = c.band_center_freq1;
        if c.cfreq2_radar {
            flags |= chan_flag::DFS_CFREQ2;
        }
    }
    let words = [c.freq, bcf1, bcf2, u32::from(c.mode) | flags];
    for w in words {
        o.extend_from_slice(&w.to_le_bytes());
    }
    // reginfo0: min_power, max_power, reg_power, reg_classid (left 0); reginfo1: antenna_max, max_tx_power
    o.extend_from_slice(&[
        c.min_power as u8,
        c.max_power as u8,
        c.max_reg_power as u8,
        0,
    ]);
    o.extend_from_slice(&[c.max_antenna_gain as u8, c.max_power as u8, 0, 0]);
}

/// `struct wmi_vdev_start_request_arg`.
#[derive(Clone, Copy, Debug, Default)]
pub struct VdevStartArg<'a> {
    pub vdev_id: u32,
    pub channel: ChannelArg,
    pub bcn_intval: u32,
    pub dtim_period: u32,
    pub ssid: Option<&'a [u8]>,
    pub bcn_tx_rate: u32,
    pub bcn_tx_power: u32,
    pub disable_hw_ack: bool,
    pub hidden_ssid: bool,
    pub pmf_enabled: bool,
}

/// `WMI_VDEV_START_*` flags.
pub const VDEV_START_HIDDEN_SSID: u32 = 1 << 0;
pub const VDEV_START_PMF_ENABLED: u32 = 1 << 1;

/// `ath10k_wmi_tlv_op_gen_vdev_start` (the TLV firmware uses the same command for start and restart): the vdev
/// start TLV, the channel TLV and an empty `ARRAY_STRUCT` where P2P NoA descriptors would go.
pub fn gen_vdev_start(a: &VdevStartArg<'_>) -> Result<Vec<u8>, WmiError> {
    if a.hidden_ssid && a.ssid.is_none() {
        return Err(WmiError::Invalid);
    }
    if a.ssid.is_some_and(|s| s.len() > 32) {
        return Err(WmiError::Invalid);
    }
    let mut flags = 0u32;
    if a.hidden_ssid {
        flags |= VDEV_START_HIDDEN_SSID;
    }
    if a.pmf_enabled {
        flags |= VDEV_START_PMF_ENABLED;
    }
    let mut o = Out::new();
    o.tlv(tag::VDEV_START_REQUEST_CMD, 72);
    o.u32(a.vdev_id);
    o.u32(0); // requestor_id
    o.u32(a.bcn_intval);
    o.u32(a.dtim_period);
    o.u32(flags);
    match a.ssid {
        Some(s) => {
            o.u32(s.len() as u32);
            o.bytes(s);
            o.zeros(32 - s.len());
        }
        None => o.zeros(36),
    }
    o.u32(a.bcn_tx_rate);
    o.u32(a.bcn_tx_power);
    o.u32(0); // num_noa_descr
    o.u32(u32::from(a.disable_hw_ack));
    o.tlv(tag::CHANNEL, 24);
    put_channel(&mut o.0, &a.channel);
    o.tlv(tag::ARRAY_STRUCT, 0);
    Ok(o.finish())
}

/// `ath10k_wmi_tlv_op_gen_mgmt_tx_send` without the mac80211 plumbing: a management frame sent by reference. The
/// first `min(frame.len(), 64)` bytes (rounded up to 4, zero padded) travel inline; `frame_len` is the whole frame
/// and `paddr` is where the firmware fetches it. The caller adds the CCMP MIC room to `frame` for protected
/// action/deauth/disassoc frames, as the C does before this point.
pub fn gen_mgmt_tx_send(vdev_id: u32, desc_id: u32, paddr: u64, frame: &[u8]) -> Vec<u8> {
    let inline = frame.len().min(MGMT_TX_FRAME_MAX_LEN);
    let buf_len = inline.div_ceil(4) * 4;
    let mut o = Out::new();
    o.tlv(tag::MGMT_TX_CMD, 28);
    o.u32(vdev_id);
    o.u32(desc_id);
    o.u32(0); // chanfreq
    o.u64(paddr);
    o.u32(frame.len() as u32);
    o.u32(buf_len as u32);
    o.tlv(tag::ARRAY_BYTE, buf_len);
    o.bytes(&frame[..inline]);
    o.zeros(buf_len - inline);
    o.finish()
}

// ---- station commands (`ath10k_wmi_tlv_op_gen_*`), for crate::driver --------------------------------------------

/// `ath10k_wmi_tlv_op_gen_pdev_set_param` (the pdev id word stays 0, "not used yet").
pub fn gen_pdev_set_param(param_id: u32, value: u32) -> Vec<u8> {
    let mut o = Out::new();
    o.tlv(tag::PDEV_SET_PARAM_CMD, 12);
    o.u32(0);
    o.u32(param_id);
    o.u32(value);
    o.finish()
}

/// `ath10k_wmi_tlv_op_gen_vdev_stop`.
pub fn gen_vdev_stop(vdev_id: u32) -> Vec<u8> {
    one_vdev_cmd(tag::VDEV_STOP_CMD, vdev_id)
}

/// `ath10k_wmi_tlv_op_gen_vdev_set_param`.
pub fn gen_vdev_set_param(vdev_id: u32, param_id: u32, value: u32) -> Vec<u8> {
    let mut o = Out::new();
    o.tlv(tag::VDEV_SET_PARAM_CMD, 12);
    o.u32(vdev_id);
    o.u32(param_id);
    o.u32(value);
    o.finish()
}

/// `struct wmi_wmm_params_arg`, as `ath10k_conf_tx` fills it: `acm` and `no_ack` stay 0. `txop` is in microseconds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WmmAc {
    pub cwmin: u32,
    pub cwmax: u32,
    pub aifs: u32,
    pub txop: u32,
}

/// `ath10k_wmi_tlv_op_gen_vdev_wmm_conf`, access categories in the order BE, BK, VI, VO: one TLV, each category with a dummy word
/// before it.
pub fn gen_vdev_wmm_conf(vdev_id: u32, acs: &[WmmAc; 4]) -> Vec<u8> {
    let mut o = Out::new();
    o.tlv(tag::VDEV_SET_WMM_PARAMS_CMD, 4 + 4 * 28);
    o.u32(vdev_id);
    for ac in acs {
        o.u32(0);
        o.u32(ac.cwmin);
        o.u32(ac.cwmax);
        o.u32(ac.aifs);
        o.u32(ac.txop);
        o.u32(0);
        o.u32(0);
    }
    o.finish()
}

/// `ath10k_wmi_tlv_op_gen_vdev_sta_uapsd` for one access category: `ath10k_conf_tx_uapsd` calls it per AC when the
/// firmware has an auto-trigger service, to switch the firmware's own U-APSD trigger off. `wmm_ac` is the AC's index
/// (BE 1, BK 0, VI 2, VO 3) and `user_priority` its TID.
pub fn gen_vdev_sta_uapsd(vdev_id: u32, peer: &[u8; 6], wmm_ac: u32, user_priority: u32) -> Vec<u8> {
    let mut o = Out::new();
    o.tlv(tag::STA_UAPSD_AUTO_TRIG_CMD, 16);
    o.u32(vdev_id);
    o.mac(peer);
    o.u32(1); // num_ac
    o.tlv(tag::ARRAY_STRUCT, 4 + 20);
    o.tlv(tag::STA_UAPSD_AUTO_TRIG_PARAM, 20);
    o.u32(wmm_ac);
    o.u32(user_priority);
    o.u32(0); // service_interval
    o.u32(u32::MAX); // suspend_interval: WMI_STA_UAPSD_MAX_INTERVAL_MSEC
    o.u32(u32::MAX); // delay_interval
    o.finish()
}

/// `ath10k_wmi_tlv_op_gen_peer_set_param`.
pub fn gen_peer_set_param(vdev_id: u32, peer: &[u8; 6], param_id: u32, value: u32) -> Vec<u8> {
    let mut o = Out::new();
    o.tlv(tag::PEER_SET_PARAM_CMD, 20);
    o.u32(vdev_id);
    o.mac(peer);
    o.u32(param_id);
    o.u32(value);
    o.finish()
}

/// `ath10k_wmi_tlv_op_gen_set_psmode`.
pub fn gen_set_psmode(vdev_id: u32, mode: u32) -> Vec<u8> {
    let mut o = Out::new();
    o.tlv(tag::STA_POWERSAVE_MODE_CMD, 8);
    o.u32(vdev_id);
    o.u32(mode);
    o.finish()
}

/// `ath10k_wmi_tlv_op_gen_set_sta_ps`.
pub fn gen_set_sta_ps(vdev_id: u32, param_id: u32, value: u32) -> Vec<u8> {
    let mut o = Out::new();
    o.tlv(tag::STA_POWERSAVE_PARAM_CMD, 12);
    o.u32(vdev_id);
    o.u32(param_id);
    o.u32(value);
    o.finish()
}

/// `ath10k_wmi_tlv_op_gen_sta_keepalive`, as `ath10k_mac_vif_disable_keepalive` calls it: method null frame, interval
/// 0 (some firmware ignores `enabled`, so the interval is what disables it) and an empty ARP response record.
pub fn gen_sta_keepalive_disable(vdev_id: u32) -> Vec<u8> {
    let mut o = Out::new();
    o.tlv(tag::STA_KEEPALIVE_CMD, 16);
    o.u32(vdev_id);
    o.u32(1); // enabled
    o.u32(1); // WMI_STA_KEEPALIVE_METHOD_NULL_FRAME
    o.u32(0); // WMI_STA_KEEPALIVE_INTERVAL_DISABLE
    o.tlv(tag::STA_KEEPALVE_ARP_RESPONSE, 16);
    o.zeros(16);
    o.finish()
}

/// `ath10k_wmi_tlv_op_gen_scan_prob_req_oui`.
pub fn gen_scan_prob_req_oui(oui: u32) -> Vec<u8> {
    let mut o = Out::new();
    o.tlv(tag::SCAN_PROB_REQ_OUI_CMD, 4);
    o.u32(oui);
    o.finish()
}

/// `ath10k_wmi_tlv_op_gen_adaptive_qcs`.
pub fn gen_adaptive_qcs(enable: bool) -> Vec<u8> {
    let mut o = Out::new();
    o.tlv(tag::RESMGR_ADAPTIVE_OCS_CMD, 4);
    o.u32(u32::from(enable));
    o.finish()
}

/// `struct wmi_vdev_install_key_arg`. `data` is `None` only with [`cipher::NONE`].
#[derive(Clone, Copy, Debug)]
pub struct InstallKeyArg<'a> {
    pub vdev_id: u32,
    pub macaddr: [u8; 6],
    pub key_idx: u32,
    pub key_flags: u32,
    pub key_cipher: u32,
    /// `key_len` as sent; for a delete the C keeps the key's length with no data.
    pub key_len: u32,
    pub key_txmic_len: u32,
    pub key_rxmic_len: u32,
    pub data: Option<&'a [u8]>,
}

/// `sizeof(struct wmi_vdev_install_key_cmd)`.
pub const INSTALL_KEY_CMD_LEN: usize = 92;

/// `ath10k_wmi_tlv_op_gen_vdev_install_key`. Beside the C's cipher/data consistency check, a key shorter than
/// `key_len` is refused (the C's `memcpy` would read past it) and so is a `key_len` above 64.
pub fn gen_vdev_install_key(a: &InstallKeyArg<'_>) -> Result<Vec<u8>, WmiError> {
    if (a.key_cipher == cipher::NONE) != a.data.is_none() {
        return Err(WmiError::Invalid);
    }
    let key_len = a.key_len as usize;
    if key_len > 64 || a.data.is_some_and(|d| d.len() < key_len) {
        return Err(WmiError::Invalid);
    }
    let rounded = key_len.div_ceil(4) * 4;
    let mut o = Out::new();
    o.tlv(tag::VDEV_INSTALL_KEY_CMD, INSTALL_KEY_CMD_LEN);
    o.u32(a.vdev_id);
    o.mac(&a.macaddr);
    o.u32(a.key_idx);
    o.u32(a.key_flags);
    o.u32(a.key_cipher);
    o.zeros(24 + 32); // the three key sequence counters and the two WPI counters
    o.u32(a.key_len);
    o.u32(a.key_txmic_len);
    o.u32(a.key_rxmic_len);
    o.tlv(tag::ARRAY_BYTE, rounded);
    match a.data {
        Some(d) => {
            o.bytes(&d[..key_len]);
            o.zeros(rounded - key_len);
        }
        None => o.zeros(rounded),
    }
    Ok(o.finish())
}

/// `struct wmi_peer_assoc_complete_arg`.
#[derive(Clone, Copy, Debug, Default)]
pub struct PeerAssocArg<'a> {
    pub addr: [u8; 6],
    pub vdev_id: u32,
    pub reassoc: bool,
    pub aid: u32,
    pub flags: u32,
    pub caps: u32,
    pub listen_intval: u32,
    pub ht_caps: u32,
    pub max_mpdu: u32,
    pub mpdu_density: u32,
    pub rate_caps: u32,
    pub num_spatial_streams: u32,
    pub vht_caps: u32,
    pub phymode: u32,
    pub legacy_rates: &'a [u8],
    pub ht_rates: &'a [u8],
    pub vht_rx_max_rate: u32,
    pub vht_rx_mcs_set: u32,
    pub vht_tx_max_rate: u32,
    pub vht_tx_mcs_set: u32,
}

/// `sizeof(struct wmi_tlv_peer_assoc_cmd)`.
pub const PEER_ASSOC_CMD_LEN: usize = 76;

/// `ath10k_wmi_tlv_op_gen_peer_assoc`.
pub fn gen_peer_assoc(a: &PeerAssocArg<'_>) -> Result<Vec<u8>, WmiError> {
    if a.mpdu_density > 16
        || a.legacy_rates.len() > MAX_SUPPORTED_RATES
        || a.ht_rates.len() > MAX_SUPPORTED_RATES
    {
        return Err(WmiError::Invalid);
    }
    let legacy_len = a.legacy_rates.len().div_ceil(4) * 4;
    let ht_len = a.ht_rates.len().div_ceil(4) * 4;
    let mut o = Out::new();
    o.tlv(tag::PEER_ASSOC_COMPLETE_CMD, PEER_ASSOC_CMD_LEN);
    o.mac(&a.addr);
    for w in [
        a.vdev_id,
        u32::from(!a.reassoc),
        a.aid,
        a.flags,
        a.caps,
        a.listen_intval,
        a.ht_caps,
        a.max_mpdu,
        a.mpdu_density,
        a.rate_caps,
        a.num_spatial_streams,
        a.vht_caps,
        a.phymode,
        0, // ht_info[0]
        0, // ht_info[1]
        a.legacy_rates.len() as u32,
        a.ht_rates.len() as u32,
    ] {
        o.u32(w);
    }
    o.tlv(tag::ARRAY_BYTE, legacy_len);
    o.bytes(a.legacy_rates);
    o.zeros(legacy_len - a.legacy_rates.len());
    o.tlv(tag::ARRAY_BYTE, ht_len);
    o.bytes(a.ht_rates);
    o.zeros(ht_len - a.ht_rates.len());
    o.tlv(tag::VHT_RATE_SET, 16);
    o.u32(a.vht_rx_max_rate);
    o.u32(a.vht_rx_mcs_set);
    o.u32(a.vht_tx_max_rate);
    o.u32(a.vht_tx_mcs_set);
    Ok(o.finish())
}

/// `ath10k_wmi_tlv_op_gen_scan_chan_list`. The nested TLV length is a `u16`, so at most 2340 channels; the caller has
/// far fewer.
pub fn gen_scan_chan_list(chans: &[ChannelArg]) -> Result<Vec<u8>, WmiError> {
    let chans_len = chans.len() * (TLV_HDR_LEN + 24);
    if chans_len > usize::from(u16::MAX) {
        return Err(WmiError::Invalid);
    }
    let mut o = Out::new();
    o.tlv(tag::SCAN_CHAN_LIST_CMD, 4);
    o.u32(chans.len() as u32);
    o.tlv(tag::ARRAY_STRUCT, chans_len);
    for c in chans {
        o.tlv(tag::CHANNEL, 24);
        put_channel(&mut o.0, c);
    }
    Ok(o.finish())
}

// ---- station events (`ath10k_wmi_tlv_op_pull_*`) -------------------------------------------------------------------

/// `struct wmi_mgmt_rx_ev_arg` and the frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MgmtRx<'a> {
    pub channel: u32,
    pub snr: u32,
    pub rate: u32,
    pub phy_mode: u32,
    pub status: u32,
    pub rssi: [u32; 4],
    pub frame: &'a [u8],
}

/// `ath10k_wmi_tlv_op_pull_mgmt_rx_ev`: the `MGMT_RX_HDR` and the last `ARRAY_BYTE`, whose start is the frame;
/// `buf_len` bytes from there must be inside the event (the C's check; they need not be inside the TLV).
pub fn parse_mgmt_rx(buf: &[u8]) -> Result<MgmtRx<'_>, WmiError> {
    let mut hdr = None;
    let mut frame_off = None;
    for tlv in iter(buf) {
        let tlv = tlv?;
        match tlv.tag {
            tag::MGMT_RX_HDR => hdr = Some(tlv.value),
            tag::ARRAY_BYTE => frame_off = Some(tlv.offset),
            _ => {}
        }
    }
    let (ev, off) = match (hdr, frame_off) {
        (Some(e), Some(o)) => (e, o),
        _ => return Err(WmiError::Protocol),
    };
    let len = le32(ev, 16) as usize;
    let end = off.checked_add(len).ok_or(WmiError::Protocol)?;
    let frame = buf.get(off..end).ok_or(WmiError::Protocol)?;
    Ok(MgmtRx {
        channel: le32(ev, 0),
        snr: le32(ev, 4),
        rate: le32(ev, 8),
        phy_mode: le32(ev, 12),
        status: le32(ev, 20),
        rssi: [le32(ev, 24), le32(ev, 28), le32(ev, 32), le32(ev, 36)],
        frame,
    })
}

/// One completed management frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MgmtTxCompl {
    pub desc_id: u32,
    pub status: u32,
    pub ack_rssi: u32,
}

/// `ath10k_wmi_tlv_op_pull_mgmt_tx_compl_ev`. No policy entry covers this tag, so a TLV shorter than the struct is
/// refused here; the C would read past it.
pub fn parse_mgmt_tx_compl(buf: &[u8]) -> Result<MgmtTxCompl, WmiError> {
    let ev = find_last(buf, tag::MGMT_TX_COMPL_EVENT)?.ok_or(WmiError::Protocol)?;
    if ev.len() < 20 {
        return Err(WmiError::Invalid);
    }
    Ok(MgmtTxCompl {
        desc_id: le32(ev, 0),
        status: le32(ev, 4),
        ack_rssi: le32(ev, 16),
    })
}

/// `ath10k_wmi_tlv_op_pull_mgmt_tx_bundle_compl_ev`: the report count, then `ARRAY_UINT32`s of ids, status, ppdu ids
/// and ACK RSSI in that order. A count beyond the id or status array is cut to what is there (the C trusts it). The
/// result keeps the status array's own value; Linux's `ath10k_wmi_event_mgmt_tx_bundle_compl` reads the status from
/// `desc_ids[i]`, a typo this does not copy.
pub fn parse_mgmt_tx_bundle_compl(buf: &[u8]) -> Result<Vec<MgmtTxCompl>, WmiError> {
    let mut num = None;
    let mut arrays: [Option<&[u8]>; 4] = [None; 4];
    let mut next = 0;
    for tlv in iter(buf) {
        let tlv = tlv?;
        match tlv.tag {
            tag::MGMT_TX_COMPL_BUNDLE_EVENT => {
                if tlv.value.len() < 4 {
                    return Err(WmiError::Invalid);
                }
                num = Some(le32(tlv.value, 0));
            }
            tag::ARRAY_UINT32 if next < 4 => {
                arrays[next] = Some(tlv.value);
                next += 1;
            }
            _ => {}
        }
    }
    let (num, ids, status) = match (num, arrays[0], arrays[1]) {
        (Some(n), Some(i), Some(s)) => (n as usize, i, s),
        _ => return Err(WmiError::Protocol),
    };
    let word = |a: &[u8], i: usize| map_word(a, i);
    let mut out = Vec::new();
    for i in 0..num.min(ids.len() / 4).min(status.len() / 4) {
        out.push(MgmtTxCompl {
            desc_id: word(ids, i).unwrap_or(0),
            status: word(status, i).unwrap_or(0),
            ack_rssi: arrays[3].and_then(|a| word(a, i)).unwrap_or(0),
        });
    }
    Ok(out)
}

/// `ath10k_wmi_tlv_op_pull_peer_kick_ev`: the peer's address.
pub fn parse_peer_kickout(buf: &[u8]) -> Result<[u8; 6], WmiError> {
    let ev = find_last(buf, tag::PEER_STA_KICKOUT_EVENT)?.ok_or(WmiError::Protocol)?;
    let mut a = [0u8; 6];
    a.copy_from_slice(&ev[..6]);
    Ok(a)
}

/// `struct wmi_roam_ev_arg`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Roam {
    pub vdev_id: u32,
    pub reason: u32,
    pub rssi: u32,
}

/// `ath10k_wmi_tlv_op_pull_roam_ev`.
pub fn parse_roam(buf: &[u8]) -> Result<Roam, WmiError> {
    let ev = find_last(buf, tag::ROAM_EVENT)?.ok_or(WmiError::Protocol)?;
    Ok(Roam {
        vdev_id: le32(ev, 0),
        reason: le32(ev, 4),
        rssi: le32(ev, 8),
    })
}

#[cfg(test)]
mod wmm_conf_tests {
    use super::*;

    /// The bytes of `ath10k_wmi_tlv_op_gen_vdev_wmm_conf`: `struct wmi_tlv_vdev_set_wmm_cmd`, a dummy word before each category.
    #[test]
    fn each_category_is_a_dummy_word_and_six_fields() {
        let be = WmmAc { cwmin: 15, cwmax: 1023, aifs: 3, txop: 0 };
        let vo = WmmAc { cwmin: 3, cwmax: 7, aifs: 2, txop: 1504 };
        let bytes = gen_vdev_wmm_conf(5, &[be, WmmAc::default(), WmmAc::default(), vo]);
        assert_eq!(bytes.len(), 4 + 4 + 4 * 28);
        let word = |i: usize| u32::from_le_bytes([bytes[i * 4], bytes[i * 4 + 1], bytes[i * 4 + 2], bytes[i * 4 + 3]]);
        // `struct wmi_tlv`: the length, then the tag.
        assert_eq!(word(0) & 0xffff, 116);
        assert_eq!(word(0) >> 16, 199);
        assert_eq!(word(1), 5);
        // BE: dummy, cwmin, cwmax, aifs, txop, acm, no_ack.
        assert_eq!((word(2), word(3), word(4), word(5), word(6), word(7), word(8)), (0, 15, 1023, 3, 0, 0, 0));
        // VO is the last category.
        assert_eq!((word(23), word(24), word(25), word(26), word(27), word(28), word(29)), (0, 3, 7, 2, 1504, 0, 0));
    }
}
