//! Beacons and probe responses into a deduplicated table, and what the driver needs to run a scan.

use crate::caps::HwCaps;
use crate::frame::{
    CAP_ESS, CAP_PRIVACY, MacAddr, MgmtFrame, STYPE_BEACON, STYPE_PROBE_RESP, le16_at,
};
use crate::ie::{
    AKM_PSK, AKM_PSK_SHA256, AKM_SAE, CIPHER_CCMP, CIPHER_TKIP, EID_COUNTRY, EID_DS_PARAMS,
    EID_HT_CAPABILITY, EID_HT_OPERATION, EID_RSN, EID_SSID, EID_TIM, EID_VHT_CAPABILITY,
    EID_VHT_OPERATION, HtOperation, RsnInfo, WmmInfo, elements, find, find_with_header, has_wpa1,
    parse_wmm, put_rates, rates_of, ssid_is_hidden,
};
use crate::regulatory::{Band, Regulatory, rule_for_channel, rule_for_freq};
use alloc::vec::Vec;

/// What a BSS demands. Anything this station cannot join on PSK is `Wpa3Only` or `Other`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Security {
    Open,
    /// AKM 00-0F-AC:2, pairwise CCMP, group CCMP or TKIP. Transition-mode APs that also offer SAE land here.
    Wpa2Psk,
    /// AKM 00-0F-AC:6 and not AKM 2, pairwise CCMP.
    Wpa2PskSha256,
    /// SAE and no PSK AKM.
    Wpa3Only,
    /// WEP, WPA1, 802.1X, TKIP-only pairwise, GCMP and everything else.
    Other,
}

/// One BSS as last heard.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanResult {
    pub bssid: MacAddr,
    pub ssid: Vec<u8>,
    pub freq_mhz: u16,
    pub channel: u8,
    pub band: Band,
    pub rssi_dbm: i8,
    pub security: Security,
    /// The whole RSN element, header included. Message 3 of the 4-way must repeat exactly these bytes.
    pub rsn_ie: Option<Vec<u8>>,
    pub rsn: Option<RsnInfo>,
    pub beacon_interval_tu: u16,
    pub capability: u16,
    /// As sent, the basic bit included, Supported and Extended Supported together.
    pub supported_rates: Vec<u8>,
    pub ht_cap: Option<Vec<u8>>,
    pub ht_oper: Option<Vec<u8>>,
    pub vht_cap: Option<Vec<u8>>,
    pub vht_oper: Option<Vec<u8>>,
    pub country: Option<Vec<u8>>,
    pub wmm: Option<WmmInfo>,
    /// From the TIM element; 1 until a beacon has been seen.
    pub dtim_period: u8,
    pub last_seen_ms: u64,
}

impl ScanResult {
    /// Parses a beacon or probe response heard at `freq_mhz`. `None` if it is neither, is malformed, or claims a
    /// channel we do not use.
    pub fn parse(frame: &[u8], rssi_dbm: i8, freq_mhz: u16, now_ms: u64) -> Option<Self> {
        let f = MgmtFrame::parse(frame)?;
        if f.subtype() != STYPE_BEACON && f.subtype() != STYPE_PROBE_RESP {
            return None;
        }
        // Timestamp 8, beacon interval 2, capability 2.
        let beacon_interval_tu = le16_at(f.body, 8)?;
        let capability = le16_at(f.body, 10)?;
        let ies = f.body.get(12..)?;
        let ssid = find(ies, EID_SSID)?;
        if ssid.len() > 32 {
            return None;
        }

        let rx_rule = rule_for_freq(freq_mhz)?;
        // `cfg80211_get_bss_channel()`: the frame's own channel wins over where it was heard, but on 2.4 GHz a
        // frame claiming a channel we do not have is dropped rather than taken as the channel it was heard on.
        let ht_oper_raw = find(ies, EID_HT_OPERATION);
        let claimed = find(ies, EID_DS_PARAMS)
            .and_then(|d| d.first().copied())
            .or_else(|| {
                ht_oper_raw
                    .and_then(HtOperation::parse)
                    .map(|o| o.primary_channel)
            });
        let rule = match claimed {
            Some(c) if c != rx_rule.channel => match rule_for_channel(rx_rule.band, c) {
                Some(r) => r,
                None if rx_rule.band == Band::Ghz2 => return None,
                None => rx_rule,
            },
            _ => rx_rule,
        };

        let rsn_ie = find_with_header(ies, EID_RSN);
        let rsn = rsn_ie.and_then(|ie| RsnInfo::parse(&ie[2..]));
        let dtim_period = find(ies, EID_TIM)
            .and_then(|t| t.get(1).copied())
            .map_or(1, |d| d.max(1));

        Some(Self {
            bssid: f.bssid,
            ssid: Vec::from(ssid),
            freq_mhz: rule.freq_mhz,
            channel: rule.channel,
            band: rule.band,
            rssi_dbm,
            security: classify(capability, ies, rsn_ie.is_some(), rsn.as_ref()),
            rsn_ie: rsn_ie.map(Vec::from),
            rsn,
            beacon_interval_tu,
            capability,
            supported_rates: rates_of(ies),
            ht_cap: find(ies, EID_HT_CAPABILITY).map(Vec::from),
            ht_oper: ht_oper_raw.map(Vec::from),
            vht_cap: find(ies, EID_VHT_CAPABILITY).map(Vec::from),
            vht_oper: find(ies, EID_VHT_OPERATION).map(Vec::from),
            country: find(ies, EID_COUNTRY).map(Vec::from),
            wmm: parse_wmm(ies),
            dtim_period,
            last_seen_ms: now_ms,
        })
    }

