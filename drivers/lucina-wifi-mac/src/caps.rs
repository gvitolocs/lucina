//! What the hardware can do, and what ath10k does with the peer's capabilities for `peer_assoc`
//! (`ath10k_get_ht_cap`, `ieee80211_ht_cap_ie_to_sta_ht_cap`, `ath10k_peer_assoc_h_ht`).

use crate::ie::{
    HT_CAP_DSSSCCK40, HT_CAP_GRN_FLD, HT_CAP_LDPC_CODING, HT_CAP_LSIG_TXOP_PROT, HT_CAP_MAX_AMSDU,
    HT_CAP_RX_STBC, HT_CAP_RX_STBC_SHIFT, HT_CAP_SGI_20, HT_CAP_SGI_40, HT_CAP_SM_PS,
    HT_CAP_SM_PS_DISABLED, HT_CAP_SM_PS_SHIFT, HT_CAP_SUP_WIDTH_20_40, HT_CAP_TX_STBC,
    HT_MCS_TX_DEFINED, HT_MCS_TX_MAX_STREAMS_MASK, HT_MCS_TX_MAX_STREAMS_SHIFT, HT_MCS_TX_RX_DIFF,
    HT_MCS_TX_UNEQUAL_MODULATION, HtCaps, VhtCaps,
};
use alloc::vec::Vec;

/// The bits of the VHT capabilities field (`ieee80211.h`).
pub const VHT_CAP_MAX_MPDU_MASK: u32 = 3;
pub const VHT_CAP_SUPP_CHAN_WIDTH_160: u32 = 1 << 2;
pub const VHT_CAP_SUPP_CHAN_WIDTH_160_80P80: u32 = 2 << 2;
pub const VHT_CAP_SUPP_CHAN_WIDTH_MASK: u32 = 3 << 2;
pub const VHT_CAP_RXLDPC: u32 = 1 << 4;
pub const VHT_CAP_SHORT_GI_80: u32 = 1 << 5;
pub const VHT_CAP_SHORT_GI_160: u32 = 1 << 6;
pub const VHT_CAP_TXSTBC: u32 = 1 << 7;
pub const VHT_CAP_RXSTBC_MASK: u32 = 7 << 8;
pub const VHT_CAP_SU_BEAMFORMER: u32 = 1 << 11;
pub const VHT_CAP_SU_BEAMFORMEE: u32 = 1 << 12;
pub const VHT_CAP_BEAMFORMEE_STS_SHIFT: u32 = 13;
pub const VHT_CAP_BEAMFORMEE_STS_MASK: u32 = 7 << 13;
pub const VHT_CAP_SOUNDING_DIM_SHIFT: u32 = 16;
pub const VHT_CAP_SOUNDING_DIM_MASK: u32 = 7 << 16;
pub const VHT_CAP_MU_BEAMFORMER: u32 = 1 << 19;
pub const VHT_CAP_MU_BEAMFORMEE: u32 = 1 << 20;
pub const VHT_CAP_VHT_TXOP_PS: u32 = 1 << 21;
pub const VHT_CAP_HTC_VHT: u32 = 1 << 22;
pub const VHT_CAP_MAX_A_MPDU_MASK: u32 = 7 << 23;
pub const VHT_CAP_LINK_ADAPT_MASK: u32 = 3 << 26;
pub const VHT_CAP_RX_ANTENNA_PATTERN: u32 = 1 << 28;
pub const VHT_CAP_TX_ANTENNA_PATTERN: u32 = 1 << 29;

/// The bits of WMI `ht_cap_info` (`wmi.h`).
pub const WMI_HT_CAP_ENABLED: u32 = 0x0001;
pub const WMI_HT_CAP_HT20_SGI: u32 = 0x0002;
pub const WMI_HT_CAP_DYNAMIC_SMPS: u32 = 0x0004;
pub const WMI_HT_CAP_TX_STBC: u32 = 0x0008;
pub const WMI_HT_CAP_RX_STBC: u32 = 0x0030;
pub const WMI_HT_CAP_RX_STBC_MASK_SHIFT: u32 = 4;
pub const WMI_HT_CAP_LDPC: u32 = 0x0040;
pub const WMI_HT_CAP_L_SIG_TXOP_PROT: u32 = 0x0080;
pub const WMI_HT_CAP_HT40_SGI: u32 = 0x0800;
pub const WMI_HT_CAP_RX_LDPC: u32 = 0x1000;
pub const WMI_HT_CAP_TX_LDPC: u32 = 0x2000;
pub const WMI_VHT_CAP_MAX_MPDU_LEN_MASK: u32 = 0x0003;

