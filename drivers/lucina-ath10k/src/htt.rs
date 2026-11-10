//! HTT, the host-target data path protocol, framing only, for the WCN3990 (64-bit target, WMI-TLV firmware).
//!
//! Derived from Linux `drivers/net/wireless/ath/ath10k/htt.h`, `htt.c`, `htt_tx.c` and `htt_rx.c`, and the
//! `ATH10K_HW_WCN3990` entry of `core.c` (SPDX ISC; Copyright (c) 2005-2011 Atheros Communications Inc., 2011-2017
//! Qualcomm Atheros, Inc., 2018 The Linux Foundation, 2022, 2024 Qualcomm Innovation Center, Inc., Qualcomm
//! Technologies, Inc. and/or its subsidiaries). `tests/htt.rs` checks every size, offset, constant and encoded
//! message against `oracle/htt_oracle.c`, which compiles the real structs and the real
//! `ath10k_rx_desc_wcn3990_get_offsets`.
//!
//! * Host to target: [`ver_req`] (`ath10k_htt_h2t_ver_req_msg`), [`rx_ring_setup_64`] / [`rx_ring_setup_32`]
//!   (`ath10k_htt_send_rx_ring_cfg_64`/`_32`; WCN3990 uses the 64-bit one with [`WCN3990_RX_DESC_OFFSETS`]),
//!   [`DataTxDesc64`] + [`tx_frm_64`] (the HTC header, HTT header and `struct htt_data_tx_desc_64` that
//!   `ath10k_htt_tx_64` builds) and [`MsduExtDesc64`] (the continuous fragment descriptor WCN3990 uses).
//! * Target to host: [`parse_resp`] splits the message type (the WMI-TLV firmware numbers its messages as
//!   `enum htt_tlv_t2h_msg_type`, [`t2h`]); [`parse_version_conf`], [`parse_tx_completion`] (with the WCN3990 ACK
//!   RSSI layout), [`parse_tx_credit_update`], [`parse_rx_in_ord_ind`] (64-bit address list) and
//!   [`parse_rx_offload_msdu`].
//!
//! Also the rest of what a data path needs: [`frag_desc_bank_cfg_64`], [`aggr_cfg_v2`], [`tx_item_64`] (the gather
//! header of a frame), [`parse_peer_map`], [`parse_peer_unmap`], [`parse_mgmt_tx_compl`], [`parse_sec_ind`],
//! [`parse_rx_ind`] and the offsets and bits of the rx descriptor the device writes in front of every received buffer
//! ([`rx_desc`], [`WCN3990_RX_DESC_OFFSETS`], [`RX_DESC_SIZE`]); `crate::driver` reads the descriptor.
//!
//! Surprises worth knowing: on WCN3990 `flags2` bit 3 means "ACK RSSI present" in the tx completion, which is also
//! the generic "PPDU duration present" bit, so [`TxCompletion::flags2`] is returned raw and the airtime records are
//! not parsed; the C bound check of the in-order indication assumes 8-byte descriptors while the 64-bit ones are 12
//! bytes (this parser checks the right size); a 64-bit tx descriptor is 19 bytes, packed.

use alloc::vec::Vec;

/// `HTT_RX_BUF_SIZE`: the size of every rx buffer.
pub const RX_BUF_SIZE: u16 = 2048;
/// `HTT_RX_RING_SIZE` (= `HTT_RX_RING_SIZE_MAX`).
pub const RX_RING_SIZE: u16 = 2048;
/// `HTT_RX_RING_FILL_LEVEL_DUAL_MAC`, `hw_params.rx_ring_fill_level` of the WCN3990.
pub const RX_RING_FILL_LEVEL: u16 = RX_RING_SIZE - 1;
/// `HTT_INVALID_PEERID`.
pub const INVALID_PEERID: u16 = 0xffff;
/// `HTT_TX_COMPL_INV_MSDU_ID`.
pub const TX_COMPL_INV_MSDU_ID: u16 = 0xffff;

/// `enum htt_h2t_msg_type`.
pub mod h2t {
    pub const VERSION_REQ: u8 = 0;
    pub const TX_FRM: u8 = 1;
    pub const RX_RING_CFG: u8 = 2;
    pub const STATS_REQ: u8 = 3;
    pub const SYNC: u8 = 4;
    pub const AGGR_CFG: u8 = 5;
    pub const FRAG_DESC_BANK_CFG: u8 = 6;
    pub const MGMT_TX: u8 = 7;
    pub const TX_FETCH_RESP: u8 = 11;
}

/// `enum htt_tlv_t2h_msg_type`: the numbering on the wire for TLV firmware (`htt_tlv_t2h_msg_types[]` maps it to the
/// generic enum). Values 0x13 and 0x18 and up are not mapped; `ath10k_htt_t2h_msg_handler` drops them.
pub mod t2h {
    pub const VERSION_CONF: u8 = 0x00;
    pub const RX_IND: u8 = 0x01;
    pub const RX_FLUSH: u8 = 0x02;
    pub const PEER_MAP: u8 = 0x03;
    pub const PEER_UNMAP: u8 = 0x04;
    pub const RX_ADDBA: u8 = 0x05;
    pub const RX_DELBA: u8 = 0x06;
    pub const TX_COMPL_IND: u8 = 0x07;
    pub const PKTLOG: u8 = 0x08;
    pub const STATS_CONF: u8 = 0x09;
    pub const RX_FRAG_IND: u8 = 0x0a;
    pub const SEC_IND: u8 = 0x0b;
    pub const RC_UPDATE_IND: u8 = 0x0c;
    pub const TX_INSPECT_IND: u8 = 0x0d;
    pub const MGMT_TX_COMPL_IND: u8 = 0x0e;
    pub const TX_CREDIT_UPDATE_IND: u8 = 0x0f;
    pub const RX_PN_IND: u8 = 0x10;
    pub const RX_OFFLOAD_DELIVER_IND: u8 = 0x11;
    pub const RX_IN_ORD_PADDR_IND: u8 = 0x12;
    pub const WDI_IPA_OP_RESPONSE: u8 = 0x14;
    pub const CHAN_CHANGE: u8 = 0x15;
    pub const RX_OFLD_PKT_ERR: u8 = 0x16;
    pub const TEST: u8 = 0x17;
    /// `HTT_TLV_T2H_NUM_MSGS`: types at or above this are dropped.
    pub const NUM_MSGS: u8 = 0x18;
}

