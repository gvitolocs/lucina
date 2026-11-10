//! The pure parts of mac80211's MLME: its timing constants, how a connection is planned from a BSS and the hardware,
//! the association request, and what the association response means for `ath10k_peer_assoc_h_*`.

use crate::caps::{HwCaps, PeerHt, PhyMode, intersect_ht, intersect_vht, peer_ht, with_sm_ps_off};
use crate::frame::{
    CAP_ESS, CAP_PRIVACY, CAP_SHORT_PREAMBLE, CAP_SHORT_SLOT_TIME, CAP_SPECTRUM_MGMT, MacAddr,
    STYPE_ASSOC_REQ, STYPE_PROBE_REQ, mgmt_header,
};
use crate::ie::{
    AcParams, EID_HT_CAPABILITY, EID_PWR_CAPABILITY, EID_SSID, EID_SUPPORTED_CHANNELS,
    EID_VHT_CAPABILITY, HT_CAP_SGI_40, HT_CAP_SUP_WIDTH_20_40, HtCaps, HtOperation, SecondaryOffset,
    VhtCaps, VhtOperation, find, parse_wmm, put_rates, put_wmm_info, rates_of,
};
use crate::regulatory::{Band, Regulatory, WORLD_CHANNELS, rule_for_channel, rule_for_freq};
use crate::scan::{ScanResult, device_rates};
use alloc::vec::Vec;

/// `IEEE80211_AUTH_TIMEOUT`: HZ / 5, used when the hardware does not report acks.
pub const AUTH_TIMEOUT_MS: u64 = 200;
/// `IEEE80211_AUTH_TIMEOUT_LONG`: HZ / 2, waiting for the ack status.
pub const AUTH_TIMEOUT_LONG_MS: u64 = 500;
/// `IEEE80211_AUTH_TIMEOUT_SHORT`: HZ / 10, after an ack.
pub const AUTH_TIMEOUT_SHORT_MS: u64 = 100;
/// `IEEE80211_AUTH_MAX_TRIES`.
pub const AUTH_MAX_TRIES: u8 = 3;
/// `IEEE80211_ASSOC_TIMEOUT`.
pub const ASSOC_TIMEOUT_MS: u64 = 200;
/// `IEEE80211_ASSOC_TIMEOUT_LONG`.
pub const ASSOC_TIMEOUT_LONG_MS: u64 = 500;
/// `IEEE80211_ASSOC_TIMEOUT_SHORT`.
pub const ASSOC_TIMEOUT_SHORT_MS: u64 = 100;
/// `IEEE80211_ASSOC_MAX_TRIES`.
pub const ASSOC_MAX_TRIES: u8 = 3;
/// `beacon_loss_count` in mlme.c: beacon intervals of silence before probing the AP.
pub const BEACON_LOSS_COUNT: u64 = 7;
/// `max_probe_tries` in mlme.c.
pub const MAX_PROBE_TRIES: u8 = 5;
/// `probe_wait_ms` in mlme.c.
pub const PROBE_WAIT_MS: u64 = 500;
/// wpa_supplicant's limit from association to the end of the handshake on PSK (`wpa_supplicant_event_assoc`).
pub const HANDSHAKE_TIMEOUT_MS: u64 = 10_000;
/// wpa_supplicant's window for two Michael MIC failures, and how long joining stays blocked afterwards.
pub const MIC_FAILURE_WINDOW_MS: u64 = 60_000;

/// `IEEE80211_MAX_AID`.
pub const MAX_AID: u16 = 2007;

/// The operating width (`enum nl80211_chan_width`, the ones used here).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelWidth {
    NoHt20,
    Ht20,
    Ht40,
    Vht80,
}