    pub fn is_ess(&self) -> bool {
        self.capability & CAP_ESS != 0
    }

    /// True when the AP insists on protected management frames, which this station does not do.
    pub fn pmf_required(&self) -> bool {
        self.rsn.as_ref().is_some_and(RsnInfo::mfp_required)
    }

    /// True if the AP offers this AKM.
    pub fn offers_akm(&self, akm: u32) -> bool {
        self.rsn.as_ref().is_some_and(|r| r.akm.contains(&akm))
    }
}

fn classify(capability: u16, ies: &[u8], has_rsn_ie: bool, rsn: Option<&RsnInfo>) -> Security {
    if !has_rsn_ie {
        // No RSN: an open network if privacy is off. WPA1 and WEP both set the privacy bit.
        return if capability & CAP_PRIVACY == 0 && !has_wpa1(ies) {
            Security::Open
        } else {
            Security::Other
        };
    }
    let Some(rsn) = rsn else {
        return Security::Other;
    };
    let psk = rsn.akm.contains(&AKM_PSK);
    let psk256 = rsn.akm.contains(&AKM_PSK_SHA256);
    let ciphers_ok = rsn.pairwise.contains(&CIPHER_CCMP)
        && (rsn.group == CIPHER_CCMP || rsn.group == CIPHER_TKIP);
    match (psk, psk256) {
        (true, _) if ciphers_ok => Security::Wpa2Psk,
        (false, true) if ciphers_ok => Security::Wpa2PskSha256,
        (false, false) if rsn.akm.contains(&AKM_SAE) => Security::Wpa3Only,
        _ => Security::Other,
    }
}

/// Maximum entries kept. Air is hostile, so the table is bounded; a full table evicts its oldest, weakest entry.
pub const SCAN_TABLE_MAX: usize = 64;
/// `IEEE80211_SCAN_RESULT_EXPIRE`, 30 s.
pub const SCAN_RESULT_EXPIRE_MS: u64 = 30_000;

/// The table of BSSes heard, one entry per BSSID and channel.
#[derive(Clone, Debug, Default)]
pub struct ScanTable {
    entries: Vec<ScanResult>,
}

impl ScanTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds or refreshes an entry. A beacon that hides its SSID does not overwrite one a probe response told us.
    pub fn update(&mut self, mut new: ScanResult) {
        if let Some(old) = self
            .entries
            .iter_mut()
            .find(|e| e.bssid == new.bssid && e.freq_mhz == new.freq_mhz)
        {
            if ssid_is_hidden(&new.ssid) && !ssid_is_hidden(&old.ssid) {
                new.ssid = core::mem::take(&mut old.ssid);
            }
            *old = new;
            return;
        }
        if self.entries.len() >= SCAN_TABLE_MAX {
            let victim = self
                .entries
                .iter()
                .enumerate()
                .min_by_key(|(_, e)| (e.last_seen_ms, e.rssi_dbm))
                .map(|(i, _)| i);
            if let Some(i) = victim {
                self.entries.swap_remove(i);
            }
        }
        self.entries.push(new);
    }

    pub fn expire(&mut self, now_ms: u64) {
        self.entries
            .retain(|e| now_ms.saturating_sub(e.last_seen_ms) < SCAN_RESULT_EXPIRE_MS);
    }

    pub fn entries(&self) -> &[ScanResult] {
        &self.entries
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    pub fn iter(&self) -> impl Iterator<Item = &ScanResult> {
        self.entries.iter()
    }
}