/// Why a message was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HttError {
    /// Shorter than the fixed part of the message.
    Short,
    /// The counts in the message promise more bytes than there are.
    Truncated,
    /// Not the message type the parser is for.
    WrongType,
    /// An address does not fit the 32-bit variant.
    AddressTooWide,
}

// ---- host to target --------------------------------------------------------------------------------------------

/// `ath10k_htt_h2t_ver_req_msg`: the message type and three bytes of padding (`sizeof(htt_cmd_hdr) +
/// sizeof(htt_ver_req)` = 4).
pub fn ver_req() -> [u8; 4] {
    [h2t::VERSION_REQ, 0, 0, 0]
}

/// `HTT_RX_RING_FLAGS_*`.
pub mod rx_ring_flags {
    pub const MAC80211_HDR: u16 = 1 << 0;
    pub const MSDU_PAYLOAD: u16 = 1 << 1;
    pub const PPDU_START: u16 = 1 << 2;
    pub const PPDU_END: u16 = 1 << 3;
    pub const MPDU_START: u16 = 1 << 4;
    pub const MPDU_END: u16 = 1 << 5;
    pub const MSDU_START: u16 = 1 << 6;
    pub const MSDU_END: u16 = 1 << 7;
    pub const RX_ATTENTION: u16 = 1 << 8;
    pub const FRAG_INFO: u16 = 1 << 9;
    pub const UNICAST_RX: u16 = 1 << 10;
    pub const MULTICAST_RX: u16 = 1 << 11;
    pub const CTRL_RX: u16 = 1 << 12;
    pub const MGMT_RX: u16 = 1 << 13;
    pub const NULL_RX: u16 = 1 << 14;
    pub const PHY_DATA_RX: u16 = 1 << 15;
    /// What `ath10k_htt_send_rx_ring_cfg_64` sets: all sixteen.
    pub const ALL: u16 = 0xffff;
}

/// `struct htt_rx_ring_rx_desc_offsets`: where each part of the rx descriptor starts, in 4-byte units.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RxDescOffsets {
    pub mac80211_hdr_offset: u16,
    pub msdu_payload_offset: u16,
    pub ppdu_start_offset: u16,
    pub ppdu_end_offset: u16,
    pub mpdu_start_offset: u16,
    pub mpdu_end_offset: u16,
    pub msdu_start_offset: u16,
    pub msdu_end_offset: u16,
    pub rx_attention_offset: u16,
    pub frag_info_offset: u16,
}

/// `sizeof(struct htt_rx_desc_v2)` = `wcn3990_rx_desc_ops.rx_desc_size`; the payload starts right after.
pub const RX_DESC_SIZE: usize = 360;

/// `ath10k_rx_desc_wcn3990_get_offsets`, i.e. `offsetof(struct htt_rx_desc_v2, ...) / 4` for each part.
pub const WCN3990_RX_DESC_OFFSETS: RxDescOffsets = RxDescOffsets {
    mac80211_hdr_offset: 296 / 4,
    msdu_payload_offset: 360 / 4,
    ppdu_start_offset: 108 / 4,
    ppdu_end_offset: 148 / 4,
    mpdu_start_offset: 16 / 4,
    mpdu_end_offset: 104 / 4,
    msdu_start_offset: 28 / 4,
    msdu_end_offset: 48 / 4,
    rx_attention_offset: 1,
    frag_info_offset: 8 / 4,
};

impl RxDescOffsets {
    pub fn to_le_bytes(&self) -> [u8; 20] {
        let w = [
            self.mac80211_hdr_offset,
            self.msdu_payload_offset,
            self.ppdu_start_offset,
            self.ppdu_end_offset,
            self.mpdu_start_offset,
            self.mpdu_end_offset,
            self.msdu_start_offset,
            self.msdu_end_offset,
            self.rx_attention_offset,
            self.frag_info_offset,
        ];
        let mut out = [0u8; 20];
        for (i, v) in w.iter().enumerate() {
            out[2 * i..2 * i + 2].copy_from_slice(&v.to_le_bytes());
        }
        out
    }
}

/// One receive ring for [`rx_ring_setup_64`] / [`rx_ring_setup_32`] (`struct htt_rx_ring_setup_ring64`/`32`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RxRingSetup {
    /// Where the firmware mirrors its write index.
    pub fw_idx_shadow_reg_paddr: u64,
    pub rx_ring_base_paddr: u64,
    /// `htt->rx_ring.size`, the number of entries.
    pub rx_ring_len: u16,
    pub rx_ring_bufsize: u16,
    pub flags: u16,
    /// The firmware index at the time of the setup (`*alloc_idx.vaddr`).
    pub fw_idx_init_val: u16,
    pub offsets: RxDescOffsets,
}

impl RxRingSetup {
    /// The setup `ath10k_htt_send_rx_ring_cfg_64` sends for the WCN3990: all rx flags, 2048-byte buffers.
    pub fn wcn3990(
        fw_idx_shadow_reg_paddr: u64,
        rx_ring_base_paddr: u64,
        fw_idx_init_val: u16,
    ) -> RxRingSetup {
        RxRingSetup {
            fw_idx_shadow_reg_paddr,
            rx_ring_base_paddr,
            rx_ring_len: RX_RING_SIZE,
            rx_ring_bufsize: RX_BUF_SIZE,
            flags: rx_ring_flags::ALL,
            fw_idx_init_val,
            offsets: WCN3990_RX_DESC_OFFSETS,
        }
    }
}

