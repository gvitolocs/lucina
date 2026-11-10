//! Information elements: walking them, parsing RSN and WMM and HT, and building what a station sends.

use crate::frame::le16_at;
use alloc::vec::Vec;

pub const EID_SSID: u8 = 0;
pub const EID_SUPP_RATES: u8 = 1;
pub const EID_DS_PARAMS: u8 = 3;
pub const EID_TIM: u8 = 5;
pub const EID_COUNTRY: u8 = 7;
pub const EID_PWR_CAPABILITY: u8 = 33;
pub const EID_SUPPORTED_CHANNELS: u8 = 36;
pub const EID_HT_CAPABILITY: u8 = 45;
pub const EID_RSN: u8 = 48;
pub const EID_EXT_SUPP_RATES: u8 = 50;
pub const EID_TIMEOUT_INTERVAL: u8 = 56;
pub const EID_HT_OPERATION: u8 = 61;
pub const EID_VHT_CAPABILITY: u8 = 191;
pub const EID_VHT_OPERATION: u8 = 192;
pub const EID_VENDOR: u8 = 221;

/// Walks elements. Stops at the first one that runs past the end, keeping what came before, as mac80211 does
/// (`ieee802_11_parse_elems`) with its `parse_error`.
#[derive(Clone)]
pub struct Elements<'a> {
    rest: &'a [u8],
}

pub fn elements(ies: &[u8]) -> Elements<'_> {
    Elements { rest: ies }
}

impl<'a> Iterator for Elements<'a> {
    type Item = (u8, &'a [u8]);

    fn next(&mut self) -> Option<Self::Item> {
        let id = *self.rest.first()?;
        let len = *self.rest.get(1)? as usize;
        let body = self.rest.get(2..2 + len)?;
        self.rest = &self.rest[2 + len..];
        Some((id, body))
    }
}

/// The first element with this id.
pub fn find(ies: &[u8], id: u8) -> Option<&[u8]> {
    elements(ies).find(|(i, _)| *i == id).map(|(_, b)| b)
}

/// The whole element, header included, for the RSN IE comparison of message 3.
pub fn find_with_header(ies: &[u8], id: u8) -> Option<&[u8]> {
    let mut offset = 0;
    for (i, b) in elements(ies) {
        if i == id {
            return ies.get(offset..offset + 2 + b.len());
        }
        offset += 2 + b.len();
    }
    None
}

/// An AKM or cipher suite selector, in network byte order (`00-0F-AC:n` is 0x000FAC00 | n).
pub type Suite = u32;

pub const CIPHER_TKIP: Suite = 0x000f_ac02;
pub const CIPHER_CCMP: Suite = 0x000f_ac04;
pub const AKM_8021X: Suite = 0x000f_ac01;
pub const AKM_PSK: Suite = 0x000f_ac02;
pub const AKM_PSK_SHA256: Suite = 0x000f_ac06;
pub const AKM_SAE: Suite = 0x000f_ac08;

pub const RSN_CAP_MFPR: u16 = 1 << 6;
pub const RSN_CAP_MFPC: u16 = 1 << 7;

/// A parsed RSN element. Missing trailing fields take the defaults of 802.11-2020 9.4.2.24.1, as in
/// `wpa_parse_wpa_ie_rsn()`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RsnInfo {
    pub group: Suite,
    pub pairwise: Vec<Suite>,
    pub akm: Vec<Suite>,
    pub capabilities: u16,
}