/// What the driver needs before the first frame: `ath10k_vdev_start` and `ath10k_peer_create`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JoinInfo {
    pub bssid: MacAddr,
    pub ssid: Vec<u8>,
    pub freq_mhz: u16,
    pub channel: u8,
    pub band: Band,
    pub width: ChannelWidth,
    /// `band_center_freq1`: the primary channel, or 10 MHz off for HT40.
    pub center_freq_mhz: u16,
    /// `chan_to_phymode`: the vdev's mode.
    pub phy_mode: PhyMode,
    pub capability: u16,
    pub beacon_interval_tu: u16,
    pub dtim_period: u8,
    pub short_slot: bool,
    pub short_preamble: bool,
    /// The lower of the world rule and the AP's Country element.
    pub max_power_dbm: u8,
    /// WMI's channel flag: `NO_IR` here until a beacon lifts it.
    pub passive: bool,
    pub dfs: bool,
}

/// `struct wmi_peer_assoc_complete_arg`'s content, as far as it is not WMI's own numbering. The driver maps the
/// booleans to `ar->wmi.peer_flags` for its WMI version and sends it for the AP's peer, then `vdev_up` with `aid`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerInfo {
    pub addr: MacAddr,
    /// 0 when the AP sent a value out of range: mac80211 then leaves power save off (`broken_ap`).
    pub aid: u16,
    /// `peer_caps`, the capability field of the association response.
    pub capability: u16,
    pub listen_interval: u16,
    /// `peer_flags->auth` is always on; `need_ptk_4_way` when there is an RSN element.
    pub need_ptk_4way: bool,
    /// `peer_flags->qos`: the BSS has WMM and we asked for it.
    pub qos: bool,
    /// `peer_legacy_rates`, 500 kb/s units (`ath10k_mac_bitrate_to_rate`).
    pub legacy_rates: Vec<u8>,
    /// The subset of `legacy_rates` the AP marks basic.
    pub basic_rates: Vec<u8>,
    pub ht: Option<PeerHt>,
    /// `peer_phymode`.
    /// `peer_vht_caps`, `peer_vht_rates`: what `ieee80211_vht_cap_ie_to_sta_vht_cap` left.
    pub vht: Option<VhtCaps>,
    pub phy_mode: PhyMode,
    /// `peer_num_spatial_streams`.
    pub num_spatial_streams: u8,
    /// For `ath10k_conf_tx`: BE, BK, VI, VO. `None` when the BSS has no WMM or its set fails mac80211's checks, in
    /// which case the defaults go out instead.
    pub wmm_params: Option<[AcParams; 4]>,
}

/// How this station will talk to a BSS, decided before authenticating.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnPlan {
    /// Our HT capabilities as advertised for this BSS, `None` for a legacy association.
    pub ht: Option<HtCaps>,
    /// Our VHT capabilities, when the BSS runs 80 MHz and every channel in them is allowed.
    pub vht: Option<VhtCaps>,
    pub width: ChannelWidth,
    pub center_freq_mhz: u16,
    pub wmm: bool,
    /// Rates to advertise: device rates the AP also lists, as `ieee80211_assoc_add_rates` does ("some APs don't
    /// like getting a superset of their rates").
    pub rates: Vec<u8>,
}