/// The RX_RING_CFG message with one 64-bit ring (48 bytes).
pub fn rx_ring_setup_64(r: &RxRingSetup) -> Vec<u8> {
    let mut v = alloc::vec![h2t::RX_RING_CFG, 1, 0, 0];
    v.extend_from_slice(&r.fw_idx_shadow_reg_paddr.to_le_bytes());
    v.extend_from_slice(&r.rx_ring_base_paddr.to_le_bytes());
    v.extend_from_slice(&r.rx_ring_len.to_le_bytes());
    v.extend_from_slice(&r.rx_ring_bufsize.to_le_bytes());
    v.extend_from_slice(&r.flags.to_le_bytes());
    v.extend_from_slice(&r.fw_idx_init_val.to_le_bytes());
    v.extend_from_slice(&r.offsets.to_le_bytes());
    v
}

/// The RX_RING_CFG message with one 32-bit ring (40 bytes), for firmware with 32-bit addresses (not the WCN3990;
/// the caller supplies that chip's own descriptor offsets).
pub fn rx_ring_setup_32(r: &RxRingSetup) -> Result<Vec<u8>, HttError> {
    let shadow = u32::try_from(r.fw_idx_shadow_reg_paddr).map_err(|_| HttError::AddressTooWide)?;
    let base = u32::try_from(r.rx_ring_base_paddr).map_err(|_| HttError::AddressTooWide)?;
    let mut v = alloc::vec![h2t::RX_RING_CFG, 1, 0, 0];
    v.extend_from_slice(&shadow.to_le_bytes());
    v.extend_from_slice(&base.to_le_bytes());
    v.extend_from_slice(&r.rx_ring_len.to_le_bytes());
    v.extend_from_slice(&r.rx_ring_bufsize.to_le_bytes());
    v.extend_from_slice(&r.flags.to_le_bytes());
    v.extend_from_slice(&r.fw_idx_init_val.to_le_bytes());
    v.extend_from_slice(&r.offsets.to_le_bytes());
    Ok(v)
}

/// `HTT_DATA_TX_DESC_FLAGS0_*`.
pub mod tx_flags0 {
    pub const MAC_HDR_PRESENT: u8 = 1 << 0;
    pub const NO_AGGR: u8 = 1 << 1;
    pub const NO_ENCRYPT: u8 = 1 << 2;
    pub const NO_CLASSIFY: u8 = 1 << 3;
    pub const PKT_TYPE_MASK: u8 = 0xe0;
    pub const PKT_TYPE_LSB: u32 = 5;
}

/// `HTT_DATA_TX_DESC_FLAGS1_*`.
pub mod tx_flags1 {
    pub const VDEV_ID_MASK: u16 = 0x003f;
    pub const VDEV_ID_LSB: u32 = 0;
    pub const EXT_TID_MASK: u16 = 0x07c0;
    pub const EXT_TID_LSB: u32 = 6;
    pub const POSTPONED: u16 = 1 << 11;
    pub const MORE_IN_BATCH: u16 = 1 << 12;
    pub const CKSUM_L3_OFFLOAD: u16 = 1 << 13;
    pub const CKSUM_L4_OFFLOAD: u16 = 1 << 14;
    pub const TX_COMPLETE: u16 = 1 << 15;
}

/// `enum ath10k_hw_txrx_mode`, the packet type of a tx descriptor.
pub mod txrx_mode {
    pub const RAW: u8 = 0;
    pub const NATIVE_WIFI: u8 = 1;
    pub const ETHERNET: u8 = 2;
    pub const MGMT: u8 = 3;
}

/// `HTT_DATA_TX_EXT_TID_*`.
pub mod ext_tid {
    pub const NON_QOS_MCAST_BCAST: u8 = 16;
    pub const MGMT: u8 = 17;
    pub const INVALID: u8 = 31;
}

/// `flags0` as `ath10k_htt_tx_64` composes it: the packet type, `MAC_HDR_PRESENT` for raw and native wifi frames
/// (and management frames), `NO_ENCRYPT` when hardware crypto is off for this frame.
pub fn tx_flags0_for(mode: u8, no_encrypt: bool) -> u8 {
    let mut f = (mode << tx_flags0::PKT_TYPE_LSB) & tx_flags0::PKT_TYPE_MASK;
    if mode == txrx_mode::RAW || mode == txrx_mode::NATIVE_WIFI || mode == txrx_mode::MGMT {
        f |= tx_flags0::MAC_HDR_PRESENT;
    }
    if no_encrypt {
        f |= tx_flags0::NO_ENCRYPT;
    }
    f
}

/// `flags1`: vdev id (6 bits), extended TID (5 bits), the always-set `POSTPONED` that keeps the firmware from
/// asking for tx inspection, and the checksum offload bits.
pub fn tx_flags1_for(vdev_id: u8, tid: u8, cksum_offload: bool) -> u16 {
    let mut f = ((u16::from(vdev_id) << tx_flags1::VDEV_ID_LSB) & tx_flags1::VDEV_ID_MASK)
        | ((u16::from(tid) << tx_flags1::EXT_TID_LSB) & tx_flags1::EXT_TID_MASK)
        | tx_flags1::POSTPONED;
    if cksum_offload {
        f |= tx_flags1::CKSUM_L3_OFFLOAD | tx_flags1::CKSUM_L4_OFFLOAD;
    }
    f
}

/// `struct htt_data_tx_desc_64` (19 bytes, packed). The peer id is always `HTT_INVALID_PEERID`: the firmware picks
/// the peer. With `freq` (`ath10k_mac_tx_frm_has_freq`) the last word is the offchannel union
/// `{peerid: u16, freq: u16}`, else a plain 32-bit peer id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DataTxDesc64 {
    pub flags0: u8,
    pub flags1: u16,
    pub len: u16,
    /// The MSDU id (index of the tx buffer).
    pub id: u16,
    pub frags_paddr: u64,
    pub freq: Option<u16>,
}