impl RsnInfo {
    /// `data` is the element's body, without id and length.
    pub fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < 2 || le16_at(data, 0)? != 1 {
            return None;
        }
        let mut info = Self {
            group: CIPHER_CCMP,
            pairwise: Vec::from([CIPHER_CCMP]),
            akm: Vec::from([AKM_8021X]),
            capabilities: 0,
        };
        let mut r = &data[2..];
        if r.len() >= 4 {
            info.group = suite(r);
            r = &r[4..];
        } else if !r.is_empty() {
            return None;
        }
        if let Some((list, rest)) = suite_list(r)? {
            info.pairwise = list;
            r = rest;
        }
        if let Some((list, rest)) = suite_list(r)? {
            info.akm = list;
            r = rest;
        }
        if r.len() >= 2 {
            info.capabilities = le16_at(r, 0)?;
        }
        Some(info)
    }

    pub fn mfp_capable(&self) -> bool {
        self.capabilities & RSN_CAP_MFPC != 0
    }

    pub fn mfp_required(&self) -> bool {
        self.capabilities & RSN_CAP_MFPR != 0
    }
}

fn suite(b: &[u8]) -> Suite {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

/// `Ok(None)` shape: the list is absent when nothing is left; truncated or empty lists are errors.
fn suite_list(r: &[u8]) -> Option<Option<(Vec<Suite>, &[u8])>> {
    if r.is_empty() {
        return Some(None);
    }
    let count = le16_at(r, 0)? as usize;
    if count == 0 || count > 32 {
        return None;
    }
    let body = r.get(2..2 + count * 4)?;
    let list = body.as_chunks::<4>().0.iter().map(|c| suite(c)).collect();
    Some(Some((list, &r[2 + count * 4..])))
}

/// The RSN element a station sends, with one pairwise (CCMP) and one AKM, and no capability bits: PMF is out of
/// scope. Same bytes as `wpa_gen_wpa_ie_rsn()` with PMF off.
pub fn build_rsn_ie(group: Suite, akm: Suite) -> Vec<u8> {
    let mut ie = Vec::with_capacity(22);
    ie.extend_from_slice(&[EID_RSN, 20, 1, 0]);
    ie.extend_from_slice(&group.to_be_bytes());
    ie.extend_from_slice(&[1, 0]);
    ie.extend_from_slice(&CIPHER_CCMP.to_be_bytes());
    ie.extend_from_slice(&[1, 0]);
    ie.extend_from_slice(&akm.to_be_bytes());
    ie.extend_from_slice(&[0, 0]);
    ie
}

/// The WPA vendor element (`00-50-F2:1`), which means WPA1: not something this station joins.
pub fn has_wpa1(ies: &[u8]) -> bool {
    elements(ies).any(|(id, b)| {
        id == EID_VENDOR && b.len() >= 4 && b[..3] == [0x00, 0x50, 0xf2] && b[3] == 1
    })
}

/// One access category's parameters from a WMM Parameter element.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AcParams {
    pub aifsn: u8,
    pub acm: bool,
    pub ecw_min: u8,
    pub ecw_max: u8,
    pub txop_limit: u16,
}

/// What a BSS says about WMM.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WmmInfo {
    pub qos_info: u8,
    /// Indexed by ACI as sent: BE, BK, VI, VO. `None` when the AP sent only a WMM Information element.
    pub params: Option<[AcParams; 4]>,
}

/// Finds a WMM Information or Parameter element (`00-50-F2:2`, subtype 0 or 1, version 1).
pub fn parse_wmm(ies: &[u8]) -> Option<WmmInfo> {
    for (id, b) in elements(ies) {
        if id != EID_VENDOR || b.len() < 7 || b[..4] != [0x00, 0x50, 0xf2, 2] || b[5] != 1 {
            continue;
        }
        match b[4] {
            0 => {
                return Some(WmmInfo {
                    qos_info: b[6],
                    params: None,
                });
            }
            1 if b.len() >= 24 => {
                let mut params = [AcParams {
                    aifsn: 0,
                    acm: false,
                    ecw_min: 0,
                    ecw_max: 0,
                    txop_limit: 0,
                }; 4];
                for rec in b[8..24].as_chunks::<4>().0 {
                    let aci = ((rec[0] >> 5) & 3) as usize;
                    params[aci] = AcParams {
                        aifsn: rec[0] & 0x0f,
                        acm: rec[0] & 0x10 != 0,
                        ecw_min: rec[1] & 0x0f,
                        ecw_max: rec[1] >> 4,
                        txop_limit: u16::from_le_bytes([rec[2], rec[3]]),
                    };
                }
                return Some(WmmInfo {
                    qos_info: b[6],
                    params: Some(params),
                });
            }
            _ => {}
        }
    }
    None
}