/// Plans a connection. `None` if the AP lists rates and none is common.
pub fn plan_connection(bss: &ScanResult, hw: &HwCaps, reg: &Regulatory) -> Option<ConnPlan> {
    let wmm = bss.wmm.is_some();
    let rates: Vec<u8> = if bss.supported_rates.is_empty() {
        Vec::from(device_rates(bss.band))
    } else {
        device_rates(bss.band)
            .iter()
            .copied()
            .filter(|r| bss.supported_rates.iter().any(|a| a & 0x7f == *r))
            .collect()
    };
    if rates.is_empty() {
        return None;
    }

    let ap_ht = bss.ht_cap.as_deref().and_then(HtCaps::parse);
    let ap_oper = bss.ht_oper.as_deref().and_then(HtOperation::parse);
    let mut width = ChannelWidth::NoHt20;
    let mut center_freq_mhz = bss.freq_mhz;
    let mut ht = None;
    let mut vht = None;
    // mac80211 `ieee80211_determine_ap_chan`: HT needs the AP's HT capabilities and operation and WMM.
    if let (Some(own), Some(ap), Some(oper), true) = (hw.ht.as_ref(), ap_ht, ap_oper, wmm) {
        width = ChannelWidth::Ht20;
        let mut cap = with_sm_ps_off(own.cap_info);
        match ht40_center(bss, &ap, &oper, own, hw, reg) {
            Some(center) => {
                width = ChannelWidth::Ht40;
                center_freq_mhz = center;
                if let Some(center) = vht80_center(bss, hw, reg) {
                    width = ChannelWidth::Vht80;
                    center_freq_mhz = center;
                    vht = hw.vht;
                }
            }
            // "If 40 MHz was disabled associate as though we weren't capable of 40 MHz -- some broken APs will
            // never fall back to trying to transmit in 20 MHz."
            None => cap &= !(HT_CAP_SUP_WIDTH_20_40 | HT_CAP_SGI_40),
        }
        ht = Some(HtCaps {
            cap_info: cap,
            ..*own
        });
    }
    Some(ConnPlan {
        ht,
        vht,
        width,
        center_freq_mhz,
        wmm,
        rates,
    })
}

/// mac80211 `ieee80211_determine_ap_chan` for 80 MHz: the AP's VHT capabilities and operation, the primary channel inside the 80 MHz, and
/// all four channels allowed by the rules that hold here. Only 80 MHz: the AP may offer more, which this chip does not do.
fn vht80_center(bss: &ScanResult, hw: &HwCaps, reg: &Regulatory) -> Option<u16> {
    hw.vht.as_ref()?;
    bss.vht_cap.as_deref().and_then(VhtCaps::parse)?;
    let oper = bss.vht_oper.as_deref().and_then(VhtOperation::parse)?;
    if oper.chan_width != 1 || bss.band != Band::Ghz5 {
        return None;
    }
    let center = 5000 + 5 * u16::from(oper.center_seg0);
    if !(center - 30..=center + 30).contains(&bss.freq_mhz) {
        return None;
    }
    for freq in [center - 30, center - 10, center + 10, center + 30] {
        rule_for_freq(freq)?;
        reg.channel(freq)?;
    }
    Some(center)
}

fn ht40_center(
    bss: &ScanResult,
    ap: &HtCaps,
    oper: &HtOperation,
    own: &HtCaps,
    hw: &HwCaps,
    reg: &Regulatory,
) -> Option<u16> {
    if !hw.ht40
        || own.cap_info & HT_CAP_SUP_WIDTH_20_40 == 0
        || ap.cap_info & HT_CAP_SUP_WIDTH_20_40 == 0
        || !oper.sta_chan_width_any
    {
        return None;
    }
    let (secondary, center) = match oper.secondary_offset {
        SecondaryOffset::None => return None,
        SecondaryOffset::Above => (bss.channel.checked_add(4)?, bss.freq_mhz + 10),
        SecondaryOffset::Below => (bss.channel.checked_sub(4)?, bss.freq_mhz - 10),
    };
    let rule = rule_for_channel(bss.band, secondary)?;
    reg.channel(rule.freq_mhz)?;
    // World rule for channels 12 and 13: 20 MHz only.
    if bss.band == Band::Ghz2 && (bss.channel > 11 || secondary > 11) {
        return None;
    }
    Some(center)
}

/// `chan_to_phymode()` for the vdev.
pub fn vdev_phy_mode(band: Band, width: ChannelWidth) -> PhyMode {
    match (band, width) {
        (Band::Ghz2, ChannelWidth::NoHt20) => PhyMode::G11,
        (Band::Ghz2, ChannelWidth::Ht20) => PhyMode::NgHt20,
        (Band::Ghz2, ChannelWidth::Ht40 | ChannelWidth::Vht80) => PhyMode::NgHt40,
        (Band::Ghz5, ChannelWidth::NoHt20) => PhyMode::A11,
        (Band::Ghz5, ChannelWidth::Ht20) => PhyMode::NaHt20,
        (Band::Ghz5, ChannelWidth::Ht40) => PhyMode::NaHt40,
        (Band::Ghz5, ChannelWidth::Vht80) => PhyMode::AcVht80,
    }
}