/// `sizeof(struct htt_data_tx_desc_64)`.
pub const DATA_TX_DESC_64_LEN: usize = 19;

impl DataTxDesc64 {
    pub fn to_le_bytes(&self) -> [u8; DATA_TX_DESC_64_LEN] {
        let mut b = [0u8; DATA_TX_DESC_64_LEN];
        b[0] = self.flags0;
        b[1..3].copy_from_slice(&self.flags1.to_le_bytes());
        b[3..5].copy_from_slice(&self.len.to_le_bytes());
        b[5..7].copy_from_slice(&self.id.to_le_bytes());
        b[7..15].copy_from_slice(&self.frags_paddr.to_le_bytes());
        b[15..17].copy_from_slice(&INVALID_PEERID.to_le_bytes());
        b[17..19].copy_from_slice(&self.freq.unwrap_or(0).to_le_bytes());
        b
    }
}

/// What `ath10k_htt_tx_64` puts in front of the first bytes of the MSDU: an HTC header for `htc_eid` (no flags, no
/// sequence number: the HTT endpoint has no credit flow control), the HTT message type and the descriptor. The
/// first `prefetch.len()` bytes of the frame follow, zero padded to a multiple of 4 (the C rounds the length up
/// and the HTC length covers it). The rest of the MSDU is referenced by `frags_paddr`, not copied.
pub fn tx_frm_64(htc_eid: u8, desc: &DataTxDesc64, prefetch: &[u8]) -> Vec<u8> {
    let prefetch_len = prefetch.len().div_ceil(4) * 4;
    let htc_len = (1 + DATA_TX_DESC_64_LEN + prefetch_len) as u16;
    let mut v = Vec::with_capacity(crate::htc::HDR_LEN + htc_len as usize);
    v.extend_from_slice(
        &crate::htc::Hdr {
            eid: htc_eid,
            len: htc_len,
            ..crate::htc::Hdr::default()
        }
        .to_le_bytes(),
    );
    v.push(h2t::TX_FRM);
    v.extend_from_slice(&desc.to_le_bytes());
    v.extend_from_slice(prefetch);
    v.resize(v.len() + prefetch_len - prefetch.len(), 0);
    v
}

/// `HTT_MSDU_EXT_DESC_FLAG_*_CSUM_ENABLE_64` (bits of `tso_flag[3]`).
pub const MSDU_CHECKSUM_ENABLE_64: u32 = 0x001f_0000;
/// `sizeof(struct htt_msdu_ext_desc_64)`.
pub const MSDU_EXT_DESC_64_LEN: usize = 72;

/// `struct htt_msdu_ext_desc_64` as `ath10k_htt_tx_64` fills it with a single fragment (the other five stay zero):
/// five TSO words, `ip_identification`, `flags`, a reserved byte, then six 8-byte fragment descriptors
/// `{paddr_lo: u32, paddr_hi: u16, len: u16}`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MsduExtDesc64 {
    pub paddr: u64,
    pub len: u16,
    pub checksum_offload: bool,
}

impl MsduExtDesc64 {
    pub fn to_le_bytes(&self) -> [u8; MSDU_EXT_DESC_64_LEN] {
        let mut b = [0u8; MSDU_EXT_DESC_64_LEN];
        if self.checksum_offload {
            b[12..16].copy_from_slice(&MSDU_CHECKSUM_ENABLE_64.to_le_bytes());
        }
        b[24..28].copy_from_slice(&(self.paddr as u32).to_le_bytes());
        b[28..30].copy_from_slice(&((self.paddr >> 32) as u16).to_le_bytes());
        b[30..32].copy_from_slice(&self.len.to_le_bytes());
        b
    }
}

// ---- target to host --------------------------------------------------------------------------------------------

/// `ath10k_htt_t2h_msg_handler`'s switch, over the TLV numbering. Only the messages this crate has parsers for or
/// that the driver acts on are named.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum T2h {
    VersionConf,
    RxInd,
    RxFlush,
    PeerMap,
    PeerUnmap,
    RxAddba,
    RxDelba,
    TxComplInd,
    Pktlog,
    StatsConf,
    RxFragInd,
    SecInd,
    RcUpdateInd,
    TxInspectInd,
    MgmtTxCompl,
    TxCreditUpdateInd,
    RxPnInd,
    RxOffloadDeliverInd,
    RxInOrdPaddrInd,
    WdiIpaOpResponse,
    ChanChange,
    RxOfldPktErr,
    Test,
    /// Reserved or beyond the table: the C logs and drops it.
    Unsupported(u8),
}

impl T2h {
    pub fn from_wire(t: u8) -> T2h {
        match t {
            t2h::VERSION_CONF => T2h::VersionConf,
            t2h::RX_IND => T2h::RxInd,
            t2h::RX_FLUSH => T2h::RxFlush,
            t2h::PEER_MAP => T2h::PeerMap,
            t2h::PEER_UNMAP => T2h::PeerUnmap,
            t2h::RX_ADDBA => T2h::RxAddba,
            t2h::RX_DELBA => T2h::RxDelba,
            t2h::TX_COMPL_IND => T2h::TxComplInd,
            t2h::PKTLOG => T2h::Pktlog,
            t2h::STATS_CONF => T2h::StatsConf,
            t2h::RX_FRAG_IND => T2h::RxFragInd,
            t2h::SEC_IND => T2h::SecInd,
            t2h::RC_UPDATE_IND => T2h::RcUpdateInd,
            t2h::TX_INSPECT_IND => T2h::TxInspectInd,
            t2h::MGMT_TX_COMPL_IND => T2h::MgmtTxCompl,
            t2h::TX_CREDIT_UPDATE_IND => T2h::TxCreditUpdateInd,
            t2h::RX_PN_IND => T2h::RxPnInd,
            t2h::RX_OFFLOAD_DELIVER_IND => T2h::RxOffloadDeliverInd,
            t2h::RX_IN_ORD_PADDR_IND => T2h::RxInOrdPaddrInd,
            t2h::WDI_IPA_OP_RESPONSE => T2h::WdiIpaOpResponse,
            t2h::CHAN_CHANGE => T2h::ChanChange,
            t2h::RX_OFLD_PKT_ERR => T2h::RxOfldPktErr,
            t2h::TEST => T2h::Test,
            other => T2h::Unsupported(other),
        }
    }
}