/// `ieee80211_add_wmm_info_ie()`.
pub fn put_wmm_info(out: &mut Vec<u8>, qos_info: u8) {
    out.extend_from_slice(&[EID_VENDOR, 7, 0x00, 0x50, 0xf2, 2, 0, 1, qos_info]);
}

/// HT capabilities, the 26 bytes of `struct ieee80211_ht_cap`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HtCaps {
    pub cap_info: u16,
    /// A-MPDU parameters: factor in bits 0-1, density in bits 2-4.
    pub ampdu_params: u8,
    /// RX MCS bitmask, bits 0-76.
    pub rx_mask: [u8; 10],
    pub rx_highest: u16,
    pub tx_params: u8,
}

pub const HT_CAP_LDPC_CODING: u16 = 0x0001;
pub const HT_CAP_SUP_WIDTH_20_40: u16 = 0x0002;
pub const HT_CAP_SM_PS: u16 = 0x000c;
pub const HT_CAP_SM_PS_SHIFT: u16 = 2;
pub const HT_CAP_GRN_FLD: u16 = 0x0010;
pub const HT_CAP_SGI_20: u16 = 0x0020;
pub const HT_CAP_SGI_40: u16 = 0x0040;
pub const HT_CAP_TX_STBC: u16 = 0x0080;
pub const HT_CAP_RX_STBC: u16 = 0x0300;
pub const HT_CAP_RX_STBC_SHIFT: u16 = 8;
pub const HT_CAP_MAX_AMSDU: u16 = 0x0800;
pub const HT_CAP_DSSSCCK40: u16 = 0x1000;
pub const HT_CAP_LSIG_TXOP_PROT: u16 = 0x8000;
/// `WLAN_HT_CAP_SM_PS_DISABLED`, in place.
pub const HT_CAP_SM_PS_DISABLED: u16 = 3 << HT_CAP_SM_PS_SHIFT;

pub const HT_MCS_TX_DEFINED: u8 = 0x01;
pub const HT_MCS_TX_RX_DIFF: u8 = 0x02;
pub const HT_MCS_TX_MAX_STREAMS_MASK: u8 = 0x0c;
pub const HT_MCS_TX_MAX_STREAMS_SHIFT: u8 = 2;
pub const HT_MCS_TX_UNEQUAL_MODULATION: u8 = 0x10;

impl HtCaps {
    /// `data` is the element's body.
    pub fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < 26 {
            return None;
        }
        let mut rx_mask = [0; 10];
        rx_mask.copy_from_slice(&data[3..13]);
        Some(Self {
            cap_info: le16_at(data, 0)?,
            ampdu_params: data[2],
            rx_mask,
            rx_highest: le16_at(data, 13)?,
            tx_params: data[15],
        })
    }

    pub fn ampdu_factor(&self) -> u8 {
        self.ampdu_params & 0x03
    }

    pub fn ampdu_density(&self) -> u8 {
        (self.ampdu_params >> 2) & 0x07
    }

    /// `ieee80211_ie_build_ht_cap()`, with `cap_info` as passed: the extended capabilities, beamforming and
    /// antenna selection fields are zero, because the hardware's are not advertised by ath10k either.
    pub fn put(&self, out: &mut Vec<u8>, cap_info: u16) {
        out.extend_from_slice(&[EID_HT_CAPABILITY, 26]);
        out.extend_from_slice(&cap_info.to_le_bytes());
        out.push(self.ampdu_params);
        out.extend_from_slice(&self.rx_mask);
        out.extend_from_slice(&self.rx_highest.to_le_bytes());
        out.push(self.tx_params);
        out.extend_from_slice(&[0; 3]);
        out.extend_from_slice(&[0; 2]);
        out.extend_from_slice(&[0; 4]);
        out.push(0);
    }
}