/// `peer_rate_caps` bits (`WMI_RC_*`).
pub const WMI_RC_DS_FLAG: u32 = 0x01;
pub const WMI_RC_CW40_FLAG: u32 = 0x02;
pub const WMI_RC_SGI_FLAG: u32 = 0x04;
pub const WMI_RC_HT_FLAG: u32 = 0x08;
pub const WMI_RC_TX_STBC_FLAG: u32 = 0x20;
pub const WMI_RC_RX_STBC_FLAG_S: u32 = 6;
pub const WMI_RC_TS_FLAG: u32 = 0x200;

/// `ATH10K_MAX_HW_LISTEN_INTERVAL`.
pub const DEFAULT_LISTEN_INTERVAL: u16 = 5;

/// What the driver tells the station about the hardware.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HwCaps {
    pub band_2ghz: bool,
    pub band_5ghz: bool,
    /// HT capabilities to advertise, `None` for no HT. [`HwCaps::ht_from_wmi`] builds this as ath10k does.
    pub ht: Option<HtCaps>,
    /// VHT capabilities to advertise, `None` for no VHT: [`HwCaps::vht_from_wmi`].
    pub vht: Option<VhtCaps>,
    /// The device reports whether a management frame was acked (`REPORTS_TX_ACK_STATUS`). If so, call
    /// [`crate::Station::on_mgmt_tx_status`] and retries follow mac80211's short and long timeouts.
    pub reports_tx_ack: bool,
    /// Set the Spectrum Management capability when the AP does (`SPECTRUM_MGMT`, which ath10k sets).
    pub spectrum_mgmt: bool,
    /// Whether the driver can start a 40 MHz vdev. Off, the station associates as 20 MHz only.
    pub ht40: bool,
    pub listen_interval: u16,
    /// The firmware watches the AP (`CONNECTION_MONITOR`, which ath10k sets) and it may not pass beacons up: no silence
    /// timer here, as in `ieee80211_sta_reset_beacon_monitor`, only [`crate::Station::on_beacon_loss`] starts probes.
    pub connection_monitor: bool,
}

impl HwCaps {
    /// ath10k's capabilities on a dual-band device, from the WMI service-ready values.
    pub fn from_wmi(
        ht_cap_info: u32,
        vht_cap_info: u32,
        num_rf_chains: u8,
        rx_chainmask: u8,
        tx_chainmask: u8,
    ) -> Self {
        Self {
            band_2ghz: true,
            band_5ghz: true,
            ht: Self::ht_from_wmi(
                ht_cap_info,
                vht_cap_info,
                num_rf_chains,
                rx_chainmask,
                tx_chainmask,
            ),
            vht: Self::vht_from_wmi(vht_cap_info, num_rf_chains, tx_chainmask),
            reports_tx_ack: true,
            spectrum_mgmt: true,
            ht40: true,
            listen_interval: DEFAULT_LISTEN_INTERVAL,
            connection_monitor: true,
        }
    }

    /// `ath10k_create_vht_cap()`, with `ath10k_mac_get_vht_cap_bf_sts` and `_bf_sound_dim`. The firmware's `vht_cap_info` has the
    /// layout of the element's field. All-zero info is a chip without VHT.
    pub fn vht_from_wmi(vht_cap_info: u32, num_rf_chains: u8, tx_chainmask: u8) -> Option<VhtCaps> {
        const VHT_MCS_0_9: u16 = 2;
        const VHT_MCS_NONE: u16 = 3;
        if vht_cap_info == 0 {
            return None;
        }
        let mut cap = vht_cap_info;
        if cap & (VHT_CAP_SU_BEAMFORMEE | VHT_CAP_MU_BEAMFORMEE) != 0 {
            let sts = (cap & VHT_CAP_BEAMFORMEE_STS_MASK) >> VHT_CAP_BEAMFORMEE_STS_SHIFT;
            // "If firmware does not deliver to host number of space-time streams supported, assume it support up to 4 BF STS".
            let sts = if sts == 0 { 3 } else { sts };
            cap |= (sts << VHT_CAP_BEAMFORMEE_STS_SHIFT) & VHT_CAP_BEAMFORMEE_STS_MASK;
        }
        if cap & (VHT_CAP_SU_BEAMFORMER | VHT_CAP_MU_BEAMFORMER) != 0 {
            let dim = (cap & VHT_CAP_SOUNDING_DIM_MASK) >> VHT_CAP_SOUNDING_DIM_SHIFT;
            let dim = if dim == 0 { 1 } else { dim };
            cap |= (dim << VHT_CAP_SOUNDING_DIM_SHIFT) & VHT_CAP_SOUNDING_DIM_MASK;
        }
        let mut map = 0u16;
        for i in 0..8u8 {
            let there = i < num_rf_chains && tx_chainmask & (1 << i) != 0;
            map |= (if there { VHT_MCS_0_9 } else { VHT_MCS_NONE }) << (i * 2);
        }
        if tx_chainmask <= 1 {
            cap &= !VHT_CAP_TXSTBC;
        }
        Some(VhtCaps {
            cap,
            rx_mcs_map: map,
            rx_highest: 0,
            tx_mcs_map: map,
            tx_highest: 0,
        })
    }