/// `struct htt_resp_hdr`: the first byte of every target-to-host message. Returns the classification and the
/// message after the header.
pub fn parse_resp(buf: &[u8]) -> Result<(T2h, &[u8]), HttError> {
    match buf.split_first() {
        Some((t, rest)) => Ok((T2h::from_wire(*t), rest)),
        None => Err(HttError::Short),
    }
}

/// `HTT_T2H_MSG_TYPE_VERSION_CONF` (`struct htt_ver_resp`): `(major, minor)`. The body is `minor`, `major`, one
/// reserved byte.
pub fn parse_version_conf(buf: &[u8]) -> Result<(u8, u8), HttError> {
    let (t, body) = parse_resp(buf)?;
    if t != T2h::VersionConf {
        return Err(HttError::WrongType);
    }
    if body.len() < 3 {
        return Err(HttError::Short);
    }
    Ok((body[1], body[0]))
}

/// `HTT_T2H_MSG_TYPE_TX_CREDIT_UPDATE_IND`: the signed credit delta (`HTT_TX_CREDIT_DELTA_ABS_GET`, sign bit 8).
pub fn parse_tx_credit_update(buf: &[u8]) -> Result<i32, HttError> {
    if buf.len() < 4 {
        return Err(HttError::Short);
    }
    if buf[0] != t2h::TX_CREDIT_UPDATE_IND {
        return Err(HttError::WrongType);
    }
    let word = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    let delta = ((word & 0xffff_0000) >> 16) as i32;
    Ok(if (word & 0x0000_0100) != 0 {
        -delta
    } else {
        delta
    })
}

/// `HTT_DATA_TX_STATUS_*`.
pub mod tx_status {
    pub const OK: u8 = 0;
    pub const DISCARD: u8 = 1;
    pub const NO_ACK: u8 = 2;
    pub const POSTPONE: u8 = 3;
}

/// `HTT_TX_CMPL_FLAG_*` and the WCN3990 variants (`HTT_TX_DATA_*`) of `flags2`.
pub mod tx_compl_flags2 {
    pub const DATA_RSSI: u8 = 1 << 0;
    pub const PPID_PRESENT: u8 = 1 << 1;
    pub const PA_PRESENT: u8 = 1 << 2;
    pub const PPDU_DURATION_PRESENT: u8 = 1 << 3;
    /// `HTT_TX_DATA_RSSI_ENABLE_WCN3990` (same bit as `PPDU_DURATION_PRESENT`).
    pub const RSSI_ENABLE_WCN3990: u8 = 1 << 3;
    /// `HTT_TX_DATA_APPEND_RETRIES`, `HTT_TX_DATA_APPEND_TIMESTAMP`: each shifts the RSSI array by two words.
    pub const APPEND_RETRIES: u8 = 1 << 0;
    pub const APPEND_TIMESTAMP: u8 = 1 << 1;
}

/// The outcome the driver derives from the status bits (`ath10k_htt_rx_tx_compl_ind`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TxResult {
    Ack,
    NoAck,
    Discard,
}

/// One MSDU of a tx completion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TxCompletionMsdu {
    pub msdu_id: u16,
    /// The ACK RSSI word, when the indication carries them (WCN3990 `RSSI_ENABLE` bit).
    pub ack_rssi: Option<u16>,
}

/// `HTT_T2H_MSG_TYPE_TX_COMPL_IND` (`struct htt_data_tx_completion`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TxCompletion {
    /// The 3-bit status as sent.
    pub raw_status: u8,
    pub result: TxResult,
    pub tid: u8,
    pub tid_invalid: bool,
    /// Raw `flags2`: see [`tx_compl_flags2`]; bit 3 is both "ACK RSSI present" and "PPDU durations present".
    pub flags2: u8,
    pub msdus: Vec<TxCompletionMsdu>,
}

/// Parse a tx completion indication: header byte, `flags` (status in bits 2:0, tid in 6:3, tid invalid in bit 7),
/// MSDU count, `flags2`, then little-endian 16-bit words: the MSDU ids (padded to an even count with 0xffff), then
/// optionally the two-word retries record and the two-word timestamp record, then one ACK RSSI per MSDU.
/// Every word the reader needs must be inside the message (the C does not check).
pub fn parse_tx_completion(buf: &[u8]) -> Result<TxCompletion, HttError> {
    if buf.len() < 4 {
        return Err(HttError::Short);
    }
    if buf[0] != t2h::TX_COMPL_IND {
        return Err(HttError::WrongType);
    }
    let flags = buf[1];
    let n = usize::from(buf[2]);
    let flags2 = buf[3];
    let words = &buf[4..];
    let word = |i: usize| -> Result<u16, HttError> {
        words
            .get(2 * i..2 * i + 2)
            .map(|w| u16::from_le_bytes([w[0], w[1]]))
            .ok_or(HttError::Truncated)
    };
    let raw_status = flags & 0x07;
    let result = match raw_status {
        tx_status::OK => TxResult::Ack,
        tx_status::NO_ACK => TxResult::NoAck,
        _ => TxResult::Discard,
    };
    let rssi = flags2 & tx_compl_flags2::RSSI_ENABLE_WCN3990 != 0;
    let mut pad = 0usize;
    if rssi {
        if flags2 & tx_compl_flags2::APPEND_RETRIES != 0 {
            pad += 2;
        }
        if flags2 & tx_compl_flags2::APPEND_TIMESTAMP != 0 {
            pad += 2;
        }
    }
    let mut msdus = Vec::with_capacity(n);
    for k in 0..n {
        let msdu_id = word(k)?;
        let ack_rssi = if rssi {
            // for an odd count the firmware pads the id array with one word
            Some(word(if n & 1 != 0 {
                n + k + 1 + pad
            } else {
                n + k + pad
            })?)
        } else {
            None
        };
        msdus.push(TxCompletionMsdu { msdu_id, ack_rssi });
    }
    Ok(TxCompletion {
        raw_status,
        result,
        tid: (flags >> 3) & 0x0f,
        tid_invalid: flags & 0x80 != 0,
        flags2,
        msdus,
    })
}