/// `JoinInfo` from a plan.
pub fn join_info(bss: &ScanResult, plan: &ConnPlan, reg: &Regulatory) -> JoinInfo {
    let state = reg.channel(bss.freq_mhz);
    JoinInfo {
        bssid: bss.bssid,
        ssid: bss.ssid.clone(),
        freq_mhz: bss.freq_mhz,
        channel: bss.channel,
        band: bss.band,
        width: plan.width,
        center_freq_mhz: plan.center_freq_mhz,
        phy_mode: vdev_phy_mode(bss.band, plan.width),
        capability: bss.capability,
        beacon_interval_tu: bss.beacon_interval_tu,
        dtim_period: bss.dtim_period,
        short_slot: bss.band == Band::Ghz5 || bss.capability & CAP_SHORT_SLOT_TIME != 0,
        short_preamble: bss.capability & CAP_SHORT_PREAMBLE != 0,
        max_power_dbm: state.map_or(0, |s| s.max_power_dbm),
        passive: state.is_some_and(|s| s.passive),
        dfs: state.is_some_and(|s| s.dfs),
    }
}

/// The association request, in mac80211's element order (`ieee80211_send_assoc`, `ieee80211_add_link_elems`): SSID,
/// rates, power capability and supported channels when spectrum management is on, RSN, HT capabilities, WMM.
pub fn build_assoc_req(
    bss: &ScanResult,
    own: &MacAddr,
    hw: &HwCaps,
    plan: &ConnPlan,
    rsn_ie: Option<&[u8]>,
    max_power_dbm: u8,
) -> Vec<u8> {
    let mut f = mgmt_header(STYPE_ASSOC_REQ, &bss.bssid, own, &bss.bssid, 128);
    let mut capab = CAP_ESS;
    if bss.capability & CAP_PRIVACY != 0 {
        capab |= CAP_PRIVACY;
    }
    if bss.band == Band::Ghz2 {
        capab |= CAP_SHORT_SLOT_TIME | CAP_SHORT_PREAMBLE;
    }
    let spectrum = bss.capability & CAP_SPECTRUM_MGMT != 0 && hw.spectrum_mgmt;
    if spectrum {
        capab |= CAP_SPECTRUM_MGMT;
    }
    f.extend_from_slice(&capab.to_le_bytes());
    f.extend_from_slice(&hw.listen_interval.to_le_bytes());
    f.extend_from_slice(&[EID_SSID, bss.ssid.len() as u8]);
    f.extend_from_slice(&bss.ssid);
    put_rates(&mut f, &plan.rates);
    if spectrum {
        f.extend_from_slice(&[EID_PWR_CAPABILITY, 2, 0, max_power_dbm]);
        let channels: Vec<u8> = WORLD_CHANNELS
            .iter()
            .filter(|c| c.band == bss.band)
            .flat_map(|c| [c.channel, 1])
            .collect();
        f.extend_from_slice(&[EID_SUPPORTED_CHANNELS, channels.len() as u8]);
        f.extend_from_slice(&channels);
    }
    if let Some(ie) = rsn_ie {
        f.extend_from_slice(ie);
    }
    if let Some(ht) = &plan.ht {
        ht.put(&mut f, ht.cap_info);
    }
    if let Some(vht) = &plan.vht {
        vht.put(&mut f);
    }
    if plan.wmm {
        // qos_info 0: this station does not ask for U-APSD.
        put_wmm_info(&mut f, 0);
    }
    f
}