    /// `ath10k_get_ht_cap()`.
    pub fn ht_from_wmi(
        ht_cap_info: u32,
        vht_cap_info: u32,
        num_rf_chains: u8,
        rx_chainmask: u8,
        tx_chainmask: u8,
    ) -> Option<HtCaps> {
        if ht_cap_info & WMI_HT_CAP_ENABLED == 0 {
            return None;
        }
        let mut cap = HT_CAP_SUP_WIDTH_20_40 | HT_CAP_DSSSCCK40 | HT_CAP_SM_PS_DISABLED;
        if ht_cap_info & WMI_HT_CAP_HT20_SGI != 0 {
            cap |= HT_CAP_SGI_20;
        }
        if ht_cap_info & WMI_HT_CAP_HT40_SGI != 0 {
            cap |= HT_CAP_SGI_40;
        }
        if ht_cap_info & WMI_HT_CAP_DYNAMIC_SMPS != 0 {
            // The C ORs dynamic (1) into disabled (3), which stays 3: the same is done here on purpose.
            cap |= 1 << HT_CAP_SM_PS_SHIFT;
        }
        if ht_cap_info & WMI_HT_CAP_TX_STBC != 0 && tx_chainmask > 1 {
            cap |= HT_CAP_TX_STBC;
        }
        if ht_cap_info & WMI_HT_CAP_RX_STBC != 0 {
            let stbc = (ht_cap_info & WMI_HT_CAP_RX_STBC) >> WMI_HT_CAP_RX_STBC_MASK_SHIFT;
            cap |= ((stbc as u16) << HT_CAP_RX_STBC_SHIFT) & HT_CAP_RX_STBC;
        }
        if ht_cap_info & WMI_HT_CAP_LDPC != 0
            || (ht_cap_info & WMI_HT_CAP_RX_LDPC != 0 && ht_cap_info & WMI_HT_CAP_TX_LDPC != 0)
        {
            cap |= HT_CAP_LDPC_CODING;
        }
        if ht_cap_info & WMI_HT_CAP_L_SIG_TXOP_PROT != 0 {
            cap |= HT_CAP_LSIG_TXOP_PROT;
        }
        if vht_cap_info & WMI_VHT_CAP_MAX_MPDU_LEN_MASK != 0 {
            cap |= HT_CAP_MAX_AMSDU;
        }
        let mut rx_mask = [0u8; 10];
        for (i, mask) in rx_mask
            .iter_mut()
            .enumerate()
            .take(num_rf_chains.min(8) as usize)
        {
            if rx_chainmask & (1 << i) != 0 {
                *mask = 0xff;
            }
        }
        Some(HtCaps {
            cap_info: cap,
            // IEEE80211_HT_MAX_AMPDU_64K (3), IEEE80211_HT_MPDU_DENSITY_8 (6).
            ampdu_params: 3 | (6 << 2),
            rx_mask,
            rx_highest: 0,
            tx_params: HT_MCS_TX_DEFINED,
        })
    }
}