/// `HTT_RX_IN_ORD_IND_INFO_*`.
pub mod in_ord_info {
    pub const TID_MASK: u8 = 0x1f;
    pub const OFFLOAD: u8 = 0x20;
    pub const FRAG: u8 = 0x40;
}

/// One entry of the 64-bit address list of an in-order indication (`struct htt_rx_in_ord_msdu_desc_ext`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InOrdMsdu {
    /// Address of the rx buffer the host posted (the host maps it back to its skb).
    pub paddr: u64,
    pub msdu_len: u16,
    pub fw_desc: u8,
}

/// `HTT_T2H_MSG_TYPE_RX_IN_ORD_PADDR_IND` (`struct htt_rx_in_ord_ind`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InOrdInd {
    pub tid: u8,
    /// The buffers hold offloaded MSDUs (a [`RxOffloadMsdu`] header and payload, no rx descriptor).
    pub offload: bool,
    pub frag: bool,
    pub peer_id: u16,
    pub vdev_id: u8,
    pub msdus: Vec<InOrdMsdu>,
}

/// Bytes before the address list: the type byte and the 7-byte `struct htt_rx_in_ord_ind`.
pub const IN_ORD_IND_HDR_LEN: usize = 8;
/// `sizeof(struct htt_rx_in_ord_msdu_desc_ext)` (the 32-bit `htt_rx_in_ord_msdu_desc` is 8).
pub const IN_ORD_MSDU_DESC_EXT_LEN: usize = 12;

/// Parse an in-order indication with 64-bit descriptors. The count must fit the message; the C only checks
/// 8 bytes per entry, which lets a 12-byte-per-entry list run past the message.
pub fn parse_rx_in_ord_ind(buf: &[u8]) -> Result<InOrdInd, HttError> {
    if buf.len() < IN_ORD_IND_HDR_LEN {
        return Err(HttError::Short);
    }
    if buf[0] != t2h::RX_IN_ORD_PADDR_IND {
        return Err(HttError::WrongType);
    }
    let info = buf[1];
    let peer_id = u16::from_le_bytes([buf[2], buf[3]]);
    let vdev_id = buf[4];
    let count = usize::from(u16::from_le_bytes([buf[6], buf[7]]));
    let list = &buf[IN_ORD_IND_HDR_LEN..];
    if list.len() / IN_ORD_MSDU_DESC_EXT_LEN < count {
        return Err(HttError::Truncated);
    }
    let msdus = list
        .as_chunks::<IN_ORD_MSDU_DESC_EXT_LEN>()
        .0
        .iter()
        .take(count)
        .map(|d| InOrdMsdu {
            paddr: u64::from_le_bytes([d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7]]),
            msdu_len: u16::from_le_bytes([d[8], d[9]]),
            fw_desc: d[10],
        })
        .collect();
    Ok(InOrdInd {
        tid: info & in_ord_info::TID_MASK,
        offload: info & in_ord_info::OFFLOAD != 0,
        frag: info & in_ord_info::FRAG != 0,
        peer_id,
        vdev_id,
        msdus,
    })
}

/// `sizeof(struct htt_rx_offload_msdu)`.
pub const RX_OFFLOAD_MSDU_HDR_LEN: usize = 7;

/// `struct htt_rx_offload_msdu` plus its payload, as it sits in a receive buffer of an offload indication
/// (`ath10k_htt_rx_h_rx_offload`). The frame is native wifi, already decrypted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RxOffloadMsdu<'a> {
    pub peer_id: u16,
    pub vdev_id: u8,
    pub tid: u8,
    pub fw_desc: u8,
    pub payload: &'a [u8],
}

/// Parse an offloaded MSDU from the start of a receive buffer. The length is the device's claim; it must fit the
/// buffer you pass (the C compares it to the skb's tailroom).
pub fn parse_rx_offload_msdu(buf: &[u8]) -> Result<RxOffloadMsdu<'_>, HttError> {
    if buf.len() < RX_OFFLOAD_MSDU_HDR_LEN {
        return Err(HttError::Short);
    }
    let msdu_len = usize::from(u16::from_le_bytes([buf[0], buf[1]]));
    let rest = &buf[RX_OFFLOAD_MSDU_HDR_LEN..];
    if rest.len() < msdu_len {
        return Err(HttError::Truncated);
    }
    Ok(RxOffloadMsdu {
        peer_id: u16::from_le_bytes([buf[2], buf[3]]),
        vdev_id: buf[4],
        tid: buf[5],
        fw_desc: buf[6],
        payload: &rest[..msdu_len],
    })
}

/// `struct htt_rx_offload_ind` (`HTT_T2H_MSG_TYPE_RX_OFFLOAD_DELIVER_IND`): the number of MSDUs; they arrive as
/// separate buffers like an in-order indication's.
pub fn parse_rx_offload_ind(buf: &[u8]) -> Result<u16, HttError> {
    if buf.len() < 4 {
        return Err(HttError::Short);
    }
    if buf[0] != t2h::RX_OFFLOAD_DELIVER_IND {
        return Err(HttError::WrongType);
    }
    Ok(u16::from_le_bytes([buf[2], buf[3]]))
}

// ---- for crate::driver: the rest of the setup and the data path indications -----------------------------------------

