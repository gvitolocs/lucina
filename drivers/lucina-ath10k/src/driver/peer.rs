//! `ath10k_peer_assoc_prepare`: from what the association said to `struct wmi_peer_assoc_complete_arg`.
//!
//! Derived from Linux `drivers/net/wireless/ath/ath10k/mac.c` (SPDX ISC; Copyright (c) 2005-2011 Atheros Communications
//! Inc., 2011-2017 Qualcomm Atheros, Inc., 2018-2019 The Linux Foundation, Qualcomm Technologies, Inc. and/or its
//! subsidiaries). Functions in the order `_prepare` calls them. There is no `bitrate_mask` here: Linux's default mask
//! lets everything through, so the masked branches are gone. `IEEE80211_HT_MAX_AMPDU_FACTOR` is 13.

use alloc::vec::Vec;

use crate::wmi_tlv::{self, PeerAssocArg, peer_flags, phy_mode, rate_caps};

/// The channel width the station uses towards the AP (`sta->deflink.bandwidth`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Bandwidth {
    #[default]
    Bw20,
    Bw40,
    Bw80,
    Bw160,
}

/// `struct ieee80211_sta_ht_cap`, as the association response gives it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HtInfo {
    /// `cap`, the HT capability info.
    pub cap: u16,
    /// A-MPDU parameters, bits 1:0.
    pub ampdu_factor: u8,
    /// A-MPDU parameters, bits 4:2.
    pub ampdu_density: u8,
    pub rx_mask: [u8; 10],
}

/// `struct ieee80211_sta_vht_cap`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VhtInfo {
    pub cap: u32,
    pub rx_mcs_map: u16,
    pub rx_highest: u16,
    pub tx_mcs_map: u16,
    pub tx_highest: u16,
}

/// What the association established with the AP.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PeerAssoc {
    pub bssid: [u8; 6],
    pub aid: u16,
    /// The AP's capability info (`bss_conf.assoc_capability`).
    pub capability: u16,
    /// The supported rates in units of 500 kb/s, as in the IE; the basic-rate bit is ignored.
    pub rates: Vec<u8>,
    pub ht: Option<HtInfo>,
    pub vht: Option<VhtInfo>,
    pub bandwidth: Bandwidth,
    /// The AP has WMM (`sta->wme`).
    pub wme: bool,
    /// The BSS has an RSN IE.
    pub rsn: bool,
    /// The BSS has a WPA IE.
    pub wpa: bool,
    pub freq: u32,
}

const IEEE80211_HT_CAP_LDPC_CODING: u16 = 0x0001;
const IEEE80211_HT_CAP_SM_PS: u16 = 0x000c;
const IEEE80211_HT_CAP_SM_PS_SHIFT: u16 = 2;
const IEEE80211_HT_CAP_SGI_20: u16 = 0x0020;
const IEEE80211_HT_CAP_SGI_40: u16 = 0x0040;
const IEEE80211_HT_CAP_TX_STBC: u16 = 0x0080;
const IEEE80211_HT_CAP_RX_STBC: u16 = 0x0300;
const IEEE80211_HT_CAP_RX_STBC_SHIFT: u16 = 8;
const IEEE80211_HT_MAX_AMPDU_FACTOR: u32 = 13;
const IEEE80211_VHT_CAP_MAX_A_MPDU_LENGTH_EXPONENT_MASK: u32 = 0x0380_0000;
const IEEE80211_VHT_CAP_MAX_A_MPDU_LENGTH_EXPONENT_SHIFT: u32 = 23;
const IEEE80211_VHT_CAP_SUPP_CHAN_WIDTH_MASK: u32 = 0x0000_000c;
const IEEE80211_VHT_CAP_SUPP_CHAN_WIDTH_160_80PLUS80MHZ: u32 = 0x0000_0008;
const IEEE80211_VHT_MCS_NOT_SUPPORTED: u16 = 3;

/// `ath10k_parse_mpdudensity`.
fn parse_mpdudensity(d: u8) -> u32 {
    match d {
        0 => 0,
        1..=3 => 1,
        4 => 2,
        5 => 4,
        6 => 8,
        7 => 16,
        _ => 0,
    }
}

/// `ath10k_mac_bitrate_to_rate`, from the 500 kb/s unit (Linux has 100 kb/s; the division by 5 is already done). The
/// four 802.11b rates carry bit 7 (`ath10k_mac_bitrate_is_cck`).
pub fn rate_code(rate_500k: u8) -> u8 {
    let r = rate_500k & 0x7f;
    if matches!(r, 2 | 4 | 11 | 22) {
        r | 0x80
    } else {
        r
    }
}

/// `ath10k_mac_sta_has_ofdm_only`, whose name misleads: there is a rate above the CCK ones.
fn has_ofdm(rates: &[u8]) -> bool {
    rates.iter().any(|r| !matches!(r & 0x7f, 2 | 4 | 11 | 22))
}