/// `ieee80211_vht_cap_ie_to_sta_vht_cap()`: what the peer's advertised VHT means given ours. `None` if no rate is left to send it.
pub fn intersect_vht(own: &VhtCaps, peer: &VhtCaps) -> Option<VhtCaps> {
    const AS_IS: u32 = VHT_CAP_RXLDPC
        | VHT_CAP_VHT_TXOP_PS
        | VHT_CAP_HTC_VHT
        | VHT_CAP_MAX_A_MPDU_MASK
        | VHT_CAP_LINK_ADAPT_MASK
        | VHT_CAP_RX_ANTENNA_PATTERN
        | VHT_CAP_TX_ANTENNA_PATTERN;
    const NONE: u16 = 3;
    let mut cap = peer.cap & AS_IS;
    cap |= (peer.cap & VHT_CAP_MAX_MPDU_MASK).min(own.cap & VHT_CAP_MAX_MPDU_MASK);
    match own.cap & VHT_CAP_SUPP_CHAN_WIDTH_MASK {
        VHT_CAP_SUPP_CHAN_WIDTH_160 => cap |= peer.cap & VHT_CAP_SUPP_CHAN_WIDTH_160,
        VHT_CAP_SUPP_CHAN_WIDTH_160_80P80 => cap |= peer.cap & VHT_CAP_SUPP_CHAN_WIDTH_MASK,
        _ => {}
    }
    cap |= peer.cap & own.cap & (VHT_CAP_SHORT_GI_80 | VHT_CAP_SHORT_GI_160);
    if own.cap & VHT_CAP_SU_BEAMFORMEE != 0 {
        cap |= peer.cap & (VHT_CAP_SU_BEAMFORMER | VHT_CAP_SOUNDING_DIM_MASK);
    }
    if own.cap & VHT_CAP_SU_BEAMFORMER != 0 {
        cap |= peer.cap & (VHT_CAP_SU_BEAMFORMEE | VHT_CAP_BEAMFORMEE_STS_MASK);
    }
    if own.cap & VHT_CAP_MU_BEAMFORMER != 0 {
        cap |= peer.cap & VHT_CAP_MU_BEAMFORMEE;
    }
    if own.cap & VHT_CAP_MU_BEAMFORMEE != 0 {
        cap |= peer.cap & VHT_CAP_MU_BEAMFORMER;
    }
    if own.cap & VHT_CAP_TXSTBC != 0 {
        cap |= peer.cap & VHT_CAP_RXSTBC_MASK;
    }
    if own.cap & VHT_CAP_RXSTBC_MASK != 0 {
        cap |= peer.cap & VHT_CAP_TXSTBC;
    }
    let (mut rx_map, mut tx_map) = (0u16, 0u16);
    for i in 0..8u8 {
        let field = |map: u16| (map >> (i * 2)) & NONE;
        let (own_rx, own_tx) = (field(own.rx_mcs_map), field(own.tx_mcs_map));
        let (mut peer_rx, mut peer_tx) = (field(peer.rx_mcs_map), field(peer.tx_mcs_map));
        if peer_tx != NONE {
            peer_tx = if own_rx == NONE { NONE } else { peer_tx.min(own_rx) };
        }
        if peer_rx != NONE {
            peer_rx = if own_tx == NONE { NONE } else { peer_rx.min(own_tx) };
        }
        rx_map |= peer_rx << (i * 2);
        tx_map |= peer_tx << (i * 2);
    }
    if rx_map == 0xffff {
        return None;
    }
    Some(VhtCaps {
        cap,
        rx_mcs_map: rx_map,
        rx_highest: peer.rx_highest,
        tx_mcs_map: tx_map,
        // `IEEE80211_VHT_EXT_NSS_BW_CAPABLE` goes: ath10k does not set `SUPPORTS_VHT_EXT_NSS_BW`.
        tx_highest: peer.tx_highest & !(1 << 13),
    })
}

/// `ieee80211_ht_cap_ie_to_sta_ht_cap()`: what the peer's advertised HT means given ours.
pub fn intersect_ht(own: &HtCaps, peer: &HtCaps) -> HtCaps {
    const COMMON: u16 = HT_CAP_LDPC_CODING
        | HT_CAP_SUP_WIDTH_20_40
        | HT_CAP_GRN_FLD
        | HT_CAP_SGI_20
        | HT_CAP_SGI_40
        | HT_CAP_DSSSCCK40;
    let mut cap = peer.cap_info & (own.cap_info | !COMMON);
    // STBC is asymmetric: our TX pairs with the peer's RX and the other way around.
    if own.cap_info & HT_CAP_TX_STBC == 0 {
        cap &= !HT_CAP_RX_STBC;
    }
    if own.cap_info & HT_CAP_RX_STBC == 0 {
        cap &= !HT_CAP_TX_STBC;
    }
    let mut result = HtCaps {
        cap_info: cap,
        ampdu_params: peer.ampdu_params,
        rx_mask: [0; 10],
        rx_highest: peer.rx_highest,
        tx_params: peer.tx_params,
    };
    if own.tx_params & HT_MCS_TX_DEFINED == 0 {
        return result;
    }
    let max_tx_streams = if own.tx_params & HT_MCS_TX_RX_DIFF != 0 {
        ((own.tx_params & HT_MCS_TX_MAX_STREAMS_MASK) >> HT_MCS_TX_MAX_STREAMS_SHIFT) as usize + 1
    } else {
        4
    };
    for i in 0..max_tx_streams {
        result.rx_mask[i] = own.rx_mask[i] & peer.rx_mask[i];
    }
    if own.tx_params & HT_MCS_TX_UNEQUAL_MODULATION != 0 {
        for i in 4..10 {
            result.rx_mask[i] = own.rx_mask[i] & peer.rx_mask[i];
        }
    }
    if own.rx_mask[4] & peer.rx_mask[4] & 1 != 0 {
        result.rx_mask[4] |= 1;
    }
    result
}