/// The fields a station uses from the HT Operation element.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HtOperation {
    pub primary_channel: u8,
    pub secondary_offset: SecondaryOffset,
    pub sta_chan_width_any: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SecondaryOffset {
    None,
    Above,
    Below,
}

impl HtOperation {
    pub fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < 22 {
            return None;
        }
        let param = data[1];
        Some(Self {
            primary_channel: data[0],
            secondary_offset: match param & 0x03 {
                1 => SecondaryOffset::Above,
                3 => SecondaryOffset::Below,
                _ => SecondaryOffset::None,
            },
            sta_chan_width_any: param & 0x04 != 0,
        })
    }
}

/// Writes `rates` (500 kb/s units, flags as given) as Supported Rates and, past eight, Extended Supported Rates.
pub fn put_rates(out: &mut Vec<u8>, rates: &[u8]) {
    let (first, second) = rates.split_at(rates.len().min(8));
    out.extend_from_slice(&[EID_SUPP_RATES, first.len() as u8]);
    out.extend_from_slice(first);
    if !second.is_empty() {
        out.extend_from_slice(&[EID_EXT_SUPP_RATES, second.len() as u8]);
        out.extend_from_slice(second);
    }
}

/// All rates from Supported and Extended Supported Rates, as sent, the basic bit included.
pub fn rates_of(ies: &[u8]) -> Vec<u8> {
    let mut rates = Vec::new();
    for (id, b) in elements(ies) {
        if id == EID_SUPP_RATES || id == EID_EXT_SUPP_RATES {
            rates.extend_from_slice(b);
        }
    }
    rates
}

/// True for an SSID hidden by blanking: empty, or all zero bytes.
pub(crate) fn ssid_is_hidden(ssid: &[u8]) -> bool {
    ssid.iter().all(|b| *b == 0)
}

/// `struct ieee80211_vht_cap`, the body of the VHT Capabilities element.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VhtCaps {
    pub cap: u32,
    pub rx_mcs_map: u16,
    pub rx_highest: u16,
    pub tx_mcs_map: u16,
    pub tx_highest: u16,
}

impl VhtCaps {
    pub fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < 12 {
            return None;
        }
        let u16_at = |at: usize| u16::from_le_bytes([data[at], data[at + 1]]);
        Some(Self {
            cap: u32::from_le_bytes([data[0], data[1], data[2], data[3]]),
            rx_mcs_map: u16_at(4),
            rx_highest: u16_at(6),
            tx_mcs_map: u16_at(8),
            tx_highest: u16_at(10),
        })
    }

    /// `ieee80211_ie_build_vht_cap()`.
    pub fn put(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&[EID_VHT_CAPABILITY, 12]);
        out.extend_from_slice(&self.cap.to_le_bytes());
        out.extend_from_slice(&self.rx_mcs_map.to_le_bytes());
        out.extend_from_slice(&self.rx_highest.to_le_bytes());
        out.extend_from_slice(&self.tx_mcs_map.to_le_bytes());
        out.extend_from_slice(&self.tx_highest.to_le_bytes());
    }
}

/// `struct ieee80211_vht_operation`, what the width says. `chan_width` is 0 for 20 or 40 MHz, 1 for 80 MHz and up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VhtOperation {
    pub chan_width: u8,
    /// The channel at the centre of the 80 MHz that hold the primary channel.
    pub center_seg0: u8,
}

impl VhtOperation {
    pub fn parse(data: &[u8]) -> Option<Self> {
        Some(Self {
            chan_width: *data.first()?,
            center_seg0: *data.get(1)?,
        })
    }
}