/// A directed or broadcast probe request, for checking on a silent AP.
pub fn build_probe_req(
    dst: &MacAddr,
    own: &MacAddr,
    ssid: &[u8],
    band: Band,
    plan: &ConnPlan,
) -> Vec<u8> {
    let mut f = mgmt_header(STYPE_PROBE_REQ, dst, own, dst, 64);
    f.extend_from_slice(&[EID_SSID, ssid.len() as u8]);
    f.extend_from_slice(ssid);
    put_rates(&mut f, device_rates(band));
    if let Some(ht) = &plan.ht {
        ht.put(&mut f, ht.cap_info);
    }
    f
}

/// mac80211's `ieee80211_set_wmm_default`: the EDCA parameters for interface up (`qos` false), disassociation (`qos`
/// false) and an association whose AP sent no valid WMM parameter set (`qos` true), BE, BK, VI, VO. `use_11b` is a
/// 2.4 GHz channel without `operating_11g_mode`, which makes aCWmin 31 instead of 15 ([`use_11b`]). The regulatory
/// WMM limit mac80211 applies on top (`ieee80211_regulatory_limit_wmm_params`) is deliberately not applied: no
/// regulatory or TXOP-limit change (owner's rule, audit D4).
pub fn wmm_default(qos: bool, use_11b: bool) -> [AcParams; 4] {
    // 802.11-2007 Table 7-37; values as cw, turned into the exponents the element carries
    let cw_min_a: u16 = if use_11b { 31 } else { 15 };
    let cw_max_a: u16 = 1023;
    let ac = |aifsn: u8, cw_min: u16, cw_max: u16, txop_limit: u16| AcParams {
        aifsn,
        acm: false,
        ecw_min: (cw_min + 1).trailing_zeros() as u8,
        ecw_max: (cw_max + 1).trailing_zeros() as u8,
        txop_limit,
    };
    if !qos {
        let d = ac(2, cw_min_a, cw_max_a, 0);
        return [d; 4];
    }
    let half = cw_min_a.div_ceil(2) - 1;
    let quarter = (cw_min_a + 1) / 4 - 1;
    [
        ac(3, cw_min_a, cw_max_a, 0),
        ac(7, cw_min_a, cw_max_a, 0),
        ac(2, half, cw_min_a, if use_11b { 6016 / 32 } else { 3008 / 32 }),
        ac(2, quarter, half, if use_11b { 3264 / 32 } else { 1504 / 32 }),
    ]
}

/// `use_11b` of `ieee80211_set_wmm_default`: a 2.4 GHz channel and no `operating_11g_mode`, which
/// `ieee80211_get_rates` sets when the BSS lists a rate above 11 Mb/s (`rates` in 500 kb/s units, basic bit allowed).
pub fn use_11b(band_2ghz: bool, rates: &[u8]) -> bool {
    band_2ghz && !rates.iter().any(|r| (r & 0x7f) > 22)
}

/// mac80211's checks on the AP's WMM parameter set (`_ieee80211_sta_wmm_params`): aifs below 2 becomes 2, and a
/// cw_min of 0, a cw_min above cw_max or a missing AC makes the whole set invalid (`None`), so the caller falls back
/// to [`wmm_default`]. The regulatory TXOP limit mac80211 applies here is deliberately not applied (no regulatory
/// change).
pub fn validate_wmm(params: &[AcParams; 4]) -> Option<[AcParams; 4]> {
    let mut out = *params;
    for ac in &mut out {
        if ac.aifsn < 2 {
            ac.aifsn = 2;
        }
        // ecw2cw: cw_min == 0 is ecw_min == 0, and cw_min > cw_max is ecw_min > ecw_max
        if ac.ecw_min == 0 || ac.ecw_min > ac.ecw_max {
            return None;
        }
    }
    Some(out)
}

/// The fixed part of an association response.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AssocRespBody<'a> {
    pub capability: u16,
    pub status: u16,
    pub aid: u16,
    pub ies: &'a [u8],
}