/// `ath10k_parse_mpdudensity()`: microseconds of spacing from the 802.11 code.
pub fn parse_mpdu_density(code: u8) -> u8 {
    match code {
        1..=3 => 1,
        4 => 2,
        5 => 4,
        6 => 8,
        7 => 16,
        _ => 0,
    }
}

/// `enum wmi_phy_mode`, the ones this station uses. The driver maps them to WMI's numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhyMode {
    B11,
    G11,
    A11,
    NgHt20,
    NgHt40,
    NaHt20,
    NaHt40,
    AcVht80,
}

/// What `ath10k_peer_assoc_h_ht` computes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerHt {
    /// `peer_ht_caps`: the intersected HT capability bits.
    pub cap_info: u16,
    /// `peer_max_mpdu`.
    pub max_mpdu: u32,
    /// `peer_mpdu_density`, microseconds.
    pub mpdu_density: u8,
    /// `peer_ht_rates`.
    pub mcs: Vec<u8>,
    /// `ldbc` peer flag.
    pub ldpc: bool,
    /// `bw40` peer flag.
    pub bw40: bool,
    /// `stbc` peer flag.
    pub stbc: bool,
    /// HT-derived `peer_rate_caps` bits (`WMI_RC_*`).
    pub rate_caps: u32,
    /// `peer_num_spatial_streams`.
    pub num_spatial_streams: u8,
}

/// `IEEE80211_HT_MAX_AMPDU_FACTOR`.
const HT_MAX_AMPDU_FACTOR: u32 = 13;

/// `ath10k_peer_assoc_h_ht()`, without bitrate masks (all rates on, as by default). `ht` is already intersected.
pub fn peer_ht(ht: &HtCaps, bw40: bool) -> PeerHt {
    let cap = ht.cap_info;
    let mut rate_caps = WMI_RC_HT_FLAG;
    let mut stbc = false;
    if bw40 {
        rate_caps |= WMI_RC_CW40_FLAG;
    }
    if cap & (HT_CAP_SGI_20 | HT_CAP_SGI_40) != 0 {
        rate_caps |= WMI_RC_SGI_FLAG;
    }
    if cap & HT_CAP_TX_STBC != 0 {
        rate_caps |= WMI_RC_TX_STBC_FLAG;
        stbc = true;
    }
    if cap & HT_CAP_RX_STBC != 0 {
        rate_caps |=
            ((cap & HT_CAP_RX_STBC) as u32 >> HT_CAP_RX_STBC_SHIFT) << WMI_RC_RX_STBC_FLAG_S;
        stbc = true;
    }
    if ht.rx_mask[1] != 0 && ht.rx_mask[2] != 0 {
        rate_caps |= WMI_RC_TS_FLAG;
    } else if ht.rx_mask[1] != 0 {
        rate_caps |= WMI_RC_DS_FLAG;
    }
    let mut mcs = Vec::new();
    let mut max_nss = 0u8;
    for i in 0..(10 * 8) {
        if ht.rx_mask[i / 8] & (1 << (i % 8)) != 0 {
            max_nss = (i / 8) as u8 + 1;
            mcs.push(i as u8);
        }
    }
    // "HT-enabled STAs which break the spec and have no HT capabilities RX mask": firmware asserts on an empty
    // set, and MCS 0-7 are mandatory anyway.
    let num_spatial_streams = if mcs.is_empty() {
        mcs.extend(0..8u8);
        1
    } else {
        max_nss.clamp(1, 4)
    };
    PeerHt {
        cap_info: cap,
        max_mpdu: (1u32 << (HT_MAX_AMPDU_FACTOR + ht.ampdu_factor() as u32)) - 1,
        mpdu_density: parse_mpdu_density(ht.ampdu_density()),
        mcs,
        ldpc: cap & HT_CAP_LDPC_CODING != 0,
        bw40,
        stbc,
        rate_caps,
        num_spatial_streams,
    }
}

/// The mode of `HT_CAP_SM_PS` this station advertises: always off, as `IEEE80211_SMPS_OFF`.
pub(crate) fn with_sm_ps_off(cap: u16) -> u16 {
    (cap & !HT_CAP_SM_PS) | HT_CAP_SM_PS_DISABLED
}