/// One channel to visit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScanChannel {
    pub freq_mhz: u16,
    /// Listen only: no probe requests here.
    pub passive: bool,
}

/// One band's part of a scan. mac80211 runs a scan one band at a time on drivers without
/// `SINGLE_SCAN_ON_ALL_BANDS`, which ath10k does not set, since the probe IEs differ by band. Each leg is one
/// `ath10k_start_scan`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanLeg {
    pub band: Band,
    pub channels: Vec<ScanChannel>,
    /// Extra IEs for probe requests (`arg->ie`): the firmware adds SSID itself from `ScanRequest::ssids`.
    pub ies: Vec<u8>,
}

/// What the driver needs to run a scan. Dwell times and the rest are `ath10k_wmi_start_scan_init()`'s defaults.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanRequest {
    /// One SSID, empty for the wildcard. Firmware sends a probe request per SSID on active channels.
    pub ssids: Vec<Vec<u8>>,
    pub legs: Vec<ScanLeg>,
    pub dwell_active_ms: u32,
    pub dwell_passive_ms: u32,
    pub min_rest_ms: u32,
    pub max_rest_ms: u32,
    pub probe_delay_ms: u32,
    pub max_scan_ms: u32,
}

/// Device rates in 500 kb/s units, in the order of `ath10k_rates`: 2.4 GHz has CCK then OFDM, 5 GHz OFDM only.
pub const RATES_2GHZ: [u8; 12] = [2, 4, 11, 22, 12, 18, 24, 36, 48, 72, 96, 108];
pub const RATES_5GHZ: [u8; 8] = [12, 18, 24, 36, 48, 72, 96, 108];

pub fn device_rates(band: Band) -> &'static [u8] {
    match band {
        Band::Ghz2 => &RATES_2GHZ,
        Band::Ghz5 => &RATES_5GHZ,
    }
}

/// Probe-request IEs for a band: `ieee80211_build_preq_ies_band()` on a device that does not set
/// `NL80211_FEATURE_DS_PARAM_SET_IE_IN_PROBES`, so rates and HT capabilities.
pub fn probe_ies(band: Band, hw: &HwCaps) -> Vec<u8> {
    let mut ies = Vec::with_capacity(52);
    put_rates(&mut ies, device_rates(band));
    if let Some(ht) = &hw.ht {
        ht.put(&mut ies, ht.cap_info);
    }
    ies
}

/// Builds a scan over the channels the regulatory state allows and the hardware has.
pub fn build_scan_request(ssid: Option<&[u8]>, hw: &HwCaps, reg: &Regulatory) -> ScanRequest {
    let channels = reg.channels();
    let mut legs = Vec::new();
    for (band, on) in [(Band::Ghz2, hw.band_2ghz), (Band::Ghz5, hw.band_5ghz)] {
        if !on {
            continue;
        }
        let list: Vec<ScanChannel> = channels
            .iter()
            .filter(|c| c.band == band)
            .map(|c| ScanChannel {
                freq_mhz: c.freq_mhz,
                passive: c.passive,
            })
            .collect();
        if !list.is_empty() {
            legs.push(ScanLeg {
                band,
                channels: list,
                ies: probe_ies(band, hw),
            });
        }
    }
    ScanRequest {
        ssids: Vec::from([Vec::from(ssid.unwrap_or(&[]))]),
        legs,
        dwell_active_ms: 50,
        dwell_passive_ms: 150,
        min_rest_ms: 50,
        max_rest_ms: 500,
        probe_delay_ms: 5,
        max_scan_ms: 20_000,
    }
}

/// True if any element of `ies` could not be walked to the end. For tests and diagnostics.
pub fn ies_complete(ies: &[u8]) -> bool {
    let consumed: usize = elements(ies).map(|(_, b)| 2 + b.len()).sum();
    consumed == ies.len()
}