impl<'a> AssocRespBody<'a> {
    pub fn parse(body: &'a [u8]) -> Option<Self> {
        Some(Self {
            capability: crate::frame::le16_at(body, 0)?,
            status: crate::frame::le16_at(body, 2)?,
            // "The 5 MSB of the AID field are reserved for a non-S1G STA."
            aid: crate::frame::le16_at(body, 4)? & 0x07ff,
            ies: body.get(6..)?,
        })
    }
}

/// `ath10k_peer_assoc_prepare` from the association response, falling back to the beacon where mac80211 does for an
/// AP that leaves elements out of the response (`ieee80211_assoc_success`).
pub fn peer_info(
    bss: &ScanResult,
    plan: &ConnPlan,
    hw: &HwCaps,
    resp: &AssocRespBody<'_>,
) -> PeerInfo {
    let ap_rates = {
        let r = rates_of(resp.ies);
        if r.is_empty() {
            bss.supported_rates.clone()
        } else {
            r
        }
    };
    let legacy_rates: Vec<u8> = device_rates(bss.band)
        .iter()
        .copied()
        .filter(|r| ap_rates.iter().any(|a| a & 0x7f == *r))
        .collect();
    let basic_rates: Vec<u8> = legacy_rates
        .iter()
        .copied()
        .filter(|r| ap_rates.iter().any(|a| a & 0x80 != 0 && a & 0x7f == *r))
        .collect();

    let peer_caps = find(resp.ies, EID_HT_CAPABILITY)
        .and_then(HtCaps::parse)
        .or_else(|| bss.ht_cap.as_deref().and_then(HtCaps::parse));
    let ht = match (plan.ht.as_ref(), peer_caps) {
        (Some(own), Some(peer)) => {
            let merged = intersect_ht(own, &peer);
            let bw40 =
                matches!(plan.width, ChannelWidth::Ht40 | ChannelWidth::Vht80)
                    && merged.cap_info & HT_CAP_SUP_WIDTH_20_40 != 0;
            Some(peer_ht(&merged, bw40))
        }
        _ => None,
    };

    let peer_vht = find(resp.ies, EID_VHT_CAPABILITY)
        .and_then(VhtCaps::parse)
        .or_else(|| bss.vht_cap.as_deref().and_then(VhtCaps::parse));
    let vht = match (plan.vht.as_ref(), peer_vht, &ht) {
        (Some(own), Some(peer), Some(_)) => intersect_vht(own, &peer),
        _ => None,
    };

    // `ath10k_peer_assoc_h_phymode`.
    let phy_mode = match (bss.band, &ht) {
        (Band::Ghz5, Some(_)) if vht.is_some() => PhyMode::AcVht80,
        (Band::Ghz2, Some(h)) => {
            if h.bw40 {
                PhyMode::NgHt40
            } else {
                PhyMode::NgHt20
            }
        }
        (Band::Ghz5, Some(h)) => {
            if h.bw40 {
                PhyMode::NaHt40
            } else {
                PhyMode::NaHt20
            }
        }
        (Band::Ghz2, None) => {
            if legacy_rates.iter().any(|r| !matches!(*r, 2 | 4 | 11 | 22)) {
                PhyMode::G11
            } else {
                PhyMode::B11
            }
        }
        (Band::Ghz5, None) => PhyMode::A11,
    };

    let wmm = if plan.wmm {
        parse_wmm(resp.ies)
            .filter(|w| w.params.is_some())
            .or(bss.wmm)
    } else {
        None
    };
    PeerInfo {
        addr: bss.bssid,
        aid: if resp.aid == 0 || resp.aid > MAX_AID {
            0
        } else {
            resp.aid
        },
        capability: resp.capability,
        listen_interval: hw.listen_interval,
        need_ptk_4way: bss.rsn_ie.is_some(),
        qos: plan.wmm,
        legacy_rates,
        basic_rates,
        num_spatial_streams: ht.as_ref().map_or(1, |h| h.num_spatial_streams),
        ht,
        vht,
        phy_mode,
        wmm_params: wmm.and_then(|w| w.params).and_then(|p| validate_wmm(&p)),
    }
}