/// `sizeof(struct htt_frag_desc_bank_cfg64)`.
pub const FRAG_DESC_BANK_CFG_64_LEN: usize = 63;
/// `ATH10K_HTT_MAX_NUM_AMPDU_DEFAULT` and `ATH10K_HTT_MAX_NUM_AMSDU_DEFAULT`.
pub const MAX_NUM_AMPDU_DEFAULT: u8 = 64;
pub const MAX_NUM_AMSDU_DEFAULT: u8 = 3;
/// `sizeof(struct ath10k_htt_txbuf_64)` and where its HTC header starts: two 8-byte fragment entries in front (unused
/// on this chip, whose fragments live in the continuous descriptor bank).
pub const TXBUF_64_LEN: usize = 44;
pub const TXBUF_64_HTC_HDR_OFF: usize = 16;
/// What the first gather item of an MSDU carries: `htc_hdr`, `cmd_hdr`, `cmd_tx`.
pub const TXBUF_64_ITEM_LEN: usize = crate::htc::HDR_LEN + 1 + DATA_TX_DESC_64_LEN;
/// `htt->prefetch_len`, `ath10k_htt_init`: 802.11 + qos + ht, 802.1q, llc snap, ip4 dscp or ip6 priority.
pub const PREFETCH_LEN: usize = 36 + 4 + 8 + 2;

/// `ath10k_htt_send_frag_desc_bank_cfg_64`: one bank at `base_paddr` of `struct htt_msdu_ext_desc_64`, ids
/// `0..=max_id`. The queue state fields stay zero except the two constants: `ATH10K_FW_FEATURE_PEER_FLOW_CONTROL` is
/// not set by this firmware (`driver::fw_ie`), so `info` is 0. `record_size` is `HTT_TX_Q_STATE_ENTRY_SIZE` (1).
pub fn frag_desc_bank_cfg_64(base_paddr: u64, max_id: u16) -> Vec<u8> {
    let mut v = alloc::vec![0u8; 1 + FRAG_DESC_BANK_CFG_64_LEN];
    v[0] = h2t::FRAG_DESC_BANK_CFG;
    v[1] = 0; // info
    v[2] = 1; // num_banks
    v[3] = MSDU_EXT_DESC_64_LEN as u8;
    v[4..12].copy_from_slice(&base_paddr.to_le_bytes());
    // bank_id[0] at +36: min 0, max
    v[38..40].copy_from_slice(&max_id.to_le_bytes());
    // q_state at +52: paddr, num_peers, num_tids, record_size, record_multiplier (0), pad
    v[60] = 1;
    v
}

/// `ath10k_htt_h2t_aggr_cfg_msg_v2`: `None` for the limits the C refuses.
pub fn aggr_cfg_v2(max_subfrms_ampdu: u8, max_subfrms_amsdu: u8) -> Option<[u8; 4]> {
    if max_subfrms_ampdu == 0 || max_subfrms_ampdu > 64 {
        return None;
    }
    if max_subfrms_amsdu == 0 || max_subfrms_amsdu > 31 {
        return None;
    }
    Some([h2t::AGGR_CFG, max_subfrms_ampdu, max_subfrms_amsdu, 0])
}

/// The first gather item `ath10k_htt_tx_64` sends: HTC header (with `prefetch_len`, already rounded up to 4, in its
/// length), HTT message type and descriptor. The prefetch bytes are the second item, straight from the frame.
pub fn tx_item_64(
    htc_eid: u8,
    desc: &DataTxDesc64,
    prefetch_len: usize,
) -> [u8; TXBUF_64_ITEM_LEN] {
    let mut b = [0u8; TXBUF_64_ITEM_LEN];
    let hdr = crate::htc::Hdr {
        eid: htc_eid,
        len: (1 + DATA_TX_DESC_64_LEN + prefetch_len) as u16,
        ..crate::htc::Hdr::default()
    };
    b[..crate::htc::HDR_LEN].copy_from_slice(&hdr.to_le_bytes());
    b[crate::htc::HDR_LEN] = h2t::TX_FRM;
    b[crate::htc::HDR_LEN + 1..].copy_from_slice(&desc.to_le_bytes());
    b
}

/// `HTT_T2H_MSG_TYPE_PEER_MAP` (`struct htt_rx_peer_map`): `(vdev_id, peer_id, addr)`.
pub fn parse_peer_map(buf: &[u8]) -> Result<(u8, u16, [u8; 6]), HttError> {
    if buf.first() != Some(&t2h::PEER_MAP) {
        return Err(if buf.is_empty() {
            HttError::Short
        } else {
            HttError::WrongType
        });
    }
    if buf.len() < 10 {
        return Err(HttError::Short);
    }
    let mut a = [0u8; 6];
    a.copy_from_slice(&buf[4..10]);
    Ok((buf[1], u16::from_le_bytes([buf[2], buf[3]]), a))
}

/// `HTT_T2H_MSG_TYPE_PEER_UNMAP`: the peer id.
pub fn parse_peer_unmap(buf: &[u8]) -> Result<u16, HttError> {
    if buf.first() != Some(&t2h::PEER_UNMAP) {
        return Err(if buf.is_empty() {
            HttError::Short
        } else {
            HttError::WrongType
        });
    }
    if buf.len() < 4 {
        return Err(HttError::Short);
    }
    Ok(u16::from_le_bytes([buf[2], buf[3]]))
}

/// `HTT_MGMT_TX_STATUS_*`.
pub mod mgmt_tx_status {
    pub const OK: u32 = 0;
    pub const RETRY: u32 = 1;
    pub const DROP: u32 = 2;
}

/// `HTT_T2H_MSG_TYPE_MGMT_TX_COMPLETION` (`struct htt_mgmt_tx_completion`): `(desc_id, status)`.
pub fn parse_mgmt_tx_compl(buf: &[u8]) -> Result<(u32, u32), HttError> {
    if buf.first() != Some(&t2h::MGMT_TX_COMPL_IND) {
        return Err(if buf.is_empty() {
            HttError::Short
        } else {
            HttError::WrongType
        });
    }
    if buf.len() < 12 {
        return Err(HttError::Short);
    }
    let w = |o: usize| u32::from_le_bytes([buf[o], buf[o + 1], buf[o + 2], buf[o + 3]]);
    Ok((w(4), w(8)))
}