/// The derived rate lists and the argument of [`wmi_tlv::gen_peer_assoc`], minus the slices.
pub(crate) struct Prepared {
    pub legacy: Vec<u8>,
    pub ht_rates: Vec<u8>,
    pub flags: u32,
    pub rate_caps: u32,
    pub max_mpdu: u32,
    pub mpdu_density: u32,
    pub nss: u32,
    pub ht_caps: u32,
    pub vht_caps: u32,
    pub phymode: u32,
    pub vht: (u32, u32, u32, u32),
    /// The `WMI_PEER_SMPS_*` value of `ath10k_setup_peer_smps`, `None` without HT.
    pub smps: Option<u32>,
}

/// `sta->deflink.rx_nss` as mac80211's `ieee80211_sta_init_nss` works it out: what the rate sets offer, limited by our
/// chains. HT counts the non-zero bytes of the first four rx_mask bytes (not the highest index), VHT takes the highest
/// MCS-map entry that is not "not supported".
fn rx_nss(a: &PeerAssoc, chains: u32) -> u32 {
    let mut nss = 0u32;
    if let Some(ht) = &a.ht {
        for m in ht.rx_mask.iter().take(4) {
            if *m != 0 {
                nss += 1;
            }
        }
    }
    if let Some(v) = &a.vht {
        for i in 0..8u16 {
            if (v.rx_mcs_map >> (2 * i)) & 3 != IEEE80211_VHT_MCS_NOT_SUPPORTED {
                nss = nss.max(u32::from(i) + 1);
            }
        }
    }
    nss.max(1).min(chains.max(1))
}