/// `HTT_T2H_MSG_TYPE_SEC_IND` (`struct htt_security_indication`): `(unicast, security type, peer id)`.
pub fn parse_sec_ind(buf: &[u8]) -> Result<(bool, u8, u16), HttError> {
    if buf.first() != Some(&t2h::SEC_IND) {
        return Err(if buf.is_empty() {
            HttError::Short
        } else {
            HttError::WrongType
        });
    }
    if buf.len() < 4 {
        return Err(HttError::Short);
    }
    Ok((
        buf[1] & 0x80 != 0,
        buf[1] & 0x7f,
        u16::from_le_bytes([buf[2], buf[3]]),
    ))
}

/// `HTT_T2H_MSG_TYPE_RX_IND`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RxInd {
    pub tid: u8,
    pub peer_id: u16,
    /// The sum of `mpdu_count` over the ranges: how many MPDUs wait in the rx ring
    /// (`ath10k_htt_rx_proc_rx_ind_ll`, which ignores each range's status).
    pub mpdu_count: u32,
}

/// Where the ranges start: the type byte, `struct htt_rx_indication_hdr` (7), `..._ppdu` (36), `..._prefix` (4).
const RX_IND_PREFIX_END: usize = 1 + 7 + 36 + 4;

/// Parse an rx indication. The C trusts `num_mpdu_ranges` and `fw_rx_desc_bytes`; here the ranges must be inside the
/// message.
pub fn parse_rx_ind(buf: &[u8]) -> Result<RxInd, HttError> {
    if buf.len() < RX_IND_PREFIX_END {
        return Err(HttError::Short);
    }
    if buf[0] != t2h::RX_IND {
        return Err(HttError::WrongType);
    }
    let info1 = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
    let n = (info1 >> 24) as usize;
    let fw_bytes = usize::from(u16::from_le_bytes([buf[44], buf[45]]));
    let off = RX_IND_PREFIX_END + fw_bytes.div_ceil(4) * 4;
    let ranges = buf.get(off..off + n * 4).ok_or(HttError::Truncated)?;
    Ok(RxInd {
        tid: buf[1] & 0x1f,
        peer_id: u16::from_le_bytes([buf[2], buf[3]]),
        mpdu_count: ranges
            .as_chunks::<4>()
            .0
            .iter()
            .map(|r| u32::from(r[0]))
            .sum(),
    })
}

/// The WCN3990 rx descriptor (`struct htt_rx_desc_v2`, `rx_desc.h`) at the start of every rx buffer that holds one.
pub mod rx_desc {
    pub const ATTENTION: usize = 4;
    pub const RING2_MORE_COUNT: usize = 10;
    pub const MPDU_START_INFO0: usize = 16;
    pub const MPDU_START_PN: usize = 20;
    pub const MPDU_START_INFO1: usize = 24;
    pub const MSDU_START_INFO0: usize = 28;
    pub const MSDU_START_INFO1: usize = 36;
    pub const MSDU_END_INFO0: usize = 64;
    pub const PPDU_RSSI_CHAINS: usize = 108;
    pub const PPDU_RSSI_COMB: usize = 124;
    pub const PPDU_INFO1: usize = 128;
    pub const PPDU_INFO2: usize = 132;
    pub const PPDU_INFO3: usize = 136;
    pub const HDR_STATUS: usize = 296;
    pub const HDR_STATUS_LEN: usize = 64;

    /// `RX_ATTENTION_FLAGS_*`.
    pub const ATT_FIRST_MPDU: u32 = 1 << 0;
    pub const ATT_LAST_MPDU: u32 = 1 << 1;
    pub const ATT_MCAST_BCAST: u32 = 1 << 2;
    pub const ATT_PEER_IDX_INVALID: u32 = 1 << 3;
    pub const ATT_MGMT_TYPE: u32 = 1 << 8;
    pub const ATT_MSDU_LENGTH_ERR: u32 = 1 << 17;
    pub const ATT_MPDU_LENGTH_ERR: u32 = 1 << 27;
    pub const ATT_TKIP_MIC_ERR: u32 = 1 << 28;
    pub const ATT_DECRYPT_ERR: u32 = 1 << 29;
    pub const ATT_FCS_ERR: u32 = 1 << 30;
    pub const ATT_MSDU_DONE: u32 = 1 << 31;
    /// `RX_MSDU_END_INFO0_*`.
    pub const MSDU_END_FIRST: u32 = 1 << 14;
    pub const MSDU_END_LAST: u32 = 1 << 15;
    /// `RX_MSDU_START_INFO0_MSDU_LENGTH_MASK`.
    pub const MSDU_LENGTH_MASK: u32 = 0x3fff;
    /// `enum htt_rx_mpdu_encrypt_type`, the top nibble of `mpdu_start.info0`.
    pub const ENCRYPT_TKIP_WPA: u32 = 4;
    pub const ENCRYPT_AES_CCM_WPA2: u32 = 6;
    pub const ENCRYPT_NONE: u32 = 7;
    /// `enum rx_msdu_decap_format`.
    pub const DECAP_RAW: u32 = 0;
    pub const DECAP_NATIVE_WIFI: u32 = 1;
    pub const DECAP_ETHERNET2: u32 = 2;
    pub const DECAP_8023_SNAP: u32 = 3;
    /// `enum htt_rx_preamble_type`.
    pub const PREAMBLE_LEGACY: u32 = 4;
    pub const PREAMBLE_HT: u32 = 8;
    pub const PREAMBLE_HT_TXBF: u32 = 9;
    pub const PREAMBLE_VHT: u32 = 12;
    pub const PREAMBLE_VHT_TXBF: u32 = 13;
    /// The invalid per-chain value `ath10k_htt_rx_h_signal` skips.
    pub const RSSI_CHAIN_INVALID: u8 = 0x80;
}