/// `ath10k_peer_assoc_h_*`.
pub(crate) fn prepare(a: &PeerAssoc, chains: u32) -> Prepared {
    let is_2g = a.freq < 3000;
    // _h_basic
    let mut p = Prepared {
        legacy: a.rates.iter().map(|r| rate_code(*r)).collect(),
        ht_rates: Vec::new(),
        flags: peer_flags::AUTH,
        rate_caps: 0,
        max_mpdu: 0,
        mpdu_density: 0,
        nss: 1,
        ht_caps: 0,
        vht_caps: 0,
        phymode: 0,
        vht: (0, 0, 0, 0),
        smps: None,
    };
    let sta_nss = rx_nss(a, chains);
    // _h_crypto
    if a.rsn || a.wpa {
        p.flags |= peer_flags::NEED_PTK_4_WAY;
    }
    if a.wpa {
        p.flags |= peer_flags::NEED_GTK_2_WAY;
    }
    // _h_ht
    if let Some(ht) = &a.ht {
        p.flags |= peer_flags::HT;
        p.max_mpdu = (1 << (IEEE80211_HT_MAX_AMPDU_FACTOR + u32::from(ht.ampdu_factor & 3))) - 1;
        p.mpdu_density = parse_mpdudensity(ht.ampdu_density);
        p.ht_caps = u32::from(ht.cap);
        p.rate_caps |= rate_caps::HT;
        if ht.cap & IEEE80211_HT_CAP_LDPC_CODING != 0 {
            p.flags |= peer_flags::LDPC;
        }
        if a.bandwidth >= Bandwidth::Bw40 {
            p.flags |= peer_flags::BW40;
            p.rate_caps |= rate_caps::CW40;
        }
        if ht.cap & (IEEE80211_HT_CAP_SGI_20 | IEEE80211_HT_CAP_SGI_40) != 0 {
            p.rate_caps |= rate_caps::SGI;
        }
        if ht.cap & IEEE80211_HT_CAP_TX_STBC != 0 {
            p.rate_caps |= rate_caps::TX_STBC;
            p.flags |= peer_flags::STBC;
        }
        if ht.cap & IEEE80211_HT_CAP_RX_STBC != 0 {
            let stbc = (ht.cap & IEEE80211_HT_CAP_RX_STBC) >> IEEE80211_HT_CAP_RX_STBC_SHIFT;
            p.rate_caps |= u32::from(stbc) << rate_caps::RX_STBC_SHIFT;
            p.flags |= peer_flags::STBC;
        }
        if ht.rx_mask[1] != 0 && ht.rx_mask[2] != 0 {
            p.rate_caps |= rate_caps::TS;
        } else if ht.rx_mask[1] != 0 {
            p.rate_caps |= rate_caps::DS;
        }
        let mut max_nss = 0u32;
        for i in 0..80u32 {
            if ht.rx_mask[(i / 8) as usize] & (1 << (i % 8)) != 0 {
                max_nss = i / 8 + 1;
                p.ht_rates.push(i as u8);
            }
        }
        if p.ht_rates.is_empty() {
            // "Firmware asserts if such situation occurs."
            p.ht_rates.extend(0..8);
        } else {
            p.nss = sta_nss.min(max_nss);
        }
        // ath10k_smps_map, in ath10k_setup_peer_smps
        let smps = (ht.cap & IEEE80211_HT_CAP_SM_PS) >> IEEE80211_HT_CAP_SM_PS_SHIFT;
        p.smps = Some(match smps {
            0 => 1, // static
            1 => 2, // dynamic
            _ => 0, // none
        });
    }
    // _h_phymode
    p.phymode = u32::from(if is_2g {
        if a.vht.is_some() {
            if a.bandwidth == Bandwidth::Bw40 {
                phy_mode::MODE_11AC_VHT40
            } else {
                phy_mode::MODE_11AC_VHT20
            }
        } else if a.ht.is_some() {
            if a.bandwidth == Bandwidth::Bw40 {
                phy_mode::MODE_11NG_HT40
            } else {
                phy_mode::MODE_11NG_HT20
            }
        } else if has_ofdm(&a.rates) {
            phy_mode::MODE_11G
        } else {
            phy_mode::MODE_11B
        }
    } else if let Some(v) = &a.vht {
        match a.bandwidth {
            Bandwidth::Bw160 => {
                if v.cap & IEEE80211_VHT_CAP_SUPP_CHAN_WIDTH_MASK
                    == IEEE80211_VHT_CAP_SUPP_CHAN_WIDTH_160_80PLUS80MHZ
                {
                    phy_mode::MODE_11AC_VHT80_80
                } else {
                    phy_mode::MODE_11AC_VHT160
                }
            }
            Bandwidth::Bw80 => phy_mode::MODE_11AC_VHT80,
            Bandwidth::Bw40 => phy_mode::MODE_11AC_VHT40,
            Bandwidth::Bw20 => phy_mode::MODE_11AC_VHT20,
        }
    } else if a.ht.is_some() {
        if a.bandwidth >= Bandwidth::Bw40 {
            phy_mode::MODE_11NA_HT40
        } else {
            phy_mode::MODE_11NA_HT20
        }
    } else {
        phy_mode::MODE_11A
    });
    // _h_vht (with an all-ones mask ath10k_peer_assoc_h_vht_limit changes nothing)
    if let Some(v) = &a.vht {
        p.flags |= peer_flags::VHT;
        // ath10k_peer_assoc_h_vht ORs in `peer_flags->vht_2g` on a 2.4 GHz channel, but the TLV map
        // (wmi_tlv_peer_flags_map) has no vht_2g entry, so on this firmware the flag is 0 and nothing is added.
        // WMI_PEER_VHT_2G (0x0800_0000) is the non-TLV value; here that bit is PMF.
        p.vht_caps = v.cap;
        let factor = (v.cap & IEEE80211_VHT_CAP_MAX_A_MPDU_LENGTH_EXPONENT_MASK)
            >> IEEE80211_VHT_CAP_MAX_A_MPDU_LENGTH_EXPONENT_SHIFT;
        p.max_mpdu = p
            .max_mpdu
            .max((1 << (IEEE80211_HT_MAX_AMPDU_FACTOR + factor)) - 1);
        if a.bandwidth == Bandwidth::Bw80 {
            p.flags |= peer_flags::BW80;
        }
        if a.bandwidth == Bandwidth::Bw160 {
            p.flags |= peer_flags::BW160;
        }
        let mut max_nss = 0u32;
        for i in 0..8u16 {
            if (v.rx_mcs_map >> (2 * i)) & 3 != IEEE80211_VHT_MCS_NOT_SUPPORTED {
                max_nss = u32::from(i) + 1;
            }
        }
        p.nss = sta_nss.min(max_nss);
        p.vht = (
            u32::from(v.rx_highest),
            u32::from(v.rx_mcs_map),
            u32::from(v.tx_highest),
            u32::from(v.tx_mcs_map),
        );
    }
    // _h_qos, the station case
    if a.wme {
        p.flags |= peer_flags::QOS;
    }
    p
}

impl Prepared {
    pub fn arg<'a>(&'a self, a: &PeerAssoc, vdev_id: u32) -> PeerAssocArg<'a> {
        wmi_tlv::PeerAssocArg {
            addr: a.bssid,
            vdev_id,
            reassoc: false,
            aid: u32::from(a.aid),
            flags: self.flags,
            caps: u32::from(a.capability),
            // ath10k_peer_assoc_h_listen_intval: 1 for a station, a firmware power save workaround
            listen_intval: 1,
            ht_caps: self.ht_caps,
            max_mpdu: self.max_mpdu,
            mpdu_density: self.mpdu_density,
            rate_caps: self.rate_caps,
            num_spatial_streams: self.nss,
            vht_caps: self.vht_caps,
            phymode: self.phymode,
            legacy_rates: &self.legacy,
            ht_rates: &self.ht_rates,
            vht_rx_max_rate: self.vht.0,
            vht_rx_mcs_set: self.vht.1,
            vht_tx_max_rate: self.vht.2,
            vht_tx_mcs_set: self.vht.3,
        }
    }
}
